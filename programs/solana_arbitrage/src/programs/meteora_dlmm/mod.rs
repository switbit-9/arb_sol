pub mod dlmm_lib;

use crate::programs::programs::{PoolKind, ProgramMeta};
use crate::programs::SolarBError;
use crate::utils::token::MintFee;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke_unchecked,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use dlmm_lib::constants::{BASIS_POINT_MAX, FEE_PRECISION};
use dlmm_lib::dlmm::accounts::{BinArrayBitmapExtension, LbPair};
use dlmm_lib::dlmm::types::Bin;
use dlmm_lib::extensions::{BinExtension, LbPairExtension};
use dlmm_lib::math::price_math::get_price_from_id;
use dlmm_lib::math::u64x64_math::ONE;
use dlmm_lib::quote::{get_active_bin_array, quote_exact_in, quote_exact_out, LbPairSlim, SwapCache};

pub const SCALE_OFFSET: u8 = 64;
const BIN_ARRAY_HEADER_SIZE: usize = 56;
const BIN_SIZE: usize = 144;
const MAX_BIN_PER_ARRAY: usize = 70;

pub const PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");
const SWAP_EXACT_IN_DISC: [u8; 8] = [65, 75, 63, 76, 235, 91, 91, 136];
const SWAP_EXACT_OUT_DISC: [u8; 8] = [43, 215, 247, 132, 137, 60, 243, 81];
// Static accounts (from static_base, 3 accounts)
pub const S_PROGRAM_ID: usize = 0;
pub const S_HOST_FEE_IN: usize = 1; // same key as PROGRAM_ID when no host fee
pub const S_EVENT_AUTHORITY: usize = 2;

// Dynamic accounts (from dyn_start, 9 accounts)
pub const D_POOL: usize = 0;
pub const D_BASE_VAULT: usize = 1;
pub const D_QUOTE_VAULT: usize = 2;
pub const D_ORACLE: usize = 3;
pub const D_BITMAP_EXT: usize = 4;
pub const D_BIN_BUY_0: usize = 5;
pub const D_BIN_BUY_1: usize = 6;
pub const D_BIN_SELL_0: usize = 7;
pub const D_BIN_SELL_1: usize = 8;

pub const MIN_ACCOUNTS: usize = 9;

/// Precomputed Q64 scale factor (2^64) for price calculations
/// Avoids recomputing `(1u128 << 64) as f64` on every call
const Q64_SCALE: f64 = 18446744073709551616.0; // (1u128 << SCALE_OFFSET) as f64

fn compute_fee_numerator(lb_pair: &LbPair) -> anyhow::Result<u64> {
    Ok(lb_pair.get_total_fee()? as u64)
}

fn compute_fee_rate(lb_pair: &LbPair) -> anyhow::Result<f64> {
    let total_fee_rate = lb_pair.get_total_fee()?;
    Ok(total_fee_rate as f64 / FEE_PRECISION as f64)
}

/// Compute price as f64 from lb_price (Q64-scaled).
fn get_price_f64(lb_price: u128) -> f64 {
    lb_price as f64 / (1u128 << 64) as f64
}

#[derive(Clone)]
pub struct MeteoraDlmm {
    pub pool_id: Pubkey,
    pub base_token_pk: Pubkey,
    pub quote_token_pk: Pubkey,
    /// Slim copy of LbPair (~28 bytes) with only swap-relevant fields
    pub lb_pair_slim: LbPairSlim,
    pub active_bin: Bin,
    pub lb_price: u128,
    pub price: f64,
    pub static_base: usize,
    pub dyn_start: usize,
    pub fee_numerator: u64,
    /// Pre-computed fee factor: 1 - fee_numerator/FEE_PRECISION
    pub fee_factor: (f64, f64),
    /// Cached from init: base→quote (X→Y) buy bins
    pub buy_max_in: u64,
    pub buy_max_out: u64,
    /// Cached from init: quote→base (Y→X) sell bins
    pub sell_max_in: u64,
    pub sell_max_out: u64,
    /// SwapCache for buy direction (swap_for_y = true) — lazily built on first swap_base_in call
    pub buy_swap_cache: Option<Box<SwapCache>>,
    /// SwapCache for sell direction (swap_for_y = false) — lazily built on first swap_base_in call
    pub sell_swap_cache: Option<Box<SwapCache>>,
}

impl ProgramMeta for MeteoraDlmm {
    fn get_id(&self) -> &Pubkey {
        &PROGRAM_ID
    }

    fn get_pool_id(&self) -> &Pubkey {
        &self.pool_id
    }

