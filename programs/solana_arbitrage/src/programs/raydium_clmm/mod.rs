pub mod error;
pub mod libraries;
pub mod states;

use self::error::ErrorCode;
use self::libraries::{full_math::MulDiv, liquidity_math, swap_math, tick_math};
use self::states::{
    AmmConfigSimple, PoolStateSimple, TickArrayState, FEE_RATE_DENOMINATOR_VALUE, TICK_ARRAY_SIZE,
};
use crate::programs::{PoolKind, ProgramMeta};
use crate::utils::cpi::invoke_cpi;
use crate::utils::token::{apply_transfer_fee, apply_transfer_inverse_fee, MintFee};
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};
use pinocchio::instruction::AccountMeta;
use pinocchio::sysvars::clock::Clock;
use crate::programs::programs::Result;
use crate::programs::SolarBError;

    /// Raydium CLMM Program ID
pub const PROGRAM_ID: Pubkey =
    five8_const::decode_32_const("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK");
const SWAP_V2_DISC: [u8; 8] = [43, 4, 237, 11, 26, 201, 30, 98];

// Static accounts (from static_base, 1 account)
pub const S_PROGRAM_ID: usize = 0;

// Dynamic accounts (from dyn_start, 10 accounts)
pub const D_POOL: usize = 0;
pub const D_VAULT_0: usize = 1;
pub const D_VAULT_1: usize = 2;
pub const D_AMM_CONFIG: usize = 3;
pub const D_OBSERVATION: usize = 4;
pub const D_BITMAP_EXT: usize = 5;
// Buy direction tick arrays (zero_for_one: token_0 -> token_1)
pub const D_TICK_BUY_0: usize = 6;
pub const D_TICK_BUY_1: usize = 7;
// Sell direction tick arrays (one_for_zero: token_1 -> token_0)
pub const D_TICK_SELL_0: usize = 8;
pub const D_TICK_SELL_1: usize = 9;
pub const DYNAMIC_ACCOUNTS: usize = 10;
// Pool byte offsets (after 8-byte Anchor discriminator, #[repr(C, packed)])
const POOL_DISC: usize = 8;
const POOL_TOKEN_MINT_0: usize = POOL_DISC + 65;   // bump(1) + amm_config(32) + owner(32)
const POOL_TOKEN_MINT_1: usize = POOL_DISC + 97;
const POOL_TOKEN_VAULT_0: usize = POOL_DISC + 129;
const POOL_TOKEN_VAULT_1: usize = POOL_DISC + 161;
const POOL_TICK_SPACING: usize = POOL_DISC + 227;  // + observation_key(32) + decimals(2)
const POOL_LIQUIDITY: usize = POOL_DISC + 229;
const POOL_SQRT_PRICE: usize = POOL_DISC + 245;
const POOL_TICK_CURRENT: usize = POOL_DISC + 261;

// AmmConfig byte offsets (after 8-byte discriminator, #[repr(C, packed)])
const CFG_DISC: usize = 8;
const CFG_PROTOCOL_FEE: usize = CFG_DISC + 35;  // bump(1) + index(2) + owner(32)
const CFG_TRADE_FEE: usize = CFG_DISC + 39;
const CFG_FUND_FEE: usize = CFG_DISC + 45;      // + tick_spacing(2)

// ── Zero-copy tick array scanning ──────────────────────────────────
// Byte layout of on-chain TickArrayState (after 8-byte discriminator):
//   [0..32]  pool_id        [32..36] start_tick_index
//   [36..]   ticks[60]      (each TickState = 168 bytes, packed)
// Within each TickState:
//   [0..4] tick (i32)   [4..20] liquidity_net (i128)   [20..36] liquidity_gross (u128)
const TA_DISC: usize = 8;
const TA_POOL_OFF: usize = TA_DISC;
const TA_START_OFF: usize = TA_POOL_OFF + 32;
const TA_TICKS_OFF: usize = TA_START_OFF + 4;
const TA_TICK_SIZE: usize = 168;
const TA_TICK_CNT: usize = 60;
const TA_MIN_LEN: usize = TA_TICKS_OFF + TA_TICK_SIZE * TA_TICK_CNT;
const TS_LIQ_NET: usize = 4;
const TS_LIQ_GROSS: usize = 20;


/// Compute price as f64 from sqrt_price_x64 (Q64.64 format).
/// price = (sqrt_price / 2^64)²
fn sqrt_price_to_f64(sqrt_price_x64: u128) -> f64 {
    let sqrt_price = sqrt_price_x64 as f64 / (1u128 << 64) as f64;
    sqrt_price * sqrt_price
}

/// Liquidity available in a contiguous tick range.
/// Each range represents the liquidity between two adjacent initialized ticks.
#[derive(Debug, Clone)]
pub struct LiquidityRange {
    /// Lower tick of this range (inclusive)
    pub tick_lower: i32,
    /// Upper tick of this range (exclusive)
    pub tick_upper: i32,
    /// Liquidity available in this range
    pub liquidity: u128,
    /// sqrt_price at lower tick (Q64.64)
    pub sqrt_price_lower_x64: u128,
    /// sqrt_price at upper tick (Q64.64)
    pub sqrt_price_upper_x64: u128,
}

impl LiquidityRange {
    /// Estimate how much of token_0 can be swapped within this range
    /// (i.e. how much token_0 is needed to move the price from lower to upper).
    /// Formula: Δx = L × (1/√P_lower − 1/√P_upper)
    pub fn token_0_capacity(&self) -> u128 {
        if self.liquidity == 0 || self.sqrt_price_lower_x64 == 0 || self.sqrt_price_upper_x64 == 0 {
            return 0;
        }
        let q64 = 1u128 << 64;
        // L × Q64 / sqrt_price_lower - L × Q64 / sqrt_price_upper
        let a = (self.liquidity as u128)
            .checked_mul(q64)
            .and_then(|v| v.checked_div(self.sqrt_price_lower_x64));
        let b = (self.liquidity as u128)
            .checked_mul(q64)
            .and_then(|v| v.checked_div(self.sqrt_price_upper_x64));
        match (a, b) {
            (Some(a), Some(b)) => a.saturating_sub(b),
            _ => 0,
        }
    }

    /// Estimate how much of token_1 can be swapped within this range
    /// (i.e. how much token_1 is needed to move the price from lower to upper).
    /// Formula: Δy = L × (√P_upper − √P_lower) / Q64
    pub fn token_1_capacity(&self) -> u128 {
        if self.liquidity == 0 {
            return 0;
        }
        let q64 = 1u128 << 64;
        let delta_sqrt = self.sqrt_price_upper_x64.saturating_sub(self.sqrt_price_lower_x64);
        (self.liquidity as u128)
            .checked_mul(delta_sqrt)
            .map(|v| v / q64)
            .unwrap_or(0)
    }
}

