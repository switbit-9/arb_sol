pub mod libraries;
pub mod states;

use self::libraries::{swap_math, tick_math};
use self::states::{OracleSimple, TickArraySimple, WhirlpoolSimple, FEE_RATE_HARD_LIMIT};
use crate::programs::{PoolKind, ProgramMeta};
use crate::programs::programs::Result;
use crate::programs::SolarBError;
use crate::utils::token::{apply_transfer_fee, apply_transfer_inverse_fee, MintFee};
use crate::utils::utils::read_token_amount;
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};
use pinocchio::instruction::AccountMeta;
use pinocchio::sysvars::clock::Clock;
use crate::utils::cpi::invoke_cpi;

/// Orca Whirlpool Program ID
pub const PROGRAM_ID: Pubkey =
    five8_const::decode_32_const("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc");
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

    fn swap_base_in(
        &mut self,
        accounts: &[AccountInfo],
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
        let data_0 = Some(unsafe { accounts[self.dyn_start + D_TICK_ARRAY_0].borrow_data_unchecked() });
        let data_1 = Some(unsafe { accounts[self.dyn_start + D_TICK_ARRAY_1].borrow_data_unchecked() });
        let data_2 = Some(unsafe { accounts[self.dyn_start + D_TICK_ARRAY_2].borrow_data_unchecked() });

        let amount_out =
            self.calculate_swap_base_in(actual_amount_in, a_to_b, &data_0, &data_1, &data_2)?;

        let out_fee = apply_transfer_fee(amount_out, output_transfer_fee);
        let final_amount_out = amount_out.saturating_sub(out_fee);

        Ok(final_amount_out)
    }

    fn swap_base_out(
        &mut self,
        accounts: &[AccountInfo],
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
            .ok_or(ProgramError::from(SolarBError::FeeOverflow))?;

        // Borrow tick array data at this level to avoid nested stack frames
        let data_0 = Some(unsafe { accounts[self.dyn_start + D_TICK_ARRAY_0].borrow_data_unchecked() });
        let data_1 = Some(unsafe { accounts[self.dyn_start + D_TICK_ARRAY_1].borrow_data_unchecked() });
        let data_2 = Some(unsafe { accounts[self.dyn_start + D_TICK_ARRAY_2].borrow_data_unchecked() });

        let amount_in =
            self.calculate_swap_base_out(amount_out_with_fee, a_to_b, &data_0, &data_1, &data_2)?;

        let in_fee = apply_transfer_inverse_fee(amount_in, input_transfer_fee);
        let final_amount_in = amount_in
            .checked_add(in_fee)
            .ok_or(ProgramError::from(SolarBError::FeeOverflow))?;

        Ok(final_amount_in)
    }

    fn invoke_swap_base_in(
        &mut self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        amount_in: u64,
        min_amount_out: Option<u64>,
        payer: &AccountInfo,
        user_mint_1_token_account: &AccountInfo,
        user_mint_2_token_account: &AccountInfo,
        mint_1_account: &AccountInfo,
        mint_2_account: &AccountInfo,
        mint_1_token_program: &AccountInfo,
        mint_2_token_program: &AccountInfo,
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
            if *mint_1_account.key() == self.base_token_pk {
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
            AccountMeta::new(token_program_a.key(), false, false),
            AccountMeta::new(token_program_b.key(), false, false),
            AccountMeta::new(memo.key(), false, false),
            AccountMeta::new(payer.key(), true, true),
            AccountMeta::new(pool_id.key(), true, false),
            AccountMeta::new(token_a.key(), false, false),
            AccountMeta::new(token_b.key(), false, false),
            AccountMeta::new(user_token_account_a.key(), true, false),
            AccountMeta::new(vault_a.key(), true, false),
            AccountMeta::new(user_token_account_b.key(), true, false),
            AccountMeta::new(vault_b.key(), true, false),
            AccountMeta::new(tick_array_0.key(), true, false),
            AccountMeta::new(tick_array_1.key(), true, false),
            AccountMeta::new(tick_array_2.key(), true, false),
            AccountMeta::new(oracle.key(), true, false),
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

        let accs: [&AccountInfo; 15] = [
            token_program_a, token_program_b, memo,
            payer, pool_id, token_a, token_b,
            user_token_account_a, vault_a,
            user_token_account_b, vault_b,
            tick_array_0, tick_array_1, tick_array_2,
            oracle,
        ];
        invoke_cpi(&PROGRAM_ID, &metas, &data, &accs)?;
        Ok(())
    }

    fn invoke_swap_base_out(
        &mut self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        max_amount_in: u64,
        amount_out: Option<u64>,
        payer: &AccountInfo,
        user_mint_1_token_account: &AccountInfo,
        user_mint_2_token_account: &AccountInfo,
        mint_1_account: &AccountInfo,
        mint_2_account: &AccountInfo,
        mint_1_token_program: &AccountInfo,
        mint_2_token_program: &AccountInfo,
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
            if *mint_1_account.key() == self.base_token_pk {
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
            AccountMeta::new(token_program_a.key(), false, false),
            AccountMeta::new(token_program_b.key(), false, false),
            AccountMeta::new(memo.key(), false, false),
            AccountMeta::new(payer.key(), true, true),
            AccountMeta::new(pool_id.key(), true, false),
            AccountMeta::new(token_a.key(), false, false),
            AccountMeta::new(token_b.key(), false, false),
            AccountMeta::new(user_token_account_a.key(), true, false),
            AccountMeta::new(vault_a.key(), true, false),
            AccountMeta::new(user_token_account_b.key(), true, false),
            AccountMeta::new(vault_b.key(), true, false),
            AccountMeta::new(tick_array_0.key(), true, false),
            AccountMeta::new(tick_array_1.key(), true, false),
            AccountMeta::new(tick_array_2.key(), true, false),
            AccountMeta::new(oracle.key(), true, false),
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

        let accs: [&AccountInfo; 15] = [
            token_program_a, token_program_b, memo,
            payer, pool_id, token_a, token_b,
            user_token_account_a, vault_a,
            user_token_account_b, vault_b,
            tick_array_0, tick_array_1, tick_array_2,
            oracle,
        ];
        invoke_cpi(&PROGRAM_ID, &metas, &data, &accs)?;
        Ok(())
    }

    #[cfg(any(test, feature = "debug"))]
    fn log_accounts(&self, _accounts: &[AccountInfo]) -> Result<()> {
        pinocchio::log::sol_log("=== Orca Whirlpool ===");
        Ok(())
    }

    fn get_max_amount_in(&self, _accounts: &[AccountInfo], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_in) } else { Ok(self.sell_max_in) }
    }

    fn get_max_amount_out(&self, _accounts: &[AccountInfo], mint: Pubkey) -> Result<u64> {
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
            return Err(ProgramError::from(SolarBError::InsufficientFunds));
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
    fn fast_quote(&mut self, _accounts: &[AccountInfo], input_mint: Pubkey, amount_in: u64, profit_pct: f64) -> Result<(u64, u64)> {
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
            .map_err(|_| ProgramError::from(SolarBError::InsufficientAccounts))?;
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
    fn get_clmm_segment(
        &self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        bin_offset: i32,
    ) -> Result<Option<(f64, f64, u64, f64)>> {
        self.get_clmm_tick_segment_impl(accounts, input_mint, bin_offset)
    }

    /// Per-tick-range segment data for the analytical multi-bin walker.
    /// Mirrors DLMM `get_bin_segment`, treating each tick range as a linear segment.
    fn get_bin_segment(
        &self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        bin_offset: i32,
    ) -> Result<Option<(f64, u64, f64)>> {
        self.get_tick_segment_impl(accounts, input_mint, bin_offset)
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
    
    pub fn new(
        accounts: &[AccountInfo],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
    ) -> Result<Self> {
        let pool_account = &accounts[dyn_start + D_POOL];

        // Parse pool state
        let pool_data = unsafe { pool_account.borrow_data_unchecked() };
        let pool = WhirlpoolSimple::try_from_bytes(pool_data)?;

        let price = sqrt_price_to_f64(pool.sqrt_price);

        // Compute total fee rate (static + adaptive from oracle)
        let oracle_data = accounts.get(dyn_start + D_ORACLE)
            .map(|a| unsafe { a.borrow_data_unchecked() });
        let total_fee_rate = compute_total_fee_rate(
            pool.fee_rate,
            oracle_data,
        );

        debug_eprintln!("OrcaWhirlpool: pool_id {:02x?}.. , price {}, inverse_price {}, fee_rate {}", &pool_account.key()[..4], price, 1.0 / price, total_fee_rate);

        // Defer max amounts and transfer fees to prepare_for_execution()
        let instance = OrcaWhirlpool {
            pool_id: *pool_account.key(),
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
    fn calculate_swap_base_in(
        &self,
        amount_in: u64,
        a_to_b: bool,
        data_0: &Option<&[u8]>,
        data_1: &Option<&[u8]>,
        data_2: &Option<&[u8]>,
    ) -> Result<u64> {
        if self.liquidity == 0 {
            return Err(ProgramError::from(SolarBError::InsufficientFunds));
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
    fn calculate_swap_base_out(
        &self,
        amount_out: u64,
        a_to_b: bool,
        data_0: &Option<&[u8]>,
        data_1: &Option<&[u8]>,
        data_2: &Option<&[u8]>,
    ) -> Result<u64> {
        if self.liquidity == 0 {
            return Err(ProgramError::from(SolarBError::InsufficientFunds));
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
    fn find_next_initialized_tick_lazy(
        &self,
        current_tick: i32,
        a_to_b: bool,
        data_0: &Option<&[u8]>,
        data_1: &Option<&[u8]>,
        data_2: &Option<&[u8]>,
    ) -> Option<(i32, Option<states::Tick>)> {
        let tick_spacing = self.tick_spacing;

        // Search through tick arrays - only parse when we actually check each one
        for maybe_data in [data_0, data_1, data_2].iter() {
            if let Some(data) = maybe_data {
                // Only parse this specific array if we need to check it
                // Dereference Ref<&mut [u8]> to get &[u8]
                if let Some(array) = TickArraySimple::try_from_bytes(data) {
                    if array.contains_tick(current_tick, tick_spacing) {
                        if let Some((found_tick_index, tick)) =
                            array.get_next_initialized_tick(current_tick, tick_spacing, a_to_b)
                        {
                            return Some((found_tick_index, Some(tick)));
                        }
                        // Array contains this tick but no initialized tick found in direction
                        // Return the array boundary so the loop can advance past it
                        let boundary = array.get_boundary_tick(a_to_b, tick_spacing);
                        return Some((boundary, None));
                    }
                }
                // TickArraySimple is dropped here - only one parsed at a time
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
    fn get_tick_segment_impl(
        &self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        bin_offset: i32,
    ) -> Result<Option<(f64, u64, f64)>> {
        if self.liquidity == 0 {
            return Ok(None);
        }
        let a_to_b = input_mint == self.base_token_pk;
        let fee_factor = self.fee_factor.0;

        let data_0 = Some(unsafe { accounts[self.dyn_start + D_TICK_ARRAY_0].borrow_data_unchecked() });
        let data_1 = Some(unsafe { accounts[self.dyn_start + D_TICK_ARRAY_1].borrow_data_unchecked() });
        let data_2 = Some(unsafe { accounts[self.dyn_start + D_TICK_ARRAY_2].borrow_data_unchecked() });

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
    fn get_clmm_tick_segment_impl(
        &self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        bin_offset: i32,
    ) -> Result<Option<(f64, f64, u64, f64)>> {
        if self.liquidity == 0 {
            return Ok(None);
        }
        let a_to_b = input_mint == self.base_token_pk;
        let fee_factor = self.fee_factor.0;

        let data_0 = Some(unsafe { accounts[self.dyn_start + D_TICK_ARRAY_0].borrow_data_unchecked() });
        let data_1 = Some(unsafe { accounts[self.dyn_start + D_TICK_ARRAY_1].borrow_data_unchecked() });
        let data_2 = Some(unsafe { accounts[self.dyn_start + D_TICK_ARRAY_2].borrow_data_unchecked() });

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
    pub fn prepare_for_execution(
        &mut self,
        accounts: &[AccountInfo],
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

// TODO: rewrite tests using LiteSVM/Mollusk

#[cfg(test)]
mod tests {
    // placeholder — anchor-based AccountInfo<'static> helpers are incompatible with pinocchio
}