    fn swap_base_in<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        clock: &Clock,
    ) -> Result<u64> {
        self.prepare_for_execution(accounts, clock)?;
        let swap_for_y = input_mint == self.base_token_pk;
        let (bin_arr_0, bin_arr_1, cache) = if swap_for_y {
            (D_BIN_BUY_0, D_BIN_BUY_1, self.buy_swap_cache.as_ref().unwrap())
        } else {
            (D_BIN_SELL_0, D_BIN_SELL_1, self.sell_swap_cache.as_ref().unwrap())
        };
        let bin_arrays: &[AccountInfo; 2] = <&[AccountInfo; 2]>::try_from(
            &accounts[self.dyn_start + bin_arr_0..self.dyn_start + bin_arr_1 + 1]
        ).map_err(|_| ProgramError::InvalidAccountData)?;

        // quote functions expect (base_fee, quote_fee), reconstruct from directional fees
        let (base_fee, quote_fee) = if swap_for_y {
            (input_transfer_fee, output_transfer_fee)
        } else {
            (output_transfer_fee, input_transfer_fee)
        };

        let quote = {
            quote_exact_in(
                &self.lb_pair_slim,
                amount_in,
                swap_for_y,
                bin_arrays,
                base_fee,
                quote_fee,
                cache,
            )
        }
        .map_err(|_e| {
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!("ERROR in quote_exact_in: {}", _e);
            msg!("DLMM quote_exact_in failed");
            anchor_lang::error::Error::from(ProgramError::Custom(2004))
        })?;
        Ok(quote.amount_out)
    }

    fn swap_base_out<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        clock: &Clock,
    ) -> Result<u64> {
        self.prepare_for_execution(accounts, clock)?;
        let swap_for_y = output_mint == self.quote_token_pk;

        let (bin_arr_0, bin_arr_1, cache) = if swap_for_y {
            (D_BIN_BUY_0, D_BIN_BUY_1, self.buy_swap_cache.as_ref().unwrap())
        } else {
            (D_BIN_SELL_0, D_BIN_SELL_1, self.sell_swap_cache.as_ref().unwrap())
        };
        let bin_arrays: &[AccountInfo; 2] = <&[AccountInfo; 2]>::try_from(
            &accounts[self.dyn_start + bin_arr_0..self.dyn_start + bin_arr_1 + 1]
        ).map_err(|_| ProgramError::InvalidAccountData)?;

        let (base_fee, quote_fee) = if swap_for_y {
            (input_transfer_fee, output_transfer_fee)
        } else {
            (output_transfer_fee, input_transfer_fee)
        };

        let quote = {
            quote_exact_out(
                &self.lb_pair_slim,
                amount_out,
                swap_for_y,
                bin_arrays,
                base_fee,
                quote_fee,
                cache,
            )
        }
        .map_err(|_e| {
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!("ERROR in quote_exact_out: {}", _e);
            msg!("DLMM quote_exact_out failed");
            anchor_lang::error::Error::from(ProgramError::Custom(2004))
        })?;
        Ok(quote.amount_in)
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        let inverse = if self.price > 0.0 { 1.0 / self.price } else { 0.0 };
        Ok((self.price, inverse))
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (&self.base_token_pk, &self.quote_token_pk)
    }

    fn name(&self) -> &'static str { "MeteoraDLMM" }
    fn pool_kind(&self) -> PoolKind { PoolKind::MeteoraDlmm }

    fn get_max_amount_in<'a>(&self, accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        MeteoraDlmm::get_max_amount_in(self, accounts, mint)
    }

    fn get_max_amount_out<'a>(&self, accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        MeteoraDlmm::get_max_amount_out(self, accounts, mint)
    }

    fn get_cached_max_amounts(&self, input_mint: Pubkey) -> (u64, u64) {
        if input_mint == self.base_token_pk { (self.buy_max_in, self.buy_max_out) } else { (self.sell_max_in, self.sell_max_out) }
    }

    #[cfg(any(test, feature = "debug"))]
    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        msg!("=== Meteora DLMM ===");
        msg!("[static] S0 program_id: {}", accounts[self.static_base + S_PROGRAM_ID].key);
        msg!("[static] S1 host_fee_in: {}", accounts[self.static_base + S_HOST_FEE_IN].key);
        msg!("[static] S2 event_authority: {}", accounts[self.static_base + S_EVENT_AUTHORITY].key);
        msg!("[dynamic] D0 pool: {}", accounts[self.dyn_start + D_POOL].key);
        msg!("[dynamic] D1 base_vault: {}", accounts[self.dyn_start + D_BASE_VAULT].key);
        msg!("[dynamic] D2 quote_vault: {}", accounts[self.dyn_start + D_QUOTE_VAULT].key);
        msg!("[dynamic] D3 oracle: {}", accounts[self.dyn_start + D_ORACLE].key);
        msg!("[dynamic] D4 bitmap_ext: {}", accounts[self.dyn_start + D_BITMAP_EXT].key);
        msg!("[dynamic] D5 bin_buy_0: {}", accounts[self.dyn_start + D_BIN_BUY_0].key);
        msg!("[dynamic] D6 bin_buy_1: {}", accounts[self.dyn_start + D_BIN_BUY_1].key);
        msg!("[dynamic] D7 bin_sell_0: {}", accounts[self.dyn_start + D_BIN_SELL_0].key);
        msg!("[dynamic] D8 bin_sell_1: {}", accounts[self.dyn_start + D_BIN_SELL_1].key);
        msg!("[mints] base_token: {}", self.base_token_pk);
        msg!("[mints] quote_token: {}", self.quote_token_pk);
        Ok(())
    }


    fn get_fee_factor(&self) -> Result<(f64, f64)> { Ok(self.fee_factor) }

    fn fast_quote(&mut self, input_mint: Pubkey, amount_in: u64, profit_pct: f64) -> Result<(u64, u64)> {
        let (max_in, max_out) = self.get_cached_max_amounts(input_mint);
        let max_in_active = self.get_max_amount_in_active_bin(input_mint).unwrap_or(u64::MAX);

        let is_base_input = input_mint == self.base_token_pk;
        let fee_factor = FEE_PRECISION - self.fee_numerator as u64; // fee_factor * FEE_PRECISION

        #[cfg(any(test, feature = "debug"))]
        {
            let max_out_active = self.get_max_amount_out_active_bin(input_mint).unwrap_or(u64::MAX);
            let (d_in, d_out) = if input_mint == Pubkey::from_str_const("So11111111111111111111111111111111111111112") { (9, 6) } else { (6, 9) };
            let bin_step_pct = self.lb_pair_slim.bin_step as f64 / 10000.0;
            eprintln!(
                "[DLMM] Active bin: in={} ({}) out={} ({}) | Total: in={} ({}) out={} ({}) | Bin step: {:.2}% | Profit: {:.4}% - {}",
                max_in_active as f64 / 10_f64.powi(d_in), max_in_active,
                max_out_active as f64 / 10_f64.powi(d_out), max_out_active,
                max_in as f64 / 10_f64.powi(d_in), max_in,
                max_out as f64 / 10_f64.powi(d_out), max_out,
                bin_step_pct * 100.0,
                profit_pct * 100.0,
                self.pool_id.to_string()
            );
        }

        // Integer Q64 quote: amount * price_q64 * fee_factor / (Q64 * FEE_PRECISION)
        // For inverse direction: amount * fee_factor * Q64 / (price_q64 * FEE_PRECISION)
        let quote_int = |amt: u64, price_q64: u128| -> u64 {
            if is_base_input {
                // base→quote: out = amt * price_q64 * fee_factor / (2^64 * FEE_PRECISION)
                let n = (amt as u128) * price_q64;
                let n = (n >> 32) * (fee_factor as u128); // split shift to avoid overflow
                (n / ((1u128 << 32) * FEE_PRECISION as u128)) as u64
            } else {
                // quote→base: out = amt * fee_factor * 2^64 / (price_q64 * FEE_PRECISION)
                let n = (amt as u128) * (fee_factor as u128);
                let n = (n << 32) / (price_q64 >> 32); // split to avoid overflow
                (n / FEE_PRECISION as u128) as u64
            }
        };

        if max_in_active < amount_in && max_in > max_in_active {
            let bin_step_bps = self.lb_pair_slim.bin_step as u64; // bin_step in basis points (1 bps = 0.01%)
            let profit_bps = (profit_pct * 10000.0) as u64;

            if profit_bps > bin_step_bps {
                debug_eprintln!("[DLMM] Crossing bins: profit {:.2}% > bin step {:.2}%", profit_pct * 100.0, bin_step_bps as f64 / 100.0);
                let out_active = quote_int(max_in_active, self.lb_price);

                let remaining = amount_in.min(max_in) - max_in_active;
                // next bin price: price / (1 + bin_step/10000)
                // In Q64: next_price = price * 10000 / (10000 + bin_step)
                let next_price = self.lb_price * 10000 / (10000 + bin_step_bps as u128);
                let out_next = quote_int(remaining, next_price);

                let total_in = max_in_active + remaining;
                let total_out = (out_active + out_next).min(max_out);
                return Ok((total_in, total_out));
            }
        }

        let clamped_in = amount_in.min(max_in).min(max_in_active);
        let out = quote_int(clamped_in, self.lb_price);
        Ok((clamped_in, out.min(max_out)))
    }

    fn get_active_bin_max_in(&self, input_mint: Pubkey) -> Result<u64> {
        MeteoraDlmm::get_max_amount_in_active_bin(self, input_mint)
    }

    fn get_bin_step_frac(&self) -> f64 {
        self.lb_pair_slim.bin_step as f64 / 10000.0
    }

    fn get_bin_segment<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        bin_offset: i32,
    ) -> Result<Option<(f64, u64)>> {
        MeteoraDlmm::get_bin_segment_impl(self, accounts, input_mint, bin_offset)
    }

    fn invoke_swap_base_in<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        max_amount_in: u64,
        amount_out: Option<u64>,
        payer: AccountInfo<'a>,
        user_mint_1_token_account: AccountInfo<'a>,
        user_mint_2_token_account: AccountInfo<'a>,
        mint_1_account: AccountInfo<'a>,
        mint_2_account: AccountInfo<'a>,
        mint_1_token_program: AccountInfo<'a>,
        mint_2_token_program: AccountInfo<'a>,
    ) -> Result<()> {
        let (
            base_token_program,
            quote_token_program,
            user_base_token_account,
            user_quote_token_account,
        ) = if mint_1_account.key == &self.base_token_pk {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
            )
        } else if mint_2_account.key == &self.base_token_pk {
            (
                mint_2_token_program,
                mint_1_token_program,
                user_mint_2_token_account,
                user_mint_1_token_account,
            )
        } else {
            return Err(ProgramError::InvalidAccountData.into());
        };

        let (user_token_in, user_token_out) = if input_mint == self.base_token_pk {
            (user_base_token_account, user_quote_token_account)
        } else {
            (user_quote_token_account, user_base_token_account)
        };

        let amount_out_value = 0 as u64;

        // Get stored accounts - static from static_base, dynamic from dyn_start
        let program_id = &accounts[self.static_base + S_PROGRAM_ID];
        let host_fee_in = &accounts[self.static_base + S_HOST_FEE_IN];
        let event_authority = &accounts[self.static_base + S_EVENT_AUTHORITY];
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let base_vault = &accounts[self.dyn_start + D_BASE_VAULT];
        let quote_vault = &accounts[self.dyn_start + D_QUOTE_VAULT];
        let oracle = &accounts[self.dyn_start + D_ORACLE];
        let bitmap_extension = &accounts[self.dyn_start + D_BITMAP_EXT];
        let memo = &accounts[3];

        let swap_for_y = input_mint == self.base_token_pk;

        let (bin_array_1, bin_array_2) = if swap_for_y {
            (
                accounts[self.dyn_start + D_BIN_BUY_0].clone(),
                accounts[self.dyn_start + D_BIN_BUY_1].clone(),
            )
        } else {
            (
                accounts[self.dyn_start + D_BIN_SELL_0].clone(),
                accounts[self.dyn_start + D_BIN_SELL_1].clone(),
            )
        };

        // Determine base/quote mint AccountInfos from the passed-in mint accounts
        let (base_mint_info, quote_mint_info) = if mint_1_account.key == &self.base_token_pk {
            (mint_1_account, mint_2_account)
        } else {
            (mint_2_account, mint_1_account)
        };

        let metas = vec![
            AccountMeta::new(*pool_id.key, false),
            if *bitmap_extension.key == PROGRAM_ID {
                AccountMeta::new_readonly(*bitmap_extension.key, false)
            } else {
                AccountMeta::new(*bitmap_extension.key, false)
            },
            AccountMeta::new(*base_vault.key, false),
            AccountMeta::new(*quote_vault.key, false),
            AccountMeta::new(*user_token_in.key, false),
            AccountMeta::new(*user_token_out.key, false),
            AccountMeta::new_readonly(self.base_token_pk, false),
            AccountMeta::new_readonly(self.quote_token_pk, false),
            AccountMeta::new(*oracle.key, false),
            AccountMeta::new(*host_fee_in.key, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*base_token_program.key, false),
            AccountMeta::new_readonly(*quote_token_program.key, false),
            AccountMeta::new_readonly(*memo.key, false),
            AccountMeta::new_readonly(*event_authority.key, false),
            AccountMeta::new_readonly(PROGRAM_ID, false),
            AccountMeta::new(*bin_array_1.key, false),
            AccountMeta::new(*bin_array_2.key, false),
        ];

        // swap2 instruction discriminator (SwapExactIn)
        let mut data = [0u8; 32];
        data[..8].copy_from_slice(&SWAP_EXACT_IN_DISC);
        data[8..16].copy_from_slice(&max_amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&amount_out_value.to_le_bytes());
        // data[24..32] already zeroed: empty vec slices + empty vec info (2x u32)

        let swap_ix = Instruction {
            program_id: PROGRAM_ID,
            accounts: metas,
            data: data.to_vec(),
        };

        // Stack-allocated account infos array — no heap Vec
        let accounts_arr = [
            pool_id.clone(),
            bitmap_extension.clone(),
            base_vault.clone(),
            quote_vault.clone(),
            unsafe { std::mem::transmute(user_token_in.to_account_info()) },
            unsafe { std::mem::transmute(user_token_out.to_account_info()) },
            unsafe { std::mem::transmute(base_mint_info.to_account_info()) },
            unsafe { std::mem::transmute(quote_mint_info.to_account_info()) },
            oracle.clone(),
            host_fee_in.clone(),
            unsafe { std::mem::transmute(payer.to_account_info()) },
            unsafe { std::mem::transmute(base_token_program.to_account_info()) },
            unsafe { std::mem::transmute(quote_token_program.to_account_info()) },
            memo.clone(),
            event_authority.clone(),
            program_id.clone(),
            bin_array_1.clone(),
            bin_array_2.clone(),
        ];

        unsafe {
            let accounts: &[AccountInfo<'a>] = std::mem::transmute(accounts_arr.as_slice());
            invoke_unchecked(&swap_ix, accounts)?;
        }
        Ok(())
    }

    fn invoke_swap_base_out<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        min_amount_out: Option<u64>,
        payer: AccountInfo<'a>,
        user_mint_1_token_account: AccountInfo<'a>,
        user_mint_2_token_account: AccountInfo<'a>,
        mint_1_account: AccountInfo<'a>,
        mint_2_account: AccountInfo<'a>,
        mint_1_token_program: AccountInfo<'a>,
        mint_2_token_program: AccountInfo<'a>,
    ) -> Result<()> {
        let (
            base_token_program,
            quote_token_program,
            user_base_token_account,
            user_quote_token_account,
        ) = if mint_1_account.key == &self.base_token_pk {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
            )
        } else if mint_2_account.key == &self.base_token_pk {
            (
                mint_2_token_program,
                mint_1_token_program,
                user_mint_2_token_account,
                user_mint_1_token_account,
            )
        } else {
            return Err(ProgramError::InvalidAccountData.into());
        };

        let (user_token_in, user_token_out) = if input_mint == self.base_token_pk {
            (user_base_token_account, user_quote_token_account)
        } else {
            (user_quote_token_account, user_base_token_account)
        };

        let min_amount_out_value = min_amount_out.unwrap_or(0);

        // Get stored accounts - static from static_base, dynamic from dyn_start
        let program_id = &accounts[self.static_base + S_PROGRAM_ID];
        let host_fee_in = &accounts[self.static_base + S_HOST_FEE_IN];
        let event_authority = &accounts[self.static_base + S_EVENT_AUTHORITY];
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let base_vault = &accounts[self.dyn_start + D_BASE_VAULT];
        let quote_vault = &accounts[self.dyn_start + D_QUOTE_VAULT];
        let oracle = &accounts[self.dyn_start + D_ORACLE];
        let bitmap_extension = &accounts[self.dyn_start + D_BITMAP_EXT];
        let memo = &accounts[3];

        let swap_for_y = input_mint == self.base_token_pk;

        let (bin_array_1, bin_array_2) = if swap_for_y {
            (
                accounts[self.dyn_start + D_BIN_BUY_0].clone(),
                accounts[self.dyn_start + D_BIN_BUY_1].clone(),
            )
        } else {
            (
                accounts[self.dyn_start + D_BIN_SELL_0].clone(),
                accounts[self.dyn_start + D_BIN_SELL_1].clone(),
            )
        };

        // Determine base/quote mint AccountInfos from the passed-in mint accounts
        let (base_mint_info, quote_mint_info) = if mint_1_account.key == &self.base_token_pk {
            (mint_1_account, mint_2_account)
        } else {
            (mint_2_account, mint_1_account)
        };

        let metas = vec![
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new(*bitmap_extension.key, false),
            AccountMeta::new(*base_vault.key, false),
            AccountMeta::new(*quote_vault.key, false),
            AccountMeta::new(*user_token_in.key, false),
            AccountMeta::new(*user_token_out.key, false),
            AccountMeta::new_readonly(self.base_token_pk, false),
            AccountMeta::new_readonly(self.quote_token_pk, false),
            AccountMeta::new(*oracle.key, false),
            AccountMeta::new(*host_fee_in.key, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*base_token_program.key, false),
            AccountMeta::new_readonly(*quote_token_program.key, false),
            AccountMeta::new_readonly(*memo.key, false),
            AccountMeta::new_readonly(*event_authority.key, false),
            AccountMeta::new_readonly(PROGRAM_ID, false),
            AccountMeta::new(*bin_array_1.key, false),
            AccountMeta::new(*bin_array_2.key, false),
        ];

        // swap_exact_out2 instruction discriminator (SwapExactOut)
        let mut data = [0u8; 32];
        data[..8].copy_from_slice(&SWAP_EXACT_OUT_DISC);
        data[8..16].copy_from_slice(&amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&min_amount_out_value.to_le_bytes());
        // data[24..32] already zeroed: empty vec slices + empty vec info (2x u32)

        let swap_ix = Instruction {
            program_id: PROGRAM_ID,
            accounts: metas,
            data: data.to_vec(),
        };

        // Stack-allocated account infos array — no heap Vec
        let accounts_arr = [
            pool_id.clone(),
            bitmap_extension.clone(),
            base_vault.clone(),
            quote_vault.clone(),
            unsafe { std::mem::transmute(user_token_in.to_account_info()) },
            unsafe { std::mem::transmute(user_token_out.to_account_info()) },
            unsafe { std::mem::transmute(base_mint_info.to_account_info()) },
            unsafe { std::mem::transmute(quote_mint_info.to_account_info()) },
            oracle.clone(),
            host_fee_in.clone(),
            unsafe { std::mem::transmute(payer.to_account_info()) },
            unsafe { std::mem::transmute(base_token_program.to_account_info()) },
            unsafe { std::mem::transmute(quote_token_program.to_account_info()) },
            memo.clone(),
            event_authority.clone(),
            program_id.clone(),
            bin_array_1.clone(),
            bin_array_2.clone(),
        ];

        unsafe {
            let accounts: &[AccountInfo<'a>] = std::mem::transmute(accounts_arr.as_slice());
            invoke_unchecked(&swap_ix, accounts)?;
        }
        Ok(())
    }


}

