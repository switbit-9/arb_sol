pub mod dlmm_lib;

use crate::programs::programs::ProgramMeta;
use crate::programs::SolarBError;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke,
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

/// Precomputed Q64 scale factor (2^64) for price calculations
/// Avoids recomputing `(1u128 << 64) as f64` on every call
const Q64_SCALE: f64 = 18446744073709551616.0; // (1u128 << SCALE_OFFSET) as f64

fn compute_fee_rate(lb_pair: &LbPair) -> anyhow::Result<f64> {
    let total_fee_rate = lb_pair.get_total_fee()?;
    Ok(total_fee_rate as f64 / FEE_PRECISION as f64)
}

fn get_prices(lb_price: u128) -> Result<(f64, f64)> {
    // Price is scaled by 2^64 (SCALE_OFFSET), so we need to divide by 2^64 to get actual price
    let price = lb_price as f64 / Q64_SCALE;
    let inverse_price = 1.0 / price;
    Ok((price, inverse_price))
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
    pub inverse_price: f64,
    pub static_base: usize,
    pub dyn_start: usize,
    pub fee_rate: f64,
    /// Cached from init: base→quote (X→Y) buy bins
    pub buy_max_in: u64,
    pub buy_max_out: u64,
    /// Cached from init: quote→base (Y→X) sell bins
    pub sell_max_in: u64,
    pub sell_max_out: u64,
    /// SwapCache for buy direction (swap_for_y = true) — boxed to reduce stack frame in new()
    pub buy_swap_cache: Box<SwapCache>,
    /// SwapCache for sell direction (swap_for_y = false) — boxed to reduce stack frame in new()
    pub sell_swap_cache: Box<SwapCache>,
    pub base_fee_rate: f64,
    pub quote_fee_rate: f64,
}

impl ProgramMeta for MeteoraDlmm {
    fn get_id(&self) -> &Pubkey {
        &Self::PROGRAM_ID
    }

    fn get_pool_id(&self) -> &Pubkey {
        &self.pool_id
    }

