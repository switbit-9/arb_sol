pub mod libraries;
pub mod states;

use self::libraries::{swap_math, tick_math};
use self::states::{OracleSimple, TickArraySimple, WhirlpoolSimple, FEE_RATE_HARD_LIMIT};
use crate::programs::{PoolKind, ProgramMeta};
use crate::utils::token::{apply_transfer_fee, apply_transfer_inverse_fee, MintFee};
use crate::utils::utils::read_token_amount;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    account_info::AccountInfo,
    instruction::AccountMeta,
    pubkey::Pubkey,
};
use crate::utils::cpi::invoke_cpi;

/// Orca Whirlpool Program ID
pub const PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc");
const SWAP_V2_DISC: [u8; 8] = [0x2b, 0x04, 0xed, 0x0b, 0x1a, 0xc9, 0x1e, 0x62];

// Static accounts (from static_base, 1 account)
pub const S_PROGRAM_ID: usize = 0;

// Dynamic accounts (from dyn_start, 7 accounts)
pub const D_POOL: usize = 0;
pub const D_VAULT_A: usize = 1;
pub const D_VAULT_B: usize = 2;
pub const D_ORACLE: usize = 3;
pub const D_TICK_ARRAY_0: usize = 4;
pub const D_TICK_ARRAY_1: usize = 5;
pub const D_TICK_ARRAY_2: usize = 6;

pub const DYNAMIC_ACCOUNTS: usize = 7;

/// Compute total fee rate (static + adaptive) as u32 in hundredths of a basis point.
/// Denominator is 1_000_000. Capped at FEE_RATE_HARD_LIMIT (100_000 = 10%).
/// If oracle data is unavailable or has no adaptive fee, returns just the static rate.
fn compute_total_fee_rate(static_fee_rate: u16, oracle_data: Option<&[u8]>) -> u32 {
    let adaptive = match oracle_data {
        Some(data) => OracleSimple::try_from_bytes(data)
            .map(|o| o.compute_adaptive_fee_rate())
            .unwrap_or(0),
        None => 0,
    };
    let total = static_fee_rate as u32 + adaptive;
    total.min(FEE_RATE_HARD_LIMIT)
}

/// Calculate price from sqrt_price_x64 as f64 (for logging only)
fn sqrt_price_to_price(sqrt_price_x64: u128) -> f64 {
    let sqrt_price = sqrt_price_x64 as f64 / (1u128 << 64) as f64;
    sqrt_price * sqrt_price
}

/// Compute price as f64 from sqrt_price_x64 (Q64.64 format).
fn sqrt_price_to_f64(sqrt_price_x64: u128) -> f64 {
    let sqrt_price = sqrt_price_x64 as f64 / (1u128 << 64) as f64;
    sqrt_price * sqrt_price
}

// ============================================================================
// OrcaWhirlpool Implementation
// ============================================================================

#[derive(Clone)]
pub struct OrcaWhirlpool {
    pub pool_id: Pubkey,
    pub whirlpools_config: Pubkey,
    pub base_token_pk: Pubkey,
    pub quote_token_pk: Pubkey,
    pub token_vault_a: Pubkey,
    pub token_vault_b: Pubkey,
    pub sqrt_price: u128,
    pub tick_current_index: i32,
    pub liquidity: u128,
    pub tick_spacing: u16,
    /// Total fee rate (static + adaptive) in hundredths of a basis point, denominator 1_000_000.
    /// Pre-computed once in new() to avoid recalculation during swaps.
    pub fee_rate: u32,
    pub protocol_fee_rate: u16,
    /// Pre-computed fee factor: 1 - fee_rate/1_000_000
    pub fee_factor: (f64, f64),
    pub price: f64,
    pub static_base: usize,
    pub dyn_start: usize,
    pub buy_max_in: u64,
    pub buy_max_out: u64,
    pub sell_max_in: u64,
    pub sell_max_out: u64,
    pub prepared: bool,
}

impl ProgramMeta for OrcaWhirlpool {
    fn get_id(&self) -> &Pubkey {
        &PROGRAM_ID
    }

    fn get_pool_id(&self) -> &Pubkey {
        &self.pool_id
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        let inverse = if self.price > 0.0 { 1.0 / self.price } else { 0.0 };
        Ok((self.price, inverse))
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (&self.base_token_pk, &self.quote_token_pk)
    }

    fn name(&self) -> &'static str { "OrcaWhirlpool" }
    fn pool_kind(&self) -> PoolKind { PoolKind::OrcaWhirlpool }

    fn get_fee_factor(&self) -> Result<(f64, f64)> { Ok(self.fee_factor) }

