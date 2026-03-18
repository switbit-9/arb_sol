pub mod error;
pub mod libraries;
pub mod states;

use self::error::ErrorCode;
use self::libraries::{full_math::MulDiv, liquidity_math, swap_math, tick_math};
use self::states::{
    AmmConfigSimple, PoolStateSimple, TickArrayState, FEE_RATE_DENOMINATOR_VALUE, TICK_ARRAY_SIZE,
};
use crate::programs::{PoolKind, ProgramMeta};
use crate::utils::token::{apply_transfer_fee, apply_transfer_inverse_fee, MintFee};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    account_info::AccountInfo,
    instruction::{AccountMeta, Instruction},
    program::invoke_signed_unchecked,
    pubkey::Pubkey,
};

    /// Raydium CLMM Program ID
pub const PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK");
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
pub const MIN_ACCOUNTS: usize = 10;
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
            return Err(error!(crate::programs::SolarBError::InsufficientFunds));
        }
        let sqrt_p = self.sqrt_price_x64 as f64 / (1u128 << 64) as f64;
        let l = self.liquidity as f64;
        let v_0 = (l / sqrt_p).min(u64::MAX as f64) as u64;
        let v_1 = (l * sqrt_p).min(u64::MAX as f64) as u64;
        Ok((v_0, v_1))
    }

    fn swap_base_in<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
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

    fn swap_base_out<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
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

    fn invoke_swap_base_in<'a>(
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
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let amm_config = &accounts[self.dyn_start + D_AMM_CONFIG];
        let vault_0 = &accounts[self.dyn_start + D_VAULT_0];
        let vault_1 = &accounts[self.dyn_start + D_VAULT_1];
        let observation = &accounts[self.dyn_start + D_OBSERVATION];
        let bitmap_extension = &accounts[self.dyn_start + D_BITMAP_EXT];
        let token_program_spl = &accounts[1];
        let token_program_2022: &AccountInfo<'a> = &accounts[2];
        let memo: &AccountInfo<'a> = &accounts[3];

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

        // Build swap instruction
        let mut metas = vec![
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*amm_config.key, false),
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new(*user_input_account.key, false),
            AccountMeta::new(*user_output_account.key, false),
            AccountMeta::new(*input_vault.key, false),
            AccountMeta::new(*output_vault.key, false),
            AccountMeta::new(*observation.key, false),
            AccountMeta::new_readonly(*token_program_spl.key, false),
            AccountMeta::new_readonly(*token_program_2022.key, false),
            AccountMeta::new_readonly(*memo.key, false),
            AccountMeta::new_readonly(*input_mint_acc.key, false),
            AccountMeta::new_readonly(*output_mint_acc.key, false),
        ];

        if *bitmap_extension.key != PROGRAM_ID {
            metas.push(AccountMeta::new(*bitmap_extension.key, false));
        }

        let (ta_from, ta_to) = Self::tick_array_range(self.dyn_start, zero_for_one);
        for i in ta_from..ta_to {
            metas.push(AccountMeta::new(*accounts[i].key, false));
        }

        let mut data = [0u8; 41];
        data[..8].copy_from_slice(&SWAP_V2_DISC);
        data[8..16].copy_from_slice(&amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&min_amount_out.unwrap_or(0).to_le_bytes());
        // data[24..40] already zeroed: sqrt_price_limit_x64 = 0
        data[40] = 1; // is_base_input = true (exact input)

        let swap_ix = Instruction {
            program_id: PROGRAM_ID,
            accounts: metas,
            data: data.to_vec(),
        };

        // Stack-allocated account infos — max 16 entries (13 base + 1 bitmap + 2 tick arrays)
        let mut accs: [AccountInfo<'a>; 16] = unsafe { core::mem::zeroed() };
        let mut ai = 0usize;
        macro_rules! push_acc {
            ($e:expr) => { accs[ai] = unsafe { std::mem::transmute($e) }; ai += 1; };
        }
        push_acc!(payer.clone());
        push_acc!(amm_config.clone());
        push_acc!(pool_id.clone());
        push_acc!(user_input_account.clone());
        push_acc!(user_output_account.clone());
        push_acc!(input_vault.clone());
        push_acc!(output_vault.clone());
        push_acc!(observation.clone());
        push_acc!(token_program_spl.clone());
        push_acc!(token_program_2022.clone());
        push_acc!(memo.clone());
        push_acc!(input_mint_acc.clone());
        push_acc!(output_mint_acc.clone());

        if *bitmap_extension.key != PROGRAM_ID {
            push_acc!(bitmap_extension.clone());
        }

        for i in ta_from..ta_to {
            push_acc!(accounts[i].clone());
        }

        invoke_signed_unchecked(&swap_ix, &accs[..ai], &[])?;

        Ok(())
    }

    fn invoke_swap_base_out<'a>(
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
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let amm_config = &accounts[self.dyn_start + D_AMM_CONFIG];
        let vault_0 = &accounts[self.dyn_start + D_VAULT_0];
        let vault_1 = &accounts[self.dyn_start + D_VAULT_1];
        let observation = &accounts[self.dyn_start + D_OBSERVATION];
        let bitmap_extension = &accounts[self.dyn_start + D_BITMAP_EXT];
        let token_program_spl = &accounts[1];
        let token_program_2022 = &accounts[2];
        let memo: &AccountInfo<'a> = &accounts[3];

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

        let mut metas = vec![
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*amm_config.key, false),
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new(*user_input_account.key, false),
            AccountMeta::new(*user_output_account.key, false),
            AccountMeta::new(*input_vault.key, false),
            AccountMeta::new(*output_vault.key, false),
            AccountMeta::new(*observation.key, false),
            AccountMeta::new_readonly(*token_program_spl.key, false),
            AccountMeta::new_readonly(*token_program_2022.key, false),
            AccountMeta::new_readonly(*memo.key, false),
            AccountMeta::new_readonly(*input_mint_acc.key, false),
            AccountMeta::new_readonly(*output_mint_acc.key, false),
        ];

        if *bitmap_extension.key != PROGRAM_ID {
            metas.push(AccountMeta::new(*bitmap_extension.key, false));
        }

        // Add tick array accounts for this swap direction
        let (ta_from, ta_to) = Self::tick_array_range(self.dyn_start, zero_for_one);
        for i in ta_from..ta_to {
            metas.push(AccountMeta::new(*accounts[i].key, false));
        }

        let mut data = [0u8; 41];
        data[..8].copy_from_slice(&SWAP_V2_DISC);
        data[8..16].copy_from_slice(&amount_out.unwrap_or(0).to_le_bytes());
        data[16..24].copy_from_slice(&max_amount_in.to_le_bytes());
        // data[24..40] already zeroed: sqrt_price_limit_x64 = 0
        data[40] = 0; // is_base_input = false (exact output)

        let swap_ix = Instruction {
            program_id: PROGRAM_ID,
            accounts: metas,
            data: data.to_vec(),
        };

        // Stack-allocated account infos — max 16 entries (13 base + 1 bitmap + 2 tick arrays)
        let mut accs: [AccountInfo<'a>; 16] = unsafe { core::mem::zeroed() };
        let mut ai = 0usize;
        macro_rules! push_acc {
            ($e:expr) => { accs[ai] = unsafe { std::mem::transmute($e) }; ai += 1; };
        }
        push_acc!(payer.clone());
        push_acc!(amm_config.clone());
        push_acc!(pool_id.clone());
        push_acc!(user_input_account.clone());
        push_acc!(user_output_account.clone());
        push_acc!(input_vault.clone());
        push_acc!(output_vault.clone());
        push_acc!(observation.clone());
        push_acc!(token_program_spl.clone());
        push_acc!(token_program_2022.clone());
        push_acc!(memo.clone());
        push_acc!(input_mint_acc.clone());
        push_acc!(output_mint_acc.clone());

        if *bitmap_extension.key != PROGRAM_ID {
            push_acc!(bitmap_extension.clone());
        }

        for i in ta_from..ta_to {
            push_acc!(accounts[i].clone());
        }

        invoke_signed_unchecked(&swap_ix, &accs[..ai], &[])?;

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

    fn has_output_liquidity(&self, _input_mint: Pubkey) -> bool {
        self.liquidity > 0
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
            .map_err(|_| error!(crate::programs::SolarBError::InsufficientAccounts))?;
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

    /// Per-tick-range segment data for the analytical multi-bin walker.
    /// Mirrors DLMM `get_bin_segment`, treating each tick range as a linear segment.
    fn get_bin_segment<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        bin_offset: i32,
    ) -> Result<Option<(f64, u64, f64)>> {
        self.get_tick_segment_impl(accounts, input_mint, bin_offset)
    }

    #[cfg(any(test, feature = "debug"))]
    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        msg!("=== Raydium CLMM ===");
        msg!("S0 program_id: {}", accounts[self.static_base + S_PROGRAM_ID].key);
        msg!("D0 pool: {}", accounts[self.dyn_start + D_POOL].key);
        msg!("D1 vault_0: {}", accounts[self.dyn_start + D_VAULT_0].key);
        msg!("D2 vault_1: {}", accounts[self.dyn_start + D_VAULT_1].key);
        msg!("D3 amm_config: {}", accounts[self.dyn_start + D_AMM_CONFIG].key);
        msg!("D4 observation: {}", accounts[self.dyn_start + D_OBSERVATION].key);
        msg!("D5 bitmap_ext: {}", accounts[self.dyn_start + D_BITMAP_EXT].key);
        msg!("D6 tick_buy_0: {}", accounts[self.dyn_start + D_TICK_BUY_0].key);
        msg!("D7 tick_buy_1: {}", accounts[self.dyn_start + D_TICK_BUY_1].key);
        msg!("D8 tick_sell_0: {}", accounts[self.dyn_start + D_TICK_SELL_0].key);
        msg!("D9 tick_sell_1: {}", accounts[self.dyn_start + D_TICK_SELL_1].key);
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



    pub fn new<'a>(
        accounts: &[AccountInfo<'a>],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
    ) -> Result<Self> {
        let pool_id = &accounts[dyn_start + D_POOL];

        // Read only the fields we need directly from pool account bytes
        let d = pool_id.try_borrow_data()?;

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
        let cfg_data = accounts[dyn_start + D_AMM_CONFIG].try_borrow_data()?;
        let trade_fee_rate_raw = u32::from_le_bytes(cfg_data[CFG_TRADE_FEE..CFG_TRADE_FEE + 4].try_into().unwrap());
        let protocol_fee_rate = u32::from_le_bytes(cfg_data[CFG_PROTOCOL_FEE..CFG_PROTOCOL_FEE + 4].try_into().unwrap());
        let fund_fee_rate = u32::from_le_bytes(cfg_data[CFG_FUND_FEE..CFG_FUND_FEE + 4].try_into().unwrap());
        drop(cfg_data);

        let fee_rate = trade_fee_rate_raw as f64 / FEE_RATE_DENOMINATOR_VALUE as f64;
        let price = sqrt_price_to_f64(sqrt_price_x64);

        debug_eprintln!("RaydiumCLMM: pool_id {} , price {}, inverse_price {}, fee_rate {}", *pool_id.key, price, 1.0 / price, fee_rate);

        let instance = RaydiumCLMM {
            pool_id: *pool_id.key,
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
    pub fn prepare_for_execution<'a>(
        &mut self,
        _accounts: &[AccountInfo<'a>],
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
            if let Ok(data) = accounts[i].try_borrow_data() {
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
            // Ref dropped here — pointer stays valid (account data is pinned for the instruction)
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
        let pool_id_bytes = self.pool_id.to_bytes();
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
        let pool_id_bytes = self.pool_id.to_bytes();

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
    fn get_tick_segment_impl<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        bin_offset: i32,
    ) -> Result<Option<(f64, u64, f64)>> {
        if self.liquidity == 0 {
            return Ok(None);
        }
        let zero_for_one = input_mint == self.base_token_pk;
        let fee_factor = self.fee_factor.0;

        let pool_id_bytes = self.pool_id.to_bytes();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::token::MintFee;
    use solana_client::nonblocking::rpc_client::RpcClient;
    use solana_sdk::pubkey::Pubkey as SdkPubkey;

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

    fn to_sdk(key: Pubkey) -> SdkPubkey {
        SdkPubkey::try_from(key.to_bytes().as_ref()).unwrap()
    }

    fn get_rpc_client() -> RpcClient {
        let api_key = "f230200b-f911-43c1-a242-4e7b066d0993";
        RpcClient::new(format!("https://mainnet.helius-rpc.com/?api-key={}", api_key))
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

    async fn build_from_pool_id(
        pool_id: Pubkey,
    ) -> (RaydiumCLMM, Vec<AccountInfo<'static>>, Clock) {
        let rpc_client = get_rpc_client();

        // Fetch pool account and parse PoolStateSimple
        let pool_account = rpc_client.get_account(&to_sdk(pool_id)).await
            .unwrap_or_else(|e| panic!("Failed to fetch pool {}: {}", pool_id, e));
        let pool_state_size = std::mem::size_of::<PoolStateSimple>();
        let pool: PoolStateSimple =
            bytemuck::pod_read_unaligned(&pool_account.data[8..8 + pool_state_size]);

        eprintln!("Pool: {}", pool_id);
        eprintln!("  token_0 (base): {}", pool.token_mint_0);
        eprintln!("  token_1 (quote): {}", pool.token_mint_1);
        let tick_current = pool.tick_current;
        let tick_spacing = pool.tick_spacing;
        eprintln!("  tick_current: {}, tick_spacing: {}", tick_current, tick_spacing);

        if pool.liquidity == 0 {
            panic!("Pool has no liquidity");
        }

        // Fetch AMM config to get fee rates
        let amm_config_raw = rpc_client.get_account(&to_sdk(pool.amm_config)).await
            .expect("Failed to fetch AMM config");
        let amm_config: AmmConfigSimple = AmmConfigSimple::try_from_bytes(&amm_config_raw.data)
            .expect("Failed to parse AmmConfig");

        // Fetch vault and observation accounts
        let vault_0_account = rpc_client.get_account(&to_sdk(pool.token_vault_0)).await
            .expect("Failed to fetch vault 0");
        let vault_1_account = rpc_client.get_account(&to_sdk(pool.token_vault_1)).await
            .expect("Failed to fetch vault 1");
        let observation_raw = rpc_client.get_account(&to_sdk(pool.observation_key)).await
            .expect("Failed to fetch observation");

        // Derive tick array PDAs
        let ticks_in_array = TICK_ARRAY_SIZE * i32::from(pool.tick_spacing);
        let current_start_index =
            TickArrayState::get_array_start_index(pool.tick_current, pool.tick_spacing);

        let tick_array_start_indices = [
            current_start_index,                        // buy_0
            current_start_index - ticks_in_array,       // buy_1
            current_start_index,                        // sell_0
            current_start_index + ticks_in_array,       // sell_1
        ];

        // Minimum valid tick array size: discriminator(8) + pool_id(32) + start_tick_index(4) + ticks(60 * 168)
        let min_ta_len = 8 + 32 + 4 + 60 * 168;

        let mut tick_keys_and_accounts = Vec::new();
        for &start_index in &tick_array_start_indices {
            let (tick_array_pda, _) = Pubkey::find_program_address(
                &[
                    b"tick_array",
                    pool_id.as_ref(),
                    &start_index.to_be_bytes(),
                ],
                &PROGRAM_ID,
            );
            eprintln!("  tick_array start_index {}: {}", start_index, tick_array_pda);
            match rpc_client.get_account(&to_sdk(tick_array_pda)).await {
                Ok(acc) => tick_keys_and_accounts.push((tick_array_pda, Some(acc))),
                Err(_) => {
                    eprintln!("    (not found, using empty mock)");
                    tick_keys_and_accounts.push((tick_array_pda, None));
                }
            }
        }

        let (buy_0_key, buy_0_acc) = tick_keys_and_accounts.remove(0);
        let (buy_1_key, buy_1_acc) = tick_keys_and_accounts.remove(0);
        let (sell_0_key, sell_0_acc) = tick_keys_and_accounts.remove(0);
        let (sell_1_key, sell_1_acc) = tick_keys_and_accounts.remove(0);

        // Build AccountInfo array
        let pool_id_info = account_to_account_info(pool_id, pool_account);
        let vault_0_info = account_to_account_info(pool.token_vault_0, vault_0_account);
        let vault_1_info = account_to_account_info(pool.token_vault_1, vault_1_account);
        let amm_config_info = account_to_account_info(pool.amm_config, amm_config_raw);
        let observation_info = account_to_account_info(pool.observation_key, observation_raw);

        let program_id_info = create_mock_account_info_with_data(
            PROGRAM_ID, anchor_lang::solana_program::system_program::id(), None,
        );
        let bitmap_ext_info = create_mock_account_info_with_data(
            PROGRAM_ID, anchor_lang::solana_program::system_program::id(), None,
        );

        let make_tick_info = |key: Pubkey, acc: Option<solana_sdk::account::Account>| -> AccountInfo<'static> {
            match acc {
                Some(a) => account_to_account_info(key, a),
                None => create_mock_account_info_with_data(
                    key, anchor_lang::solana_program::system_program::id(),
                    Some(vec![0u8; min_ta_len]),
                ),
            }
        };
        let tick_buy_0 = make_tick_info(buy_0_key, buy_0_acc);
        let tick_buy_1 = make_tick_info(buy_1_key, buy_1_acc);
        let tick_sell_0 = make_tick_info(sell_0_key, sell_0_acc);
        let tick_sell_1 = make_tick_info(sell_1_key, sell_1_acc);

        // Layout:
        // Static (static_base=0): [program_id]
        // Dynamic (dyn_start=1): [pool, vault_0, vault_1, amm_config, observation, bitmap_ext, tick_buy_0, tick_buy_1, tick_sell_0, tick_sell_1]
        let accounts = vec![
            program_id_info,         // S0
            pool_id_info,            // D0
            vault_0_info,            // D1
            vault_1_info,            // D2
            amm_config_info,         // D3
            observation_info,        // D4
            bitmap_ext_info,         // D5
            tick_buy_0,              // D6
            tick_buy_1,              // D7
            tick_sell_0,             // D8
            tick_sell_1,             // D9
        ];

        let static_base: usize = 0;
        let dyn_start: usize = 1;
        let dyn_end: usize = accounts.len();

        let mut clmm = RaydiumCLMM::new(&accounts, static_base, dyn_start, dyn_end)
            .expect("RaydiumCLMM::new failed");

        clmm.prepare_for_execution(&accounts);

        let clock = get_clock_from_rpc(&rpc_client).await;

        eprintln!("  price: {}", clmm.price);
        eprintln!("  trade_fee_rate: {}", clmm.trade_fee_rate);

        (clmm, accounts, clock)
    }

    #[tokio::test]
    async fn test_clmm_round_trip() {
        let pool_id = Pubkey::from_str_const("AFT2PaCYfy93g47aTyG3wKu4KDEg2YMhUmwbdPDdcmCG");
        let (mut clmm, accounts, clock) = build_from_pool_id(pool_id).await;

        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let no_fee = MintFee::ZERO;

        // 1. Print all account pubkeys
        eprintln!("\n=== Account Pubkeys ===");
        eprintln!("base_mint        : {}", clmm.base_token_pk);
        eprintln!("quote_mint       : {}", clmm.quote_token_pk);
        eprintln!("pool_id          : {}", accounts[1 + D_POOL].key);
        eprintln!("vault_0          : {}", accounts[1 + D_VAULT_0].key);
        eprintln!("vault_1          : {}", accounts[1 + D_VAULT_1].key);
        eprintln!("amm_config       : {}", accounts[1 + D_AMM_CONFIG].key);
        eprintln!("observation      : {}", accounts[1 + D_OBSERVATION].key);
        eprintln!("bitmap_ext       : {}", accounts[1 + D_BITMAP_EXT].key);
        eprintln!("program_id       : {}", accounts[S_PROGRAM_ID].key);

        // 2. Prices
        let (price, inverse_price) = clmm.get_prices().unwrap();
        eprintln!("\n=== Prices ===");
        eprintln!("price            : {}", price);
        eprintln!("inverse_price    : {}", inverse_price);

        // 3. Fees
        let (fee_factor, fee_factor_2) = clmm.get_fee_factor().unwrap();
        eprintln!("\n=== Fees ===");
        eprintln!("trade_fee_rate   : {}", clmm.trade_fee_rate);
        eprintln!("fee_factor       : {}", fee_factor);
        eprintln!("fee_factor_2     : {}", fee_factor_2);

        // 4. Max amounts
        eprintln!("\n=== After prepare_for_execution ===");
        eprintln!("buy_max_in       : {}", clmm.buy_max_in);
        eprintln!("buy_max_out      : {}", clmm.buy_max_out);
        eprintln!("sell_max_in      : {}", clmm.sell_max_in);
        eprintln!("sell_max_out     : {}", clmm.sell_max_out);

        // 5. Round-trip with start_amount = 1 WSOL
        let start_amount: u64 = 1_000_000_000;

        let other_mint = if clmm.base_token_pk == sol_mint {
            clmm.quote_token_pk
        } else {
            clmm.base_token_pk
        };

        let rpc = get_rpc_client();
        let other_mint_account = rpc.get_account(
            &SdkPubkey::try_from(other_mint.to_bytes().as_ref()).unwrap()
        ).await.unwrap();
        let token_decimals = other_mint_account.data[44] as i32;
        let sol_div = 10f64.powi(9);
        let tok_div = 10f64.powi(token_decimals);

        // Direction 1: SOL -> TOKEN -> SOL
        eprintln!("\n=== Direction 1: SOL -> TOKEN -> SOL ===");
        let token_out = clmm.swap_base_in(
            &accounts, sol_mint, start_amount, no_fee, no_fee, &clock,
        ).unwrap();
        let max_sol_in = clmm.swap_base_out(
            &accounts, other_mint, token_out, no_fee, no_fee, &clock,
        ).unwrap();
        eprintln!("AMOUNT_IN {} -> AMOUNT_OUT {} -> MAX_AMOUNT_IN {}", start_amount as f64 / sol_div, token_out as f64 / tok_div, max_sol_in as f64 / sol_div);

        // Direction 2: TOKEN -> SOL -> TOKEN
        eprintln!("\n=== Direction 2: TOKEN -> SOL -> TOKEN ===");
        let sol_out = clmm.swap_base_in(
            &accounts, other_mint, token_out, no_fee, no_fee, &clock,
        ).unwrap();
        let max_token_in = clmm.swap_base_out(
            &accounts, sol_mint, sol_out, no_fee, no_fee, &clock,
        ).unwrap();
        eprintln!("AMOUNT_IN {} -> AMOUNT_OUT {} -> MAX_AMOUNT_IN {}", token_out as f64 / tok_div, sol_out as f64 / sol_div, max_token_in as f64 / tok_div);
    }
}