impl MeteoraDlmm {

    /// Lazily initialize expensive fields (bin arrays, swap caches, active bin, max amounts).
    /// Called on first swap_base_in/swap_base_out. Skipped entirely on no-profit path.
    #[inline(never)]
    pub fn prepare_for_execution(&mut self, accounts: &[AccountInfo], clock: &Clock) -> Result<()> {
        if self.buy_swap_cache.is_some() {
            return Ok(());
        }

        let pool_acc = &accounts[self.dyn_start + D_POOL];

        // Read only the fields we need from pool data instead of copying the full ~896-byte LbPair.
        // LbPair layout (after 8-byte discriminator):
        //   StaticParameters (32 bytes) at offset 0
        //   VariableParameters (32 bytes) at offset 32
        //   bump_seed [u8;1] + bin_step_seed [u8;2] + pair_type u8 (4 bytes) at offset 64
        //   active_id i32 at offset 68
        //   bin_step u16 at offset 72
        let (
            filter_period, decay_period, reduction_factor,
            base_factor, base_fee_power_factor,
            variable_fee_control, max_volatility_accumulator,
            v_volatility_accumulator, v_volatility_reference, v_index_reference,
            v_last_update_timestamp,
            active_id, bin_step,
        ) = {
            let d = pool_acc.try_borrow_data()?;
            let d = &d[8..]; // skip discriminator
            (
                u16::from_le_bytes([d[2], d[3]]),    // parameters.filter_period
                u16::from_le_bytes([d[4], d[5]]),    // parameters.decay_period
                u16::from_le_bytes([d[6], d[7]]),    // parameters.reduction_factor
                u16::from_le_bytes([d[0], d[1]]),    // parameters.base_factor
                d[26],                                // parameters.base_fee_power_factor
                u32::from_le_bytes([d[8], d[9], d[10], d[11]]),   // parameters.variable_fee_control
                u32::from_le_bytes([d[12], d[13], d[14], d[15]]), // parameters.max_volatility_accumulator
                u32::from_le_bytes([d[32], d[33], d[34], d[35]]), // v_parameters.volatility_accumulator
                u32::from_le_bytes([d[36], d[37], d[38], d[39]]), // v_parameters.volatility_reference
                i32::from_le_bytes([d[40], d[41], d[42], d[43]]), // v_parameters.index_reference
                i64::from_le_bytes([d[48], d[49], d[50], d[51], d[52], d[53], d[54], d[55]]), // v_parameters.last_update_timestamp
                i32::from_le_bytes([d[68], d[69], d[70], d[71]]), // active_id
                u16::from_le_bytes([d[72], d[73]]),               // bin_step
            )
        };

        // Get active bin directly from bin arrays — skip the bitmap walk.
        // Compute which bin array contains active_id and find it among our accounts.
        {
            // Inline bin_id_to_bin_array_index to avoid anyhow/anchor Result mismatch
            let needed_index: i64 = {
                let idx = active_id / MAX_BIN_PER_ARRAY as i32;
                let rem = active_id % MAX_BIN_PER_ARRAY as i32;
                if active_id < 0 && rem != 0 { idx as i64 - 1 } else { idx as i64 }
            };
            let bin_array_slice = &accounts[self.dyn_start + D_BIN_BUY_0..self.dyn_start + D_BIN_SELL_1 + 1];
            let active_bin_array_acc = bin_array_slice.iter()
                .find(|acc| Self::read_bin_array_index(acc) == needed_index)
                .ok_or_else(|| error!(SolarBError::InsufficientBinArray))?;
            let bin_data = active_bin_array_acc.try_borrow_data()
                .map_err(|_| error!(SolarBError::InsufficientBinArray))?;
            let lower_bin_id = needed_index as i32 * MAX_BIN_PER_ARRAY as i32;
            let bin_index_in_array = (active_id - lower_bin_id) as usize;
            let bin_offset = BIN_ARRAY_HEADER_SIZE + bin_index_in_array * BIN_SIZE;
            if bin_offset + BIN_SIZE > bin_data.len() {
                return Err(error!(SolarBError::InsufficientBinArray));
            }
            let mut active_bin: Bin = bytemuck::pod_read_unaligned(&bin_data[bin_offset..bin_offset + BIN_SIZE]);
            let _ = active_bin.get_or_store_bin_price(active_id, bin_step);
            self.active_bin = active_bin;
        }

        // Inline update_references: only reads a few fields, avoids full LbPair copy
        let (updated_index_reference, updated_volatility_reference) = {
            let elapsed = clock.unix_timestamp.saturating_sub(v_last_update_timestamp);
            if elapsed >= filter_period as i64 {
                let new_index_ref = active_id;
                let new_vol_ref = if elapsed < decay_period as i64 {
                    v_volatility_accumulator
                        .saturating_mul(reduction_factor as u32)
                        / BASIS_POINT_MAX as u32
                } else {
                    0
                };
                (new_index_ref, new_vol_ref)
            } else {
                (v_index_reference, v_volatility_reference)
            }
        };

        // Propagate time-adjusted volatility fields to slim
        self.lb_pair_slim.volatility_accumulator = v_volatility_accumulator;
        self.lb_pair_slim.volatility_reference = updated_volatility_reference;
        self.lb_pair_slim.index_reference = updated_index_reference;

        // Compute max amounts from bin arrays
        let buy_acc = &accounts[self.dyn_start + D_BIN_BUY_0];
        let sell_acc = &accounts[self.dyn_start + D_BIN_SELL_0];
        let (buy_total_y, sell_total_x) = if buy_acc.key == sell_acc.key {
            let (tx, ty) = Self::sum_bin_array_raw(buy_acc);
            (ty, tx)
        } else {
            let (_, ty) = Self::sum_bin_array_raw(buy_acc);
            let (tx, _) = Self::sum_bin_array_raw(sell_acc);
            (ty, tx)
        };
        self.buy_max_out = buy_total_y;
        let price_f64 = self.price;
        self.buy_max_in =
            if price_f64 > 0.0 { (buy_total_y as f64 / price_f64) as u64 } else { 0 };
        self.sell_max_out = sell_total_x;
        self.sell_max_in =
            if price_f64 > 0.0 { (sell_total_x as f64 * price_f64) as u64 } else { 0 };

        // Build swap caches from raw fields (no full LbPair needed)
        let (buy, sell) = Self::build_swap_caches_raw(
            active_id, bin_step, base_factor, base_fee_power_factor,
            variable_fee_control, max_volatility_accumulator,
            updated_index_reference, updated_volatility_reference,
            v_volatility_accumulator,
            accounts, self.dyn_start,
        ).map_err(|_| error!(SolarBError::TransferFeeCalculationError))?;
        self.buy_swap_cache = Some(buy);
        self.sell_swap_cache = Some(sell);

        Ok(())
    }