    fn swap_base_in<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        _clock: &Clock,
    ) -> Result<u64> {
        let a_to_b = input_mint == self.base_token_pk;

        let transfer_fee = apply_transfer_fee(amount_in, input_transfer_fee);
        let actual_amount_in = amount_in.saturating_sub(transfer_fee);

        // Borrow tick array data at this level to avoid nested stack frames
        let data_0 = accounts[self.dyn_start + D_TICK_ARRAY_0]
            .try_borrow_data()
            .ok();
        let data_1 = accounts[self.dyn_start + D_TICK_ARRAY_1]
            .try_borrow_data()
            .ok();
        let data_2 = accounts[self.dyn_start + D_TICK_ARRAY_2]
            .try_borrow_data()
            .ok();

        let amount_out =
            self.calculate_swap_base_in(actual_amount_in, a_to_b, &data_0, &data_1, &data_2)?;

        let out_fee = apply_transfer_fee(amount_out, output_transfer_fee);
        let final_amount_out = amount_out.saturating_sub(out_fee);

        Ok(final_amount_out)
    }

    fn swap_base_out<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        _clock: &Clock,
    ) -> Result<u64> {
        let a_to_b = output_mint == self.quote_token_pk;

        let out_fee = apply_transfer_inverse_fee(amount_out, output_transfer_fee);
        let amount_out_with_fee = amount_out
            .checked_add(out_fee)
            .ok_or(error!(crate::programs::SolarBError::FeeOverflow))?;

        // Borrow tick array data at this level to avoid nested stack frames
        let data_0 = accounts[self.dyn_start + D_TICK_ARRAY_0]
            .try_borrow_data()
            .ok();
        let data_1 = accounts[self.dyn_start + D_TICK_ARRAY_1]
            .try_borrow_data()
            .ok();
        let data_2 = accounts[self.dyn_start + D_TICK_ARRAY_2]
            .try_borrow_data()
            .ok();

        let amount_in =
            self.calculate_swap_base_out(amount_out_with_fee, a_to_b, &data_0, &data_1, &data_2)?;

        let in_fee = apply_transfer_inverse_fee(amount_in, input_transfer_fee);
        let final_amount_in = amount_in
            .checked_add(in_fee)
            .ok_or(error!(crate::programs::SolarBError::FeeOverflow))?;

        Ok(final_amount_in)
    }

    fn invoke_swap_base_in<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        min_amount_out: Option<u64>,
        payer: &AccountInfo<'a>,
        user_mint_1_token_account: &AccountInfo<'a>,
        user_mint_2_token_account: &AccountInfo<'a>,
        mint_1_account: &AccountInfo<'a>,
        mint_2_account: &AccountInfo<'a>,
        mint_1_token_program: &AccountInfo<'a>,
        mint_2_token_program: &AccountInfo<'a>,
    ) -> Result<()> {
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let vault_a = &accounts[self.dyn_start + D_VAULT_A];
        let vault_b = &accounts[self.dyn_start + D_VAULT_B];
        let oracle = &accounts[self.dyn_start + D_ORACLE];
        let memo = &accounts[3];
        let tick_array_0 = &accounts[self.dyn_start + D_TICK_ARRAY_0];
        let tick_array_1 = &accounts[self.dyn_start + D_TICK_ARRAY_1];
        let tick_array_2 = &accounts[self.dyn_start + D_TICK_ARRAY_2];

        let a_to_b = input_mint == self.base_token_pk;

        // Map mint_1/mint_2 to A/B order using stored mint pubkeys (mints from header)
        let (user_token_account_a, user_token_account_b, token_a, token_b, token_program_a, token_program_b) =
            if *mint_1_account.key == self.base_token_pk {
                (
                    user_mint_1_token_account,
                    user_mint_2_token_account,
                    mint_1_account,
                    mint_2_account,
                    mint_1_token_program,
                    mint_2_token_program,
                )
            } else {
                (
                    user_mint_2_token_account,
                    user_mint_1_token_account,
                    mint_2_account,
                    mint_1_account,
                    mint_2_token_program,
                    mint_1_token_program,
                )
            };

        // Build swap instruction
        let metas = [
            AccountMeta::new_readonly(*token_program_a.key, false),
            AccountMeta::new_readonly(*token_program_b.key, false),
            AccountMeta::new_readonly(*memo.key, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new_readonly(*token_a.key, false),
            AccountMeta::new_readonly(*token_b.key, false),
            AccountMeta::new(*user_token_account_a.key, false),
            AccountMeta::new(*vault_a.key, false),
            AccountMeta::new(*user_token_account_b.key, false),
            AccountMeta::new(*vault_b.key, false),
            AccountMeta::new(*tick_array_0.key, false),
            AccountMeta::new(*tick_array_1.key, false),
            AccountMeta::new(*tick_array_2.key, false),
            AccountMeta::new(*oracle.key, false),
        ];

        // Swap discriminator: swap_v2 (SHA256("global:swap_v2")[..8])
        let mut data = [0u8; 43];
        data[..8].copy_from_slice(&SWAP_V2_DISC);
        data[8..16].copy_from_slice(&amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&min_amount_out.unwrap_or(0).to_le_bytes());
        // data[24..40] already zeroed: sqrt_price_limit u128 = 0
        data[40] = 1; // amount_specified_is_input: bool = true
        data[41] = if a_to_b { 1 } else { 0 }; // a_to_b
        // data[42] = 0: remaining_accounts_info = None

        let accounts_arr = [
            token_program_a.clone(),
            token_program_b.clone(),
            memo.clone(),
            payer.clone(),
            pool_id.clone(),
            token_a.clone(),
            token_b.clone(),
            user_token_account_a.clone(),
            vault_a.clone(),
            user_token_account_b.clone(),
            vault_b.clone(),
            tick_array_0.clone(),
            tick_array_1.clone(),
            tick_array_2.clone(),
            oracle.clone(),
        ];

        invoke_cpi(&PROGRAM_ID, &metas, &data, &accounts_arr)?;
        Ok(())
    }

    fn invoke_swap_base_out<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        max_amount_in: u64,
        amount_out: Option<u64>,
        payer: &AccountInfo<'a>,
        user_mint_1_token_account: &AccountInfo<'a>,
        user_mint_2_token_account: &AccountInfo<'a>,
        mint_1_account: &AccountInfo<'a>,
        mint_2_account: &AccountInfo<'a>,
        mint_1_token_program: &AccountInfo<'a>,
        mint_2_token_program: &AccountInfo<'a>,
    ) -> Result<()> {
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let vault_a = &accounts[self.dyn_start + D_VAULT_A];
        let vault_b = &accounts[self.dyn_start + D_VAULT_B];
        let oracle = &accounts[self.dyn_start + D_ORACLE];
        let memo = &accounts[3];
        let tick_array_0 = &accounts[self.dyn_start + D_TICK_ARRAY_0];
        let tick_array_1 = &accounts[self.dyn_start + D_TICK_ARRAY_1];
        let tick_array_2 = &accounts[self.dyn_start + D_TICK_ARRAY_2];

        let a_to_b = input_mint == self.base_token_pk;

        // Map mint_1/mint_2 to A/B order using stored mint pubkeys (mints from header)
        let (user_token_account_a, user_token_account_b, token_a, token_b, token_program_a, token_program_b) =
            if *mint_1_account.key == self.base_token_pk {
                (
                    user_mint_1_token_account,
                    user_mint_2_token_account,
                    mint_1_account,
                    mint_2_account,
                    mint_1_token_program,
                    mint_2_token_program,
                )
            } else {
                (
                    user_mint_2_token_account,
                    user_mint_1_token_account,
                    mint_2_account,
                    mint_1_account,
                    mint_2_token_program,
                    mint_1_token_program,
                )
            };

        // Build swap instruction -- accounts always in A/B order
        let metas = [
            AccountMeta::new_readonly(*token_program_a.key, false),
            AccountMeta::new_readonly(*token_program_b.key, false),
            AccountMeta::new_readonly(*memo.key, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new_readonly(*token_a.key, false),
            AccountMeta::new_readonly(*token_b.key, false),
            AccountMeta::new(*user_token_account_a.key, false),
            AccountMeta::new(*vault_a.key, false),
            AccountMeta::new(*user_token_account_b.key, false),
            AccountMeta::new(*vault_b.key, false),
            AccountMeta::new(*tick_array_0.key, false),
            AccountMeta::new(*tick_array_1.key, false),
            AccountMeta::new(*tick_array_2.key, false),
            AccountMeta::new(*oracle.key, false),
        ];

        // Swap discriminator: swap_v2 (SHA256("global:swap_v2")[..8])
        let mut data = [0u8; 43];
        data[..8].copy_from_slice(&SWAP_V2_DISC);
        data[8..16].copy_from_slice(&amount_out.unwrap_or(0).to_le_bytes());
        data[16..24].copy_from_slice(&max_amount_in.to_le_bytes());
        // data[24..40] already zeroed: sqrt_price_limit u128 = 0
        // data[40] = 0: amount_specified_is_input = false
        data[41] = if a_to_b { 1 } else { 0 }; // a_to_b
        // data[42] = 0: remaining_accounts_info = None

        let accounts_arr = [
            token_program_a.clone(),
            token_program_b.clone(),
            memo.clone(),
            payer.clone(),
            pool_id.clone(),
            token_a.clone(),
            token_b.clone(),
            user_token_account_a.clone(),
            vault_a.clone(),
            user_token_account_b.clone(),
            vault_b.clone(),
            tick_array_0.clone(),
            tick_array_1.clone(),
            tick_array_2.clone(),
            oracle.clone(),
        ];

        invoke_cpi(&PROGRAM_ID, &metas, &data, &accounts_arr)?;
        Ok(())
    }

    #[cfg(any(test, feature = "debug"))]
    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        msg!("=== Orca Whirlpool ===");
        msg!(
            "S0 program_id: {}",
            accounts[self.static_base + S_PROGRAM_ID].key
        );
        msg!(
            "D0 pool: {}",
            accounts[self.dyn_start + D_POOL].key
        );
        msg!(
            "D1 vault_a: {}",
            accounts[self.dyn_start + D_VAULT_A].key
        );
        msg!(
            "D2 vault_b: {}",
            accounts[self.dyn_start + D_VAULT_B].key
        );
        msg!(
            "D3 oracle: {}",
            accounts[self.dyn_start + D_ORACLE].key
        );
        msg!(
            "D4 tick_array_0: {}",
            accounts[self.dyn_start + D_TICK_ARRAY_0].key
        );
        msg!(
            "D5 tick_array_1: {}",
            accounts[self.dyn_start + D_TICK_ARRAY_1].key
        );
        msg!(
            "D6 tick_array_2: {}",
            accounts[self.dyn_start + D_TICK_ARRAY_2].key
        );
        msg!("   token_mint_a: {}", self.base_token_pk);
        msg!("   token_mint_b: {}", self.quote_token_pk);
        Ok(())
    }

    fn get_max_amount_in<'a>(&self, _accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_in) } else { Ok(self.sell_max_in) }
    }

    fn get_max_amount_out<'a>(&self, _accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_out) } else { Ok(self.sell_max_out) }
    }

    fn get_cached_max_amounts(&self, input_mint: Pubkey) -> (u64, u64) {
        if input_mint == self.base_token_pk { (self.buy_max_in, self.buy_max_out) } else { (self.sell_max_in, self.sell_max_out) }
    }



    /// Virtual reserves from concentrated liquidity within the active tick range.
    /// Within a single tick range (constant L), a Whirlpool swap is mathematically
    /// equivalent to a constant-product AMM:
    ///   v_a = L / √P,  v_b = L × √P
    /// These are returned as (base_reserve, quote_reserve).
    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        if self.liquidity == 0 || self.sqrt_price == 0 {
            return Err(error!(crate::programs::SolarBError::InsufficientFunds));
        }
        let sqrt_p = self.sqrt_price as f64 / (1u128 << 64) as f64;
        let l = self.liquidity as f64;
        let v_a = (l / sqrt_p).min(u64::MAX as f64) as u64;
        let v_b = (l * sqrt_p).min(u64::MAX as f64) as u64;
        Ok((v_a, v_b))
    }

    /// Swap estimate using cached state with CP math and tick-crossing support.
    /// Within the active tick range, uses the constant-product formula with virtual
    /// reserves (exact for concentrated liquidity). If the amount exceeds the active
    /// range and profit justifies crossing, estimates the next tick linearly.
    fn fast_quote<'a>(&mut self, _accounts: &[AccountInfo<'a>], input_mint: Pubkey, amount_in: u64, profit_pct: f64) -> Result<(u64, u64)> {
        let (max_in, max_out) = self.get_cached_max_amounts(input_mint);
        if max_in == 0 || max_out == 0 {
            return Ok((0, 0));
        }
        let max_in_active = self.get_active_bin_max_in(input_mint).unwrap_or(u64::MAX);
        let a_to_b = input_mint == self.base_token_pk;
        let fee_factor = self.fee_factor.0;

        // Virtual reserves for CP formula within active tick range
        let (v_a, v_b) = self.get_vault_amounts().unwrap_or((0, 0));
        let (res_in, res_out) = if a_to_b { (v_a as u128, v_b as u128) } else { (v_b as u128, v_a as u128) };
        if res_in == 0 || res_out == 0 {
            return Ok((0, 0));
        }

        // CP quote: out = res_out * in_after_fee / (res_in + in_after_fee)
        let cp_quote = |amt: u64| -> u64 {
            let in_after_fee = (amt as f64 * fee_factor) as u128;
            let denom = res_in.saturating_add(in_after_fee);
            if denom == 0 { return 0; }
            (res_out.saturating_mul(in_after_fee) / denom).min(u64::MAX as u128) as u64
        };

        // Cross into next tick range if amount exceeds active range and profit justifies it
        if max_in_active < amount_in && max_in > max_in_active {
            let tick_step_bps = self.tick_spacing as u64;
            let profit_bps = (profit_pct * 10000.0) as u64;

            if profit_bps > tick_step_bps {
                debug_eprintln!(
                    "[Orca] Crossing ticks: profit {:.2}% > tick step {:.2}%",
                    profit_pct * 100.0, tick_step_bps as f64 / 100.0
                );
                let out_active = cp_quote(max_in_active);

                let remaining = amount_in.min(max_in) - max_in_active;
                // Linear estimate at next tick's marginal price
                let (price, inverse_price) = self.get_prices()?;
                let tick_step_frac = self.tick_spacing as f64 * 0.0001;
                let next_price = if a_to_b {
                    price / (1.0 + tick_step_frac)
                } else {
                    inverse_price / (1.0 + tick_step_frac)
                };
                let out_next = (remaining as f64 * next_price * fee_factor) as u64;

                let total_in = max_in_active + remaining;
                let total_out = (out_active + out_next).min(max_out);
                return Ok((total_in, total_out));
            }
        }

        let clamped_in = amount_in.min(max_in).min(max_in_active);
        let out = cp_quote(clamped_in);
        Ok((clamped_in, out.min(max_out)))
    }

    /// Tick step fraction: each Orca tick = 0.01% price change (1.0001),
    /// so tick_spacing ticks ≈ tick_spacing × 0.01%.
    fn get_bin_step_frac(&self) -> f64 {
        self.tick_spacing as f64 * 0.0001
    }

    /// Gross input capacity of the active tick range (before fee deduction).
    /// Computed analytically from sqrt_price + liquidity — no account reads.
    fn get_active_bin_max_in(&self, input_mint: Pubkey) -> Result<u64> {
        if self.liquidity == 0 {
            return Ok(0);
        }
        let a_to_b = input_mint == self.base_token_pk;
        let tick_spacing = self.tick_spacing as i32;
        let tick_lower = floor_to_tick_spacing(self.tick_current_index, tick_spacing);
        let tick_boundary = if a_to_b { tick_lower } else { tick_lower + tick_spacing };
        let sqrt_price_boundary = tick_math::get_sqrt_price_at_tick(tick_boundary)
            .map_err(|_| error!(crate::programs::SolarBError::InsufficientAccounts))?;
        let net_cap = libraries::liquidity_math::get_amount_in_for_liquidity(
            self.sqrt_price, sqrt_price_boundary, self.liquidity, a_to_b,
        )
        .unwrap_or(0);
        // Convert net → gross (what the user actually sends, including the fee portion)
        let fee_factor = self.fee_factor.0;
        let gross_cap = if fee_factor > 0.0 {
            (net_cap as f64 / fee_factor) as u64
        } else {
            0
        };
        Ok(gross_cap)
    }

    /// Virtual reserves for the active tick range, computed from sqrt_price × liquidity.
    fn get_clmm_virtual_reserves(&self, input_mint: Pubkey) -> Option<(f64, f64)> {
        if self.liquidity == 0 || self.sqrt_price == 0 {
            return None;
        }
        let a_to_b = input_mint == self.base_token_pk;
        let q64 = (1u128 << 64) as f64;
        let sqrt_p = self.sqrt_price as f64 / q64;
        let l = self.liquidity as f64;
        if a_to_b {
            Some((l / sqrt_p, l * sqrt_p))
        } else {
            Some((l * sqrt_p, l / sqrt_p))
        }
    }

    /// Per-tick-range segment data for the CLMM multi-tick walker.
    fn get_clmm_segment<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        bin_offset: i32,
    ) -> Result<Option<(f64, f64, u64, f64)>> {
        self.get_clmm_tick_segment_impl(accounts, input_mint, bin_offset)
    }

    /// Per-tick-range segment data for the analytical multi-bin walker.
    /// Mirrors DLMM `get_bin_segment`, treating each tick range as a linear segment.
    fn get_bin_segment<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        bin_offset: i32,
        _prev_price_q64: Option<u128>,
    ) -> Result<Option<(f64, u64, u64, f64, u128)>> {
        self.get_tick_segment_impl(accounts, input_mint, bin_offset)
            .map(|opt| opt.map(|(s, c, f)| (s, c, 0u64, f, 0u128)))
    }
}