    fn swap_base_in<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        clock: &Clock,
    ) -> Result<u64> {
        let swap_for_y = input_mint == self.base_token_pk;
        let (bin_arrays, cache) = if swap_for_y {
            ([
                accounts[self.dyn_start + Self::D_BIN_BUY_0].clone(),
                accounts[self.dyn_start + Self::D_BIN_BUY_1].clone(),
            ], &self.buy_swap_cache)
        } else {
            ([
                accounts[self.dyn_start + Self::D_BIN_SELL_0].clone(),
                accounts[self.dyn_start + Self::D_BIN_SELL_1].clone(),
            ], &self.sell_swap_cache)
        };

        let quote = {
            quote_exact_in(
                &self.lb_pair_slim,
                amount_in,
                swap_for_y,
                bin_arrays,
                self.base_fee_rate,
                self.quote_fee_rate,
                cache,
            )
        }
        .map_err(|e| {
            let error_msg = format!("ERROR in quote_exact_in: {}", e);
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!("{}", error_msg);
            msg!("{}", error_msg);
            anchor_lang::error::Error::from(ProgramError::Custom(2004))
        })?;
        Ok(quote.amount_out)
    }

    fn swap_base_out<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        clock: &Clock,
    ) -> Result<u64> {
        let swap_for_y = output_mint == self.quote_token_pk;

        let (bin_arrays, cache) = if swap_for_y {
            ([
                accounts[self.dyn_start + Self::D_BIN_BUY_0].clone(),
                accounts[self.dyn_start + Self::D_BIN_BUY_1].clone(),
            ], &self.buy_swap_cache)
        } else {
            ([
                accounts[self.dyn_start + Self::D_BIN_SELL_0].clone(),
                accounts[self.dyn_start + Self::D_BIN_SELL_1].clone(),
            ], &self.sell_swap_cache)
        };

        let quote = {
            quote_exact_out(
                &self.lb_pair_slim,
                amount_out,
                swap_for_y,
                bin_arrays,
                self.base_fee_rate,
                self.quote_fee_rate,
                cache,
            )
        }
        .map_err(|e| {
            let error_msg = format!("ERROR in quote_exact_out: {}", e);
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!("{}", error_msg);
            msg!("{}", error_msg);
            anchor_lang::error::Error::from(ProgramError::Custom(2004))
        })?;
        Ok(quote.amount_in)
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        Ok((self.price, self.inverse_price))
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (&self.base_token_pk, &self.quote_token_pk)
    }

    fn name(&self) -> &'static str { "MeteoraDLMM" }

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
        msg!("[static] S0 program_id: {}", accounts[self.static_base + Self::S_PROGRAM_ID].key);
        msg!("[static] S1 host_fee_in: {}", accounts[self.static_base + Self::S_HOST_FEE_IN].key);
        msg!("[static] S2 event_authority: {}", accounts[self.static_base + Self::S_EVENT_AUTHORITY].key);
        msg!("[dynamic] D0 pool: {}", accounts[self.dyn_start + Self::D_POOL].key);
        msg!("[dynamic] D1 base_vault: {}", accounts[self.dyn_start + Self::D_BASE_VAULT].key);
        msg!("[dynamic] D2 quote_vault: {}", accounts[self.dyn_start + Self::D_QUOTE_VAULT].key);
        msg!("[dynamic] D3 oracle: {}", accounts[self.dyn_start + Self::D_ORACLE].key);
        msg!("[dynamic] D4 bitmap_ext: {}", accounts[self.dyn_start + Self::D_BITMAP_EXT].key);
        msg!("[dynamic] D5 bin_buy_0: {}", accounts[self.dyn_start + Self::D_BIN_BUY_0].key);
        msg!("[dynamic] D6 bin_buy_1: {}", accounts[self.dyn_start + Self::D_BIN_BUY_1].key);
        msg!("[dynamic] D7 bin_sell_0: {}", accounts[self.dyn_start + Self::D_BIN_SELL_0].key);
        msg!("[dynamic] D8 bin_sell_1: {}", accounts[self.dyn_start + Self::D_BIN_SELL_1].key);
        msg!("[mints] base_token: {}", self.base_token_pk);
        msg!("[mints] quote_token: {}", self.quote_token_pk);
        Ok(())
    }


    fn get_fee_factor(&self) -> Result<(f64, f64)> {
        let f = 1.0 - self.fee_rate;
        Ok((f, f))
    }

    fn fast_quote(&mut self, input_mint: Pubkey, amount_in: u64, profit_pct: f64) -> Result<(u64, u64)> {
        let (max_in, max_out) = self.get_cached_max_amounts(input_mint);
        let max_in_active = self.get_max_amount_in_active_bin(input_mint).unwrap_or(u64::MAX);

        let (p, f) = if input_mint == self.base_token_pk {
            (self.price, 1.0 - self.fee_rate)
        } else {
            (self.inverse_price, 1.0 - self.fee_rate)
        };

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


        if max_in_active < amount_in && max_in > max_in_active {
            let bin_step_frac = self.lb_pair_slim.bin_step as f64 / 10000.0;

            if profit_pct > bin_step_frac {
                debug_eprintln!("[DLMM] Crossing bins: profit {:.2}% > bin step {:.2}%", profit_pct * 100.0, bin_step_frac * 100.0);
                let pf = p * f;
                let out_active = (max_in_active as f64 * pf) as u64;

                let remaining = amount_in.min(max_in) - max_in_active;
                let next_p = p / (1.0 + bin_step_frac);
                let out_next = (remaining as f64 * next_p * f) as u64;

                let total_in = max_in_active + remaining;
                let total_out = (out_active + out_next).min(max_out);
                return Ok((total_in, total_out));
            }
        }

        let clamped_in = amount_in.min(max_in).min(max_in_active);
        let out = (clamped_in as f64 * p * f) as u64;
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
        let program_id = &accounts[self.static_base + Self::S_PROGRAM_ID];
        let host_fee_in = &accounts[self.static_base + Self::S_HOST_FEE_IN];
        let event_authority = &accounts[self.static_base + Self::S_EVENT_AUTHORITY];
        let pool_id = &accounts[self.dyn_start + Self::D_POOL];
        let base_vault = &accounts[self.dyn_start + Self::D_BASE_VAULT];
        let quote_vault = &accounts[self.dyn_start + Self::D_QUOTE_VAULT];
        let oracle = &accounts[self.dyn_start + Self::D_ORACLE];
        let bitmap_extension = &accounts[self.dyn_start + Self::D_BITMAP_EXT];
        let memo = &accounts[3];

        let swap_for_y = input_mint == self.base_token_pk;

        let (bin_array_1, bin_array_2) = if swap_for_y {
            (
                accounts[self.dyn_start + Self::D_BIN_BUY_0].clone(),
                accounts[self.dyn_start + Self::D_BIN_BUY_1].clone(),
            )
        } else {
            (
                accounts[self.dyn_start + Self::D_BIN_SELL_0].clone(),
                accounts[self.dyn_start + Self::D_BIN_SELL_1].clone(),
            )
        };

        let mut metas = Vec::with_capacity(18);
        // Determine base/quote mint AccountInfos from the passed-in mint accounts
        let (base_mint_info, quote_mint_info) = if mint_1_account.key == &self.base_token_pk {
            (mint_1_account, mint_2_account)
        } else {
            (mint_2_account, mint_1_account)
        };

        metas.push(AccountMeta::new(*pool_id.key, false));
        metas.push(if *bitmap_extension.key == Self::PROGRAM_ID {
            AccountMeta::new_readonly(*bitmap_extension.key, false)
        } else {
            AccountMeta::new(*bitmap_extension.key, false)
        });
        metas.push(AccountMeta::new(*base_vault.key, false));
        metas.push(AccountMeta::new(*quote_vault.key, false));
        metas.push(AccountMeta::new(*user_token_in.key, false));
        metas.push(AccountMeta::new(*user_token_out.key, false));
        metas.push(AccountMeta::new_readonly(self.base_token_pk, false));
        metas.push(AccountMeta::new_readonly(self.quote_token_pk, false));
        metas.push(AccountMeta::new(*oracle.key, false));
        metas.push(AccountMeta::new(*host_fee_in.key, false));
        metas.push(AccountMeta::new(*payer.key, true));
        metas.push(AccountMeta::new_readonly(*base_token_program.key, false));
        metas.push(AccountMeta::new_readonly(*quote_token_program.key, false));
        metas.push(AccountMeta::new_readonly(*memo.key, false));
        metas.push(AccountMeta::new_readonly(*event_authority.key, false));
        metas.push(AccountMeta::new_readonly(Self::PROGRAM_ID, false));
        metas.push(AccountMeta::new(*bin_array_1.key, false));
        metas.push(AccountMeta::new(*bin_array_2.key, false));

        // swap2 instruction discriminator (SwapExactIn)
        let mut data = [0u8; 32];
        data[..8].copy_from_slice(&Self::SWAP_EXACT_IN_DISC);
        data[8..16].copy_from_slice(&max_amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&amount_out_value.to_le_bytes());
        // data[24..32] already zeroed: empty vec slices + empty vec info (2x u32)

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data: data.to_vec(),
        };

        // Collect AccountInfo into a vector and use unsafe to cast lifetimes
        // Order must match metas order exactly
        let mut accounts_vec: Vec<AccountInfo<'a>> = Vec::with_capacity(18);
        accounts_vec.push(pool_id.clone());
        accounts_vec.push(bitmap_extension.clone());
        accounts_vec.push(base_vault.clone());
        accounts_vec.push(quote_vault.clone());
        accounts_vec.push(unsafe { std::mem::transmute(user_token_in.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(user_token_out.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(base_mint_info.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(quote_mint_info.to_account_info()) });
        accounts_vec.push(oracle.clone());
        accounts_vec.push(host_fee_in.clone());
        accounts_vec.push(unsafe { std::mem::transmute(payer.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(base_token_program.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(quote_token_program.to_account_info()) });
        accounts_vec.push(memo.clone());
        accounts_vec.push(event_authority.clone());
        accounts_vec.push(program_id.clone());
        accounts_vec.push(bin_array_1.clone());
        accounts_vec.push(bin_array_2.clone());

        unsafe {
            let accounts: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts)?;
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

        let min_amount_out_value = min_amount_out.unwrap_or(0);

        // Get stored accounts - static from static_base, dynamic from dyn_start
        let program_id = &accounts[self.static_base + Self::S_PROGRAM_ID];
        let host_fee_in = &accounts[self.static_base + Self::S_HOST_FEE_IN];
        let event_authority = &accounts[self.static_base + Self::S_EVENT_AUTHORITY];
        let pool_id = &accounts[self.dyn_start + Self::D_POOL];
        let base_vault = &accounts[self.dyn_start + Self::D_BASE_VAULT];
        let quote_vault = &accounts[self.dyn_start + Self::D_QUOTE_VAULT];
        let oracle = &accounts[self.dyn_start + Self::D_ORACLE];
        let bitmap_extension = &accounts[self.dyn_start + Self::D_BITMAP_EXT];
        let memo = &accounts[3];

        let swap_for_y = input_mint == self.base_token_pk;

        let (bin_array_1, bin_array_2) = if swap_for_y {
            (
                accounts[self.dyn_start + Self::D_BIN_BUY_0].clone(),
                accounts[self.dyn_start + Self::D_BIN_BUY_1].clone(),
            )
        } else {
            (
                accounts[self.dyn_start + Self::D_BIN_SELL_0].clone(),
                accounts[self.dyn_start + Self::D_BIN_SELL_1].clone(),
            )
        };

        // Determine base/quote mint AccountInfos from the passed-in mint accounts
        let (base_mint_info, quote_mint_info) = if mint_1_account.key == &self.base_token_pk {
            (mint_1_account, mint_2_account)
        } else {
            (mint_2_account, mint_1_account)
        };

        let mut metas = Vec::with_capacity(18);
        metas.push(AccountMeta::new(*pool_id.key, false));
        metas.push(AccountMeta::new(*bitmap_extension.key, false));
        metas.push(AccountMeta::new(*base_vault.key, false));
        metas.push(AccountMeta::new(*quote_vault.key, false));
        metas.push(AccountMeta::new(*user_base_token_account.key, false));
        metas.push(AccountMeta::new(*user_quote_token_account.key, false));
        metas.push(AccountMeta::new_readonly(self.base_token_pk, false));
        metas.push(AccountMeta::new_readonly(self.quote_token_pk, false));
        metas.push(AccountMeta::new(*oracle.key, false));
        metas.push(AccountMeta::new(*host_fee_in.key, false));
        metas.push(AccountMeta::new(*payer.key, true));
        metas.push(AccountMeta::new_readonly(*base_token_program.key, false));
        metas.push(AccountMeta::new_readonly(*quote_token_program.key, false));
        metas.push(AccountMeta::new_readonly(*memo.key, false));
        metas.push(AccountMeta::new_readonly(*event_authority.key, false));
        metas.push(AccountMeta::new_readonly(Self::PROGRAM_ID, false));
        metas.push(AccountMeta::new(*bin_array_1.key, false));
        metas.push(AccountMeta::new(*bin_array_2.key, false));

        // swap_exact_out2 instruction discriminator (SwapExactOut)
        let mut data = [0u8; 32];
        data[..8].copy_from_slice(&Self::SWAP_EXACT_OUT_DISC);
        data[8..16].copy_from_slice(&amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&min_amount_out_value.to_le_bytes());
        // data[24..32] already zeroed: empty vec slices + empty vec info (2x u32)

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data: data.to_vec(),
        };

        // Collect AccountInfo into a vector and use unsafe to cast lifetimes
        // Order must match metas order exactly
        let accounts_vec: Vec<AccountInfo<'a>> = vec![
            pool_id.clone(),
            bitmap_extension.clone(),
            base_vault.clone(),
            quote_vault.clone(),
            unsafe { std::mem::transmute(user_base_token_account.to_account_info()) },
            unsafe { std::mem::transmute(user_quote_token_account.to_account_info()) },
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
            let accounts: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts)?;
        }
        Ok(())
    }


}

