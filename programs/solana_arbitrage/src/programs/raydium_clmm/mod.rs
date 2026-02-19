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

/// Step computations during swap
#[derive(Debug, Clone, Default)]
struct StepComputations {
    sqrt_price_start_x64: u128,
    tick_next: i32,
    initialized: bool,
    sqrt_price_next_x64: u128,
    amount_in: u64,
    amount_out: u64,
    fee_amount: u64,
}

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
        _clock: Clock,
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
        let amount_out =
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
        _clock: Clock,
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
        let amount_in =
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
        let memo = &accounts[self.start_index + Self::MEMO_IDX];
        let bitmap_extension = &accounts[self.start_index + Self::BITMAP_EXTENSION_IDX];

        let zero_for_one = input_mint == self.base_token_pk;

        let (
            input_token_program,
            output_token_program,
            user_input_account,
            user_output_account,
            input_vault,
            output_vault,
            input_mint_acc,
            output_mint_acc,
        ) = if zero_for_one {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
                vault_0,
                vault_1,
                mint_1_account,
                mint_2_account,
            )
        } else {
            (
                mint_2_token_program,
                mint_1_token_program,
                user_mint_2_token_account,
                user_mint_1_token_account,
                vault_1,
                vault_0,
                mint_2_account,
                mint_1_account,
            )
        };

        // Build swap instruction
        let mut metas = vec![
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(self.amm_config_key, false),
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new(*user_input_account.key, false),
            AccountMeta::new(*user_output_account.key, false),
            AccountMeta::new(*input_vault.key, false),
            AccountMeta::new(*output_vault.key, false),
            AccountMeta::new(*observation.key, false),
            AccountMeta::new_readonly(*input_token_program.key, false),
            AccountMeta::new_readonly(*output_token_program.key, false),
            AccountMeta::new_readonly(*memo.key, false),
            AccountMeta::new_readonly(*input_mint_acc.key, false),
            AccountMeta::new_readonly(*output_mint_acc.key, false),
        ];

        if *bitmap_extension.key != Self::PROGRAM_ID {
            metas.push(AccountMeta::new(*bitmap_extension.key, false));
        }

        // Add tick array accounts as remaining accounts
        for i in (self.start_index + Self::TICK_ARRAYS_START_IDX)..self.end_index {
            let tick_array = &accounts[i];
            metas.push(AccountMeta::new(*tick_array.key, false));
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

        let mut accounts_vec: Vec<AccountInfo<'a>> = vec![
            payer.clone(),
            amm_config.clone(),
            pool_id.clone(),
            user_input_account.clone(),
            user_output_account.clone(),
            input_vault.clone(),
            output_vault.clone(),
            observation.clone(),
            input_token_program.clone(),
            output_token_program.clone(),
            memo.clone(),
            input_mint_acc.clone(),
            output_mint_acc.clone(),
        ];

        if *bitmap_extension.key != Self::PROGRAM_ID {
            accounts_vec.push(bitmap_extension.clone());
        }

        // Add tick array accounts
        for i in (self.start_index + Self::TICK_ARRAYS_START_IDX)..self.end_index {
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
        let memo = &accounts[self.start_index + Self::MEMO_IDX];
        let bitmap_extension = &accounts[self.start_index + Self::BITMAP_EXTENSION_IDX];

        let zero_for_one = input_mint == self.base_token_pk;

        let (
            input_token_program,
            output_token_program,
            user_input_account,
            user_output_account,
            input_vault,
            output_vault,
            input_mint_acc,
            output_mint_acc,
        ) = if zero_for_one {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
                vault_0,
                vault_1,
                mint_1_account,
                mint_2_account,
            )
        } else {
            (
                mint_2_token_program,
                mint_1_token_program,
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
            AccountMeta::new_readonly(*input_token_program.key, false),
            AccountMeta::new_readonly(*output_token_program.key, false),
            AccountMeta::new_readonly(*memo.key, false),
            AccountMeta::new_readonly(*input_mint_acc.key, false),
            AccountMeta::new_readonly(*output_mint_acc.key, false),
        ];

        if *bitmap_extension.key != Self::PROGRAM_ID {
            metas.push(AccountMeta::new(*bitmap_extension.key, false));
        }

        // Add tick array accounts as remaining accounts
        for i in (self.start_index + Self::TICK_ARRAYS_START_IDX)..self.end_index {
            let tick_array = &accounts[i];
            metas.push(AccountMeta::new(*tick_array.key, false));
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

        let mut accounts_vec: Vec<AccountInfo<'a>> = vec![
            payer.clone(),
            amm_config.clone(),
            pool_id.clone(),
            user_input_account.clone(),
            user_output_account.clone(),
            input_vault.clone(),
            output_vault.clone(),
            observation.clone(),
            input_token_program.clone(),
            output_token_program.clone(),
            memo.clone(),
            input_mint_acc.clone(),
            output_mint_acc.clone(),
        ];

        if *bitmap_extension.key != Self::PROGRAM_ID {
            accounts_vec.push(bitmap_extension.clone());
        }

        // Add tick array accounts
        for i in (self.start_index + Self::TICK_ARRAYS_START_IDX)..self.end_index {
            accounts_vec.push(accounts[i].clone());
        }

        unsafe {
            let accounts_slice: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts_slice)?;
        }

        Ok(())
    }

    fn get_max_amounts_in_out(&self, input_mint: Pubkey) -> Result<(u64, u64)> {
        RaydiumCLMM::get_max_amounts_in_out(self, input_mint)
    }

    fn get_max_amount_in(&self, mint: Pubkey) -> Result<u64> {
        self.get_max_amount_in_current_tick(&mint)
    }

    fn get_max_amount_out(&self, mint: Pubkey) -> Result<u64> {
        self.get_max_amount_out_current_tick(&mint)
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
        msg!("8 memo: {}", accounts[self.start_index + Self::MEMO_IDX].key);
        msg!("9 bitmap_extension: {}", accounts[self.start_index + Self::BITMAP_EXTENSION_IDX].key);
        for i in (self.start_index + Self::TICK_ARRAYS_START_IDX)..self.end_index {
            msg!("{} tick_array: {}", i - self.start_index, accounts[i].key);
        }
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
    pub const MEMO_IDX: usize = 8;
    pub const BITMAP_EXTENSION_IDX: usize = 9;
    // Tick arrays start at index 10
    pub const TICK_ARRAYS_START_IDX: usize = 10;

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

    /// Scan one account's raw data for an initialized tick.
    /// `current_tick` controls the search bound:
    ///   zero_for_one  → highest initialized tick where tick <= current_tick
    ///   !zero_for_one → lowest  initialized tick where tick >  current_tick
    /// Pass i32::MAX / i32::MIN as sentinel to match any tick (first_initialized_tick).
    fn scan_tick_in_data(
        data: &[u8],
        pool_id_bytes: &[u8; 32],
        target_start: i32,
        current_tick: i32,
        zero_for_one: bool,
    ) -> Option<(i32, i128)> {
        if data.len() < Self::TA_MIN_LEN {
            return None;
        }
        if &data[Self::TA_POOL_OFF..Self::TA_POOL_OFF + 32] != pool_id_bytes {
            return None;
        }
        let start = i32::from_le_bytes(
            data[Self::TA_START_OFF..Self::TA_START_OFF + 4].try_into().ok()?,
        );
        if start != target_start {
            return None;
        }

        if zero_for_one {
            for i in (0..Self::TA_TICK_CNT).rev() {
                let b = Self::TA_TICKS_OFF + i * Self::TA_TICK_SIZE;
                let lg = u128::from_le_bytes(
                    data[b + Self::TS_LIQ_GROSS..b + Self::TS_LIQ_GROSS + 16].try_into().ok()?,
                );
                if lg != 0 {
                    let t = i32::from_le_bytes(data[b..b + 4].try_into().ok()?);
                    if t <= current_tick {
                        let ln = i128::from_le_bytes(
                            data[b + Self::TS_LIQ_NET..b + Self::TS_LIQ_NET + 16].try_into().ok()?,
                        );
                        return Some((t, ln));
                    }
                }
            }
        } else {
            for i in 0..Self::TA_TICK_CNT {
                let b = Self::TA_TICKS_OFF + i * Self::TA_TICK_SIZE;
                let lg = u128::from_le_bytes(
                    data[b + Self::TS_LIQ_GROSS..b + Self::TS_LIQ_GROSS + 16].try_into().ok()?,
                );
                if lg != 0 {
                    let t = i32::from_le_bytes(data[b..b + 4].try_into().ok()?);
                    if t > current_tick {
                        let ln = i128::from_le_bytes(
                            data[b + Self::TS_LIQ_NET..b + Self::TS_LIQ_NET + 16].try_into().ok()?,
                        );
                        return Some((t, ln));
                    }
                }
            }
        }
        None
    }

    /// Scan tick array accounts for a matching initialized tick.
    /// Borrows account data on-the-fly and drops immediately — zero heap allocation.
    fn scan_accounts_for_tick(
        &self,
        accounts: &[AccountInfo],
        target_start: i32,
        current_tick: i32,
        zero_for_one: bool,
    ) -> Option<(i32, i128)> {
        let pool_id_bytes = self.pool_id.to_bytes();
        for i in (self.start_index + Self::TICK_ARRAYS_START_IDX)..self.end_index {
            if let Ok(data) = accounts[i].try_borrow_data() {
                if let Some(result) =
                    Self::scan_tick_in_data(&data, &pool_id_bytes, target_start, current_tick, zero_for_one)
                {
                    return Some(result);
                }
            }
        }
        None
    }

    /// Find the next initialized tick by scanning account data on-the-fly.
    /// Zero heap allocation — borrows are dropped after each account.
    fn find_next_initialized_tick(
        &self,
        accounts: &[AccountInfo],
        current_tick: i32,
        zero_for_one: bool,
    ) -> Option<(i32, i128)> {
        let current_start =
            TickArrayState::get_array_start_index(current_tick, self.tick_spacing);

        // Try current tick array
        if let Some(result) = self.scan_accounts_for_tick(accounts, current_start, current_tick, zero_for_one) {
            return Some(result);
        }

        // Try adjacent tick array (use sentinel to accept any initialized tick)
        let ticks_in_array = TICK_ARRAY_SIZE * i32::from(self.tick_spacing);
        let next_start = if zero_for_one {
            current_start - ticks_in_array
        } else {
            current_start + ticks_in_array
        };
        let sentinel = if zero_for_one { i32::MAX } else { i32::MIN };
        self.scan_accounts_for_tick(accounts, next_start, sentinel, zero_for_one)
    }

    /// Calculate swap using tick arrays scanned on-the-fly from account data.
    fn calculate_swap_with_tick_arrays(
        &self,
        accounts: &[AccountInfo],
        amount_specified: u64,
        zero_for_one: bool,
        is_base_input: bool,
    ) -> Result<u64> {
        let has_tick_arrays = self.end_index > self.start_index + Self::TICK_ARRAYS_START_IDX;
        if self.liquidity == 0 && !has_tick_arrays {
            return Err(ErrorCode::InsufficientLiquidityForDirection.into());
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

        let mut iterations = 0;
        const MAX_ITERATIONS: usize = 100;

        while state.amount_specified_remaining != 0
            && state.sqrt_price_x64 != sqrt_price_limit_x64
            && iterations < MAX_ITERATIONS
        {
            iterations += 1;
            let mut step = StepComputations::default();
            step.sqrt_price_start_x64 = state.sqrt_price_x64;

            // Find the next initialized tick (scanned on-the-fly from account data)
            let (tick_next, liquidity_net) =
                match self.find_next_initialized_tick(accounts, state.tick, zero_for_one) {
                    Some((t, ln)) => (t, ln),
                    None => {
                        // No more initialized ticks - use tick boundary based on current tick spacing
                        let tick_next = if zero_for_one {
                            self.get_lower_tick_boundary(state.tick)
                        } else {
                            self.get_upper_tick_boundary(state.tick)
                        };
                        (tick_next, 0i128)
                    }
                };

            step.tick_next = tick_next.max(tick_math::MIN_TICK).min(tick_math::MAX_TICK);
            step.initialized = liquidity_net != 0;

            step.sqrt_price_next_x64 =
                tick_math::get_sqrt_price_at_tick(step.tick_next).unwrap_or(if zero_for_one {
                    tick_math::MIN_SQRT_PRICE_X64
                } else {
                    tick_math::MAX_SQRT_PRICE_X64
                });

            let target_price = if (zero_for_one && step.sqrt_price_next_x64 < sqrt_price_limit_x64)
                || (!zero_for_one && step.sqrt_price_next_x64 > sqrt_price_limit_x64)
            {
                sqrt_price_limit_x64
            } else {
                step.sqrt_price_next_x64
            };

            // Skip if no liquidity
            if state.liquidity == 0 {
                state.tick = step.tick_next;
                state.sqrt_price_x64 = step.sqrt_price_next_x64;
                continue;
            }

            // Compute swap step
            let swap_step = swap_math::compute_swap_step(
                step.sqrt_price_start_x64,
                target_price,
                state.liquidity,
                state.amount_specified_remaining,
                self.trade_fee_rate,
                is_base_input,
                zero_for_one,
            );

            state.sqrt_price_x64 = swap_step.sqrt_price_next_x64;
            step.amount_in = swap_step.amount_in;
            step.amount_out = swap_step.amount_out;
            step.fee_amount = swap_step.fee_amount;

            // Update amounts
            if is_base_input {
                state.amount_specified_remaining = state
                    .amount_specified_remaining
                    .saturating_sub(step.amount_in + step.fee_amount);
                state.amount_calculated = state.amount_calculated.saturating_add(step.amount_out);
            } else {
                state.amount_specified_remaining = state
                    .amount_specified_remaining
                    .saturating_sub(step.amount_out);
                state.amount_calculated = state
                    .amount_calculated
                    .saturating_add(step.amount_in)
                    .saturating_add(step.fee_amount);
            }

            // Calculate protocol and fund fees (deducted from LP fee)
            let step_fee_amount = step.fee_amount;
            if self.protocol_fee_rate > 0 {
                let delta = (step_fee_amount as u128)
                    .mul_div_floor(
                        self.protocol_fee_rate as u128,
                        FEE_RATE_DENOMINATOR_VALUE as u128,
                    )
                    .unwrap_or(0) as u64;
                step.fee_amount = step.fee_amount.saturating_sub(delta);
                state.protocol_fee = state.protocol_fee.saturating_add(delta);
            }
            if self.fund_fee_rate > 0 {
                let delta = (step_fee_amount as u128)
                    .mul_div_floor(
                        self.fund_fee_rate as u128,
                        FEE_RATE_DENOMINATOR_VALUE as u128,
                    )
                    .unwrap_or(0) as u64;
                step.fee_amount = step.fee_amount.saturating_sub(delta);
                state.fund_fee = state.fund_fee.saturating_add(delta);
            }

            state.fee_amount = state.fee_amount.saturating_add(step.fee_amount);

            // Shift tick if we reached the next price
            if state.sqrt_price_x64 == step.sqrt_price_next_x64 {
                if step.initialized {
                    // Cross the tick - update liquidity
                    let net = if zero_for_one {
                        -liquidity_net
                    } else {
                        liquidity_net
                    };
                    state.liquidity =
                        liquidity_math::add_delta(state.liquidity, net).unwrap_or(state.liquidity);
                }
                state.tick = if zero_for_one {
                    step.tick_next - 1
                } else {
                    step.tick_next
                };
            } else if state.sqrt_price_x64 != step.sqrt_price_start_x64 {
                state.tick =
                    tick_math::get_tick_at_sqrt_price(state.sqrt_price_x64).unwrap_or(state.tick);
            }
        }

        Ok(state.amount_calculated)
    }

    /// Get the tick boundaries for the current tick
    fn get_current_tick_boundaries(&self) -> (i32, i32) {
        let tick_spacing = self.tick_spacing as i32;
        let tick_lower = self.get_lower_tick_boundary(self.tick_current);
        let tick_upper = tick_lower + tick_spacing;
        (tick_lower, tick_upper)
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

    /// Get the maximum amount that can be swapped in within the current tick range
    /// This is useful for arbitrage to know how much can be traded without crossing ticks
    pub fn get_max_amount_in_current_tick(&self, input_mint: &Pubkey) -> Result<u64> {
        let zero_for_one = *input_mint == self.base_token_pk;

        if self.liquidity == 0 {
            return Ok(0);
        }

        // Get the tick boundaries
        let (tick_lower, tick_upper) = self.get_current_tick_boundaries();

        // Get sqrt price at boundary
        let sqrt_price_boundary = if zero_for_one {
            tick_math::get_sqrt_price_at_tick(tick_lower)?
        } else {
            tick_math::get_sqrt_price_at_tick(tick_upper)?
        };

        // Calculate max input to reach boundary (without fees)
        let max_amount_in_raw = if zero_for_one {
            liquidity_math::get_delta_amount_0_unsigned(
                sqrt_price_boundary,
                self.sqrt_price_x64,
                self.liquidity,
                true,
            )
            .unwrap_or(0)
        } else {
            liquidity_math::get_delta_amount_1_unsigned(
                self.sqrt_price_x64,
                sqrt_price_boundary,
                self.liquidity,
                true,
            )
            .unwrap_or(0)
        };

        // Add fee to get the actual input amount needed
        let max_amount_in_with_fee = (max_amount_in_raw as u128)
            .mul_div_ceil(
                FEE_RATE_DENOMINATOR_VALUE as u128,
                (FEE_RATE_DENOMINATOR_VALUE - self.trade_fee_rate) as u128,
            )
            .unwrap_or(max_amount_in_raw as u128) as u64;

        Ok(max_amount_in_with_fee)
    }

    /// Get the maximum amount that can be received out within the current tick range
    pub fn get_max_amount_out_current_tick(&self, input_mint: &Pubkey) -> Result<u64> {
        let zero_for_one = *input_mint == self.base_token_pk;

        if self.liquidity == 0 {
            return Ok(0);
        }

        // Get the tick boundaries
        let (tick_lower, tick_upper) = self.get_current_tick_boundaries();

        // Get sqrt price at boundary
        let sqrt_price_boundary = if zero_for_one {
            tick_math::get_sqrt_price_at_tick(tick_lower)?
        } else {
            tick_math::get_sqrt_price_at_tick(tick_upper)?
        };

        // Calculate max output to reach boundary
        let max_amount_out = if zero_for_one {
            // Output is token_1
            liquidity_math::get_delta_amount_1_unsigned(
                sqrt_price_boundary,
                self.sqrt_price_x64,
                self.liquidity,
                false,
            )
            .unwrap_or(0)
        } else {
            // Output is token_0
            liquidity_math::get_delta_amount_0_unsigned(
                self.sqrt_price_x64,
                sqrt_price_boundary,
                self.liquidity,
                false,
            )
            .unwrap_or(0)
        };

        Ok(max_amount_out)
    }

    /// Get the maximum amounts in/out for a specific input_mint within the current tick range.
    /// Returns (max_amount_in, max_amount_out) - the maximum that can be swapped while staying
    /// in the same tick price range without crossing to the next tick.
    pub fn get_max_amounts_in_out(&self, input_mint: Pubkey) -> Result<(u64, u64)> {
        let zero_for_one = input_mint == self.base_token_pk;

        if self.liquidity == 0 {
            return Ok((0, 0));
        }

        // Get the tick boundaries
        let (tick_lower, tick_upper) = self.get_current_tick_boundaries();

        // Get sqrt prices at boundaries
        let sqrt_price_lower =
            tick_math::get_sqrt_price_at_tick(tick_lower).unwrap_or(tick_math::MIN_SQRT_PRICE_X64);
        let sqrt_price_upper =
            tick_math::get_sqrt_price_at_tick(tick_upper).unwrap_or(tick_math::MAX_SQRT_PRICE_X64);

        let (max_in, max_out) = if zero_for_one {
            // Moving price down (token_0 -> token_1)
            // Max input is token_0 needed to reach lower tick boundary
            let max_input = liquidity_math::get_amount_in_for_liquidity(
                self.sqrt_price_x64,
                sqrt_price_lower,
                self.liquidity,
                true, // zero_for_one
            )
            .unwrap_or(u64::MAX);

            // Max output is token_1 that can be received
            let max_output = liquidity_math::get_amount_out_for_liquidity(
                self.sqrt_price_x64,
                sqrt_price_lower,
                self.liquidity,
                true, // zero_for_one
            )
            .unwrap_or(0);

            (max_input, max_output)
        } else {
            // Moving price up (token_1 -> token_0)
            // Max input is token_1 needed to reach upper tick boundary
            let max_input = liquidity_math::get_amount_in_for_liquidity(
                self.sqrt_price_x64,
                sqrt_price_upper,
                self.liquidity,
                false, // one_for_zero
            )
            .unwrap_or(u64::MAX);

            // Max output is token_0 that can be received
            let max_output = liquidity_math::get_amount_out_for_liquidity(
                self.sqrt_price_x64,
                sqrt_price_upper,
                self.liquidity,
                false, // one_for_zero
            )
            .unwrap_or(0);

            (max_input, max_output)
        };

        Ok((max_in, max_out))
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

        // Derive tick array PDAs dynamically from pool state
        let ticks_in_array = TICK_ARRAY_SIZE * i32::from(pool.tick_spacing);
        let current_start_index =
            TickArrayState::get_array_start_index(pool.tick_current, pool.tick_spacing);

        let tick_array_start_indices = [current_start_index, current_start_index - ticks_in_array];

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

        let (tick_array_1_key, tick_array_1_account) = tick_array_keys_and_accounts.remove(0);
        let (tick_array_2_key, tick_array_2_account) = tick_array_keys_and_accounts.remove(0);

        eprintln!("Tick Array 1 Pubkey: {}", tick_array_1_key);
        eprintln!("Tick Array 2 Pubkey: {}", tick_array_2_key);

        let clock = get_clock(&rpc_client).await.unwrap();

        let pool_id_account_info = account_to_account_info(pool_id_key, pool_account);
        let vault_0 = account_to_account_info(pool.token_vault_0, vault_0_account);
        let vault_1 = account_to_account_info(pool.token_vault_1, vault_1_account);
        let token_0 = account_to_account_info(pool.token_mint_0, mint_0_account);
        let token_1 = account_to_account_info(pool.token_mint_1, mint_1_account);
        let amm_config = account_to_account_info(pool.amm_config, amm_config_account);
        let observation = account_to_account_info(pool.observation_key, observation_account);
        let tick_array_1 = account_to_account_info(tick_array_1_key, tick_array_1_account);
        let tick_array_2 = account_to_account_info(tick_array_2_key, tick_array_2_account);

        let program_id_key = RaydiumCLMM::PROGRAM_ID;
        let program_id_account =
            create_mock_account_info_with_data(program_id_key, system_program::id(), None);

        let accounts = vec![
            program_id_account,
            pool_id_account_info,
            vault_0.clone(),
            vault_1.clone(),
            token_0.clone(),
            token_1.clone(),
            amm_config,
            observation,
            tick_array_1,
            tick_array_2,
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
        eprintln!("Tick array accounts: {}", accounts.len() - RaydiumCLMM::TICK_ARRAYS_START_IDX);
        let (max_in, max_out) = raydium_clmm
            .get_max_amounts_in_out(raydium_clmm.base_token_pk)
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
            match raydium_clmm.swap_base_in(&accounts, input_mint, amount_in, clock.clone()) {
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
            match raydium_clmm.swap_base_out(&accounts, output_mint, token_out, clock.clone()) {
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
            match raydium_clmm.swap_base_in(&accounts, output_mint, token_out, clock.clone()) {
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
            match raydium_clmm.swap_base_out(&accounts, input_mint, sol_out, clock.clone()) {
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
}