/// Floor `tick_index` to the nearest multiple of `tick_spacing` towards −∞.
fn floor_to_tick_spacing(tick_index: i32, tick_spacing: i32) -> i32 {
    if tick_spacing <= 0 {
        return tick_index;
    }
    let mut lower = (tick_index / tick_spacing) * tick_spacing;
    if tick_index < 0 && tick_index % tick_spacing != 0 {
        lower -= tick_spacing;
    }
    lower
}

impl OrcaWhirlpool {
    
    pub fn new<'a>(
        accounts: &[AccountInfo<'a>],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
    ) -> Result<Self> {
        let pool_account = &accounts[dyn_start + D_POOL];

        // Parse pool state
        let pool_data = pool_account.try_borrow_data()?;
        let pool = WhirlpoolSimple::try_from_bytes(&pool_data)?;

        let price = sqrt_price_to_f64(pool.sqrt_price);

        // Compute total fee rate (static + adaptive from oracle)
        let oracle_data = accounts.get(dyn_start + D_ORACLE)
            .and_then(|a| a.try_borrow_data().ok());
        let total_fee_rate = compute_total_fee_rate(
            pool.fee_rate,
            oracle_data.as_deref().map(|d| &**d),
        );

        debug_eprintln!("OrcaWhirlpool: pool_id {} , price {}, inverse_price {}, fee_rate {}%", *pool_account.key, price, 1.0 / price, total_fee_rate as f64 / 1_000_000.0 * 100.0);

        // Defer max amounts and transfer fees to prepare_for_execution()
        let instance = OrcaWhirlpool {
            pool_id: *pool_account.key,
            whirlpools_config: pool.whirlpools_config,
            base_token_pk: pool.token_mint_a,
            quote_token_pk: pool.token_mint_b,
            token_vault_a: pool.token_vault_a,
            token_vault_b: pool.token_vault_b,
            sqrt_price: pool.sqrt_price,
            tick_current_index: pool.tick_current_index,
            liquidity: pool.liquidity,
            tick_spacing: pool.tick_spacing,
            fee_rate: total_fee_rate,
            protocol_fee_rate: pool.protocol_fee_rate,
            fee_factor: { let f = 1.0 - (total_fee_rate as f64 / 1_000_000.0); (f, f) },
            price,
            static_base,
            dyn_start,
            buy_max_in: 0,
            buy_max_out: 0,
            sell_max_in: 0,
            sell_max_out: 0,
            prepared: false,
        };
        // instance.log_accounts(accounts)?;
        Ok(instance)
    }