    #[inline(never)]
    pub fn new(
        accounts: &[AccountInfo],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
    ) -> Result<Self> {
        let pool_acc = &accounts[dyn_start + D_POOL];
        // Read only the fields we need directly from account bytes (~50 CU vs ~600 for full LbPair parse + Box).
        // LbPair layout after 8-byte discriminator:
        //   [0..32]   StaticParameters   (variable_fee_control @8, max_vol_acc @12)
        //   [32..64]  VariableParameters (vol_acc @32, vol_ref @36, idx_ref @40)
        //   [64..68]  bump_seed + bin_step_seed + pair_type
        //   [68..72]  active_id (i32)
        //   [72..74]  bin_step (u16)
        //   [80..112] token_x_mint (Pubkey)
        //   [112..144] token_y_mint (Pubkey)
        let (lb_pair_slim, base_token_pk, quote_token_pk, lb_price, fee_numerator) = {
            let d = pool_acc.try_borrow_data()?;
            let d = &d[8..]; // skip discriminator

            let base_factor = u16::from_le_bytes([d[0], d[1]]);
            let base_fee_power_factor = d[26];
            let variable_fee_control = u32::from_le_bytes([d[8], d[9], d[10], d[11]]);
            let volatility_accumulator = u32::from_le_bytes([d[32], d[33], d[34], d[35]]);
            let active_id = i32::from_le_bytes([d[68], d[69], d[70], d[71]]);
            let bin_step = u16::from_le_bytes([d[72], d[73]]);

            // Inline get_base_fee: base_factor * bin_step * 10 * 10^base_fee_power_factor
            let base_fee = (base_factor as u128)
                * (bin_step as u128)
                * 10u128
                * 10u128.pow(base_fee_power_factor as u32);

            // Inline get_variable_fee
            let variable_fee = if variable_fee_control > 0 {
                let square_vfa_bin = (volatility_accumulator as u128)
                    .saturating_mul(bin_step as u128)
                    .saturating_pow(2);
                (variable_fee_control as u128)
                    .saturating_mul(square_vfa_bin)
                    .saturating_add(99_999_999_999)
                    / 100_000_000_000
            } else {
                0
            };

            // total_fee capped at MAX_FEE_RATE (100_000_000)
            let total_fee = std::cmp::min(base_fee + variable_fee, 100_000_000u128) as u64;

            let slim = LbPairSlim {
                active_id,
                bin_step,
                volatility_accumulator,
                volatility_reference: u32::from_le_bytes([d[36], d[37], d[38], d[39]]),
                index_reference: i32::from_le_bytes([d[40], d[41], d[42], d[43]]),
                max_vol_acc: u32::from_le_bytes([d[12], d[13], d[14], d[15]]),
                variable_fee_control,
            };

            let token_x: Pubkey = Pubkey::new_from_array(d[80..112].try_into().unwrap());
            let token_y: Pubkey = Pubkey::new_from_array(d[112..144].try_into().unwrap());

            let pr: u128 = get_price_from_id(active_id, bin_step)
                .map_err(|_| error!(SolarBError::InsufficientAccounts))?;

            (slim, token_x, token_y, pr, total_fee)
        };

        let price = get_price_f64(lb_price);

        #[cfg(test)]
        {
            let mut price = get_price_f64(lb_price);
            let skew: f64 = if lb_price % 2 == 0 { 1.03 } else { 0.97 };
            price *= skew;
        }

        // LAZY INIT: skip bin array reads, active_bin computation, swap cache building.
        // These are deferred to prepare_for_execution() (called on first swap_base_in).
        // For the analytical profit check, we only need price + fee_rate + bin_step.
        // active_bin is zeroed → get_active_bin_max_in returns 0 → extract_pool_model
        // falls back to get_cached_max_amounts (u64::MAX) with conservative price.
        let active_bin: Bin = bytemuck::Zeroable::zeroed();

        let instance = MeteoraDlmm {
            base_token_pk,
            quote_token_pk,
            pool_id: *pool_acc.key,
            lb_pair_slim,
            active_bin,
            lb_price,
            price,
            fee_numerator,
            fee_factor: { let f = 1.0 - fee_numerator as f64 / FEE_PRECISION as f64; (f, f) },
            static_base,
            dyn_start,
            buy_max_in: u64::MAX,
            buy_max_out: u64::MAX,
            sell_max_in: u64::MAX,
            sell_max_out: u64::MAX,
            buy_swap_cache: None,
            sell_swap_cache: None
        };
        Ok(instance)
    }