/// Swap state used during swap calculation
#[derive(Debug, Clone, Default)]
struct SwapState {
    amount_specified_remaining: u64,
    amount_calculated: u64,
    sqrt_price_x64: u128,
    tick: i32,
    liquidity: u128,
    fee_amount: u64,
    protocol_fee: u64,
    fund_fee: u64,
}

/// Pre-loaded tick array: header + raw data pointer (valid for entire instruction)
#[derive(Clone, Copy)]
struct TickArrayRef {
    start_tick_index: i32,
    data: *const u8,
}

impl TickArrayRef {
    const EMPTY: Self = Self { start_tick_index: 0, data: std::ptr::null() };
}

const MAX_TA: usize = 8;

#[derive(Clone)]
pub struct RaydiumCLMM {
    pub pool_id: Pubkey,
    pub base_token_pk: Pubkey,
    pub quote_token_pk: Pubkey,
    pub token_vault_0: Pubkey,
    pub token_vault_1: Pubkey,
    pub sqrt_price_x64: u128,
    pub tick_current: i32,
    pub liquidity: u128,
    pub tick_spacing: u16,
    pub trade_fee_rate: u32,
    pub protocol_fee_rate: u32,
    pub fund_fee_rate: u32,
    /// Effective fee rate as f64 (0.0 - 1.0), e.g. 0.0025 = 0.25%
    pub fee_rate: f64,
    /// Pre-computed fee factor: 1 - fee_rate
    pub fee_factor: (f64, f64),
    pub price: f64,
    pub static_base: usize,
    pub dyn_start: usize,
    pub buy_max_in: u64,
    pub buy_max_out: u64,
    pub sell_max_in: u64,
    pub sell_max_out: u64,
    /// Whether max amounts have been lazily computed via tick array traversal
    pub max_amounts_initialized: bool,
}

impl ProgramMeta for RaydiumCLMM {

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

    fn name(&self) -> &'static str { "RaydiumCLMM" }
    fn pool_kind(&self) -> PoolKind { PoolKind::RaydiumCLMM }

    fn get_fee_factor(&self) -> Result<(f64, f64)> { Ok(self.fee_factor) }