    /// Calculate swap output using tick array traversal
    /// This provides accurate pricing by considering liquidity distribution
    /// Takes pre-borrowed tick array data to keep borrows in the caller's stack frame
    fn calculate_swap_base_in<'a>(
        &self,
        amount_in: u64,
        a_to_b: bool,
        data_0: &Option<std::cell::Ref<'a, &mut [u8]>>,
        data_1: &Option<std::cell::Ref<'a, &mut [u8]>>,
        data_2: &Option<std::cell::Ref<'a, &mut [u8]>>,
    ) -> Result<u64> {
        if self.liquidity == 0 {
            return Err(error!(crate::programs::SolarBError::InsufficientFunds));
        }

        let mut amount_remaining = amount_in;
        let mut amount_out_total = 0u64;
        let mut current_sqrt_price = self.sqrt_price;
        let mut current_liquidity = self.liquidity;
        let mut current_tick = self.tick_current_index;

        // Maximum iterations to prevent infinite loops
        const MAX_ITERATIONS: usize = 20;
        let mut iterations = 0;

        while amount_remaining > 0 && iterations < MAX_ITERATIONS {
            iterations += 1;

            // Find next initialized tick - returns None when tick arrays are exhausted
            let (next_tick_index, next_tick) = match self.find_next_initialized_tick_lazy(
                current_tick,
                a_to_b,
                data_0,
                data_1,
                data_2,
            ) {
                Some(result) => result,
                None => break, // Tick arrays exhausted — stop and return accumulated output
            };

            // Get sqrt price at next tick
            let sqrt_price_next_tick = tick_math::get_sqrt_price_at_tick(next_tick_index)
                .unwrap_or(if a_to_b {
                    tick_math::MIN_SQRT_PRICE_X64
                } else {
                    tick_math::MAX_SQRT_PRICE_X64
                });

            // Determine target price (tick boundary or price limit)
            let sqrt_price_target = if a_to_b {
                sqrt_price_next_tick.max(tick_math::MIN_SQRT_PRICE_X64 + 1)
            } else {
                sqrt_price_next_tick.min(tick_math::MAX_SQRT_PRICE_X64 - 1)
            };

            // Compute swap step
            let step = swap_math::compute_swap_step(
                current_sqrt_price,
                sqrt_price_target,
                current_liquidity,
                amount_remaining,
                self.fee_rate,
                true, // is_base_input
                a_to_b,
            );

            // Update amounts
            amount_remaining = amount_remaining
                .saturating_sub(step.amount_in)
                .saturating_sub(step.fee_amount);
            amount_out_total = amount_out_total.saturating_add(step.amount_out);

            // Update price
            current_sqrt_price = step.sqrt_price_next_x64;

            // Check if we crossed a tick
            if step.sqrt_price_next_x64 == sqrt_price_target {
                // Crossed the tick - update liquidity
                if let Some(tick) = next_tick {
                    if tick.initialized {
                        current_liquidity = if a_to_b {
                            libraries::liquidity_math::add_delta(
                                current_liquidity,
                                -tick.liquidity_net,
                            )
                            .unwrap_or(0)
                        } else {
                            libraries::liquidity_math::add_delta(
                                current_liquidity,
                                tick.liquidity_net,
                            )
                            .unwrap_or(current_liquidity)
                        };
                    }
                }

                // Update current tick
                current_tick = if a_to_b {
                    next_tick_index - 1
                } else {
                    next_tick_index
                };
            } else {
                // Didn't cross tick, calculate new tick from price
                current_tick =
                    tick_math::get_tick_at_sqrt_price(current_sqrt_price).unwrap_or(current_tick);
                break; // No more swapping needed
            }
        }

        Ok(amount_out_total)
    }

    /// Calculate required input for exact output swap
    /// Takes pre-borrowed tick array data to keep borrows in the caller's stack frame
    fn calculate_swap_base_out<'a>(
        &self,
        amount_out: u64,
        a_to_b: bool,
        data_0: &Option<std::cell::Ref<'a, &mut [u8]>>,
        data_1: &Option<std::cell::Ref<'a, &mut [u8]>>,
        data_2: &Option<std::cell::Ref<'a, &mut [u8]>>,
    ) -> Result<u64> {
        if self.liquidity == 0 {
            return Err(error!(crate::programs::SolarBError::InsufficientFunds));
        }

        let mut amount_remaining = amount_out;
        let mut amount_in_total = 0u64;
        let mut current_sqrt_price = self.sqrt_price;
        let mut current_liquidity = self.liquidity;
        let mut current_tick = self.tick_current_index;

        const MAX_ITERATIONS: usize = 20;
        let mut iterations = 0;

        while amount_remaining > 0 && iterations < MAX_ITERATIONS {
            iterations += 1;

            // Find next initialized tick - returns None when tick arrays are exhausted
            let (next_tick_index, next_tick) = match self.find_next_initialized_tick_lazy(
                current_tick,
                a_to_b,
                data_0,
                data_1,
                data_2,
            ) {
                Some(result) => result,
                None => break, // Tick arrays exhausted — stop and return accumulated output
            };

            let sqrt_price_next_tick = tick_math::get_sqrt_price_at_tick(next_tick_index)
                .unwrap_or(if a_to_b {
                    tick_math::MIN_SQRT_PRICE_X64
                } else {
                    tick_math::MAX_SQRT_PRICE_X64
                });

            let sqrt_price_target = if a_to_b {
                sqrt_price_next_tick.max(tick_math::MIN_SQRT_PRICE_X64 + 1)
            } else {
                sqrt_price_next_tick.min(tick_math::MAX_SQRT_PRICE_X64 - 1)
            };

            // Compute swap step for exact output
            let step = swap_math::compute_swap_step(
                current_sqrt_price,
                sqrt_price_target,
                current_liquidity,
                amount_remaining,
                self.fee_rate,
                false, // is_base_input = false for exact output
                a_to_b,
            );

            amount_remaining = amount_remaining.saturating_sub(step.amount_out);
            amount_in_total = amount_in_total
                .saturating_add(step.amount_in)
                .saturating_add(step.fee_amount);

            current_sqrt_price = step.sqrt_price_next_x64;

            if step.sqrt_price_next_x64 == sqrt_price_target {
                if let Some(tick) = next_tick {
                    if tick.initialized {
                        current_liquidity = if a_to_b {
                            libraries::liquidity_math::add_delta(
                                current_liquidity,
                                -tick.liquidity_net,
                            )
                            .unwrap_or(0)
                        } else {
                            libraries::liquidity_math::add_delta(
                                current_liquidity,
                                tick.liquidity_net,
                            )
                            .unwrap_or(current_liquidity)
                        };
                    }
                }

                current_tick = if a_to_b {
                    next_tick_index - 1
                } else {
                    next_tick_index
                };
            } else {
                current_tick =
                    tick_math::get_tick_at_sqrt_price(current_sqrt_price).unwrap_or(current_tick);
                break;
            }
        }

        Ok(amount_in_total)
    }

    /// Find the next initialized tick in the swap direction (fully lazy - parses only when needed)
    /// Takes raw data references and only parses tick arrays on-demand to minimize heap usage
    /// Returns None when no tick array covers the current tick position (tick arrays exhausted)
    fn find_next_initialized_tick_lazy<'a>(
        &self,
        current_tick: i32,
        a_to_b: bool,
        data_0: &Option<std::cell::Ref<'a, &mut [u8]>>,
        data_1: &Option<std::cell::Ref<'a, &mut [u8]>>,
        data_2: &Option<std::cell::Ref<'a, &mut [u8]>>,
    ) -> Option<(i32, Option<states::Tick>)> {
        let tick_spacing = self.tick_spacing;

        // Search through tick arrays - only parse when we actually check each one
        for maybe_data in [data_0, data_1, data_2].iter() {
            if let Some(data) = maybe_data {
                // Try fixed TickArray first
                if let Some(array) = TickArraySimple::try_from_bytes(&**data) {
                    if array.contains_tick(current_tick, tick_spacing) {
                        if let Some((found_tick_index, tick)) =
                            array.get_next_initialized_tick(current_tick, tick_spacing, a_to_b)
                        {
                            return Some((found_tick_index, Some(tick)));
                        }
                        let boundary = array.get_boundary_tick(a_to_b, tick_spacing);
                        return Some((boundary, None));
                    }
                }
                // Try DynamicTickArray
                else if let Some(result) = states::tick::dynamic_find_next_initialized_tick(
                    &**data, current_tick, tick_spacing, a_to_b,
                ) {
                    return Some(result);
                }
            }
        }

        // No tick array covers the current tick — tick arrays exhausted
        None
    }

    /// Per-tick-range segment for the analytical multi-bin walker.
    ///
    /// Walks tick arrays from the current position, advancing `bin_offset` tick ranges.
    /// Each Orca tick range is approximated as linear using the geometric mean of the
    /// two sqrt-price bounds as the effective exchange rate.
    ///
    /// Returns `(slope, net_capacity, fee_factor)` where:
    ///   slope        = geometric_mean_price × fee_factor
    ///   net_capacity = net input tokens (after fee) to push price to the tick boundary
    ///   fee_factor   = 1 − fee_rate
    ///
    /// The multi-bin walker converts net_capacity → gross via `c / fee_factor`.
    fn get_tick_segment_impl<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        bin_offset: i32,
    ) -> Result<Option<(f64, u64, f64)>> {
        if self.liquidity == 0 {
            return Ok(None);
        }
        let a_to_b = input_mint == self.base_token_pk;
        let fee_factor = self.fee_factor.0;

        let data_0 = accounts[self.dyn_start + D_TICK_ARRAY_0].try_borrow_data().ok();
        let data_1 = accounts[self.dyn_start + D_TICK_ARRAY_1].try_borrow_data().ok();
        let data_2 = accounts[self.dyn_start + D_TICK_ARRAY_2].try_borrow_data().ok();

        let mut current_sqrt_price = self.sqrt_price;
        let mut current_liquidity = self.liquidity;
        let mut current_tick = self.tick_current_index;

        const Q64: f64 = (1u128 << 64) as f64;

        for i in 0..=bin_offset {
            let (next_tick_index, next_tick_opt) = match self.find_next_initialized_tick_lazy(
                current_tick, a_to_b, &data_0, &data_1, &data_2,
            ) {
                Some(r) => r,
                None => return Ok(None),
            };

            let sqrt_price_target_raw = match tick_math::get_sqrt_price_at_tick(next_tick_index) {
                Ok(p) => p,
                Err(_) => return Ok(None),
            };
            let sqrt_price_target = if a_to_b {
                sqrt_price_target_raw.max(tick_math::MIN_SQRT_PRICE_X64 + 1)
            } else {
                sqrt_price_target_raw.min(tick_math::MAX_SQRT_PRICE_X64 - 1)
            };

            if i == bin_offset {
                // Net capacity: tokens that push price exactly to this tick boundary
                let capacity = libraries::liquidity_math::get_amount_in_for_liquidity(
                    current_sqrt_price, sqrt_price_target, current_liquidity, a_to_b,
                )
                .unwrap_or(0);
                if capacity == 0 {
                    return Ok(None);
                }
                // Geometric mean price = sqrt_P_curr × sqrt_P_target (as Q64 fractions).
                // For a_to_b:  delta_B / delta_A = sqrt_P_curr × sqrt_P_target
                // For b_to_a:  delta_A / delta_B = 1 / (sqrt_P_curr × sqrt_P_target)
                let sqrt_p_curr_f = current_sqrt_price as f64 / Q64;
                let sqrt_p_target_f = sqrt_price_target as f64 / Q64;
                let geo_mean = sqrt_p_curr_f * sqrt_p_target_f;
                if geo_mean <= 0.0 {
                    return Ok(None);
                }
                let price_mid = if a_to_b { geo_mean } else { 1.0 / geo_mean };
                return Ok(Some((price_mid * fee_factor, capacity, fee_factor)));
            }

            // Advance state: cross this tick boundary and update active liquidity
            if let Some(tick) = next_tick_opt {
                if tick.initialized {
                    current_liquidity = if a_to_b {
                        libraries::liquidity_math::add_delta(current_liquidity, -tick.liquidity_net)
                            .unwrap_or(0)
                    } else {
                        libraries::liquidity_math::add_delta(current_liquidity, tick.liquidity_net)
                            .unwrap_or(current_liquidity)
                    };
                }
            }
            current_sqrt_price = sqrt_price_target;
            current_tick = if a_to_b { next_tick_index - 1 } else { next_tick_index };

            if current_liquidity == 0 {
                return Ok(None);
            }
        }

        Ok(None)
    }

    /// Per-tick-range segment with virtual reserves for the CLMM multi-tick walker.
    /// Returns (reserve_in, reserve_out, net_capacity, fee_factor).
    fn get_clmm_tick_segment_impl<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        bin_offset: i32,
    ) -> Result<Option<(f64, f64, u64, f64)>> {
        if self.liquidity == 0 {
            return Ok(None);
        }
        let a_to_b = input_mint == self.base_token_pk;
        let fee_factor = self.fee_factor.0;

        let data_0 = accounts[self.dyn_start + D_TICK_ARRAY_0].try_borrow_data().ok();
        let data_1 = accounts[self.dyn_start + D_TICK_ARRAY_1].try_borrow_data().ok();
        let data_2 = accounts[self.dyn_start + D_TICK_ARRAY_2].try_borrow_data().ok();

        let mut current_sqrt_price = self.sqrt_price;
        let mut current_liquidity = self.liquidity;
        let mut current_tick = self.tick_current_index;

        const Q64: f64 = (1u128 << 64) as f64;

        for i in 0..=bin_offset {
            let (next_tick_index, next_tick_opt) = match self.find_next_initialized_tick_lazy(
                current_tick, a_to_b, &data_0, &data_1, &data_2,
            ) {
                Some(r) => r,
                None => return Ok(None),
            };

            let sqrt_price_target_raw = match tick_math::get_sqrt_price_at_tick(next_tick_index) {
                Ok(p) => p,
                Err(_) => return Ok(None),
            };
            let sqrt_price_target = if a_to_b {
                sqrt_price_target_raw.max(tick_math::MIN_SQRT_PRICE_X64 + 1)
            } else {
                sqrt_price_target_raw.min(tick_math::MAX_SQRT_PRICE_X64 - 1)
            };

            if i == bin_offset {
                let capacity = libraries::liquidity_math::get_amount_in_for_liquidity(
                    current_sqrt_price, sqrt_price_target, current_liquidity, a_to_b,
                )
                .unwrap_or(0);
                if capacity == 0 {
                    return Ok(Some((0.0, 0.0, 0, fee_factor)));
                }
                // Virtual reserves from sqrt_price and liquidity
                let sqrt_p = current_sqrt_price as f64 / Q64;
                let l = current_liquidity as f64;
                let (vr_in, vr_out) = if a_to_b {
                    (l / sqrt_p, l * sqrt_p)
                } else {
                    (l * sqrt_p, l / sqrt_p)
                };
                return Ok(Some((vr_in, vr_out, capacity, fee_factor)));
            }

            // Advance state: cross tick boundary
            if let Some(tick) = next_tick_opt {
                if tick.initialized {
                    current_liquidity = if a_to_b {
                        libraries::liquidity_math::add_delta(current_liquidity, -tick.liquidity_net)
                            .unwrap_or(0)
                    } else {
                        libraries::liquidity_math::add_delta(current_liquidity, tick.liquidity_net)
                            .unwrap_or(current_liquidity)
                    };
                }
            }
            current_sqrt_price = sqrt_price_target;
            current_tick = if a_to_b { next_tick_index - 1 } else { next_tick_index };

            if current_liquidity == 0 {
                return Ok(None);
            }
        }

        Ok(None)
    }

    /// Compute deferred fields: max amounts, transfer fee rates.
    /// Called only for instances that participate in a profitable arb path.
    pub fn prepare_for_execution<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
    ) {
        if self.prepared {
            return;
        }
        self.prepared = true;

        let fee_factor = self.fee_factor.0;
        let vault_b_amount = read_token_amount(&accounts[self.dyn_start + D_VAULT_B])
            .unwrap_or(0);
        let vault_a_amount = read_token_amount(&accounts[self.dyn_start + D_VAULT_A])
            .unwrap_or(0);
        // a_to_b: output = vault_b
        self.buy_max_out = vault_b_amount;
        let price = self.price;
        let inverse_price = 1.0 / self.price;
        self.buy_max_in = if price > 0.0 && fee_factor > 0.0 {
            (vault_b_amount as f64 / (price * fee_factor)) as u64
        } else { 0 };
        // b_to_a: output = vault_a
        self.sell_max_out = vault_a_amount;
        self.sell_max_in = if inverse_price > 0.0 && fee_factor > 0.0 {
            (vault_a_amount as f64 / (inverse_price * fee_factor)) as u64
        } else { 0 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::programs::orca::states::{get_tick_array_start_index, TICK_ARRAY_SIZE};
    use crate::utils::token::MintFee;
    use anchor_lang::prelude::Clock;
    use anchor_lang::solana_program::pubkey::Pubkey;
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

    /// Build an OrcaWhirlpool instance from a pool_id by fetching all needed accounts from RPC.
    /// Returns (instance, accounts_vec, clock) ready for testing.
    async fn build_from_pool_id(
        pool_id: Pubkey,
    ) -> (OrcaWhirlpool, Vec<AccountInfo<'static>>, Clock) {
        let rpc_client = get_rpc_client();

        // Fetch pool account and parse WhirlpoolSimple
        let pool_account = fetch_account_info_from_rpc(&rpc_client, pool_id).await;
        let pool = {
            let data = pool_account.try_borrow_data().unwrap();
            WhirlpoolSimple::try_from_bytes(&data)
                .expect("Failed to parse WhirlpoolSimple")
        };

        eprintln!("Pool: {}", pool_id);
        eprintln!("  token_a (base): {}", pool.token_mint_a);
        eprintln!("  token_b (quote): {}", pool.token_mint_b);
        let tick_current = pool.tick_current_index;
        let tick_spacing = pool.tick_spacing;
        eprintln!("  tick_current: {}, tick_spacing: {}", tick_current, tick_spacing);

        // Fetch vault accounts
        let vault_a_info = fetch_account_info_from_rpc(&rpc_client, pool.token_vault_a).await;
        let vault_b_info = fetch_account_info_from_rpc(&rpc_client, pool.token_vault_b).await;

        // Derive oracle PDA
        let (oracle_key, _) = Pubkey::find_program_address(
            &[b"oracle", pool_id.as_ref()],
            &PROGRAM_ID,
        );
        let oracle_info = try_fetch_account_info_from_rpc(&rpc_client, oracle_key)
            .await
            .unwrap_or_else(|| create_mock_account_info_with_data(
                oracle_key, anchor_lang::solana_program::system_program::id(), None,
            ));

        // Derive tick array PDAs based on current tick and tick spacing
        let ticks_in_array = TICK_ARRAY_SIZE * pool.tick_spacing as i32;
        let start_tick_index_0 = get_tick_array_start_index(pool.tick_current_index, pool.tick_spacing);
        let start_tick_index_1 = start_tick_index_0 - ticks_in_array;
        let start_tick_index_2 = start_tick_index_0 + ticks_in_array;

        let (tick_array_0_key, _) = Pubkey::find_program_address(
            &[b"tick_array", pool_id.as_ref(), start_tick_index_0.to_string().as_bytes()],
            &PROGRAM_ID,
        );
        let (tick_array_1_key, _) = Pubkey::find_program_address(
            &[b"tick_array", pool_id.as_ref(), start_tick_index_1.to_string().as_bytes()],
            &PROGRAM_ID,
        );
        let (tick_array_2_key, _) = Pubkey::find_program_address(
            &[b"tick_array", pool_id.as_ref(), start_tick_index_2.to_string().as_bytes()],
            &PROGRAM_ID,
        );

        eprintln!("  tick_array_0 (start={}): {}", start_tick_index_0, tick_array_0_key);
        eprintln!("  tick_array_1 (start={}): {}", start_tick_index_1, tick_array_1_key);
        eprintln!("  tick_array_2 (start={}): {}", start_tick_index_2, tick_array_2_key);
        eprintln!("  oracle: {}", oracle_key);

        // Fetch tick array accounts (may not exist on-chain)
        let tick_array_0_info = try_fetch_account_info_from_rpc(&rpc_client, tick_array_0_key)
            .await
            .unwrap_or_else(|| create_mock_account_info_with_data(
                tick_array_0_key, PROGRAM_ID, Some(vec![0u8; 1000]),
            ));
        let tick_array_1_info = try_fetch_account_info_from_rpc(&rpc_client, tick_array_1_key)
            .await
            .unwrap_or_else(|| create_mock_account_info_with_data(
                tick_array_1_key, PROGRAM_ID, Some(vec![0u8; 1000]),
            ));
        let tick_array_2_info = try_fetch_account_info_from_rpc(&rpc_client, tick_array_2_key)
            .await
            .unwrap_or_else(|| create_mock_account_info_with_data(
                tick_array_2_key, PROGRAM_ID, Some(vec![0u8; 1000]),
            ));

        // Compute total fee rate (static + adaptive from oracle)
        let total_fee_rate = {
            let oracle_data = oracle_info.try_borrow_data().ok();
            compute_total_fee_rate(
                pool.fee_rate,
                oracle_data.as_ref().map(|d| d.as_ref()),
            )
        };
        let static_fee_rate = pool.fee_rate;
        eprintln!("  fee_rate (static): {}, total_fee_rate: {}", static_fee_rate, total_fee_rate);

        let program_id_info = create_mock_account_info_with_data(
            PROGRAM_ID, anchor_lang::solana_program::system_program::id(), None,
        );

        // Layout:
        // Static (static_base=0): [program_id]
        // Dynamic (dyn_start=1): [pool, vault_a, vault_b, oracle, tick0, tick1, tick2]
        let accounts = vec![
            program_id_info,        // S0
            pool_account,           // D0
            vault_a_info,           // D1
            vault_b_info,           // D2
            oracle_info,            // D3
            tick_array_0_info,      // D4
            tick_array_1_info,      // D5
            tick_array_2_info,      // D6
        ];

        let static_base: usize = 0;
        let dyn_start: usize = 1;
        let dyn_end: usize = accounts.len();

        let mut orca = OrcaWhirlpool::new(&accounts, static_base, dyn_start, dyn_end)
            .expect("OrcaWhirlpool::new failed");

        orca.prepare_for_execution(&accounts);

        let clock = get_clock_from_rpc(&rpc_client).await;

        eprintln!("  price: {}", orca.price);
        eprintln!("  fee_rate: {}", orca.fee_rate);

        (orca, accounts, clock)
    }

    // ---- Tests ----

    #[tokio::test]
    async fn test_orca_whirlpool_round_trip() {
        let pool_id = Pubkey::from_str_const("HktfL7iwGKT5QHjywQkcDnZXScoh811k7akrMZJkCcEF");
        let (mut orca, accounts, clock) = build_from_pool_id(pool_id).await;

        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let no_fee = MintFee::ZERO;

        // 1. Print all account pubkeys
        eprintln!("\n=== Account Pubkeys ===");
        eprintln!("base_mint        : {}", orca.base_token_pk);
        eprintln!("quote_mint       : {}", orca.quote_token_pk);
        eprintln!("pool_id          : {}", accounts[1 + D_POOL].key);
        eprintln!("vault_a          : {}", accounts[1 + D_VAULT_A].key);
        eprintln!("vault_b          : {}", accounts[1 + D_VAULT_B].key);
        eprintln!("oracle           : {}", accounts[1 + D_ORACLE].key);
        eprintln!("tick_array_0     : {}", accounts[1 + D_TICK_ARRAY_0].key);
        eprintln!("tick_array_1     : {}", accounts[1 + D_TICK_ARRAY_1].key);
        eprintln!("tick_array_2     : {}", accounts[1 + D_TICK_ARRAY_2].key);
        eprintln!("program_id       : {}", accounts[S_PROGRAM_ID].key);

        // 2. Prices
        let (price, inverse_price) = orca.get_prices().unwrap();
        eprintln!("\n=== Prices ===");
        eprintln!("price            : {}", price);
        eprintln!("inverse_price    : {}", inverse_price);

        // 3. Fees
        let (fee_factor, fee_factor_2) = orca.get_fee_factor().unwrap();
        eprintln!("\n=== Fees ===");
        eprintln!("fee_rate         : {}", orca.fee_rate);
        eprintln!("fee_factor       : {}", fee_factor);
        eprintln!("fee_factor_2     : {}", fee_factor_2);

        // 4. Max amounts
        eprintln!("\n=== After prepare_for_execution ===");
        eprintln!("buy_max_in       : {}", orca.buy_max_in);
        eprintln!("buy_max_out      : {}", orca.buy_max_out);
        eprintln!("sell_max_in      : {}", orca.sell_max_in);
        eprintln!("sell_max_out     : {}", orca.sell_max_out);

        // 5. Round-trip with start_amount = 1 WSOL
        let start_amount: u64 = 1_000_000_000;

        let other_mint = if orca.base_token_pk == sol_mint {
            orca.quote_token_pk
        } else {
            orca.base_token_pk
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
        let token_out = orca.swap_base_in(
            &accounts, sol_mint, start_amount, no_fee, no_fee, &clock,
        ).unwrap();
        let max_sol_in = orca.swap_base_out(
            &accounts, other_mint, token_out, no_fee, no_fee, &clock,
        ).unwrap();
        eprintln!("AMOUNT_IN {} -> AMOUNT_OUT {} -> MAX_AMOUNT_IN {}", start_amount as f64 / sol_div, token_out as f64 / tok_div, max_sol_in as f64 / sol_div);

        // Direction 2: TOKEN -> SOL -> TOKEN
        eprintln!("\n=== Direction 2: TOKEN -> SOL -> TOKEN ===");
        let sol_out = orca.swap_base_in(
            &accounts, other_mint, token_out, no_fee, no_fee, &clock,
        ).unwrap();
        let max_token_in = orca.swap_base_out(
            &accounts, sol_mint, sol_out, no_fee, no_fee, &clock,
        ).unwrap();
        eprintln!("AMOUNT_IN {} -> AMOUNT_OUT {} -> MAX_AMOUNT_IN {}", token_out as f64 / tok_div, sol_out as f64 / sol_div, max_token_in as f64 / tok_div);
    }
}