    /// Sum raw (amount_x, amount_y) from a single bin array — no price math, just byte reads.
    fn sum_bin_array_raw(acc: &AccountInfo) -> (u64, u64) {


        let data = match acc.try_borrow_data() {
            Ok(d) => d,
            Err(_) => return (0, 0),
        };
        if data.len() < BIN_ARRAY_HEADER_SIZE { return (0, 0); }

        let mut total_x: u64 = 0;
        let mut total_y: u64 = 0;

        for i in 0..MAX_BIN_PER_ARRAY {
            let base = BIN_ARRAY_HEADER_SIZE + i * BIN_SIZE;
            if base + 16 > data.len() { break; }

            let amount_x = u64::from_le_bytes(data[base..base + 8].try_into().unwrap());
            let amount_y = u64::from_le_bytes(data[base + 8..base + 16].try_into().unwrap());

            total_x = total_x.saturating_add(amount_x);
            total_y = total_y.saturating_add(amount_y);
        }

        (total_x, total_y)
    }

    /// Read the bin array index (i64) from the first 8 bytes after the discriminator.
    /// Returns i64::MAX as sentinel if the account is too small or unreadable.
    fn read_bin_array_index(acc: &AccountInfo) -> i64 {
        if acc.data_len() > 16 {
            if let Ok(data) = acc.try_borrow_data() {
                return bytemuck::pod_read_unaligned(&data[8..16]);
            }
        }
        i64::MAX
    }

    /// Build SwapCache for both directions + fee_rate. Separated from new() to reduce stack frame.
    #[inline(never)]
    fn build_swap_caches(
        lb_pair: &mut LbPair,
        accounts: &[AccountInfo],
        dyn_start: usize,
    ) -> anyhow::Result<(Box<SwapCache>, Box<SwapCache>, f64)> {
        // Compute initial_vol_acc: update_volatility_accumulator result after update_references
        let initial_vol_acc = {
            let delta_id = (i64::from(lb_pair.v_parameters.index_reference)
                - i64::from(lb_pair.active_id))
                .unsigned_abs();
            let va = u64::from(lb_pair.v_parameters.volatility_reference)
                .saturating_add(delta_id.saturating_mul(BASIS_POINT_MAX as u64));
            std::cmp::min(va, lb_pair.parameters.max_volatility_accumulator.into()) as u32
        };
        // Set for accurate fee_rate computation
        lb_pair.v_parameters.volatility_accumulator = initial_vol_acc;
        let fee_rate = compute_fee_rate(lb_pair)?;

        let base_fee = lb_pair.get_base_fee().unwrap_or(0);
        let price_base = {
            let bps = u128::from(lb_pair.bin_step)
                .checked_shl(SCALE_OFFSET.into())
                .unwrap()
                .checked_div(BASIS_POINT_MAX as u128)
                .unwrap();
            ONE.checked_add(bps).unwrap()
        };
        let has_variable_fee = lb_pair.parameters.variable_fee_control > 0;

        let buy = Box::new(SwapCache {
            base_fee,
            price_base,
            bin_array_indices: [
                Self::read_bin_array_index(&accounts[dyn_start + D_BIN_BUY_0]),
                Self::read_bin_array_index(&accounts[dyn_start + D_BIN_BUY_1]),
            ],
            initial_vol_acc,
            has_variable_fee,
        });
        let sell = Box::new(SwapCache {
            base_fee,
            price_base,
            bin_array_indices: [
                Self::read_bin_array_index(&accounts[dyn_start + D_BIN_SELL_0]),
                Self::read_bin_array_index(&accounts[dyn_start + D_BIN_SELL_1]),
            ],
            initial_vol_acc,
            has_variable_fee,
        });
        Ok((buy, sell, fee_rate))
    }