    /// Virtual reserves from concentrated liquidity within the active tick range.
    /// Within a single tick range (constant L), a CLMM swap is mathematically
    /// equivalent to a constant-product AMM:
    ///   v_0 = L / √P,  v_1 = L × √P
    /// These are returned as (token_0_reserve, token_1_reserve).
    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        if self.liquidity == 0 || self.sqrt_price_x64 == 0 {
            return Err(ProgramError::from(SolarBError::InsufficientFunds));
        }
        let sqrt_p = self.sqrt_price_x64 as f64 / (1u128 << 64) as f64;
        let l = self.liquidity as f64;
        let v_0 = (l / sqrt_p).min(u64::MAX as f64) as u64;
        let v_1 = (l * sqrt_p).min(u64::MAX as f64) as u64;
        Ok((v_0, v_1))
    }

    fn swap_base_in(
        &mut self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        amount_in: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        _clock: &Clock,
    ) -> Result<u64> {
        // Lazily compute max amounts on first swap call
        if !self.max_amounts_initialized {
            if let Ok((mi, mo)) = self.calculate_swap_with_tick_arrays(accounts, u64::MAX, true, true) {
                self.buy_max_in = mi;
                self.buy_max_out = mo;
            }
            if let Ok((mi, mo)) = self.calculate_swap_with_tick_arrays(accounts, u64::MAX, false, true) {
                self.sell_max_in = mi;
                self.sell_max_out = mo;
            }
            self.max_amounts_initialized = true;
        }

        let zero_for_one = input_mint == self.base_token_pk;

        let transfer_fee = apply_transfer_fee(amount_in, input_transfer_fee);
        let actual_amount_in = amount_in.saturating_sub(transfer_fee);

        let (_, amount_out) =
            self.calculate_swap_with_tick_arrays(accounts, actual_amount_in, zero_for_one, true)?;

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
        let zero_for_one = output_mint == self.quote_token_pk;

        let out_fee = apply_transfer_inverse_fee(amount_out, output_transfer_fee);
        let amount_out_with_fee = amount_out
            .checked_add(out_fee)
            .ok_or(ErrorCode::AmountOverflow)?;

        let (_, amount_in) =
            self.calculate_swap_with_tick_arrays(accounts, amount_out_with_fee, zero_for_one, false)?;

        let in_fee = apply_transfer_inverse_fee(amount_in, input_transfer_fee);
        let final_amount_in = amount_in
            .checked_add(in_fee)
            .ok_or(ErrorCode::AmountOverflow)?;

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
        let amm_config = &accounts[self.dyn_start + D_AMM_CONFIG];
        let vault_0 = &accounts[self.dyn_start + D_VAULT_0];
        let vault_1 = &accounts[self.dyn_start + D_VAULT_1];
        let observation = &accounts[self.dyn_start + D_OBSERVATION];
        let bitmap_extension = &accounts[self.dyn_start + D_BITMAP_EXT];
        let token_program_spl = &accounts[1];
        let token_program_2022 = &accounts[2];
        let memo = &accounts[3];

        let zero_for_one = input_mint == self.base_token_pk;

        let (
            user_input_account,
            user_output_account,
            input_vault,
            output_vault,
            input_mint_acc,
            output_mint_acc,
        ) = if zero_for_one {
            (
                user_mint_1_token_account,
                user_mint_2_token_account,
                vault_0,
                vault_1,
                mint_1_account,
                mint_2_account,
            )
        } else {
            (
                user_mint_1_token_account,
                user_mint_2_token_account,
                vault_1,
                vault_0,
                mint_1_account,
                mint_2_account,
            )
        };

        // Stack-allocated metas — max 16 entries (13 base + 1 bitmap + 2 tick arrays)
        let mut metas: [core::mem::MaybeUninit<AccountMeta>; 16] = unsafe { core::mem::MaybeUninit::uninit().assume_init() };
        let mut mn = 0usize;
        macro_rules! push_meta {
            (w $key:expr) => { metas[mn].write(AccountMeta::new($key, true, false)); mn += 1; };
            (ws $key:expr) => { metas[mn].write(AccountMeta::new($key, true, true)); mn += 1; };
            (r $key:expr) => { metas[mn].write(AccountMeta::new($key, false, false)); mn += 1; };
        }
        push_meta!(ws payer.key());
        push_meta!(r amm_config.key());
        push_meta!(w pool_id.key());
        push_meta!(w user_input_account.key());
        push_meta!(w user_output_account.key());
        push_meta!(w input_vault.key());
        push_meta!(w output_vault.key());
        push_meta!(w observation.key());
        push_meta!(r token_program_spl.key());
        push_meta!(r token_program_2022.key());
        push_meta!(r memo.key());
        push_meta!(r input_mint_acc.key());
        push_meta!(r output_mint_acc.key());

        if *bitmap_extension.key() != PROGRAM_ID {
            push_meta!(w bitmap_extension.key());
        }

        let (ta_from, ta_to) = Self::tick_array_range(self.dyn_start, zero_for_one);
        for i in ta_from..ta_to {
            push_meta!(w accounts[i].key());
        }

        let mut data = [0u8; 41];
        data[..8].copy_from_slice(&SWAP_V2_DISC);
        data[8..16].copy_from_slice(&amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&min_amount_out.unwrap_or(0).to_le_bytes());
        // data[24..40] already zeroed: sqrt_price_limit_x64 = 0
        data[40] = 1; // is_base_input = true (exact input)

        let mut accs: [core::mem::MaybeUninit<&AccountInfo>; 16] = [core::mem::MaybeUninit::uninit(); 16];
        let mut ai = 0usize;
        macro_rules! push_acc {
            ($e:expr) => { accs[ai].write($e); ai += 1; };
        }
        push_acc!(payer);
        push_acc!(amm_config);
        push_acc!(pool_id);
        push_acc!(user_input_account);
        push_acc!(user_output_account);
        push_acc!(input_vault);
        push_acc!(output_vault);
        push_acc!(observation);
        push_acc!(token_program_spl);
        push_acc!(token_program_2022);
        push_acc!(memo);
        push_acc!(input_mint_acc);
        push_acc!(output_mint_acc);

        if *bitmap_extension.key() != PROGRAM_ID {
            push_acc!(bitmap_extension);
        }

        for i in ta_from..ta_to {
            accs[ai].write(&accounts[i]); ai += 1;
        }

        let metas_slice: &[AccountMeta] = unsafe {
            core::slice::from_raw_parts(metas.as_ptr() as *const AccountMeta, mn)
        };
        let accs_slice: &[&AccountInfo] = unsafe {
            core::slice::from_raw_parts(accs.as_ptr() as *const &AccountInfo, ai)
        };
        invoke_cpi(&PROGRAM_ID, metas_slice, &data, accs_slice)?;

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
        let amm_config = &accounts[self.dyn_start + D_AMM_CONFIG];
        let vault_0 = &accounts[self.dyn_start + D_VAULT_0];
        let vault_1 = &accounts[self.dyn_start + D_VAULT_1];
        let observation = &accounts[self.dyn_start + D_OBSERVATION];
        let bitmap_extension = &accounts[self.dyn_start + D_BITMAP_EXT];
        let token_program_spl = &accounts[1];
        let token_program_2022 = &accounts[2];
        let memo = &accounts[3];

        let zero_for_one = input_mint == self.base_token_pk;

        let (
            user_input_account,
            user_output_account,
            input_vault,
            output_vault,
            input_mint_acc,
            output_mint_acc,
        ) = if zero_for_one {
            (
                user_mint_1_token_account,
                user_mint_2_token_account,
                vault_0,
                vault_1,
                mint_1_account,
                mint_2_account,
            )
        } else {
            (
                user_mint_1_token_account,
                user_mint_2_token_account,
                vault_1,
                vault_0,
                mint_1_account,
                mint_2_account,
            )
        };

        // Stack-allocated metas — max 16 entries
        let mut metas: [core::mem::MaybeUninit<AccountMeta>; 16] = unsafe { core::mem::MaybeUninit::uninit().assume_init() };
        let mut mn = 0usize;
        macro_rules! push_meta {
            (w $key:expr) => { metas[mn].write(AccountMeta::new($key, true, false)); mn += 1; };
            (ws $key:expr) => { metas[mn].write(AccountMeta::new($key, true, true)); mn += 1; };
            (r $key:expr) => { metas[mn].write(AccountMeta::new($key, false, false)); mn += 1; };
        }
        push_meta!(ws payer.key());
        push_meta!(r amm_config.key());
        push_meta!(w pool_id.key());
        push_meta!(w user_input_account.key());
        push_meta!(w user_output_account.key());
        push_meta!(w input_vault.key());
        push_meta!(w output_vault.key());
        push_meta!(w observation.key());
        push_meta!(r token_program_spl.key());
        push_meta!(r token_program_2022.key());
        push_meta!(r memo.key());
        push_meta!(r input_mint_acc.key());
        push_meta!(r output_mint_acc.key());

        if *bitmap_extension.key() != PROGRAM_ID {
            push_meta!(w bitmap_extension.key());
        }

        // Add tick array accounts for this swap direction
        let (ta_from, ta_to) = Self::tick_array_range(self.dyn_start, zero_for_one);
        for i in ta_from..ta_to {
            push_meta!(w accounts[i].key());
        }

        let mut data = [0u8; 41];
        data[..8].copy_from_slice(&SWAP_V2_DISC);
        data[8..16].copy_from_slice(&amount_out.unwrap_or(0).to_le_bytes());
        data[16..24].copy_from_slice(&max_amount_in.to_le_bytes());
        // data[24..40] already zeroed: sqrt_price_limit_x64 = 0
        data[40] = 0; // is_base_input = false (exact output)

        let mut accs: [core::mem::MaybeUninit<&AccountInfo>; 16] = [core::mem::MaybeUninit::uninit(); 16];
        let mut ai = 0usize;
        macro_rules! push_acc {
            ($e:expr) => { accs[ai].write($e); ai += 1; };
        }
        push_acc!(payer);
        push_acc!(amm_config);
        push_acc!(pool_id);
        push_acc!(user_input_account);
        push_acc!(user_output_account);
        push_acc!(input_vault);
        push_acc!(output_vault);
        push_acc!(observation);
        push_acc!(token_program_spl);
        push_acc!(token_program_2022);
        push_acc!(memo);
        push_acc!(input_mint_acc);
        push_acc!(output_mint_acc);

        if *bitmap_extension.key() != PROGRAM_ID {
            push_acc!(bitmap_extension);
        }

        for i in ta_from..ta_to {
            accs[ai].write(&accounts[i]); ai += 1;
        }

        let metas_slice: &[AccountMeta] = unsafe {
            core::slice::from_raw_parts(metas.as_ptr() as *const AccountMeta, mn)
        };
        let accs_slice: &[&AccountInfo] = unsafe {
            core::slice::from_raw_parts(accs.as_ptr() as *const &AccountInfo, ai)
        };
        invoke_cpi(&PROGRAM_ID, metas_slice, &data, accs_slice)?;

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
        let zero_for_one = input_mint == self.base_token_pk;
        let fee_factor = self.fee_factor.0;

        // Virtual reserves for CP formula within active tick range
        let (v_0, v_1) = self.get_vault_amounts().unwrap_or((0, 0));
        let (res_in, res_out) = if zero_for_one { (v_0 as u128, v_1 as u128) } else { (v_1 as u128, v_0 as u128) };
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
                    "[RaydiumCLMM] Crossing ticks: profit {:.2}% > tick step {:.2}%",
                    profit_pct * 100.0, tick_step_bps as f64 / 100.0
                );
                let out_active = cp_quote(max_in_active);

                let remaining = amount_in.min(max_in) - max_in_active;
                // Linear estimate at next tick's marginal price
                let (price, inverse_price) = self.get_prices()?;
                let tick_step_frac = self.tick_spacing as f64 * 0.0001;
                let next_price = if zero_for_one {
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

    /// Bin step as a fraction: each Raydium CLMM tick = 0.01% price change (1.0001),
    /// so tick_spacing ticks ≈ tick_spacing × 0.01%.
    fn get_bin_step_frac(&self) -> f64 {
        self.tick_spacing as f64 * 0.0001
    }

    /// Gross input capacity of the active tick range (before fee deduction).
    /// Computed analytically from sqrt_price_x64 + liquidity — no account reads.
    fn get_active_bin_max_in(&self, input_mint: Pubkey) -> Result<u64> {
        if self.liquidity == 0 {
            return Ok(0);
        }
        let zero_for_one = input_mint == self.base_token_pk;
        let tick_spacing = self.tick_spacing as i32;
        let tick_lower = self.get_lower_tick_boundary(self.tick_current);
        let tick_boundary = if zero_for_one { tick_lower } else { tick_lower + tick_spacing };
        let sqrt_price_boundary = tick_math::get_sqrt_price_at_tick(tick_boundary)
            .map_err(|_| ProgramError::from(SolarBError::InsufficientAccounts))?;
        let net_cap = liquidity_math::get_amount_in_for_liquidity(
            self.sqrt_price_x64, sqrt_price_boundary, self.liquidity, zero_for_one,
        )
        .unwrap_or(0);
        // Convert net → gross (what the user sends, including the fee portion)
        let fee_factor = self.fee_factor.0;
        let gross_cap = if fee_factor > 0.0 {
            (net_cap as f64 / fee_factor) as u64
        } else {
            0
        };
        Ok(gross_cap)
    }

    /// Virtual reserves for the active tick range, computed from sqrt_price × liquidity.
    /// Within a tick range, CLMM behaves as constant-product: out = r_out * dx / (r_in + dx).
    fn get_clmm_virtual_reserves(&self, input_mint: Pubkey) -> Option<(f64, f64)> {
        if self.liquidity == 0 || self.sqrt_price_x64 == 0 {
            return None;
        }
        let zero_for_one = input_mint == self.base_token_pk;
        let q64 = (1u128 << 64) as f64;
        let sqrt_p = self.sqrt_price_x64 as f64 / q64;
        let l = self.liquidity as f64;
        if zero_for_one {
            // Input = token A: reserve_in = L / sqrt_P, reserve_out = L * sqrt_P
            Some((l / sqrt_p, l * sqrt_p))
        } else {
            // Input = token B: reserve_in = L * sqrt_P, reserve_out = L / sqrt_P
            Some((l * sqrt_p, l / sqrt_p))
        }
    }

    /// Per-tick-range segment data for the CLMM multi-tick walker.
    /// Returns (reserve_in, reserve_out, net_capacity, fee_factor) using virtual reserves.
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

    #[cfg(any(test, feature = "debug"))]
    fn log_accounts(&self, _accounts: &[AccountInfo]) -> Result<()> {
        pinocchio::log::sol_log("=== Raydium CLMM ===");
        Ok(())
    }
}

