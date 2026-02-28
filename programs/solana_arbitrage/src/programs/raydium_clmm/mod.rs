pub mod error;
pub mod libraries;
pub mod states;

use self::error::ErrorCode;
use self::libraries::{full_math::MulDiv, liquidity_math, swap_math, tick_math};
use self::states::{
    AmmConfigSimple, PoolStateSimple, TickArrayState, FEE_RATE_DENOMINATOR_VALUE, TICK_ARRAY_SIZE,
};
use crate::programs::ProgramMeta;
use crate::utils::token::{get_transfer_fee, get_transfer_inverse_fee};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    account_info::AccountInfo,
    instruction::{AccountMeta, Instruction},
    program::invoke,
    pubkey::Pubkey,
};
use std::marker::PhantomData;

/// Calculate price from sqrt_price_x64
fn sqrt_price_to_price(sqrt_price_x64: u128) -> f64 {
    let sqrt_price = sqrt_price_x64 as f64 / (1u128 << 64) as f64;
    let raw_price = sqrt_price * sqrt_price;
    raw_price
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
pub struct RaydiumCLMM<'info> {
    pub pool_id: Pubkey,
    pub amm_config_key: Pubkey,
    pub base_token_pk: Pubkey,
    pub quote_token_pk: Pubkey,
    pub token_vault_0: Pubkey,
    pub token_vault_1: Pubkey,
    pub observation_key: Pubkey,
    pub sqrt_price_x64: u128,
    pub tick_current: i32,
    pub liquidity: u128,
    pub tick_spacing: u16,
    pub mint_decimals_0: u8,
    pub mint_decimals_1: u8,
    pub trade_fee_rate: u32,
    pub protocol_fee_rate: u32,
    pub fund_fee_rate: u32,
    /// Effective fee rate as f64 (0.0 - 1.0), e.g. 0.0025 = 0.25%
    pub fee_rate: f64,
    pub price: f64,
    pub inverse_price: f64,
    pub start_index: usize,
    pub end_index: usize,
    pub phantom: PhantomData<&'info ()>,
}

impl<'info> ProgramMeta for RaydiumCLMM<'info> {
    fn get_id(&self) -> &Pubkey {
        &Self::PROGRAM_ID
    }

    fn get_pool_id(&self) -> &Pubkey {
        &self.pool_id
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        Ok((self.price, self.inverse_price))
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (&self.base_token_pk, &self.quote_token_pk)
    }

    fn name(&self) -> &'static str { "RaydiumCLMM" }

    fn get_fee_factor(&self) -> Result<(f64, f64)> { let f = 1.0 - self.fee_rate; Ok((f, f)) }

    fn get_liquidity(&self) -> Result<u128> { Ok(self.liquidity) }

    fn get_sqrt_price(&self) -> Result<u128> { Ok(self.sqrt_price_x64) }