    /// Build SwapCache from raw fields (no full LbPair needed). Used by prepare_for_execution.
    #[inline(never)]
    fn build_swap_caches_raw(
        active_id: i32,
        bin_step: u16,
        base_factor: u16,
        base_fee_power_factor: u8,
        variable_fee_control: u32,
        max_volatility_accumulator: u32,
        index_reference: i32,
        volatility_reference: u32,
        volatility_accumulator: u32,
        accounts: &[AccountInfo],
        dyn_start: usize,
    ) -> anyhow::Result<(Box<SwapCache>, Box<SwapCache>)> {
        // Compute initial_vol_acc
        let initial_vol_acc = {
            let delta_id = (i64::from(index_reference) - i64::from(active_id)).unsigned_abs();
            let va = u64::from(volatility_reference)
                .saturating_add(delta_id.saturating_mul(BASIS_POINT_MAX as u64));
            std::cmp::min(va, max_volatility_accumulator.into()) as u32
        };

        // base_fee = base_factor * bin_step * 10 * 10^base_fee_power_factor
        let base_fee = u128::from(base_factor)
            .checked_mul(bin_step.into())
            .unwrap_or(0)
            .checked_mul(10u128)
            .unwrap_or(0)
            .checked_mul(10u128.pow(base_fee_power_factor.into()))
            .unwrap_or(0);

        let price_base = {
            let bps = u128::from(bin_step)
                .checked_shl(SCALE_OFFSET.into())
                .unwrap()
                .checked_div(BASIS_POINT_MAX as u128)
                .unwrap();
            ONE.checked_add(bps).unwrap()
        };
        let has_variable_fee = variable_fee_control > 0;

        let buy = Box::new(SwapCache {
            base_fee,
            price_base,
            bin_array_indices: [
                Self::read_bin_array_index(&accounts[dyn_start + D_BIN_BUY_0]),
                Self::read_bin_array_index(&accounts[dyn_start + D_BIN_BUY_1]),
            ],
            initial_vol_acc,
            has_variable_fee,
        });
        let sell = Box::new(SwapCache {
            base_fee,
            price_base,
            bin_array_indices: [
                Self::read_bin_array_index(&accounts[dyn_start + D_BIN_SELL_0]),
                Self::read_bin_array_index(&accounts[dyn_start + D_BIN_SELL_1]),
            ],
            initial_vol_acc,
            has_variable_fee,
        });
        Ok((buy, sell))
    }

    /// Compute approximate (max_amount_in, max_amount_out) from first bin array + active price.
    pub fn compute_max_amounts_in_out(&self, accounts: &[AccountInfo], input_mint: Pubkey) -> Result<(u64, u64)> {
        let swap_for_y = self.base_token_pk == input_mint;
        let (arr0, arr1) = if swap_for_y {
            (
                &accounts[self.dyn_start + D_BIN_BUY_0],
                &accounts[self.dyn_start + D_BIN_BUY_1],
            )
        } else {
            (
                &accounts[self.dyn_start + D_BIN_SELL_0],
                &accounts[self.dyn_start + D_BIN_SELL_1],
            )
        };
        let (x0, y0) = Self::sum_bin_array_raw(arr0);
        let (x1, y1) = if arr0.key() != arr1.key() {
            Self::sum_bin_array_raw(arr1)
        } else {
            (0, 0)
        };
        let (total_x, total_y) = (x0.saturating_add(x1), y0.saturating_add(y1));
        let price_f64 = self.price;
        if swap_for_y {
            let max_in = if price_f64 > 0.0 { (total_y as f64 / price_f64) as u64 } else { 0 };
            Ok((max_in, total_y))
        } else {
            let max_in = if price_f64 > 0.0 { (total_x as f64 * price_f64) as u64 } else { 0 };
            Ok((max_in, total_x))
        }
    }

    pub fn get_max_amount_in(&self, accounts: &[AccountInfo], input_mint: Pubkey) -> Result<u64> {
        Ok(self.compute_max_amounts_in_out(accounts, input_mint)?.0)
    }

    pub fn get_max_amount_out(&self, accounts: &[AccountInfo], input_mint: Pubkey) -> Result<u64> {
        Ok(self.compute_max_amounts_in_out(accounts, input_mint)?.1)
    }