impl RaydiumCLMM {


    /// Returns (from, to) absolute account indices for the tick arrays of a given swap direction.
    #[inline(always)]
    fn tick_array_range(dyn_start: usize, zero_for_one: bool) -> (usize, usize) {
        if zero_for_one {
            (dyn_start + D_TICK_BUY_0, dyn_start + D_TICK_BUY_1 + 1)
        } else {
            (dyn_start + D_TICK_SELL_0, dyn_start + D_TICK_SELL_1 + 1)
        }
    }



    pub fn new(
        accounts: &[AccountInfo],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
    ) -> Result<Self> {
        let pool_id = &accounts[dyn_start + D_POOL];

        // Read only the fields we need directly from pool account bytes
        let d = unsafe { pool_id.borrow_data_unchecked() };

        let base_token_pk = Pubkey::try_from(&d[POOL_TOKEN_MINT_0..POOL_TOKEN_MINT_0 + 32]).unwrap();
        let quote_token_pk = Pubkey::try_from(&d[POOL_TOKEN_MINT_1..POOL_TOKEN_MINT_1 + 32]).unwrap();
        let token_vault_0 = Pubkey::try_from(&d[POOL_TOKEN_VAULT_0..POOL_TOKEN_VAULT_0 + 32]).unwrap();
        let token_vault_1 = Pubkey::try_from(&d[POOL_TOKEN_VAULT_1..POOL_TOKEN_VAULT_1 + 32]).unwrap();
        let tick_spacing = u16::from_le_bytes(d[POOL_TICK_SPACING..POOL_TICK_SPACING + 2].try_into().unwrap());
        let liquidity = u128::from_le_bytes(d[POOL_LIQUIDITY..POOL_LIQUIDITY + 16].try_into().unwrap());
        let sqrt_price_x64 = u128::from_le_bytes(d[POOL_SQRT_PRICE..POOL_SQRT_PRICE + 16].try_into().unwrap());
        let tick_current = i32::from_le_bytes(d[POOL_TICK_CURRENT..POOL_TICK_CURRENT + 4].try_into().unwrap());
        drop(d);

        // Read fee rates directly from AmmConfig account bytes
        let cfg_data = unsafe { accounts[dyn_start + D_AMM_CONFIG].borrow_data_unchecked() };
        let trade_fee_rate_raw = u32::from_le_bytes(cfg_data[CFG_TRADE_FEE..CFG_TRADE_FEE + 4].try_into().unwrap());
        let protocol_fee_rate = u32::from_le_bytes(cfg_data[CFG_PROTOCOL_FEE..CFG_PROTOCOL_FEE + 4].try_into().unwrap());
        let fund_fee_rate = u32::from_le_bytes(cfg_data[CFG_FUND_FEE..CFG_FUND_FEE + 4].try_into().unwrap());
        drop(cfg_data);

        let fee_rate = trade_fee_rate_raw as f64 / FEE_RATE_DENOMINATOR_VALUE as f64;
        let price = sqrt_price_to_f64(sqrt_price_x64);

        debug_eprintln!("RaydiumCLMM: pool_id {:?} , price {}, inverse_price {}, fee_rate {}", pool_id.key(), price, 1.0 / price, fee_rate);

        let instance = RaydiumCLMM {
            pool_id: *pool_id.key(),
            base_token_pk,
            quote_token_pk,
            token_vault_0,
            token_vault_1,
            sqrt_price_x64,
            tick_current,
            liquidity,
            tick_spacing,
            trade_fee_rate: trade_fee_rate_raw,
            protocol_fee_rate,
            fund_fee_rate,
            fee_rate,
            fee_factor: { let f = 1.0 - fee_rate; (f, f) },
            price,
            static_base,
            dyn_start,
            buy_max_in: u64::MAX,
            buy_max_out: u64::MAX,
            sell_max_in: u64::MAX,
            sell_max_out: u64::MAX,
            max_amounts_initialized: false,
        };
        Ok(instance)
    }