    fn swap_base_in<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        _clock: &Clock,
    ) -> Result<u64> {
        let token_0_account = &accounts[self.start_index + Self::TOKEN_0_IDX];
        let token_1_account = &accounts[self.start_index + Self::TOKEN_1_IDX];

        let zero_for_one = input_mint == self.base_token_pk;

        let (input_token_account, output_token_account) = if zero_for_one {
            (token_0_account, token_1_account)
        } else {
            (token_1_account, token_0_account)
        };

        // Account for transfer fees
        let transfer_fee = get_transfer_fee(input_token_account, amount_in)?;
        let actual_amount_in = amount_in.saturating_sub(transfer_fee);

        // Calculate swap output - tick arrays parsed on-the-fly from account data (zero heap)
        let (_, amount_out) =
            self.calculate_swap_with_tick_arrays(accounts, actual_amount_in, zero_for_one, true)?;

        // Account for output transfer fees
        let out_transfer_fee = get_transfer_fee(output_token_account, amount_out)?;
        let final_amount_out = amount_out.saturating_sub(out_transfer_fee);

        Ok(final_amount_out)
    }

    fn swap_base_out<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        _clock: &Clock,
    ) -> Result<u64> {
        let token_0_account = &accounts[self.start_index + Self::TOKEN_0_IDX];
        let token_1_account = &accounts[self.start_index + Self::TOKEN_1_IDX];

        let zero_for_one = output_mint == self.quote_token_pk;

        let (input_token_account, output_token_account) = if zero_for_one {
            (token_0_account, token_1_account)
        } else {
            (token_1_account, token_0_account)
        };

        // Account for output transfer fees
        let out_transfer_fee = get_transfer_inverse_fee(output_token_account, amount_out)?;
        let amount_out_with_fee = amount_out
            .checked_add(out_transfer_fee)
            .ok_or(ErrorCode::AmountOverflow)?;

        // Calculate required input - tick arrays parsed on-the-fly from account data (zero heap)
        let (_, amount_in) =
            self.calculate_swap_with_tick_arrays(accounts, amount_out_with_fee, zero_for_one, false)?;

        // Account for input transfer fees
        let in_transfer_fee = get_transfer_inverse_fee(input_token_account, amount_in)?;
        let final_amount_in = amount_in
            .checked_add(in_transfer_fee)
            .ok_or(ErrorCode::AmountOverflow)?;

        Ok(final_amount_in)
    }

    fn invoke_swap_base_in<'a>(
        &self,
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
        let pool_id = &accounts[self.start_index + Self::POOL_ID_IDX];
        let amm_config = &accounts[self.start_index + Self::AMM_CONFIG_IDX];
        let vault_0 = &accounts[self.start_index + Self::VAULT_0_IDX];
        let vault_1 = &accounts[self.start_index + Self::VAULT_1_IDX];
        let observation = &accounts[self.start_index + Self::OBSERVATION_IDX];
        let bitmap_extension = &accounts[self.start_index + Self::BITMAP_EXTENSION_IDX];
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
                user_mint_2_token_account,
                user_mint_1_token_account,
                vault_1,
                vault_0,
                mint_2_account,
                mint_1_account,
            )
        };

        // Build swap instruction
        let mut metas = Vec::with_capacity(20);
        metas.push(AccountMeta::new(*payer.key, true));
        metas.push(AccountMeta::new_readonly(self.amm_config_key, false));
        metas.push(AccountMeta::new(*pool_id.key, false));
        metas.push(AccountMeta::new(*user_input_account.key, false));
        metas.push(AccountMeta::new(*user_output_account.key, false));
        metas.push(AccountMeta::new(*input_vault.key, false));
        metas.push(AccountMeta::new(*output_vault.key, false));
        metas.push(AccountMeta::new(*observation.key, false));
        metas.push(AccountMeta::new_readonly(*token_program_spl.key, false));
        metas.push(AccountMeta::new_readonly(*token_program_2022.key, false));
        metas.push(AccountMeta::new_readonly(*memo.key, false));
        metas.push(AccountMeta::new_readonly(*input_mint_acc.key, false));
        metas.push(AccountMeta::new_readonly(*output_mint_acc.key, false));

        if *bitmap_extension.key != Self::PROGRAM_ID {
            metas.push(AccountMeta::new(*bitmap_extension.key, false));
        }

        let (ta_from, ta_to) = Self::tick_array_range(self.start_index, zero_for_one);
        for i in ta_from..ta_to {
            metas.push(AccountMeta::new(*accounts[i].key, false));
        }

        let mut data: Vec<u8> = vec![43, 4, 237, 11, 26, 201, 30, 98]; // swap_v2 discriminator
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.unwrap_or(0).to_le_bytes());
        data.extend_from_slice(&0u128.to_le_bytes()); // sqrt_price_limit_x64 = 0
        data.push(if zero_for_one { 1 } else { 0 }); // is_base_input = true

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data,
        };

        let mut accounts_vec: Vec<AccountInfo<'a>> = Vec::with_capacity(20);
        accounts_vec.push(payer.clone());
        accounts_vec.push(amm_config.clone());
        accounts_vec.push(pool_id.clone());
        accounts_vec.push(user_input_account.clone());
        accounts_vec.push(user_output_account.clone());
        accounts_vec.push(input_vault.clone());
        accounts_vec.push(output_vault.clone());
        accounts_vec.push(observation.clone());
        accounts_vec.push(token_program_spl.clone());
        accounts_vec.push(token_program_2022.clone());
        accounts_vec.push(memo.clone());
        accounts_vec.push(input_mint_acc.clone());
        accounts_vec.push(output_mint_acc.clone());

        if *bitmap_extension.key != Self::PROGRAM_ID {
            accounts_vec.push(bitmap_extension.clone());
        }

        for i in ta_from..ta_to {
            accounts_vec.push(accounts[i].clone());
        }

        unsafe {
            let accounts_slice: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts_slice)?;
        }

        Ok(())
    }

    fn invoke_swap_base_out<'a>(
        &self,
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
        let pool_id = &accounts[self.start_index + Self::POOL_ID_IDX];
        let amm_config = &accounts[self.start_index + Self::AMM_CONFIG_IDX];
        let vault_0 = &accounts[self.start_index + Self::VAULT_0_IDX];
        let vault_1 = &accounts[self.start_index + Self::VAULT_1_IDX];
        let observation = &accounts[self.start_index + Self::OBSERVATION_IDX];
        let bitmap_extension = &accounts[self.start_index + Self::BITMAP_EXTENSION_IDX];
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
                user_mint_2_token_account,
                user_mint_1_token_account,
                vault_1,
                vault_0,
                mint_2_account,
                mint_1_account,
            )
        };

        let mut metas = vec![
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(self.amm_config_key, false),
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

        if *bitmap_extension.key != Self::PROGRAM_ID {
            metas.push(AccountMeta::new(*bitmap_extension.key, false));
        }

        // Add tick array accounts for this swap direction
        let (ta_from, ta_to) = Self::tick_array_range(self.start_index, zero_for_one);
        for i in ta_from..ta_to {
            metas.push(AccountMeta::new(*accounts[i].key, false));
        }

        let mut data = vec![43, 4, 237, 11, 26, 201, 30, 98]; // swap_v2 discriminator
        data.extend_from_slice(&amount_out.unwrap_or(0).to_le_bytes());
        data.extend_from_slice(&max_amount_in.to_le_bytes());
        data.extend_from_slice(&0u128.to_le_bytes()); // sqrt_price_limit_x64 = 0
        data.push(if zero_for_one { 0 } else { 1 }); // is_base_input = false

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data,
        };

        let mut accounts_vec: Vec<AccountInfo<'a>> = Vec::with_capacity(20);
        accounts_vec.push(payer.clone());
        accounts_vec.push(amm_config.clone());
        accounts_vec.push(pool_id.clone());
        accounts_vec.push(user_input_account.clone());
        accounts_vec.push(user_output_account.clone());
        accounts_vec.push(input_vault.clone());
        accounts_vec.push(output_vault.clone());
        accounts_vec.push(observation.clone());
        accounts_vec.push(token_program_spl.clone());
        accounts_vec.push(token_program_2022.clone());
        accounts_vec.push(memo.clone());
        accounts_vec.push(input_mint_acc.clone());
        accounts_vec.push(output_mint_acc.clone());

        if *bitmap_extension.key != Self::PROGRAM_ID {
            accounts_vec.push(bitmap_extension.clone());
        }

        for i in ta_from..ta_to {
            accounts_vec.push(accounts[i].clone());
        }

        unsafe {
            let accounts_slice: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts_slice)?;
        }

        Ok(())
    }

    fn get_max_amounts_in_out<'a>(&self, accounts: &[AccountInfo<'a>], input_mint: Pubkey) -> Result<(u64, u64)> {
        let zero_for_one = input_mint == self.base_token_pk;
        // Simulate swap with u64::MAX input to exhaust all available liquidity across tick arrays
        let (max_in, max_out) =
            self.calculate_swap_with_tick_arrays(accounts, u64::MAX, zero_for_one, true)?;
        Ok((max_in, max_out))
    }

    fn get_max_amount_in<'a>(&self, accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        let zero_for_one = mint == self.base_token_pk;
        let (max_in, _) =
            self.calculate_swap_with_tick_arrays(accounts, u64::MAX, zero_for_one, true)?;
        Ok(max_in)
    }

    fn get_max_amount_out<'a>(&self, accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        let zero_for_one = mint == self.base_token_pk;
        let (_, max_out) =
            self.calculate_swap_with_tick_arrays(accounts, u64::MAX, zero_for_one, true)?;
        Ok(max_out)
    }

    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        msg!("=== Raydium CLMM ===");
        msg!("0 program_id: {}", accounts[self.start_index + Self::PROGRAM_ID_IDX].key);
        msg!("1 pool_id: {}", accounts[self.start_index + Self::POOL_ID_IDX].key);
        msg!("2 vault_0: {}", accounts[self.start_index + Self::VAULT_0_IDX].key);
        msg!("3 vault_1: {}", accounts[self.start_index + Self::VAULT_1_IDX].key);
        msg!("4 token_0: {}", accounts[self.start_index + Self::TOKEN_0_IDX].key);
        msg!("5 token_1: {}", accounts[self.start_index + Self::TOKEN_1_IDX].key);
        msg!("6 amm_config: {}", accounts[self.start_index + Self::AMM_CONFIG_IDX].key);
        msg!("7 observation: {}", accounts[self.start_index + Self::OBSERVATION_IDX].key);
        msg!("8 memo: {}", accounts[3].key);
        msg!("8 bitmap_extension: {}", accounts[self.start_index + Self::BITMAP_EXTENSION_IDX].key);
        msg!("9 tick_buy_1: {}", accounts[self.start_index + Self::TICK_BUY_1_IDX].key);
        msg!("10 tick_buy_2: {}", accounts[self.start_index + Self::TICK_BUY_2_IDX].key);
        msg!("11 tick_sell_1: {}", accounts[self.start_index + Self::TICK_SELL_1_IDX].key);
        msg!("12 tick_sell_2: {}", accounts[self.start_index + Self::TICK_SELL_2_IDX].key);
        Ok(())
    }
}