    /// Get slope (price_f64 * fee_factor) and capacity (max_amount_in) for a bin
    /// at `bin_offset` from the active bin in the swap direction.
    /// offset=0 is the active bin, 1 is the next bin, etc.
    fn get_bin_segment_impl<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        bin_offset: i32,
    ) -> Result<Option<(f64, u64)>> {
        use dlmm_lib::dlmm::accounts::BinArray;
        use dlmm_lib::extensions::BinArrayExtension;
        use dlmm_lib::math::u128x128_math::{mul_shr, shl_div};
        use dlmm_lib::dlmm::types::Rounding;

        const BIN_ARRAY_HEADER_SIZE: usize = 56;
        const BIN_SIZE: usize = 144;

        let swap_for_y = self.base_token_pk == input_mint;
        let (bin_arrays, cache) = if swap_for_y {
            let cache = match self.buy_swap_cache.as_ref() {
                Some(c) => c,
                None => return Ok(None), // not initialized yet — caller falls back to golden section
            };
            ([
                &accounts[self.dyn_start + D_BIN_BUY_0],
                &accounts[self.dyn_start + D_BIN_BUY_1],
            ], cache)
        } else {
            let cache = match self.sell_swap_cache.as_ref() {
                Some(c) => c,
                None => return Ok(None),
            };
            ([
                &accounts[self.dyn_start + D_BIN_SELL_0],
                &accounts[self.dyn_start + D_BIN_SELL_1],
            ], cache)
        };

        // Compute target bin_id: active moves down for swap_for_y, up otherwise
        let target_bin_id = if swap_for_y {
            self.lb_pair_slim.active_id.checked_sub(bin_offset)
        } else {
            self.lb_pair_slim.active_id.checked_add(bin_offset)
        }.ok_or_else(|| error!(SolarBError::InsufficientAccounts))?;

        // Find which bin array contains this bin
        let needed_index = BinArray::bin_id_to_bin_array_index(target_bin_id)
            .map_err(|_| error!(SolarBError::InsufficientAccounts))? as i64;
        let bin_array_acc = if needed_index == cache.bin_array_indices[0] {
            bin_arrays[0]
        } else if needed_index == cache.bin_array_indices[1] {
            bin_arrays[1]
        } else {
            return Ok(None); // bin array not available
        };

        let data = bin_array_acc.try_borrow_data()
            .map_err(|_| error!(SolarBError::InsufficientAccounts))?;
        let bin_array_index: i64 = bytemuck::pod_read_unaligned(&data[8..16]);
        let (lower_bin_id, upper_bin_id) = BinArray::get_bin_array_lower_upper_bin_id(bin_array_index as i32)
            .map_err(|_| error!(SolarBError::InsufficientAccounts))?;

        if target_bin_id < lower_bin_id || target_bin_id > upper_bin_id {
            return Ok(None);
        }

        let bin_index_in_array = (target_bin_id - lower_bin_id) as usize;
        let bin_data_offset = BIN_ARRAY_HEADER_SIZE + bin_index_in_array * BIN_SIZE;
        if bin_data_offset + BIN_SIZE > data.len() {
            return Ok(None);
        }

        let bin: dlmm_lib::dlmm::types::Bin =
            bytemuck::pod_read_unaligned(&data[bin_data_offset..bin_data_offset + BIN_SIZE]);

        // Check if this bin has liquidity in the output direction
        if (swap_for_y && bin.amount_y == 0) || (!swap_for_y && bin.amount_x == 0) {
            return Ok(Some((0.0, 0)));
        }

        // Compute price for this bin: start from active price and step incrementally
        let price_q64 = if bin_offset == 0 {
            self.lb_price
        } else {
            let mut p = self.lb_price;
            for _ in 0..bin_offset {
                p = if swap_for_y {
                    shl_div(p, cache.price_base, SCALE_OFFSET, Rounding::Down)
                        .unwrap_or(0)
                } else {
                    mul_shr(p, cache.price_base, SCALE_OFFSET, Rounding::Down)
                        .unwrap_or(0)
                };
                if p == 0 { return Ok(None); }
            }
            p
        };

        // Compute capacity: max_amount_in for this bin (before fees)
        let capacity = bin.get_max_amount_in(price_q64, swap_for_y)
            .map_err(|_| error!(SolarBError::InsufficientAccounts))?;

        // Compute slope = price_f64 * fee_factor
        let price_f64 = price_q64 as f64 / Q64_SCALE;
        let fee_factor = self.fee_factor.0;
        // For swap_for_y: input X, output Y → slope = price * fee (X→Y: multiply by price)
        // For swap_for_x: input Y, output X → slope = (1/price) * fee (Y→X: divide by price)
        let slope = if swap_for_y {
            price_f64 * fee_factor
        } else {
            (1.0 / price_f64) * fee_factor
        };

        Ok(Some((slope, capacity)))
    }

    // Max input amount in the current active bin only (no price movement)
    pub fn get_max_amount_in_active_bin(&self, input_mint: Pubkey) -> Result<u64> {
        let swap_for_y = self.base_token_pk == input_mint;
        let amount: u64 = self
            .active_bin
            .get_max_amount_in(self.lb_price, swap_for_y)
            .map_err(|_| error!(SolarBError::InsufficientAccounts))?;
        Ok(amount)
    }

    #[cfg(any(test, feature = "debug"))]
    pub fn get_max_amount_out_active_bin(&self, input_mint: Pubkey) -> Result<u64> {
        let swap_for_y = self.base_token_pk == input_mint;
        Ok(self.active_bin.get_max_amount_out(swap_for_y))
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::token::MintFee;
    use dlmm_lib::pda;
    use dlmm_lib::quote::get_bin_array_pubkeys_for_swap;
    use solana_client::nonblocking::rpc_client::RpcClient;

    fn create_mock_account_info_with_data(
        key: Pubkey,
        owner: Pubkey,
        data: Option<Vec<u8>>,
    ) -> AccountInfo<'static> {
        let data_vec = data.unwrap_or_else(|| vec![0u8; 8]);
        let data_vec = Box::leak(Box::new(data_vec));
        let lamports = Box::leak(Box::new(0u64));
        let owner_static = Box::leak(Box::new(owner));
        let key_static = Box::leak(Box::new(key));
        AccountInfo::new(
            key_static, false, true, lamports, data_vec, owner_static, false, 0,
        )
    }

    fn account_to_account_info(
        key: Pubkey,
        account: solana_sdk::account::Account,
    ) -> AccountInfo<'static> {
        let data = Box::leak(Box::new(account.data));
        let lamports = Box::leak(Box::new(account.lamports));
        let owner_bytes: [u8; 32] = account.owner.to_bytes();
        let owner = Pubkey::try_from(owner_bytes.as_ref()).unwrap();
        let owner_static = Box::leak(Box::new(owner));
        let key_static = Box::leak(Box::new(key));
        AccountInfo::new(
            key_static, false, false, lamports, data, owner_static,
            account.executable, account.rent_epoch,
        )
    }

    async fn fetch_account_info_from_rpc(
        rpc_client: &RpcClient,
        key: Pubkey,
    ) -> AccountInfo<'static> {
        use solana_sdk::pubkey::Pubkey as SdkPubkey;
        let sdk_pubkey = SdkPubkey::try_from(key.to_bytes().as_ref())
            .expect("Failed to convert Pubkey");
        let account = rpc_client.get_account(&sdk_pubkey).await
            .unwrap_or_else(|e| panic!("Failed to fetch account {}: {}", key, e));
        account_to_account_info(key, account)
    }

    async fn try_fetch_account_info_from_rpc(
        rpc_client: &RpcClient,
        key: Pubkey,
    ) -> Option<AccountInfo<'static>> {
        use solana_sdk::pubkey::Pubkey as SdkPubkey;
        let sdk_pubkey = SdkPubkey::try_from(key.to_bytes().as_ref()).ok()?;
        let account = rpc_client.get_account(&sdk_pubkey).await.ok()?;
        Some(account_to_account_info(key, account))
    }

    async fn get_clock_from_rpc(rpc_client: &RpcClient) -> Clock {
        use anchor_client::solana_sdk::sysvar;
        let clock_account = rpc_client.get_account(&sysvar::clock::ID).await
            .expect("Failed to fetch clock");
        let data = &clock_account.data;
        assert!(data.len() >= 40, "Clock account data too short");
        Clock {
            slot: u64::from_le_bytes(data[0..8].try_into().unwrap()),
            epoch_start_timestamp: i64::from_le_bytes(data[8..16].try_into().unwrap()),
            epoch: u64::from_le_bytes(data[16..24].try_into().unwrap()),
            leader_schedule_epoch: u64::from_le_bytes(data[24..32].try_into().unwrap()),
            unix_timestamp: i64::from_le_bytes(data[32..40].try_into().unwrap()),
        }
    }

    fn get_rpc_client() -> RpcClient {
        let api_key = "f230200b-f911-43c1-a242-4e7b066d0993";
        RpcClient::new(format!("https://mainnet.helius-rpc.com/?api-key={}", api_key))
    }

    /// Build a MeteoraDlmm instance from a pool_id by fetching all needed accounts from RPC.
    /// Returns (instance, accounts_vec, clock) ready for testing.
    async fn build_from_pool_id(
        pool_id: Pubkey,
    ) -> (MeteoraDlmm, Vec<AccountInfo<'static>>, Clock) {
        let rpc_client = get_rpc_client();

        // Fetch pool account
        let sdk_pool_id = solana_sdk::pubkey::Pubkey::try_from(pool_id.to_bytes().as_ref()).unwrap();
        let lb_pair_account = rpc_client.get_account(&sdk_pool_id).await
            .unwrap_or_else(|e| panic!("Failed to fetch pool {}: {}", pool_id, e));
        let lb_pair: LbPair = bytemuck::pod_read_unaligned(&lb_pair_account.data[8..]);

        eprintln!("Pool: {}", pool_id);
        eprintln!("  token_x (base): {}", lb_pair.token_x_mint);
        eprintln!("  token_y (quote): {}", lb_pair.token_y_mint);
        eprintln!("  active_id: {}, bin_step: {}", lb_pair.active_id, lb_pair.bin_step);

        // Derive PDAs
        let (bitmap_extension_key, _) = pda::derive_bin_array_bitmap_extension(pool_id);
        let (event_authority_key, _) = pda::derive_event_authority_pda();

        // Get bin array pubkeys (2 buy, 2 sell)
        let buy_bin_pubkeys = get_bin_array_pubkeys_for_swap(pool_id, &lb_pair, None, true, 2)
            .expect("failed to derive buy bin array pubkeys");
        let sell_bin_pubkeys = get_bin_array_pubkeys_for_swap(pool_id, &lb_pair, None, false, 2)
            .expect("failed to derive sell bin array pubkeys");

        eprintln!("  buy bins: {:?}", buy_bin_pubkeys);
        eprintln!("  sell bins: {:?}", sell_bin_pubkeys);

        // Fetch all needed accounts from RPC
        let pool_id_info = account_to_account_info(pool_id, lb_pair_account);
        let base_vault_info = fetch_account_info_from_rpc(&rpc_client, lb_pair.reserve_x).await;
        let quote_vault_info = fetch_account_info_from_rpc(&rpc_client, lb_pair.reserve_y).await;
        let oracle_info = fetch_account_info_from_rpc(&rpc_client, lb_pair.oracle).await;

        let program_id_info = create_mock_account_info_with_data(
            PROGRAM_ID, anchor_lang::solana_program::system_program::id(), None,
        );
        let bitmap_info = try_fetch_account_info_from_rpc(&rpc_client, bitmap_extension_key)
            .await
            .unwrap_or_else(|| create_mock_account_info_with_data(
                PROGRAM_ID, anchor_lang::solana_program::system_program::id(), None,
            ));
        let host_fee_info = create_mock_account_info_with_data(
            PROGRAM_ID, anchor_lang::solana_program::system_program::id(), None,
        );
        let event_authority_info = create_mock_account_info_with_data(
            event_authority_key, anchor_lang::solana_program::system_program::id(), None,
        );

        // Fetch bin arrays
        let buy_accounts = rpc_client.get_multiple_accounts(&buy_bin_pubkeys).await.unwrap();
        let sell_accounts = rpc_client.get_multiple_accounts(&sell_bin_pubkeys).await.unwrap();

        let mut buy_infos = Vec::new();
        for (acc_opt, key) in buy_accounts.iter().zip(buy_bin_pubkeys.iter()) {
            if let Some(acc) = acc_opt {
                buy_infos.push(account_to_account_info(*key, acc.clone()));
            }
        }
        let mut sell_infos = Vec::new();
        for (acc_opt, key) in sell_accounts.iter().zip(sell_bin_pubkeys.iter()) {
            if let Some(acc) = acc_opt {
                sell_infos.push(account_to_account_info(*key, acc.clone()));
            }
        }

        assert!(buy_infos.len() >= 2, "need at least 2 buy bin arrays, got {}", buy_infos.len());
        assert!(sell_infos.len() >= 2, "need at least 2 sell bin arrays, got {}", sell_infos.len());

        // Layout:
        // Static (static_base=0): [program_id, host_fee_in, event_authority]
        // Dynamic (dyn_start=3): [pool, base_vault, quote_vault, oracle, bitmap_ext, bin_buy_0, bin_buy_1, bin_sell_0, bin_sell_1]
        let accounts = vec![
            program_id_info,             // S0
            host_fee_info,               // S1
            event_authority_info,        // S2
            pool_id_info,                // D0
            base_vault_info,             // D1
            quote_vault_info,            // D2
            oracle_info,                 // D3
            bitmap_info,                 // D4
            buy_infos[0].clone(),        // D5
            buy_infos[1].clone(),        // D6
            sell_infos[0].clone(),       // D7
            sell_infos[1].clone(),       // D8
        ];

        let static_base: usize = 0;
        let dyn_start: usize = 3;
        let dyn_end: usize = accounts.len();

        let meteora = MeteoraDlmm::new(&accounts, static_base, dyn_start, dyn_end)
            .expect("MeteoraDlmm::new failed");

        let clock = get_clock_from_rpc(&rpc_client).await;

        eprintln!("  price: {}", meteora.price);
        eprintln!("  fee_numerator: {}", meteora.fee_numerator);

        (meteora, accounts, clock)
    }

    // ---- Tests ----

    #[tokio::test]
    async fn test_dlmm_round_trip() {
        let pool_id = Pubkey::from_str_const("8G3W9d9gFZNx98kqKMnsciM9p2A8sK4xekDeZRXK4arr");
        let (mut meteora, accounts, clock) = build_from_pool_id(pool_id).await;

        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let no_fee = MintFee::ZERO;

        // 1. Print all account pubkeys
        eprintln!("\n=== Account Pubkeys ===");
        eprintln!("base_mint        : {}", meteora.base_token_pk);
        eprintln!("quote_mint       : {}", meteora.quote_token_pk);
        eprintln!("pool_id          : {}", accounts[3 + D_POOL].key);
        eprintln!("base_vault       : {}", accounts[3 + D_BASE_VAULT].key);
        eprintln!("quote_vault      : {}", accounts[3 + D_QUOTE_VAULT].key);
        eprintln!("oracle           : {}", accounts[3 + D_ORACLE].key);
        eprintln!("bitmap_ext       : {}", accounts[3 + D_BITMAP_EXT].key);
        eprintln!("program_id       : {}", accounts[S_PROGRAM_ID].key);
        eprintln!("host_fee_in      : {}", accounts[S_HOST_FEE_IN].key);
        eprintln!("event_authority  : {}", accounts[S_EVENT_AUTHORITY].key);
        eprintln!("bin_buy_0        : {}", accounts[3 + D_BIN_BUY_0].key);
        eprintln!("bin_buy_1        : {}", accounts[3 + D_BIN_BUY_1].key);
        eprintln!("bin_sell_0       : {}", accounts[3 + D_BIN_SELL_0].key);
        eprintln!("bin_sell_1       : {}", accounts[3 + D_BIN_SELL_1].key);

        // 2. Program.new() -> print price and inverse_price
        let (price, inverse_price) = meteora.get_prices().unwrap();
        eprintln!("\n=== Prices ===");
        eprintln!("price            : {}", price);
        eprintln!("inverse_price    : {}", inverse_price);

        // 3. Print fees
        let (fee_factor, fee_factor_2) = meteora.get_fee_factor().unwrap();
        eprintln!("\n=== Fees ===");
        eprintln!("fee_numerator    : {}", meteora.fee_numerator);
        eprintln!("fee_factor       : {}", fee_factor);
        eprintln!("fee_factor_2     : {}", fee_factor_2);

        // 4. prepare_for_execution (DLMM's equivalent of prepare_for_execution)
        meteora.prepare_for_execution(&accounts, &clock).unwrap();
        eprintln!("\n=== After prepare_for_execution ===");
        eprintln!("buy_max_in       : {}", meteora.buy_max_in);
        eprintln!("buy_max_out      : {}", meteora.buy_max_out);
        eprintln!("sell_max_in      : {}", meteora.sell_max_in);
        eprintln!("sell_max_out     : {}", meteora.sell_max_out);

        // 5. Round-trip with start_amount = 1 WSOL
        let start_amount: u64 = 1_000_000_000; // 1 SOL

        let other_mint = if meteora.base_token_pk == sol_mint {
            meteora.quote_token_pk
        } else {
            meteora.base_token_pk
        };

        let rpc = get_rpc_client();
        let other_mint_account = rpc.get_account(
            &solana_sdk::pubkey::Pubkey::try_from(other_mint.to_bytes().as_ref()).unwrap()
        ).await.unwrap();
        let token_decimals = other_mint_account.data[44] as i32;
        let sol_div = 10f64.powi(9);
        let tok_div = 10f64.powi(token_decimals);

        // Direction 1: SOL -> TOKEN -> SOL
        eprintln!("\n=== Direction 1: SOL -> TOKEN -> SOL ===");
        let token_out = meteora.swap_base_in(
            &accounts, sol_mint, start_amount, no_fee, no_fee, &clock,
        ).unwrap();
        let max_sol_in = meteora.swap_base_out(
            &accounts, other_mint, token_out, no_fee, no_fee, &clock,
        ).unwrap();
        eprintln!("AMOUNT_IN {} -> AMOUNT_OUT {} -> MAX_AMOUNT_IN {}", start_amount as f64 / sol_div, token_out as f64 / tok_div, max_sol_in as f64 / sol_div);

        // Direction 2: TOKEN -> SOL -> TOKEN
        eprintln!("\n=== Direction 2: TOKEN -> SOL -> TOKEN ===");
        let sol_out = meteora.swap_base_in(
            &accounts, other_mint, token_out, no_fee, no_fee, &clock,
        ).unwrap();
        let max_token_in = meteora.swap_base_out(
            &accounts, sol_mint, sol_out, no_fee, no_fee, &clock,
        ).unwrap();
        eprintln!("AMOUNT_IN {} -> AMOUNT_OUT {} -> MAX_AMOUNT_IN {}", token_out as f64 / tok_div, sol_out as f64 / sol_div, max_token_in as f64 / tok_div);
    }
}