impl MeteoraDlmm {
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

    #[inline(never)]
    pub fn new(
        accounts: &[AccountInfo],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
        clock: &Clock,
        mint_fees: &[(Pubkey, f64)],
    ) -> Result<Self> {
        // Clone accounts we need before using the Vec
        let pool_id = accounts[dyn_start + Self::D_POOL].clone();
        let (mut lb_pair, lb_price) = {
            // Borrow data from the cloned pool_id, not from the Vec
            let pool_data = pool_id.try_borrow_data()?;
            let slice = &pool_data[8..];
            // Box immediately to avoid large LbPair on the stack
            let lb: Box<LbPair> = Box::new(bytemuck::pod_read_unaligned(slice));
            let pr: u128 = get_price_from_id(lb.active_id, lb.bin_step)
                .map_err(|_| error!(SolarBError::InsufficientAccounts))?;
            (lb, pr)
        };

        // Read token mints from pool state (no longer passed as accounts)
        let base_token_pk = lb_pair.token_x_mint;
        let quote_token_pk = lb_pair.token_y_mint;

        let bitmap_extension_account = &accounts[dyn_start + Self::D_BITMAP_EXT];
        let bitmap_extension: Option<BinArrayBitmapExtension> = if *bitmap_extension_account.key
            != Self::PROGRAM_ID
            && bitmap_extension_account.data_len() > 8
        {
            Some(bytemuck::pod_read_unaligned(
                &bitmap_extension_account.try_borrow_data()?[8..],
            ))
        } else {
            None
        };

        // Fixed layout: [buy_bin_0, buy_bin_1, sell_bin_0, sell_bin_1]
        let all_bin_arrays = vec![
            accounts[dyn_start + Self::D_BIN_BUY_0].clone(),
            accounts[dyn_start + Self::D_BIN_BUY_1].clone(),
            accounts[dyn_start + Self::D_BIN_SELL_0].clone(),
            accounts[dyn_start + Self::D_BIN_SELL_1].clone(),
        ];

        let active_bin = get_active_bin_array(
            *pool_id.key,
            &lb_pair,
            bitmap_extension.as_ref(),
            true,
            all_bin_arrays,
        )
        .map_err(|_| {
            msg!("pool_id: {}", pool_id.key);
            error!(SolarBError::InsufficientBinArray)
        })?;
        let (price, inverse_price) = get_prices(lb_price)?;

        #[cfg(test)]
        let (price, inverse_price) = {
            let skew = if lb_price % 2 == 0 { 1.03 } else { 0.97 };
            (price * skew, inverse_price * (1.0 / skew))
        };
        
        // Pre-apply update_references on the stored lb_pair (saves ~5K CU per swap call)
        let _ = lb_pair.update_references(clock.unix_timestamp);

        // Approximate max amounts from first bin array only, using active bin price.
        let buy_acc = &accounts[dyn_start + Self::D_BIN_BUY_0];
        let sell_acc = &accounts[dyn_start + Self::D_BIN_SELL_0];
        let (buy_total_y, sell_total_x) = if buy_acc.key == sell_acc.key {
            let (tx, ty) = Self::sum_bin_array_raw(buy_acc);
            (ty, tx)
        } else {
            let (_, ty) = Self::sum_bin_array_raw(buy_acc);
            let (tx, _) = Self::sum_bin_array_raw(sell_acc);
            (ty, tx)
        };
        // Buy (swap_for_y): input X, output Y
        let buy_max_out_val = buy_total_y;
        let buy_max_in_val = if price > 0.0 { (buy_total_y as f64 / price) as u64 } else { 0 };
        // Sell (swap_for_x): input Y, output X
        let sell_max_out_val = sell_total_x;
        let sell_max_in_val = if price > 0.0 { (sell_total_x as f64 * price) as u64 } else { 0 };

        let base_fee_rate = crate::utils::token::lookup_fee_rate(mint_fees, &base_token_pk);
        let quote_fee_rate = crate::utils::token::lookup_fee_rate(mint_fees, &quote_token_pk);

        let (buy_swap_cache, sell_swap_cache, fee_rate) =
            Self::build_swap_caches(&mut lb_pair, accounts, dyn_start)
                .map_err(|_| error!(SolarBError::TransferFeeCalculationError))?;

        // Extract slim fields — full LbPair is dropped after this
        let lb_pair_slim = LbPairSlim {
            active_id: lb_pair.active_id,
            bin_step: lb_pair.bin_step,
            volatility_accumulator: lb_pair.v_parameters.volatility_accumulator,
            volatility_reference: lb_pair.v_parameters.volatility_reference,
            index_reference: lb_pair.v_parameters.index_reference,
            max_vol_acc: lb_pair.parameters.max_volatility_accumulator,
            variable_fee_control: lb_pair.parameters.variable_fee_control,
        };

        let instance = MeteoraDlmm {
            base_token_pk,
            quote_token_pk,
            pool_id: *pool_id.key,
            lb_pair_slim,
            active_bin,
            lb_price,
            price,
            inverse_price,
            fee_rate,
            static_base,
            dyn_start,
            buy_max_in: buy_max_in_val,
            buy_max_out: buy_max_out_val,
            sell_max_in: sell_max_in_val,
            sell_max_out: sell_max_out_val,
            buy_swap_cache,
            sell_swap_cache,
            base_fee_rate,
            quote_fee_rate,
        };
        // instance.log_accounts(accounts)?;
        Ok(instance)
    }