impl<'info> RaydiumCLMM<'info> {
    /// Raydium CLMM Program ID
    pub const PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK");

    // Account indices
    pub const PROGRAM_ID_IDX: usize = 0;
    pub const POOL_ID_IDX: usize = 1;
    pub const VAULT_0_IDX: usize = 2;
    pub const VAULT_1_IDX: usize = 3;
    pub const TOKEN_0_IDX: usize = 4;
    pub const TOKEN_1_IDX: usize = 5;
    pub const AMM_CONFIG_IDX: usize = 6;
    pub const OBSERVATION_IDX: usize = 7;
    pub const BITMAP_EXTENSION_IDX: usize = 8;
    // Buy direction tick arrays (zero_for_one: token_0 -> token_1)
    pub const TICK_BUY_1_IDX: usize = 9;
    pub const TICK_BUY_2_IDX: usize = 10;
    // Sell direction tick arrays (one_for_zero: token_1 -> token_0)
    pub const TICK_SELL_1_IDX: usize = 11;
    pub const TICK_SELL_2_IDX: usize = 12;
    pub const ACCOUNT_COUNT: usize = 13;

    /// Returns (from, to) absolute account indices for the tick arrays of a given swap direction.
    #[inline(always)]
    fn tick_array_range(start_index: usize, zero_for_one: bool) -> (usize, usize) {
        if zero_for_one {
            (start_index + Self::TICK_BUY_1_IDX, start_index + Self::TICK_BUY_2_IDX + 1)
        } else {
            (start_index + Self::TICK_SELL_1_IDX, start_index + Self::TICK_SELL_2_IDX + 1)
        }
    }