    /// Compute deferred fields: transfer fee rates.
    /// Max amounts are already lazily initialized on first swap call.
    pub fn prepare_for_execution(
        &mut self,
        _accounts: &[AccountInfo],
    ) {
    }

    /// Pre-load all tick array accounts: validate once, store raw data pointers.
    /// Pointers remain valid for the entire Solana instruction lifetime.
    fn preload_tick_arrays(
        accounts: &[AccountInfo],
        pool_id_bytes: &[u8; 32],
        from: usize,
        to: usize,
    ) -> ([TickArrayRef; MAX_TA], usize) {
        let mut arr = [TickArrayRef::EMPTY; MAX_TA];
        let mut count = 0;
        for i in from..to {
            if count >= MAX_TA { break; }
            {
                let data = unsafe { accounts[i].borrow_data_unchecked() };
                if data.len() >= TA_MIN_LEN
                    && data[TA_POOL_OFF..TA_POOL_OFF + 32] == *pool_id_bytes
                {
                    let start = i32::from_le_bytes(
                        unsafe { *(data.as_ptr().add(TA_START_OFF) as *const [u8; 4]) }
                    );
                    arr[count] = TickArrayRef { start_tick_index: start, data: data.as_ptr() };
                    count += 1;
                }
            }
            // Pointer stays valid (account data is pinned for the instruction)
        }
        (arr, count)
    }

    /// Scan a tick array's raw data for the next initialized tick.
    /// All validation was done at preload time — pure pointer arithmetic here.
    #[inline(always)]
    unsafe fn scan_tick_raw(
        data: *const u8,
        start: i32,
        tick_spacing: i32,
        current_tick: i32,
        zero_for_one: bool,
    ) -> Option<(i32, i128)> {
        let raw_offset = current_tick.saturating_sub(start) / tick_spacing;

        if zero_for_one {
            let mut i = raw_offset.min(TA_TICK_CNT as i32 - 1).max(0) as usize;
            loop {
                let b = data.add(TA_TICKS_OFF + i * TA_TICK_SIZE);
                if u128::from_le_bytes(*(b.add(TS_LIQ_GROSS) as *const [u8; 16])) != 0 {
                    let t = i32::from_le_bytes(*(b as *const [u8; 4]));
                    if t <= current_tick {
                        let ln = i128::from_le_bytes(*(b.add(TS_LIQ_NET) as *const [u8; 16]));
                        return Some((t, ln));
                    }
                }
                if i == 0 { break; }
                i -= 1;
            }
        } else {
            let from = (raw_offset + 1).max(0).min(TA_TICK_CNT as i32 - 1) as usize;
            let mut i = from;
            while i < TA_TICK_CNT {
                let b = data.add(TA_TICKS_OFF + i * TA_TICK_SIZE);
                if u128::from_le_bytes(*(b.add(TS_LIQ_GROSS) as *const [u8; 16])) != 0 {
                    let t = i32::from_le_bytes(*(b as *const [u8; 4]));
                    if t > current_tick {
                        let ln = i128::from_le_bytes(*(b.add(TS_LIQ_NET) as *const [u8; 16]));
                        return Some((t, ln));
                    }
                }
                i += 1;
            }
        }
        None
    }