    /// Sum raw (amount_x, amount_y) from a single bin array — no price math, just byte reads.
    fn sum_bin_array_raw(acc: &AccountInfo) -> (u64, u64) {
        const BIN_ARRAY_HEADER_SIZE: usize = 56;
        const BIN_SIZE: usize = 144;
        const MAX_BIN_PER_ARRAY: usize = 70;

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
                Self::read_bin_array_index(&accounts[dyn_start + Self::D_BIN_BUY_0]),
                Self::read_bin_array_index(&accounts[dyn_start + Self::D_BIN_BUY_1]),
            ],
            initial_vol_acc,
            has_variable_fee,
        });
        let sell = Box::new(SwapCache {
            base_fee,
            price_base,
            bin_array_indices: [
                Self::read_bin_array_index(&accounts[dyn_start + Self::D_BIN_SELL_0]),
                Self::read_bin_array_index(&accounts[dyn_start + Self::D_BIN_SELL_1]),
            ],
            initial_vol_acc,
            has_variable_fee,
        });
        Ok((buy, sell, fee_rate))
    }

    /// Compute approximate (max_amount_in, max_amount_out) from first bin array + active price.
    pub fn compute_max_amounts_in_out(&self, accounts: &[AccountInfo], input_mint: Pubkey) -> Result<(u64, u64)> {
        let swap_for_y = self.base_token_pk == input_mint;
        let (arr0, arr1) = if swap_for_y {
            (
                &accounts[self.dyn_start + Self::D_BIN_BUY_0],
                &accounts[self.dyn_start + Self::D_BIN_BUY_1],
            )
        } else {
            (
                &accounts[self.dyn_start + Self::D_BIN_SELL_0],
                &accounts[self.dyn_start + Self::D_BIN_SELL_1],
            )
        };
        let (x0, y0) = Self::sum_bin_array_raw(arr0);
        let (x1, y1) = if arr0.key() != arr1.key() {
            Self::sum_bin_array_raw(arr1)
        } else {
            (0, 0)
        };
        let (total_x, total_y) = (x0.saturating_add(x1), y0.saturating_add(y1));
        if swap_for_y {
            let max_in = if self.price > 0.0 { (total_y as f64 / self.price) as u64 } else { 0 };
            Ok((max_in, total_y))
        } else {
            let max_in = if self.price > 0.0 { (total_x as f64 * self.price) as u64 } else { 0 };
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
            ([
                &accounts[self.dyn_start + Self::D_BIN_BUY_0],
                &accounts[self.dyn_start + Self::D_BIN_BUY_1],
            ], &self.buy_swap_cache)
        } else {
            ([
                &accounts[self.dyn_start + Self::D_BIN_SELL_0],
                &accounts[self.dyn_start + Self::D_BIN_SELL_1],
            ], &self.sell_swap_cache)
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
        let fee_factor = 1.0 - self.fee_rate;
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
    use anchor_lang::prelude::{Clock, InterfaceAccount};
    use anchor_lang::solana_program::{account_info::AccountInfo, pubkey::Pubkey, system_program};
    use anchor_spl::token_interface::Mint;
    use super::dlmm_lib;
    use dlmm_lib::dlmm::accounts::BinArray;
    use dlmm_lib::quote::get_bin_array_pubkeys_for_swap;

    // Helper function to create a mock AccountInfo with provided data
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
            key_static,
            false,
            true,
            lamports,
            data_vec,
            owner_static,
            false,
            0,
        )
    }

    // Helper to convert solana_sdk::account::Account to AccountInfo
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
            key_static,
            false, // is_signer
            false, // is_writable
            lamports,
            data,
            owner_static,
            account.executable,
            account.rent_epoch,
        )
    }

    // Helper function to fetch account from RPC and convert to AccountInfo
    async fn fetch_account_info_from_rpc(
        rpc_client: &solana_client::nonblocking::rpc_client::RpcClient,
        key: Pubkey,
    ) -> AccountInfo<'static> {
        use solana_sdk::pubkey::Pubkey as SdkPubkey;

        let sdk_pubkey = SdkPubkey::try_from(key.to_bytes().as_ref())
            .expect("Failed to convert Pubkey to SdkPubkey");
        let account = rpc_client
            .get_account(&sdk_pubkey)
            .await
            .expect(&format!("Failed to fetch account {}", key));
        account_to_account_info(key, account)
    }

    // Helper function to fetch account from RPC with fallback - returns Option
    async fn try_fetch_account_info_from_rpc(
        rpc_client: &solana_client::nonblocking::rpc_client::RpcClient,
        key: Pubkey,
    ) -> Option<AccountInfo<'static>> {
        use solana_sdk::pubkey::Pubkey as SdkPubkey;

        let sdk_pubkey = SdkPubkey::try_from(key.to_bytes().as_ref()).ok()?;
        let account = rpc_client.get_account(&sdk_pubkey).await.ok()?;
        Some(account_to_account_info(key, account))
    }

    /// Get on chain clock from RPC
    async fn get_clock(
        rpc_client: &solana_client::nonblocking::rpc_client::RpcClient,
    ) -> anyhow::Result<Clock> {
        use anchor_client::solana_sdk::sysvar;

        let clock_account = rpc_client.get_account(&sysvar::clock::ID).await?;

        // Clock from Solana is borsh-serialized with these fields in order:
        // slot: u64 (8 bytes)
        // epoch_start_timestamp: i64 (8 bytes)
        // epoch: u64 (8 bytes)
        // leader_schedule_epoch: u64 (8 bytes)
        // unix_timestamp: i64 (8 bytes)
        // Total: 40 bytes
        if clock_account.data.len() < 40 {
            return Err(anyhow::anyhow!(
                "Clock account data too short: {} bytes",
                clock_account.data.len()
            ));
        }

        let data = &clock_account.data;
        let slot = u64::from_le_bytes(
            data[0..8]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to parse slot"))?,
        );
        let epoch_start_timestamp = i64::from_le_bytes(
            data[8..16]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to parse epoch_start_timestamp"))?,
        );
        let epoch = u64::from_le_bytes(
            data[16..24]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to parse epoch"))?,
        );
        let leader_schedule_epoch = u64::from_le_bytes(
            data[24..32]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to parse leader_schedule_epoch"))?,
        );
        let unix_timestamp = i64::from_le_bytes(
            data[32..40]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to parse unix_timestamp"))?,
        );

        Ok(Clock {
            slot,
            epoch_start_timestamp,
            epoch,
            leader_schedule_epoch,
            unix_timestamp,
        })
    }

    /// Convert raw RPC account to InterfaceAccount<Mint>
    fn account_to_interface_mint(
        account: solana_sdk::account::Account,
        pubkey: Pubkey,
    ) -> InterfaceAccount<'static, Mint> {
        let data = Box::leak(Box::new(account.data));
        let lamports = Box::leak(Box::new(account.lamports));
        let owner = Box::leak(Box::new(account.owner));
        let key = Box::leak(Box::new(pubkey));

        // Create AccountInfo with 'static lifetime
        let account_info: &'static AccountInfo<'static> = Box::leak(Box::new(AccountInfo::new(
            key, false, false, lamports, data, owner, false, 0,
        )));

        // Create InterfaceAccount from AccountInfo
        // Since AccountInfo is 'static, InterfaceAccount will also be 'static
        InterfaceAccount::<Mint>::try_from(account_info).expect("Failed to create InterfaceAccount")
    }

    async fn build_test_scenario() -> (MeteoraDlmm, Vec<AccountInfo<'static>>) {
        use anchor_client::Cluster;
        use dlmm_lib::pda;
        use solana_client::nonblocking::rpc_client::RpcClient;
        use std::collections::HashMap;

        let cluster: Cluster = Cluster::Mainnet;
        let rpc_client = RpcClient::new(cluster.url().to_string());

        let pool_id: Pubkey = if cluster == Cluster::Mainnet {
            Pubkey::from_str_const("7eexH14UjhNxJe6zTT3f1Vb1E8iACsBMVaWheDEmxdT2")
        } else {
            Pubkey::from_str_const("FT8ueq7bP7DpBoP6b3QSsos3TkRY9JYCbGLCLKA3tgUn")
        };

        let lb_pair_account = rpc_client.get_account(&pool_id).await.unwrap();

        let lb_pair: LbPair = bytemuck::pod_read_unaligned(&lb_pair_account.data[8..]);

        let program_id_key = MeteoraDlmm::PROGRAM_ID;
        let base_vault_key = lb_pair.reserve_x;
        let quote_vault_key = lb_pair.reserve_y;
        let token_x_mint_key = lb_pair.token_x_mint;
        let token_y_mint_key = lb_pair.token_y_mint;
        let oracle_key = lb_pair.oracle;
        let (bitmap_extension_key, _) = pda::derive_bin_array_bitmap_extension(pool_id);

        // Get exactly 2 buy bin arrays and 2 sell bin arrays (fixed layout)
        let left_bin_array_pubkeys =
            get_bin_array_pubkeys_for_swap(pool_id, &lb_pair, None, true, 2).unwrap();

        let right_bin_array_pubkeys =
            get_bin_array_pubkeys_for_swap(pool_id, &lb_pair, None, false, 2)
                .unwrap();

        debug_eprintln!("mint_x_account: {:?}", token_x_mint_key);
        debug_eprintln!("mint_y_account: {:?}", token_y_mint_key);
        debug_eprintln!("reserve_x: {:?}", base_vault_key);
        debug_eprintln!("reserve_y: {:?}", quote_vault_key);
        debug_eprintln!("oracle: {:?}", oracle_key);
        debug_eprintln!("bitmap_extension: {:?}", bitmap_extension_key);
        for key in left_bin_array_pubkeys.clone() {
            debug_eprintln!("left_bin_array: {:?}", key);
        }

        // Fetch bin arrays separately to maintain order
        let left_bin_array_accounts = rpc_client
            .get_multiple_accounts(&left_bin_array_pubkeys)
            .await
            .unwrap();

        let right_bin_array_accounts = rpc_client
            .get_multiple_accounts(&right_bin_array_pubkeys)
            .await
            .unwrap();

        // Process left bin arrays (buy arrays)
        let mut bin_array_buy_infos = Vec::new();
        let mut bin_arrays_map = HashMap::new();
        for (account_opt, key) in left_bin_array_accounts
            .iter()
            .zip(left_bin_array_pubkeys.iter())
        {
            if let Some(account) = account_opt {
                let bin_array: BinArray = bytemuck::pod_read_unaligned(&account.data[8..]);
                let account_info = account_to_account_info(*key, account.clone());
                bin_array_buy_infos.push(account_info);
                bin_arrays_map.insert(*key, bin_array);
            }
        }

        // Process right bin arrays (sell arrays)
        let mut bin_array_sell_infos = Vec::new();
        for (account_opt, key) in right_bin_array_accounts
            .iter()
            .zip(right_bin_array_pubkeys.iter())
        {
            if let Some(account) = account_opt {
                let bin_array: BinArray = bytemuck::pod_read_unaligned(&account.data[8..]);
                let account_info = account_to_account_info(*key, account.clone());
                bin_array_sell_infos.push(account_info);
                bin_arrays_map.insert(*key, bin_array);
            }
        }

        // Combine all bin arrays for quote function
        let mut bin_array_all_infos = bin_array_buy_infos.clone();
        bin_array_all_infos.extend(bin_array_sell_infos.clone());

        // Create program_id account
        let program_id_account =
            create_mock_account_info_with_data(program_id_key, system_program::id(), None);

        let pool_id_account_info = account_to_account_info(pool_id, lb_pair_account);
        let base_vault_account = fetch_account_info_from_rpc(&rpc_client, lb_pair.reserve_x).await;
        let quote_vault_account = fetch_account_info_from_rpc(&rpc_client, lb_pair.reserve_y).await;
        let oracle_account = fetch_account_info_from_rpc(&rpc_client, lb_pair.oracle).await;
        // Derive bitmap extension PDA
        let bitmap_extension_account: AccountInfo<'static> =
            try_fetch_account_info_from_rpc(&rpc_client, bitmap_extension_key)
                .await
                .unwrap_or_else(|| program_id_account.clone());

        // host_fee_in, memo, and event_authority are not fields on LbPair - use placeholder accounts
        // These are optional accounts used in swap instructions
        let host_fee_in_key = Pubkey::default(); // Placeholder - can be zero for swaps without host fee
        let (event_authority_key, _) = pda::derive_event_authority_pda();

        let host_fee_in_account =
            create_mock_account_info_with_data(host_fee_in_key, system_program::id(), None);
        let event_authority_account =
            create_mock_account_info_with_data(event_authority_key, system_program::id(), None);

        assert!(bin_array_buy_infos.len() >= 2, "need at least 2 buy bin arrays");
        assert!(bin_array_sell_infos.len() >= 2, "need at least 2 sell bin arrays");

        debug_eprintln!("program_id_account: {:?}", program_id_account.key);
        // Static accounts (static_base = 0): program_id, host_fee_in, event_authority
        // Dynamic accounts (dyn_start = 3): pool, base_vault, quote_vault, oracle, bitmap_ext, bin_buy_0, bin_buy_1, bin_sell_0, bin_sell_1
        let accounts = vec![
            // --- static (indices 0..2) ---
            program_id_account,         // S0: program_id
            host_fee_in_account,        // S1: host_fee_in
            event_authority_account,    // S2: event_authority
            // --- dynamic (indices 3..11) ---
            pool_id_account_info,       // D0: pool
            base_vault_account,         // D1: base_vault
            quote_vault_account,        // D2: quote_vault
            oracle_account,             // D3: oracle
            bitmap_extension_account,   // D4: bitmap_ext
            bin_array_buy_infos[0].clone(),  // D5: bin_buy_0
            bin_array_buy_infos[1].clone(),  // D6: bin_buy_1
            bin_array_sell_infos[0].clone(), // D7: bin_sell_0
            bin_array_sell_infos[1].clone(), // D8: bin_sell_1
        ];
        let static_base: usize = 0;
        let dyn_start: usize = 3;
        let dyn_end: usize = accounts.len();
        let clock = get_clock(&rpc_client).await.unwrap();
        let meteora_dlmm = MeteoraDlmm::new(&accounts, static_base, dyn_start, dyn_end, &clock, &[]).unwrap();
        (meteora_dlmm, accounts)
    }

    #[tokio::test]
    async fn test_dlmm_swap_base_in_both_directions() {
        use anchor_client::Cluster;
        use solana_client::nonblocking::rpc_client::RpcClient;
        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let (mut meteora_dlmm, accounts) = build_test_scenario().await;
        let rpc_client: RpcClient = RpcClient::new(Cluster::Mainnet.url().to_string());
        let clock = get_clock(&rpc_client).await.unwrap();

        let (price, inverse_price) = meteora_dlmm.get_prices().unwrap();
        debug_eprintln!("price (Y per X): {:?}", price);
        debug_eprintln!("inverse_price (X per Y): {:?}", inverse_price);

        let other_mint = if meteora_dlmm.base_token_pk == sol_mint {
            meteora_dlmm.quote_token_pk
        } else {
            meteora_dlmm.base_token_pk
        };

        // Step 1: swap_base_in SOL -> TOKEN (1 SOL in, get tokens out)
        let in_sol_amount: u64 = 1_000_000_000;
        let tokens_out = meteora_dlmm
            .swap_base_in(&accounts, sol_mint, in_sol_amount, &clock)
            .unwrap();
        debug_eprintln!(
            "swap_base_in: {} SOL -> {} TOKEN",
            in_sol_amount as f64 / 1e9,
            tokens_out as f64 / 1e6,
        );

        let max_sol_in = meteora_dlmm.swap_base_out(&accounts, other_mint, tokens_out, &clock).unwrap();
        debug_eprintln!(
            "swap_base_out: {} TOKEN -> {} SOL",
            tokens_out as f64 / 1e6,
            max_sol_in as f64 / 1e9,
        );

        // Step 2: swap_base_in TOKEN -> SOL (reverse direction)
        let sol_out = meteora_dlmm
            .swap_base_in(&accounts, other_mint, tokens_out, &clock)
            .unwrap();
        debug_eprintln!(
            "swap_base_in: {} TOKEN -> {} SOL",
            tokens_out as f64 / 1e6,
            sol_out as f64 / 1e9,
        );

        let max_token_in = meteora_dlmm.swap_base_out(&accounts, sol_mint, sol_out, &clock).unwrap();
        debug_eprintln!(
            "swap_base_out: {} SOL -> {} TOKEN",
            sol_out as f64 / 1e9,
            max_token_in as f64 / 1e6,
        );
        // But shouldn't lose more than ~5% in a reasonable pool
        let loss_pct = 1.0 - (sol_out as f64 / in_sol_amount as f64);
        debug_eprintln!("round-trip loss: {:.4}%", loss_pct * 100.0);
        assert!(
            loss_pct < 0.05,
            "round-trip loss too high: {:.4}%",
            loss_pct * 100.0
        );
    }

    #[tokio::test]
    async fn test_dlmm_swap_base_out_both_directions() {
        use anchor_client::Cluster;
        use solana_client::nonblocking::rpc_client::RpcClient;
        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let (mut meteora_dlmm, accounts) = build_test_scenario().await;
        let rpc_client: RpcClient = RpcClient::new(Cluster::Mainnet.url().to_string());
        let clock = get_clock(&rpc_client).await.unwrap();

        let other_mint = if meteora_dlmm.base_token_pk == sol_mint {
            meteora_dlmm.quote_token_pk
        } else {
            meteora_dlmm.base_token_pk
        };

        // First get a reference amount: how many tokens do we get for 1 SOL?
        let in_sol_amount: u64 = 1_000_000_000;
        let tokens_from_base_in = meteora_dlmm
            .swap_base_in(&accounts, sol_mint, in_sol_amount, &clock)
            .unwrap();
        debug_eprintln!(
            "reference swap_base_in: {} SOL -> {} TOKEN",
            in_sol_amount as f64 / 1e9,
            tokens_from_base_in as f64 / 1e6,
        );

        // swap_base_out: "I want exactly tokens_from_base_in tokens, how much SOL do I need?"
        // output_mint = other_mint (the token), amount_out = tokens_from_base_in
        let sol_needed = meteora_dlmm
            .swap_base_out(&accounts, other_mint, tokens_from_base_in, &clock)
            .unwrap();
        debug_eprintln!(
            "swap_base_out: need {} SOL to get {} TOKEN",
            sol_needed as f64 / 1e9,
            tokens_from_base_in as f64 / 1e6,
        );
        assert!(sol_needed > 0, "swap_base_out should require input");
        // For the same direction (SOL->TOKEN), exact_out should require
        // the same or slightly more SOL than exact_in provided
        let ratio = sol_needed as f64 / in_sol_amount as f64;
        debug_eprintln!("base_out/base_in ratio: {:.6}", ratio);
        assert!(
            ratio > 0.95 && ratio < 1.05,
            "swap_base_out should be close to swap_base_in: ratio={}",
            ratio,
        );

        // swap_base_out reverse: "I want 1 SOL out, how many tokens do I need?"
        let desired_sol_out: u64 = 1_000_000_000;
        let tokens_needed = meteora_dlmm
            .swap_base_out(&accounts, sol_mint, desired_sol_out, &clock)
            .unwrap();
        debug_eprintln!(
            "swap_base_out: need {} TOKEN to get {} SOL",
            tokens_needed as f64 / 1e6,
            desired_sol_out as f64 / 1e9,
        );
        assert!(tokens_needed > 0, "swap_base_out reverse should require input");
    }

    #[tokio::test]
    async fn test_dlmm_round_trip_consistency() {
        use anchor_client::Cluster;
        use solana_client::nonblocking::rpc_client::RpcClient;
        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let (mut meteora_dlmm, accounts) = build_test_scenario().await;
        let rpc_client: RpcClient = RpcClient::new(Cluster::Mainnet.url().to_string());
        let clock = get_clock(&rpc_client).await.unwrap();

        let other_mint = if meteora_dlmm.base_token_pk == sol_mint {
            meteora_dlmm.quote_token_pk
        } else {
            meteora_dlmm.base_token_pk
        };

        let (fee_factor, inverse_fee_factor) = meteora_dlmm.get_fee_factor().unwrap();
        debug_eprintln!("fee_factor: {:.6}, inverse: {:.6}", fee_factor, inverse_fee_factor);

        // swap_base_in: 1 SOL -> tokens
        let in_sol_amount: u64 = 1_000_000_000;
        let tokens_out = meteora_dlmm
            .swap_base_in(&accounts, sol_mint, in_sol_amount, &clock)
            .unwrap();

        // swap_base_out: to get those same tokens, how much SOL is needed?
        let sol_needed_for_same_tokens = meteora_dlmm
            .swap_base_out(&accounts, other_mint, tokens_out, &clock)
            .unwrap();

        debug_eprintln!(
            "swap_base_in:  {} SOL -> {} TOKEN",
            in_sol_amount as f64 / 1e9,
            tokens_out as f64 / 1e6,
        );
        debug_eprintln!(
            "swap_base_out: {} SOL needed for {} TOKEN",
            sol_needed_for_same_tokens as f64 / 1e9,
            tokens_out as f64 / 1e6,
        );

        // For the same direction (SOL->TOKEN), exact_out should require
        // the same or slightly more SOL than exact_in provided
        assert!(
            sol_needed_for_same_tokens >= in_sol_amount
                || (in_sol_amount - sol_needed_for_same_tokens) < in_sol_amount / 100,
            "exact_out should require ~same or more input: needed={}, provided={}",
            sol_needed_for_same_tokens,
            in_sol_amount,
        );
    }
}