    pub fn new(
        accounts: &[AccountInfo<'info>],
        start_index: usize,
        end_index: usize,
    ) -> Result<Self> {
        let pool_id = &accounts[start_index + Self::POOL_ID_IDX];
        let amm_config = &accounts[start_index + Self::AMM_CONFIG_IDX];

        // Parse pool state
        let pool_data = pool_id.try_borrow_data()?;
        let pool_state_size = std::mem::size_of::<PoolStateSimple>();
        if pool_data.len() < 8 + pool_state_size {
            return Err(ErrorCode::InvalidTickArray.into());
        }
        let pool: PoolStateSimple =
            bytemuck::pod_read_unaligned(&pool_data[8..8 + pool_state_size]);

        // Check if swap is enabled
        if !pool.swap_enabled() {
            return Err(ErrorCode::SwapNotEnabled.into());
        }

        // Parse AMM config
        let amm_data = amm_config.try_borrow_data()?;
        let amm_config_parsed = AmmConfigSimple::try_from_bytes(&amm_data)?;

        // Tick arrays are NOT parsed here - they are scanned on-the-fly from
        // account data during swap calculation (zero heap allocation).

        // Calculate prices
        let price = sqrt_price_to_price(pool.sqrt_price_x64);

        let inverse_price = if price > 0.0 { 1.0 / price } else { 0.0 };

        let instance = RaydiumCLMM {
            pool_id: *pool_id.key,
            amm_config_key: pool.amm_config,
            base_token_pk: pool.token_mint_0,
            quote_token_pk: pool.token_mint_1,
            token_vault_0: pool.token_vault_0,
            token_vault_1: pool.token_vault_1,
            observation_key: pool.observation_key,
            sqrt_price_x64: pool.sqrt_price_x64,
            tick_current: pool.tick_current,
            liquidity: pool.liquidity,
            tick_spacing: pool.tick_spacing,
            mint_decimals_0: pool.mint_decimals_0,
            mint_decimals_1: pool.mint_decimals_1,
            trade_fee_rate: amm_config_parsed.trade_fee_rate,
            protocol_fee_rate: amm_config_parsed.protocol_fee_rate,
            fund_fee_rate: amm_config_parsed.fund_fee_rate,
            fee_rate: amm_config_parsed.trade_fee_rate as f64 / FEE_RATE_DENOMINATOR_VALUE as f64,
            price,
            inverse_price,
            start_index,
            end_index,
            phantom: PhantomData,
        };
        // instance.log_accounts(accounts)?;
        Ok(instance)
    }

    // ── Zero-copy tick array scanning ──────────────────────────────────
    // Byte layout of on-chain TickArrayState (after 8-byte discriminator):
    //   [0..32]  pool_id        [32..36] start_tick_index
    //   [36..]   ticks[60]      (each TickState = 168 bytes, packed)
    // Within each TickState:
    //   [0..4] tick (i32)   [4..20] liquidity_net (i128)   [20..36] liquidity_gross (u128)
    const TA_DISC: usize = 8;
    const TA_POOL_OFF: usize = Self::TA_DISC;
    const TA_START_OFF: usize = Self::TA_POOL_OFF + 32;
    const TA_TICKS_OFF: usize = Self::TA_START_OFF + 4;
    const TA_TICK_SIZE: usize = 168;
    const TA_TICK_CNT: usize = 60;
    const TA_MIN_LEN: usize = Self::TA_TICKS_OFF + Self::TA_TICK_SIZE * Self::TA_TICK_CNT;
    const TS_LIQ_NET: usize = 4;
    const TS_LIQ_GROSS: usize = 20;

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
                if data.len() >= Self::TA_MIN_LEN
                    && data[Self::TA_POOL_OFF..Self::TA_POOL_OFF + 32] == *pool_id_bytes
                {
                    let start = i32::from_le_bytes(
                        unsafe { *(data.as_ptr().add(Self::TA_START_OFF) as *const [u8; 4]) }
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
            let mut i = raw_offset.min(Self::TA_TICK_CNT as i32 - 1).max(0) as usize;
            loop {
                let b = data.add(Self::TA_TICKS_OFF + i * Self::TA_TICK_SIZE);
                if u128::from_le_bytes(*(b.add(Self::TS_LIQ_GROSS) as *const [u8; 16])) != 0 {
                    let t = i32::from_le_bytes(*(b as *const [u8; 4]));
                    if t <= current_tick {
                        let ln = i128::from_le_bytes(*(b.add(Self::TS_LIQ_NET) as *const [u8; 16]));
                        return Some((t, ln));
                    }
                }
                if i == 0 { break; }
                i -= 1;
            }
        } else {
            let from = (raw_offset + 1).max(0).min(Self::TA_TICK_CNT as i32 - 1) as usize;
            let mut i = from;
            while i < Self::TA_TICK_CNT {
                let b = data.add(Self::TA_TICKS_OFF + i * Self::TA_TICK_SIZE);
                if u128::from_le_bytes(*(b.add(Self::TS_LIQ_GROSS) as *const [u8; 16])) != 0 {
                    let t = i32::from_le_bytes(*(b as *const [u8; 4]));
                    if t > current_tick {
                        let ln = i128::from_le_bytes(*(b.add(Self::TS_LIQ_NET) as *const [u8; 16]));
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
        let (ta_from, ta_to) = Self::tick_array_range(self.start_index, zero_for_one);

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

        let (ta_from, ta_to) = Self::tick_array_range(self.start_index, zero_for_one);
        let (ta, ta_count) = Self::preload_tick_arrays(accounts, &pool_id_bytes, ta_from, ta_to);

        // Collect all initialized ticks across all arrays, sorted ascending
        let mut all_ticks: Vec<(i32, i128)> = Vec::new();
        for idx in 0..ta_count {
            let data = ta[idx].data;
            for slot in 0..Self::TA_TICK_CNT {
                unsafe {
                    let b = data.add(Self::TA_TICKS_OFF + slot * Self::TA_TICK_SIZE);
                    let lg = u128::from_le_bytes(*(b.add(Self::TS_LIQ_GROSS) as *const [u8; 16]));
                    if lg != 0 {
                        let t = i32::from_le_bytes(*(b as *const [u8; 4]));
                        let ln = i128::from_le_bytes(*(b.add(Self::TS_LIQ_NET) as *const [u8; 16]));
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
    use anchor_lang::prelude::Clock;
    use anchor_lang::solana_program::{account_info::AccountInfo, pubkey::Pubkey, system_program};
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
            false,
            false,
            lamports,
            data,
            owner_static,
            account.executable,
            account.rent_epoch,
        )
    }

    async fn get_clock(rpc_client: &RpcClient) -> anyhow::Result<Clock> {
        use anchor_client::solana_sdk::sysvar;

        let clock_account = rpc_client.get_account(&sysvar::clock::ID).await?;

        if clock_account.data.len() < 40 {
            return Err(anyhow::anyhow!(
                "Clock account data too short: {} bytes",
                clock_account.data.len()
            ));
        }

        let data = &clock_account.data;
        let slot = u64::from_le_bytes(data[0..8].try_into()?);
        let epoch_start_timestamp = i64::from_le_bytes(data[8..16].try_into()?);
        let epoch = u64::from_le_bytes(data[16..24].try_into()?);
        let leader_schedule_epoch = u64::from_le_bytes(data[24..32].try_into()?);
        let unix_timestamp = i64::from_le_bytes(data[32..40].try_into()?);

        Ok(Clock {
            slot,
            epoch_start_timestamp,
            epoch,
            leader_schedule_epoch,
            unix_timestamp,
        })
    }

    #[tokio::test]
    async fn test_raydium_clmm_round_trip_swap() {
        use anchor_client::Cluster;

        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());
        let pool_id_key = Pubkey::from_str_const("AFT2PaCYfy93g47aTyG3wKu4KDEg2YMhUmwbdPDdcmCG");

        eprintln!(
            "Testing Raydium CLMM round trip swap for pool: {}",
            pool_id_key
        );

        let pool_account = match rpc_client
            .get_account(&SdkPubkey::try_from(pool_id_key.to_bytes().as_ref()).unwrap())
            .await
        {
            Ok(acc) => acc,
            Err(e) => {
                eprintln!("Warning: Could not fetch pool account: {:?}", e);
                return;
            }
        };

        let pool_state_size = std::mem::size_of::<PoolStateSimple>();
        let pool: PoolStateSimple =
            bytemuck::pod_read_unaligned(&pool_account.data[8..8 + pool_state_size]);

        eprintln!("\n=== Pool State ===");
        eprintln!("Token Mint 0: {}", pool.token_mint_0);
        eprintln!("Token Mint 1: {}", pool.token_mint_1);
        eprintln!(
            "Decimals 0: {}, Decimals 1: {}",
            pool.mint_decimals_0, pool.mint_decimals_1
        );

        if pool.liquidity == 0 {
            eprintln!("Warning: Pool has no liquidity. Skipping test.");
            return;
        }

        let vault_0_account = match rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_vault_0.to_bytes().as_ref()).unwrap())
            .await
        {
            Ok(acc) => acc,
            Err(_) => return,
        };
        let vault_1_account = match rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_vault_1.to_bytes().as_ref()).unwrap())
            .await
        {
            Ok(acc) => acc,
            Err(_) => return,
        };
        let mint_0_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_mint_0.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();
        let mint_1_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_mint_1.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();
        let amm_config_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.amm_config.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();
        let observation_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.observation_key.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();

        // Derive tick array PDAs for buy (zero_for_one) and sell (one_for_zero) directions
        let ticks_in_array = TICK_ARRAY_SIZE * i32::from(pool.tick_spacing);
        let current_start_index =
            TickArrayState::get_array_start_index(pool.tick_current, pool.tick_spacing);

        // Buy direction: current + below (price goes down)
        // Sell direction: current + above (price goes up)
        let tick_array_start_indices = [
            current_start_index,                        // buy_1
            current_start_index - ticks_in_array,       // buy_2
            current_start_index,                        // sell_1
            current_start_index + ticks_in_array,       // sell_2
        ];

        let mut tick_array_keys_and_accounts = Vec::new();
        for &start_index in &tick_array_start_indices {
            let (tick_array_pda, _) = Pubkey::find_program_address(
                &[
                    b"tick_array",
                    pool_id_key.as_ref(),
                    &start_index.to_be_bytes(),
                ],
                &RaydiumCLMM::PROGRAM_ID,
            );
            eprintln!(
                "Fetching tick array PDA for start_index {}: {}",
                start_index, tick_array_pda
            );
            let tick_array_account = rpc_client
                .get_account(&SdkPubkey::try_from(tick_array_pda.to_bytes().as_ref()).unwrap())
                .await
                .unwrap();
            tick_array_keys_and_accounts.push((tick_array_pda, tick_array_account));
        }

        let (buy_1_key, buy_1_account) = tick_array_keys_and_accounts.remove(0);
        let (buy_2_key, buy_2_account) = tick_array_keys_and_accounts.remove(0);
        let (sell_1_key, sell_1_account) = tick_array_keys_and_accounts.remove(0);
        let (sell_2_key, sell_2_account) = tick_array_keys_and_accounts.remove(0);

//         eprintln!("Buy Tick Array 1: {}", buy_1_key);
//         eprintln!("Buy Tick Array 2: {}", buy_2_key);
//         eprintln!("Sell Tick Array 1: {}", sell_1_key);
//         eprintln!("Sell Tick Array 2: {}", sell_2_key);

        let clock = get_clock(&rpc_client).await.unwrap();

        let pool_id_account_info = account_to_account_info(pool_id_key, pool_account);
        let vault_0 = account_to_account_info(pool.token_vault_0, vault_0_account);
        let vault_1 = account_to_account_info(pool.token_vault_1, vault_1_account);
        let token_0 = account_to_account_info(pool.token_mint_0, mint_0_account);
        let token_1 = account_to_account_info(pool.token_mint_1, mint_1_account);
        let amm_config = account_to_account_info(pool.amm_config, amm_config_account);
        let observation = account_to_account_info(pool.observation_key, observation_account);
        let tick_buy_1 = account_to_account_info(buy_1_key, buy_1_account);
        let tick_buy_2 = account_to_account_info(buy_2_key, buy_2_account);
        let tick_sell_1 = account_to_account_info(sell_1_key, sell_1_account);
        let tick_sell_2 = account_to_account_info(sell_2_key, sell_2_account);

        let program_id_key = RaydiumCLMM::PROGRAM_ID;
        let program_id_account =
            create_mock_account_info_with_data(program_id_key, system_program::id(), None);

        // Account layout: [program_id, pool_id, vault_0, vault_1, token_0, token_1,
        //                   amm_config, observation, bitmap_ext, buy_1, buy_2, sell_1, sell_2]
        let bitmap_ext = create_mock_account_info_with_data(
            RaydiumCLMM::PROGRAM_ID, system_program::id(), None,
        );
        let accounts = vec![
            program_id_account,
            pool_id_account_info,
            vault_0.clone(),
            vault_1.clone(),
            token_0.clone(),
            token_1.clone(),
            amm_config,
            observation,
            bitmap_ext,
            tick_buy_1,
            tick_buy_2,
            tick_sell_1,
            tick_sell_2,
        ];

        let raydium_clmm = match RaydiumCLMM::new(&accounts, 0, accounts.len()) {
            Ok(clmm) => clmm,
            Err(e) => {
                eprintln!("Failed to create RaydiumCLMM: {:?}", e);
                return;
            }
        };

        eprintln!("\n=== Prices ===");
        eprintln!("Price (token_1/token_0): {:.10}", raydium_clmm.price);
        eprintln!(
            "Inverse Price (token_0/token_1): {:.10}",
            raydium_clmm.inverse_price
        );
        eprintln!(
            "Trade Fee Rate: {} ({}%)",
            raydium_clmm.trade_fee_rate,
            raydium_clmm.trade_fee_rate as f64 / 10000.0
        );
        eprintln!("Tick array accounts: {} (2 buy + 2 sell)", RaydiumCLMM::ACCOUNT_COUNT - RaydiumCLMM::TICK_BUY_1_IDX);
        let (max_in, max_out) = raydium_clmm
            .get_max_amounts_in_out(&accounts, raydium_clmm.base_token_pk)
            .unwrap();
        eprintln!(
            "Max SOL IN: {} -> MAX TOKEN OUT: {}",
            max_in as f64 / 1_000_000_000.0,
            max_out as f64 / 1_000_000.0
        );

        let input_mint = raydium_clmm.base_token_pk;
        let output_mint = raydium_clmm.quote_token_pk;
        let amount_in = 100_000_000_000u64; // 1 token (assuming 9 decimals)

        eprintln!("================================================");
        // Step 1: Swap token_0 -> token_1
        let token_out =
            match raydium_clmm.swap_base_in(&accounts, input_mint, amount_in, &clock) {
                Ok(out) => out,
                Err(e) => {
                    eprintln!("swap_base_in failed: {:?}", e);
                    return;
                }
            };

        eprintln!(
            "Step 1 (swap_base_in): {} SOL -> {} TOKEN",
            amount_in as f64 / 1_000_000_000.0,
            token_out as f64 / 1_000_000.0,
        );

        let max_sol_in =
            match raydium_clmm.swap_base_out(&accounts, output_mint, token_out, &clock) {
                Ok(max_in) => max_in,
                Err(e) => {
                    eprintln!("swap_base_out failed: {:?}", e);
                    return;
                }
            };

        eprintln!(
            "Step 1 (swap_base_out): MAX SOL IN {} -> {} TOKEN OUT",
            max_sol_in as f64 / 1_000_000_000.0,
            token_out as f64 / 1_000_000.0,
        );

        eprintln!("================================================");

        // Step 2: Swap token_1 -> token_0
        let sol_out =
            match raydium_clmm.swap_base_in(&accounts, output_mint, token_out, &clock) {
                Ok(back) => back,
                Err(e) => {
                    eprintln!("reverse swap_base_in failed: {:?}", e);
                    return;
                }
            };

        eprintln!(
            "Step 2 (swap_base_in): {} TOKEN -> {} SOL",
            token_out as f64 / 1_000_000.0,
            sol_out as f64 / 1_000_000_000.0,
        );

        let max_token_in =
            match raydium_clmm.swap_base_out(&accounts, input_mint, sol_out, &clock) {
                Ok(max_in) => max_in,
                Err(e) => {
                    eprintln!("reverse swap_base_out failed: {:?}", e);
                    return;
                }
            };

        eprintln!(
            "Step 2 (swap_base_out): {} MAX TOKEN IN -> {} SOL OUT",
            max_token_in as f64 / 1_000_000.0,
            sol_out as f64 / 1_000_000_000.0
        );

        eprintln!("================================================");
        eprintln!("Round trip completed!");
    }

    #[tokio::test]
    async fn test_raydium_clmm_liquidity_distribution() {
        use anchor_client::Cluster;

        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());
        let pool_id_key = Pubkey::from_str_const("AFT2PaCYfy93g47aTyG3wKu4KDEg2YMhUmwbdPDdcmCG");

        eprintln!("Testing liquidity distribution for pool: {}", pool_id_key);

        let pool_account = match rpc_client
            .get_account(&SdkPubkey::try_from(pool_id_key.to_bytes().as_ref()).unwrap())
            .await
        {
            Ok(acc) => acc,
            Err(e) => { eprintln!("Could not fetch pool: {:?}", e); return; }
        };

        let pool_state_size = std::mem::size_of::<PoolStateSimple>();
        let pool: PoolStateSimple =
            bytemuck::pod_read_unaligned(&pool_account.data[8..8 + pool_state_size]);

        if pool.liquidity == 0 {
            eprintln!("Pool has no liquidity"); return;
        }

        let vault_0_account = rpc_client.get_account(&SdkPubkey::try_from(pool.token_vault_0.to_bytes().as_ref()).unwrap()).await.unwrap();
        let vault_1_account = rpc_client.get_account(&SdkPubkey::try_from(pool.token_vault_1.to_bytes().as_ref()).unwrap()).await.unwrap();
        let mint_0_account = rpc_client.get_account(&SdkPubkey::try_from(pool.token_mint_0.to_bytes().as_ref()).unwrap()).await.unwrap();
        let mint_1_account = rpc_client.get_account(&SdkPubkey::try_from(pool.token_mint_1.to_bytes().as_ref()).unwrap()).await.unwrap();
        let amm_config_account = rpc_client.get_account(&SdkPubkey::try_from(pool.amm_config.to_bytes().as_ref()).unwrap()).await.unwrap();
        let observation_account = rpc_client.get_account(&SdkPubkey::try_from(pool.observation_key.to_bytes().as_ref()).unwrap()).await.unwrap();

        let ticks_in_array = TICK_ARRAY_SIZE * i32::from(pool.tick_spacing);
        let current_start_index = TickArrayState::get_array_start_index(pool.tick_current, pool.tick_spacing);

        let tick_array_start_indices = [
            current_start_index,
            current_start_index - ticks_in_array,
            current_start_index,
            current_start_index + ticks_in_array,
        ];

        let mut tick_array_keys_and_accounts = Vec::new();
        for &start_index in &tick_array_start_indices {
            let (tick_array_pda, _) = Pubkey::find_program_address(
                &[b"tick_array", pool_id_key.as_ref(), &start_index.to_be_bytes()],
                &RaydiumCLMM::PROGRAM_ID,
            );
            let tick_array_account = rpc_client
                .get_account(&SdkPubkey::try_from(tick_array_pda.to_bytes().as_ref()).unwrap())
                .await.unwrap();
            tick_array_keys_and_accounts.push((tick_array_pda, tick_array_account));
        }

        let (buy_1_key, buy_1_account) = tick_array_keys_and_accounts.remove(0);
        let (buy_2_key, buy_2_account) = tick_array_keys_and_accounts.remove(0);
        let (sell_1_key, sell_1_account) = tick_array_keys_and_accounts.remove(0);
        let (sell_2_key, sell_2_account) = tick_array_keys_and_accounts.remove(0);

        let pool_id_account_info = account_to_account_info(pool_id_key, pool_account);
        let vault_0 = account_to_account_info(pool.token_vault_0, vault_0_account);
        let vault_1 = account_to_account_info(pool.token_vault_1, vault_1_account);
        let token_0 = account_to_account_info(pool.token_mint_0, mint_0_account);
        let token_1 = account_to_account_info(pool.token_mint_1, mint_1_account);
        let amm_config = account_to_account_info(pool.amm_config, amm_config_account);
        let observation = account_to_account_info(pool.observation_key, observation_account);
        let tick_buy_1 = account_to_account_info(buy_1_key, buy_1_account);
        let tick_buy_2 = account_to_account_info(buy_2_key, buy_2_account);
        let tick_sell_1 = account_to_account_info(sell_1_key, sell_1_account);
        let tick_sell_2 = account_to_account_info(sell_2_key, sell_2_account);

        let program_id_key = RaydiumCLMM::PROGRAM_ID;
        let program_id_account = create_mock_account_info_with_data(program_id_key, system_program::id(), None);
        let bitmap_ext = create_mock_account_info_with_data(RaydiumCLMM::PROGRAM_ID, system_program::id(), None);

        let accounts = vec![
            program_id_account, pool_id_account_info, vault_0, vault_1,
            token_0, token_1, amm_config, observation, bitmap_ext,
            tick_buy_1, tick_buy_2, tick_sell_1, tick_sell_2,
        ];

        let raydium_clmm = match RaydiumCLMM::new(&accounts, 0, accounts.len()) {
            Ok(clmm) => clmm,
            Err(e) => { eprintln!("Failed to create RaydiumCLMM: {:?}", e); return; }
        };

        eprintln!("\n=== Pool Info ===");
        eprintln!("Current tick: {}", raydium_clmm.tick_current);
        eprintln!("Current liquidity: {}", raydium_clmm.liquidity);
        eprintln!("Tick spacing: {}", raydium_clmm.tick_spacing);
        eprintln!("Price: {:.10}", raydium_clmm.price);

        // Buy direction (zero_for_one): price goes down
        eprintln!("\n=== Buy Direction (zero_for_one) - Liquidity Distribution ===");
        let buy_ranges = raydium_clmm.get_liquidity_distribution(&accounts, true).unwrap();
        for (i, r) in buy_ranges.iter().enumerate() {
            let price_lower = sqrt_price_to_price(r.sqrt_price_lower_x64);
            let price_upper = sqrt_price_to_price(r.sqrt_price_upper_x64);
            eprintln!(
                "  Range {}: ticks [{}, {}) | liquidity: {} | price: [{:.10}, {:.10}) | token_0 capacity: {} | token_1 capacity: {}",
                i, r.tick_lower, r.tick_upper, r.liquidity,
                price_lower, price_upper,
                r.token_0_capacity(), r.token_1_capacity(),
            );
        }

        // Sell direction (one_for_zero): price goes up
        eprintln!("\n=== Sell Direction (one_for_zero) - Liquidity Distribution ===");
        let sell_ranges = raydium_clmm.get_liquidity_distribution(&accounts, false).unwrap();
        for (i, r) in sell_ranges.iter().enumerate() {
            let price_lower = sqrt_price_to_price(r.sqrt_price_lower_x64);
            let price_upper = sqrt_price_to_price(r.sqrt_price_upper_x64);
            eprintln!(
                "  Range {}: ticks [{}, {}) | liquidity: {} | price: [{:.10}, {:.10}) | token_0 capacity: {} | token_1 capacity: {}",
                i, r.tick_lower, r.tick_upper, r.liquidity,
                price_lower, price_upper,
                r.token_0_capacity(), r.token_1_capacity(),
            );
        }

        eprintln!("\nTotal buy ranges: {}, sell ranges: {}", buy_ranges.len(), sell_ranges.len());
    }
}