    /// Calculate swap with pre-loaded tick arrays and tracked current array.
    /// Zero try_borrow_data() calls in the hot loop.
    /// Returns (amount_consumed, amount_calculated).
    fn calculate_swap_with_tick_arrays(
        &self,
        accounts: &[AccountInfo],
        amount_specified: u64,
        zero_for_one: bool,
        is_base_input: bool,
    ) -> Result<(u64, u64)> {
        let pool_id_bytes = self.pool_id;
        let ts = self.tick_spacing as i32;
        let ticks_in_array = TICK_ARRAY_SIZE * ts;

        // Select tick arrays for the swap direction
        let (ta_from, ta_to) = Self::tick_array_range(self.dyn_start, zero_for_one);

        // Pre-load direction-specific tick arrays ONCE — store raw data pointers
        let (ta, ta_count) = Self::preload_tick_arrays(
            accounts, &pool_id_bytes, ta_from, ta_to,
        );

        if self.liquidity == 0 && ta_count == 0 {
            return Err(ErrorCode::InsufficientLiquidityForDirection.into());
        }

        // Find initial current tick array
        let initial_start = TickArrayState::get_array_start_index(self.tick_current, self.tick_spacing);
        let mut cur: usize = 0;
        let mut cur_valid = false;
        {
            let mut j = 0;
            while j < ta_count {
                if ta[j].start_tick_index == initial_start {
                    cur = j;
                    cur_valid = true;
                    break;
                }
                j += 1;
            }
        }

        let sqrt_price_limit_x64 = if zero_for_one {
            tick_math::MIN_SQRT_PRICE_X64 + 1
        } else {
            tick_math::MAX_SQRT_PRICE_X64 - 1
        };

        let mut state = SwapState {
            amount_specified_remaining: amount_specified,
            amount_calculated: 0,
            sqrt_price_x64: self.sqrt_price_x64,
            tick: self.tick_current,
            liquidity: self.liquidity,
            fee_amount: 0,
            protocol_fee: 0,
            fund_fee: 0,
        };

        let mut iterations = 0u32;

        while state.amount_specified_remaining != 0
            && state.sqrt_price_x64 != sqrt_price_limit_x64
            && iterations < 100
        {
            iterations += 1;
            let step_sqrt_price_start = state.sqrt_price_x64;

            // ── Find next initialized tick (zero borrows, pure pointer reads) ──
            let (tick_next, liquidity_net) = unsafe { 'find: {
                if cur_valid {
                    // Scan current array
                    if let Some(r) = Self::scan_tick_raw(
                        ta[cur].data, ta[cur].start_tick_index, ts, state.tick, zero_for_one,
                    ) {
                        break 'find r;
                    }
                    // Current array exhausted — try adjacent
                    let next_start = if zero_for_one {
                        ta[cur].start_tick_index - ticks_in_array
                    } else {
                        ta[cur].start_tick_index + ticks_in_array
                    };
                    let mut k = 0;
                    while k < ta_count {
                        if ta[k].start_tick_index == next_start {
                            cur = k;
                            let sentinel = if zero_for_one { i32::MAX } else { i32::MIN };
                            if let Some(r) = Self::scan_tick_raw(
                                ta[cur].data, next_start, ts, sentinel, zero_for_one,
                            ) {
                                break 'find r;
                            }
                            break;
                        }
                        k += 1;
                    }
                    if k == ta_count { cur_valid = false; }
                }
                // No tick found — use boundary
                let t = if zero_for_one {
                    self.get_lower_tick_boundary(state.tick)
                } else {
                    self.get_upper_tick_boundary(state.tick)
                };
                (t, 0i128)
            }};

            let tick_next = tick_next.max(tick_math::MIN_TICK).min(tick_math::MAX_TICK);
            let initialized = liquidity_net != 0;

            let sqrt_price_next_x64 =
                tick_math::get_sqrt_price_at_tick(tick_next).unwrap_or(if zero_for_one {
                    tick_math::MIN_SQRT_PRICE_X64
                } else {
                    tick_math::MAX_SQRT_PRICE_X64
                });

            let target_price = if (zero_for_one && sqrt_price_next_x64 < sqrt_price_limit_x64)
                || (!zero_for_one && sqrt_price_next_x64 > sqrt_price_limit_x64)
            {
                sqrt_price_limit_x64
            } else {
                sqrt_price_next_x64
            };

            // Skip if no liquidity
            if state.liquidity == 0 {
                state.tick = tick_next;
                state.sqrt_price_x64 = sqrt_price_next_x64;
                continue;
            }

            // Compute swap step
            let swap_step = swap_math::compute_swap_step(
                step_sqrt_price_start,
                target_price,
                state.liquidity,
                state.amount_specified_remaining,
                self.trade_fee_rate,
                is_base_input,
                zero_for_one,
            );

            state.sqrt_price_x64 = swap_step.sqrt_price_next_x64;
            let amount_in = swap_step.amount_in;
            let amount_out = swap_step.amount_out;
            let mut fee_amount = swap_step.fee_amount;

            // Update amounts
            if is_base_input {
                state.amount_specified_remaining = state
                    .amount_specified_remaining
                    .saturating_sub(amount_in + fee_amount);
                state.amount_calculated = state.amount_calculated.saturating_add(amount_out);
            } else {
                state.amount_specified_remaining = state
                    .amount_specified_remaining
                    .saturating_sub(amount_out);
                state.amount_calculated = state
                    .amount_calculated
                    .saturating_add(amount_in)
                    .saturating_add(fee_amount);
            }

            // Calculate protocol and fund fees (deducted from LP fee)
            if self.protocol_fee_rate > 0 {
                let delta = (fee_amount as u128)
                    .mul_div_floor(
                        self.protocol_fee_rate as u128,
                        FEE_RATE_DENOMINATOR_VALUE as u128,
                    )
                    .unwrap_or(0) as u64;
                fee_amount -= delta;
                state.protocol_fee += delta;
            }
            if self.fund_fee_rate > 0 {
                let delta = (fee_amount as u128)
                    .mul_div_floor(
                        self.fund_fee_rate as u128,
                        FEE_RATE_DENOMINATOR_VALUE as u128,
                    )
                    .unwrap_or(0) as u64;
                fee_amount -= delta;
                state.fund_fee += delta;
            }

            state.fee_amount += fee_amount;

            // Shift tick if we reached the next price
            if state.sqrt_price_x64 == sqrt_price_next_x64 {
                if initialized {
                    let net = if zero_for_one { -liquidity_net } else { liquidity_net };
                    state.liquidity =
                        liquidity_math::add_delta(state.liquidity, net).unwrap_or(state.liquidity);
                }
                state.tick = if zero_for_one { tick_next - 1 } else { tick_next };
            } else if state.sqrt_price_x64 != step_sqrt_price_start {
                state.tick =
                    tick_math::get_tick_at_sqrt_price(state.sqrt_price_x64).unwrap_or(state.tick);
            }
        }

        let amount_consumed = amount_specified.saturating_sub(state.amount_specified_remaining);
        Ok((amount_consumed, state.amount_calculated))
    }

    /// Walk all provided tick arrays for the given direction and return the liquidity
    /// distribution: a sorted list of `LiquidityRange` values showing how liquidity
    /// changes across initialized ticks.
    ///
    /// `zero_for_one = true`  → buy direction (price decreasing, token_0 in)
    /// `zero_for_one = false` → sell direction (price increasing, token_1 in)
    pub fn get_liquidity_distribution(
        &self,
        accounts: &[AccountInfo],
        zero_for_one: bool,
    ) -> Result<Vec<LiquidityRange>> {
        let pool_id_bytes = self.pool_id;

        let (ta_from, ta_to) = Self::tick_array_range(self.dyn_start, zero_for_one);
        let (ta, ta_count) = Self::preload_tick_arrays(accounts, &pool_id_bytes, ta_from, ta_to);

        // Collect all initialized ticks across all arrays, sorted ascending
        let mut all_ticks: Vec<(i32, i128)> = Vec::new();
        for idx in 0..ta_count {
            let data = ta[idx].data;
            for slot in 0..TA_TICK_CNT {
                unsafe {
                    let b = data.add(TA_TICKS_OFF + slot * TA_TICK_SIZE);
                    let lg = u128::from_le_bytes(*(b.add(TS_LIQ_GROSS) as *const [u8; 16]));
                    if lg != 0 {
                        let t = i32::from_le_bytes(*(b as *const [u8; 4]));
                        let ln = i128::from_le_bytes(*(b.add(TS_LIQ_NET) as *const [u8; 16]));
                        all_ticks.push((t, ln));
                    }
                }
            }
        }

        // Sort by tick ascending
        all_ticks.sort_unstable_by_key(|&(t, _)| t);
        // Deduplicate (same tick can't appear twice, but just in case overlapping arrays)
        all_ticks.dedup_by_key(|entry| entry.0);

        if all_ticks.is_empty() {
            return Ok(Vec::new());
        }

        // Build ranges by walking ticks and tracking running liquidity.
        // Start with pool's current liquidity and walk outward from current_tick.
        let mut ranges = Vec::new();
        let current_tick = self.tick_current;
        let current_liq = self.liquidity;

        if zero_for_one {
            // Walking downward: from current tick toward lower ticks.
            // Find ticks at or below current_tick, descending.
            let mut liq = current_liq;
            // Ticks below current_tick, in descending order
            let below: Vec<_> = all_ticks.iter().filter(|&&(t, _)| t <= current_tick).copied().collect();

            for i in (0..below.len()).rev() {
                let (tick_at, liq_net) = below[i];
                let tick_upper = if i + 1 < below.len() { below[i + 1].0 } else { current_tick + 1 };
                let tick_lower = tick_at;

                let sp_lower = tick_math::get_sqrt_price_at_tick(tick_lower)
                    .unwrap_or(tick_math::MIN_SQRT_PRICE_X64);
                let sp_upper = tick_math::get_sqrt_price_at_tick(tick_upper)
                    .unwrap_or(tick_math::MAX_SQRT_PRICE_X64);

                ranges.push(LiquidityRange {
                    tick_lower,
                    tick_upper,
                    liquidity: liq,
                    sqrt_price_lower_x64: sp_lower,
                    sqrt_price_upper_x64: sp_upper,
                });

                // Crossing this tick downward: negate liquidity_net
                liq = liquidity_math::add_delta(liq, -liq_net).unwrap_or(0);
            }
            // Return in descending order (current price → lower)
            // ranges are already in the right order (highest tick first)
        } else {
            // Walking upward: from current tick toward higher ticks.
            let mut liq = current_liq;
            let above: Vec<_> = all_ticks.iter().filter(|&&(t, _)| t > current_tick).copied().collect();

            for i in 0..above.len() {
                let (tick_at, liq_net) = above[i];
                let tick_lower = if i == 0 { current_tick } else { above[i - 1].0 };
                let tick_upper = tick_at;

                let sp_lower = tick_math::get_sqrt_price_at_tick(tick_lower)
                    .unwrap_or(tick_math::MIN_SQRT_PRICE_X64);
                let sp_upper = tick_math::get_sqrt_price_at_tick(tick_upper)
                    .unwrap_or(tick_math::MAX_SQRT_PRICE_X64);

                ranges.push(LiquidityRange {
                    tick_lower,
                    tick_upper,
                    liquidity: liq,
                    sqrt_price_lower_x64: sp_lower,
                    sqrt_price_upper_x64: sp_upper,
                });

                // Crossing this tick upward: add liquidity_net directly
                liq = liquidity_math::add_delta(liq, liq_net).unwrap_or(0);
            }
        }

        Ok(ranges)
    }

    /// Per-tick-range segment for the analytical multi-bin walker.
    ///
    /// `bin_offset=0` is the active tick range, `bin_offset=1` is the next, etc.
    /// Returns `(slope, net_capacity, fee_factor)` where:
    ///   slope        = geometric_mean_price × fee_factor
    ///   net_capacity = net input tokens (after fee) to push price to the tick boundary
    ///   fee_factor   = 1 − fee_rate
    fn get_tick_segment_impl(
        &self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        bin_offset: i32,
    ) -> Result<Option<(f64, u64, f64)>> {
        if self.liquidity == 0 {
            return Ok(None);
        }
        let zero_for_one = input_mint == self.base_token_pk;
        let fee_factor = self.fee_factor.0;

        let pool_id_bytes = self.pool_id;
        let ts = self.tick_spacing as i32;
        let ticks_in_array = TICK_ARRAY_SIZE * ts;

        let (ta_from, ta_to) = Self::tick_array_range(self.dyn_start, zero_for_one);
        let (ta, ta_count) = Self::preload_tick_arrays(accounts, &pool_id_bytes, ta_from, ta_to);

        let initial_start = TickArrayState::get_array_start_index(self.tick_current, self.tick_spacing);
        let mut cur: usize = 0;
        let mut cur_valid = false;
        {
            let mut j = 0;
            while j < ta_count {
                if ta[j].start_tick_index == initial_start {
                    cur = j;
                    cur_valid = true;
                    break;
                }
                j += 1;
            }
        }

        let mut current_sqrt_price = self.sqrt_price_x64;
        let mut current_liquidity = self.liquidity;
        let mut current_tick = self.tick_current;

        const Q64: f64 = (1u128 << 64) as f64;

        for i in 0..=bin_offset {
            let (tick_next, liquidity_net) = unsafe { 'find: {
                if cur_valid {
                    if let Some(r) = Self::scan_tick_raw(
                        ta[cur].data, ta[cur].start_tick_index, ts, current_tick, zero_for_one,
                    ) {
                        break 'find r;
                    }
                    let next_start = if zero_for_one {
                        ta[cur].start_tick_index - ticks_in_array
                    } else {
                        ta[cur].start_tick_index + ticks_in_array
                    };
                    let mut k = 0;
                    while k < ta_count {
                        if ta[k].start_tick_index == next_start {
                            cur = k;
                            let sentinel = if zero_for_one { i32::MAX } else { i32::MIN };
                            if let Some(r) = Self::scan_tick_raw(
                                ta[cur].data, next_start, ts, sentinel, zero_for_one,
                            ) {
                                break 'find r;
                            }
                            break;
                        }
                        k += 1;
                    }
                    if k == ta_count { cur_valid = false; }
                }
                // No tick found — use tick boundary as sentinel
                let t = if zero_for_one {
                    self.get_lower_tick_boundary(current_tick)
                } else {
                    self.get_upper_tick_boundary(current_tick)
                };
                (t, 0i128)
            }};

            let tick_next = tick_next.max(tick_math::MIN_TICK).min(tick_math::MAX_TICK);

            let sqrt_price_target_raw = match tick_math::get_sqrt_price_at_tick(tick_next) {
                Ok(p) => p,
                Err(_) => return Ok(None),
            };
            let sqrt_price_target = if zero_for_one {
                sqrt_price_target_raw.max(tick_math::MIN_SQRT_PRICE_X64 + 1)
            } else {
                sqrt_price_target_raw.min(tick_math::MAX_SQRT_PRICE_X64 - 1)
            };

            if i == bin_offset {
                // Net capacity: tokens to push price exactly to this tick boundary
                let capacity = liquidity_math::get_amount_in_for_liquidity(
                    current_sqrt_price, sqrt_price_target, current_liquidity, zero_for_one,
                )
                .unwrap_or(0);
                if capacity == 0 {
                    return Ok(None);
                }
                // Geometric mean price over the range: sqrt(P_curr * P_target)
                let sqrt_p_curr_f = current_sqrt_price as f64 / Q64;
                let sqrt_p_target_f = sqrt_price_target as f64 / Q64;
                let geo_mean = sqrt_p_curr_f * sqrt_p_target_f;
                if geo_mean <= 0.0 {
                    return Ok(None);
                }
                let price_mid = if zero_for_one { geo_mean } else { 1.0 / geo_mean };
                return Ok(Some((price_mid * fee_factor, capacity, fee_factor)));
            }

            // Advance: cross this tick boundary and update active liquidity
            if liquidity_net != 0 {
                let net = if zero_for_one { -liquidity_net } else { liquidity_net };
                current_liquidity = liquidity_math::add_delta(current_liquidity, net).unwrap_or(0);
            }
            current_sqrt_price = sqrt_price_target;
            current_tick = if zero_for_one { tick_next - 1 } else { tick_next };

            if current_liquidity == 0 {
                return Ok(None);
            }
        }

        Ok(None)
    }

    /// Per-tick-range segment with virtual reserves for the CLMM multi-tick walker.
    /// Returns (reserve_in, reserve_out, net_capacity, fee_factor).
    /// Virtual reserves: within a tick range, CLMM = CP with r_in = L/sqrt_P, r_out = L*sqrt_P.
    fn get_clmm_tick_segment_impl(
        &self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        bin_offset: i32,
    ) -> Result<Option<(f64, f64, u64, f64)>> {
        if self.liquidity == 0 {
            return Ok(None);
        }
        let zero_for_one = input_mint == self.base_token_pk;
        let fee_factor = self.fee_factor.0;

        let pool_id_bytes = self.pool_id;
        let ts = self.tick_spacing as i32;
        let ticks_in_array = TICK_ARRAY_SIZE * ts;

        let (ta_from, ta_to) = Self::tick_array_range(self.dyn_start, zero_for_one);
        let (ta, ta_count) = Self::preload_tick_arrays(accounts, &pool_id_bytes, ta_from, ta_to);

        let initial_start = TickArrayState::get_array_start_index(self.tick_current, self.tick_spacing);
        let mut cur: usize = 0;
        let mut cur_valid = false;
        {
            let mut j = 0;
            while j < ta_count {
                if ta[j].start_tick_index == initial_start {
                    cur = j;
                    cur_valid = true;
                    break;
                }
                j += 1;
            }
        }

        let mut current_sqrt_price = self.sqrt_price_x64;
        let mut current_liquidity = self.liquidity;
        let mut current_tick = self.tick_current;

        const Q64: f64 = (1u128 << 64) as f64;

        for i in 0..=bin_offset {
            let (tick_next, liquidity_net) = unsafe { 'find: {
                if cur_valid {
                    if let Some(r) = Self::scan_tick_raw(
                        ta[cur].data, ta[cur].start_tick_index, ts, current_tick, zero_for_one,
                    ) {
                        break 'find r;
                    }
                    let next_start = if zero_for_one {
                        ta[cur].start_tick_index - ticks_in_array
                    } else {
                        ta[cur].start_tick_index + ticks_in_array
                    };
                    let mut k = 0;
                    while k < ta_count {
                        if ta[k].start_tick_index == next_start {
                            cur = k;
                            let sentinel = if zero_for_one { i32::MAX } else { i32::MIN };
                            if let Some(r) = Self::scan_tick_raw(
                                ta[cur].data, next_start, ts, sentinel, zero_for_one,
                            ) {
                                break 'find r;
                            }
                            break;
                        }
                        k += 1;
                    }
                    if k == ta_count { cur_valid = false; }
                }
                let t = if zero_for_one {
                    self.get_lower_tick_boundary(current_tick)
                } else {
                    self.get_upper_tick_boundary(current_tick)
                };
                (t, 0i128)
            }};

            let tick_next = tick_next.max(tick_math::MIN_TICK).min(tick_math::MAX_TICK);

            let sqrt_price_target_raw = match tick_math::get_sqrt_price_at_tick(tick_next) {
                Ok(p) => p,
                Err(_) => return Ok(None),
            };
            let sqrt_price_target = if zero_for_one {
                sqrt_price_target_raw.max(tick_math::MIN_SQRT_PRICE_X64 + 1)
            } else {
                sqrt_price_target_raw.min(tick_math::MAX_SQRT_PRICE_X64 - 1)
            };

            if i == bin_offset {
                let capacity = liquidity_math::get_amount_in_for_liquidity(
                    current_sqrt_price, sqrt_price_target, current_liquidity, zero_for_one,
                )
                .unwrap_or(0);
                if capacity == 0 {
                    // Empty tick range — return zero-capacity so walker can skip (continue)
                    return Ok(Some((0.0, 0.0, 0, fee_factor)));
                }
                // Virtual reserves from sqrt_price and liquidity
                let sqrt_p = current_sqrt_price as f64 / Q64;
                let l = current_liquidity as f64;
                let (vr_in, vr_out) = if zero_for_one {
                    (l / sqrt_p, l * sqrt_p)
                } else {
                    (l * sqrt_p, l / sqrt_p)
                };
                return Ok(Some((vr_in, vr_out, capacity, fee_factor)));
            }

            // Advance: cross tick boundary
            if liquidity_net != 0 {
                let net = if zero_for_one { -liquidity_net } else { liquidity_net };
                current_liquidity = liquidity_math::add_delta(current_liquidity, net).unwrap_or(0);
            }
            current_sqrt_price = sqrt_price_target;
            current_tick = if zero_for_one { tick_next - 1 } else { tick_next };

            if current_liquidity == 0 {
                return Ok(None);
            }
        }

        Ok(None)
    }

    /// Get the lower tick boundary for a given tick
    fn get_lower_tick_boundary(&self, tick: i32) -> i32 {
        let tick_spacing = self.tick_spacing as i32;
        if tick >= 0 {
            (tick / tick_spacing) * tick_spacing
        } else {
            ((tick - tick_spacing + 1) / tick_spacing) * tick_spacing
        }
    }

    /// Get the upper tick boundary for a given tick
    fn get_upper_tick_boundary(&self, tick: i32) -> i32 {
        self.get_lower_tick_boundary(tick) + self.tick_spacing as i32
    }

}

// TODO: rewrite tests using LiteSVM/Mollusk

#[cfg(test)]
mod tests {}
