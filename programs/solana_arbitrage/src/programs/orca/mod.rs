pub mod libraries;
pub mod states;

use self::libraries::{swap_math, tick_math};
use self::states::{OracleSimple, TickArraySimple, WhirlpoolSimple, FEE_RATE_HARD_LIMIT};
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

/// Calculate price from sqrt_price_x64
fn sqrt_price_to_price(sqrt_price_x64: u128) -> f64 {
    let sqrt_price = sqrt_price_x64 as f64 / (1u128 << 64) as f64;
    let raw_price = sqrt_price * sqrt_price;
    raw_price
}

// ============================================================================
// OrcaWhirlpool Implementation
// ============================================================================

#[derive(Clone)]
pub struct OrcaWhirlpool<'info> {
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
    pub price: f64,
    pub inverse_price: f64,
    pub start_index: usize,
    pub end_index: usize,
    pub phantom: PhantomData<&'info ()>,
}

impl<'info> ProgramMeta for OrcaWhirlpool<'info> {
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

    fn swap_base_in<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        _clock: Clock,
    ) -> Result<u64> {
        let token_a_account = &accounts[self.start_index + Self::TOKEN_A_IDX];
        let token_b_account = &accounts[self.start_index + Self::TOKEN_B_IDX];

        let a_to_b = input_mint == self.base_token_pk;

        let (input_token_account, output_token_account) = if a_to_b {
            (token_a_account, token_b_account)
        } else {
            (token_b_account, token_a_account)
        };

        // Account for input transfer fees
        let transfer_fee = get_transfer_fee(input_token_account, amount_in)?;
        let actual_amount_in = amount_in.saturating_sub(transfer_fee);

        // Borrow tick array data at this level to avoid nested stack frames
        let data_0 = accounts[self.start_index + Self::TICK_ARRAY_0_IDX].try_borrow_data().ok();
        let data_1 = accounts[self.start_index + Self::TICK_ARRAY_1_IDX].try_borrow_data().ok();
        let data_2 = accounts[self.start_index + Self::TICK_ARRAY_2_IDX].try_borrow_data().ok();

        // Calculate swap output using tick traversal
        let amount_out = self.calculate_swap_base_in(
            actual_amount_in,
            a_to_b,
            &data_0,
            &data_1,
            &data_2,
        )?;

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
        let token_a_account = &accounts[self.start_index + Self::TOKEN_A_IDX];
        let token_b_account = &accounts[self.start_index + Self::TOKEN_B_IDX];

        // For swap_base_out, output_mint determines direction
        // If output is token_b, we're swapping a->b
        let a_to_b = output_mint == self.quote_token_pk;

        let (input_token_account, output_token_account) = if a_to_b {
            (token_a_account, token_b_account)
        } else {
            (token_b_account, token_a_account)
        };

        // Account for output transfer fees - need more output to cover fees
        let out_transfer_fee = get_transfer_inverse_fee(output_token_account, amount_out)?;
        let amount_out_with_fee = amount_out
            .checked_add(out_transfer_fee)
            .ok_or(error!(crate::programs::SolarBError::FeeOverflow))?;

        // Borrow tick array data at this level to avoid nested stack frames
        let data_0 = accounts[self.start_index + Self::TICK_ARRAY_0_IDX].try_borrow_data().ok();
        let data_1 = accounts[self.start_index + Self::TICK_ARRAY_1_IDX].try_borrow_data().ok();
        let data_2 = accounts[self.start_index + Self::TICK_ARRAY_2_IDX].try_borrow_data().ok();

        // Calculate required input
        let amount_in = self.calculate_swap_base_out(
            amount_out_with_fee,
            a_to_b,
            &data_0,
            &data_1,
            &data_2,
        )?;

        // Account for input transfer fees
        let in_transfer_fee = get_transfer_inverse_fee(input_token_account, amount_in)?;
        let final_amount_in = amount_in
            .checked_add(in_transfer_fee)
            .ok_or(error!(crate::programs::SolarBError::FeeOverflow))?;

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
        let vault_a = &accounts[self.start_index + Self::VAULT_A_IDX];
        let vault_b = &accounts[self.start_index + Self::VAULT_B_IDX];
        let token_a = &accounts[self.start_index + Self::TOKEN_A_IDX];
        let token_b = &accounts[self.start_index + Self::TOKEN_B_IDX];
        let oracle = &accounts[self.start_index + Self::ORACLE_IDX];
        let memo = &accounts[self.start_index + Self::MEMO_IDX];
        let tick_array_0 = &accounts[self.start_index + Self::TICK_ARRAY_0_IDX];
        let tick_array_1 = &accounts[self.start_index + Self::TICK_ARRAY_1_IDX];
        let tick_array_2 = &accounts[self.start_index + Self::TICK_ARRAY_2_IDX];

        let a_to_b = input_mint == self.base_token_pk;

        let (user_token_account_a, user_token_account_b, token_program_a, token_program_b) = if mint_1_account.key == token_a.key {
            (user_mint_1_token_account, user_mint_2_token_account, mint_1_token_program, mint_2_token_program)
        } else {
            (user_mint_2_token_account, user_mint_1_token_account, mint_2_token_program, mint_1_token_program)
        };

        // Build swap instruction
        let metas = vec![
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
        let mut data = vec![0x2b, 0x04, 0xed, 0x0b, 0x1a, 0xc9, 0x1e, 0x62];
        data.extend_from_slice(&amount_in.to_le_bytes());              // amount: u64
        data.extend_from_slice(&min_amount_out.unwrap_or(0).to_le_bytes()); // other_amount_threshold: u64
        data.extend_from_slice(&0u128.to_le_bytes());                  // sqrt_price_limit: u128 = 0
        data.push(1);                                                  // amount_specified_is_input: bool = true
        data.push(if a_to_b { 1 } else { 0 });                        // a_to_b: bool
        // remaining_accounts_info: Option<RemainingAccountsInfo> = None (Borsh: 0x00)
        // For Token-2022 transfer hooks, serialize as:
        //   0x01 (Some) | vec_len: u32 LE | [AccountsType: u8, length: u8] per slice
        // AccountsType: TransferHookA=0, TransferHookB=1, TransferHookReward=2,
        //   TransferHookInput=3, TransferHookIntermediate=4, TransferHookOutput=5,
        //   SupplementalTickArrays=6, SupplementalTickArraysOne=7, SupplementalTickArraysTwo=8
        data.push(0);

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data,
        };

        let accounts_vec: Vec<AccountInfo<'a>> = vec![
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

        invoke(&swap_ix, &accounts_vec)?;
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
        let vault_a = &accounts[self.start_index + Self::VAULT_A_IDX];
        let vault_b = &accounts[self.start_index + Self::VAULT_B_IDX];
        let tick_array_0 = &accounts[self.start_index + Self::TICK_ARRAY_0_IDX];
        let tick_array_1 = &accounts[self.start_index + Self::TICK_ARRAY_1_IDX];
        let tick_array_2 = &accounts[self.start_index + Self::TICK_ARRAY_2_IDX];
        let oracle = &accounts[self.start_index + Self::ORACLE_IDX];

        let a_to_b = input_mint == self.base_token_pk;

        let (
            input_token_program,
            output_token_program,
            user_input_account,
            user_output_account,
            input_vault,
            output_vault,
            input_mint_acc,
            output_mint_acc,
        ) = if a_to_b {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
                vault_a,
                vault_b,
                mint_1_account,
                mint_2_account,
            )
        } else {
            (
                mint_2_token_program,
                mint_1_token_program,
                user_mint_2_token_account,
                user_mint_1_token_account,
                vault_b,
                vault_a,
                mint_2_account,
                mint_1_account,
            )
        };

        let metas = vec![
            AccountMeta::new_readonly(*input_token_program.key, false),
            AccountMeta::new_readonly(*output_token_program.key, false),
            AccountMeta::new_readonly(anchor_spl::associated_token::ID, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new_readonly(*input_mint_acc.key, false),
            AccountMeta::new_readonly(*output_mint_acc.key, false),
            AccountMeta::new(*user_input_account.key, false),
            AccountMeta::new(*input_vault.key, false),
            AccountMeta::new(*user_output_account.key, false),
            AccountMeta::new(*output_vault.key, false),
            AccountMeta::new(*tick_array_0.key, false),
            AccountMeta::new(*tick_array_1.key, false),
            AccountMeta::new(*tick_array_2.key, false),
            AccountMeta::new(*oracle.key, false),
        ];

        // Swap discriminator: swap_v2 (SHA256("global:swap_v2")[..8])
        let mut data = vec![0x2b, 0x04, 0xed, 0x0b, 0x1a, 0xc9, 0x1e, 0x62];
        data.extend_from_slice(&amount_out.unwrap_or(0).to_le_bytes()); // amount: u64 (specified output)
        data.extend_from_slice(&max_amount_in.to_le_bytes());           // other_amount_threshold: u64 (max input)
        data.extend_from_slice(&0u128.to_le_bytes());                   // sqrt_price_limit: u128 = 0
        data.push(0);                                                   // amount_specified_is_input: bool = false
        data.push(if a_to_b { 1 } else { 0 });                         // a_to_b: bool
        // remaining_accounts_info: Option<RemainingAccountsInfo> = None (Borsh: 0x00)
        data.push(0);

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data,
        };

        let accounts_vec: Vec<AccountInfo<'a>> = vec![
            input_token_program.clone(),
            output_token_program.clone(),
            payer.clone(),
            payer.clone(),
            pool_id.clone(),
            input_mint_acc.clone(),
            output_mint_acc.clone(),
            user_input_account.clone(),
            input_vault.clone(),
            user_output_account.clone(),
            output_vault.clone(),
            tick_array_0.clone(),
            tick_array_1.clone(),
            tick_array_2.clone(),
            oracle.clone(),
        ];

        invoke(&swap_ix, &accounts_vec)?;
        Ok(())
    }

    fn log_accounts<'a>(&self, _accounts: &[AccountInfo<'a>]) -> Result<()> {
        msg!(
            "Orca Whirlpool: pool={}, mint_a={}, mint_b={}, sqrt_price={}, tick={}",
            self.pool_id,
            self.base_token_pk,
            self.quote_token_pk,
            self.sqrt_price,
            self.tick_current_index,
        );
        Ok(())
    }

    fn get_max_amounts_in_out(&self, input_mint: Pubkey) -> Result<(u64, u64)> {
        let a_to_b = input_mint == self.base_token_pk;

        // For concentrated liquidity, max amounts depend on the liquidity depth
        // Estimate based on moving the price to the boundary of the current tick range
        let tick_spacing = self.tick_spacing as i32;

        // Get the current tick boundaries
        let tick_lower = (self.tick_current_index / tick_spacing) * tick_spacing;
        let tick_upper = tick_lower + tick_spacing;

        // Get sqrt prices at boundaries
        let sqrt_price_lower =
            tick_math::get_sqrt_price_at_tick(tick_lower).unwrap_or(tick_math::MIN_SQRT_PRICE_X64);
        let sqrt_price_upper =
            tick_math::get_sqrt_price_at_tick(tick_upper).unwrap_or(tick_math::MAX_SQRT_PRICE_X64);

        // Calculate max amounts based on direction
        let (max_in, max_out) = if a_to_b {
            // Moving price down - max input is token A to reach lower bound
            let max_input = libraries::liquidity_math::get_amount_in_for_liquidity(
                self.sqrt_price,
                sqrt_price_lower,
                self.liquidity,
                true,
            )
            .unwrap_or(u64::MAX);

            let max_output = libraries::liquidity_math::get_amount_out_for_liquidity(
                self.sqrt_price,
                sqrt_price_lower,
                self.liquidity,
                true,
            )
            .unwrap_or(0);

            (max_input, max_output)
        } else {
            // Moving price up - max input is token B to reach upper bound
            let max_input = libraries::liquidity_math::get_amount_in_for_liquidity(
                self.sqrt_price,
                sqrt_price_upper,
                self.liquidity,
                false,
            )
            .unwrap_or(u64::MAX);

            let max_output = libraries::liquidity_math::get_amount_out_for_liquidity(
                self.sqrt_price,
                sqrt_price_upper,
                self.liquidity,
                false,
            )
            .unwrap_or(0);

            (max_input, max_output)
        };

        Ok((max_in, max_out))
    }
}

impl<'info> OrcaWhirlpool<'info> {
    /// Orca Whirlpool Program ID
    pub const PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc");

    // Account indices
    pub const PROGRAM_ID_IDX: usize = 0;
    pub const POOL_ID_IDX: usize = 1;
    pub const VAULT_A_IDX: usize = 2;
    pub const VAULT_B_IDX: usize = 3;
    pub const TOKEN_A_IDX: usize = 4;
    pub const TOKEN_B_IDX: usize = 5;
    pub const ORACLE_IDX: usize = 6;
    pub const MEMO_IDX: usize = 7;
    pub const TICK_ARRAY_0_IDX: usize = 8;
    pub const TICK_ARRAY_1_IDX: usize = 9;
    pub const TICK_ARRAY_2_IDX: usize = 10;

    pub fn new(
        accounts: &[AccountInfo<'info>],
        start_index: usize,
        end_index: usize,
    ) -> Result<Self> {
        let pool_account = &accounts[start_index + Self::POOL_ID_IDX];

        // Parse pool state
        let pool_data = pool_account.try_borrow_data()?;
        let pool = WhirlpoolSimple::try_from_bytes(&pool_data)?;

        // Calculate prices from sqrt_price
        let price = sqrt_price_to_price(pool.sqrt_price);
        let inverse_price = if price > 0.0 { 1.0 / price } else { 0.0 };

        // Compute total fee rate (static + adaptive) once from Oracle account
        let oracle_account = &accounts[start_index + Self::ORACLE_IDX];
        let oracle_data = oracle_account.try_borrow_data().ok();
        let total_fee_rate = compute_total_fee_rate(
            pool.fee_rate,
            oracle_data.as_ref().map(|d| &***d),
        );

        Ok(OrcaWhirlpool {
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
            price,
            inverse_price,
            start_index,
            end_index,
            phantom: PhantomData,
        })
    }

    /// Get the tick boundaries for the current tick
    fn get_current_tick_boundaries(&self) -> (i32, i32) {
        let tick_spacing = self.tick_spacing as i32;
        let tick_lower = if self.tick_current_index >= 0 {
            (self.tick_current_index / tick_spacing) * tick_spacing
        } else {
            ((self.tick_current_index - tick_spacing + 1) / tick_spacing) * tick_spacing
        };
        let tick_upper = tick_lower + tick_spacing;
        (tick_lower, tick_upper)
    }

    /// Get the sqrt price at the next tick boundary in the swap direction
    fn get_next_tick_sqrt_price(&self, a_to_b: bool) -> Result<u128> {
        let (tick_lower, tick_upper) = self.get_current_tick_boundaries();

        let target_tick = if a_to_b { tick_lower } else { tick_upper };

        let clamped_tick = target_tick
            .max(tick_math::MIN_TICK)
            .min(tick_math::MAX_TICK);
        tick_math::get_sqrt_price_at_tick(clamped_tick)
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
                // Only parse this specific array if we need to check it
                // Dereference Ref<&mut [u8]> to get &[u8]
                if let Some(array) = TickArraySimple::try_from_bytes(&**data) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::prelude::Clock;
    use anchor_lang::solana_program::{account_info::AccountInfo, pubkey::Pubkey, system_program};
    use solana_client::nonblocking::rpc_client::RpcClient;
    use solana_sdk::pubkey::Pubkey as SdkPubkey;
    use crate::utils::utils::get_mint_decimals;
    use crate::programs::orca::states::{TICK_ARRAY_SIZE, get_tick_array_start_index};

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
    async fn test_orca_whirlpool_pool_parsing() {
        use anchor_client::Cluster;

        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        // SOL/USDC Whirlpool on mainnet
        let pool_id_key = Pubkey::from_str_const("HJPjoWUrhoZzkNfRpHuieeFk9WcZWjwy6PBjZ81ngndJ");

        eprintln!("Testing Orca Whirlpool pool parsing: {}", pool_id_key);

        let pool_account_result = rpc_client
            .get_account(&SdkPubkey::try_from(pool_id_key.to_bytes().as_ref()).unwrap())
            .await;

        let pool_account = match pool_account_result {
            Ok(acc) => acc,
            Err(e) => {
                eprintln!("Warning: Could not fetch pool account: {:?}", e);
                return;
            }
        };

        // Debug: print actual discriminator
        if pool_account.data.len() >= 8 {
            eprintln!("Actual discriminator: {:02x?}", &pool_account.data[0..8]);
            eprintln!(
                "Expected discriminator: {:02x?}",
                states::whirlpool::WHIRLPOOL_DISCRIMINATOR
            );
        }

        // Parse pool state
        let pool = match WhirlpoolSimple::try_from_bytes(&pool_account.data) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Failed to parse pool state: {:?}", e);
                return;
            }
        };

        // Copy packed struct fields to avoid unaligned reference errors
        let liquidity = pool.liquidity;
        let sqrt_price = pool.sqrt_price;
        let tick_current_index = pool.tick_current_index;
        let tick_spacing = pool.tick_spacing;
        let fee_rate = pool.fee_rate;

        eprintln!("\n=== Pool State ===");
        eprintln!("Token Mint A: {}", pool.token_mint_a);
        eprintln!("Token Mint B: {}", pool.token_mint_b);
        eprintln!("Liquidity: {}", liquidity);
        eprintln!("Sqrt Price: {}", sqrt_price);
        eprintln!("Current Tick: {}", tick_current_index);
        eprintln!("Tick Spacing: {}", tick_spacing);
        eprintln!("Fee Rate: {} ({}%)", fee_rate, fee_rate as f64 / 10000.0); // 1_000_000 denominator, displayed as %

        eprintln!("\n✓ Pool parsing test passed!");
    }

    #[tokio::test]
    async fn test_orca_whirlpool_round_trip_swap() {
        use anchor_client::Cluster;

        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        // SOL/USDC Whirlpool on mainnet
        let pool_id_key = Pubkey::from_str_const("HktfL7iwGKT5QHjywQkcDnZXScoh811k7akrMZJkCcEF");
        
        // let pool_id_key = Pubkey::from_str_const("Czfq3xZZDmsdGdUyrNLtRhGc47cXcZtLG4crryfu44zE");
        // let pool_id_key = Pubkey::from_str_const("6jwmmjnx3mDbA6QauSZ7DY8Z1B8wZncxXM1tJd2unpuS");



        eprintln!("Testing Orca Whirlpool round trip swap: {}", pool_id_key);

        // Fetch pool account
        let pool_account_result = rpc_client
            .get_account(&SdkPubkey::try_from(pool_id_key.to_bytes().as_ref()).unwrap())
            .await;

        let pool_account = match pool_account_result {
            Ok(acc) => acc,
            Err(e) => {
                eprintln!("Warning: Could not fetch pool account: {:?}", e);
                return;
            }
        };

        // Parse pool state
        let pool = match WhirlpoolSimple::try_from_bytes(&pool_account.data) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Failed to parse pool state: {:?}", e);
                return;
            }
        };

        // Copy packed struct fields to avoid unaligned reference errors
        let liquidity = pool.liquidity;
        let sqrt_price = pool.sqrt_price;
        let tick_current_index = pool.tick_current_index;

        eprintln!("Pool state parsed successfully");
        eprintln!("Token Mint A: {}", pool.token_mint_a);
        eprintln!("Token Mint B: {}", pool.token_mint_b);
        eprintln!("Liquidity: {}", liquidity);
        eprintln!("Sqrt Price: {}", sqrt_price);
        eprintln!("Current Tick: {}", tick_current_index);

        // Fetch vault accounts
        let vault_a_result = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_vault_a.to_bytes().as_ref()).unwrap())
            .await;
        let vault_b_result = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_vault_b.to_bytes().as_ref()).unwrap())
            .await;

        if vault_a_result.is_err() || vault_b_result.is_err() {
            eprintln!("Warning: Could not fetch vault accounts");
            eprintln!("Vault A fetch: {:?}", vault_a_result.as_ref().err());
            eprintln!("Vault B fetch: {:?}", vault_b_result.as_ref().err());
            return;
        }

        let vault_a_account = vault_a_result.unwrap();
        let vault_b_account = vault_b_result.unwrap();

        // Fetch token mint accounts
        let token_a_result = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_mint_a.to_bytes().as_ref()).unwrap())
            .await;
        let token_b_result = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_mint_b.to_bytes().as_ref()).unwrap())
            .await;

        if token_a_result.is_err() || token_b_result.is_err() {
            eprintln!("Warning: Could not fetch token mint accounts");
            return;
        }

        let token_a_account = token_a_result.unwrap();
        let token_b_account = token_b_result.unwrap();

        // Derive tick array PDAs based on current tick and tick spacing
        let tick_spacing = pool.tick_spacing;
        let ticks_in_array = TICK_ARRAY_SIZE * tick_spacing as i32;

        let start_tick_index_0 = get_tick_array_start_index(pool.tick_current_index, tick_spacing);
        let start_tick_index_1 = start_tick_index_0 - ticks_in_array;
        let start_tick_index_2 = start_tick_index_0 + ticks_in_array;

        let (tick_array_0_key, _) = Pubkey::find_program_address(
            &[
                b"tick_array",
                pool_id_key.as_ref(),
                start_tick_index_0.to_string().as_bytes(),
            ],
            &OrcaWhirlpool::PROGRAM_ID,
        );
        let (tick_array_1_key, _) = Pubkey::find_program_address(
            &[
                b"tick_array",
                pool_id_key.as_ref(),
                start_tick_index_1.to_string().as_bytes(),
            ],
            &OrcaWhirlpool::PROGRAM_ID,
        );
        let (tick_array_2_key, _) = Pubkey::find_program_address(
            &[
                b"tick_array",
                pool_id_key.as_ref(),
                start_tick_index_2.to_string().as_bytes(),
            ],
            &OrcaWhirlpool::PROGRAM_ID,
        );

        // Derive oracle PDA
        let (oracle_key, _) = Pubkey::find_program_address(
            &[b"oracle", pool_id_key.as_ref()],
            &OrcaWhirlpool::PROGRAM_ID,
        );

        eprintln!("Tick array 0 (start={}): {}", start_tick_index_0, tick_array_0_key);
        eprintln!("Tick array 1 (start={}): {}", start_tick_index_1, tick_array_1_key);
        eprintln!("Tick array 2 (start={}): {}", start_tick_index_2, tick_array_2_key);
        eprintln!("Oracle: {}", oracle_key);

        // Fetch tick array and oracle accounts from RPC
        // Some tick arrays may not exist on-chain (no positions in that range), use empty mock as fallback
        let program_id_key = OrcaWhirlpool::PROGRAM_ID;

        let tick_array_0_account = match rpc_client
            .get_account(&SdkPubkey::try_from(tick_array_0_key.to_bytes().as_ref()).unwrap())
            .await
        {
            Ok(acct) => account_to_account_info(tick_array_0_key, acct),
            Err(e) => {
                eprintln!("Tick array 0 not found ({}), using empty mock", e);
                create_mock_account_info_with_data(tick_array_0_key, program_id_key, Some(vec![0u8; 1000]))
            }
        };
        let tick_array_1_account = match rpc_client
            .get_account(&SdkPubkey::try_from(tick_array_1_key.to_bytes().as_ref()).unwrap())
            .await
        {
            Ok(acct) => account_to_account_info(tick_array_1_key, acct),
            Err(e) => {
                eprintln!("Tick array 1 not found ({}), using empty mock", e);
                create_mock_account_info_with_data(tick_array_1_key, program_id_key, Some(vec![0u8; 1000]))
            }
        };
        let tick_array_2_account = match rpc_client
            .get_account(&SdkPubkey::try_from(tick_array_2_key.to_bytes().as_ref()).unwrap())
            .await
        {
            Ok(acct) => account_to_account_info(tick_array_2_key, acct),
            Err(e) => {
                eprintln!("Tick array 2 not found ({}), using empty mock", e);
                create_mock_account_info_with_data(tick_array_2_key, program_id_key, Some(vec![0u8; 1000]))
            }
        };
        let oracle_account = match rpc_client
            .get_account(&SdkPubkey::try_from(oracle_key.to_bytes().as_ref()).unwrap())
            .await
        {
            Ok(acct) => account_to_account_info(oracle_key, acct),
            Err(e) => {
                eprintln!("Oracle not found ({}), using empty mock", e);
                create_mock_account_info_with_data(oracle_key, program_id_key, Some(vec![0u8; 1000]))
            }
        };

        // Create AccountInfo instances
        let program_id_account =
            create_mock_account_info_with_data(program_id_key, system_program::id(), None);

        let pool_id_account_info = account_to_account_info(pool_id_key, pool_account);
        let vault_a_account_info = account_to_account_info(pool.token_vault_a, vault_a_account);
        let vault_b_account_info = account_to_account_info(pool.token_vault_b, vault_b_account);
        let token_a_account_info = account_to_account_info(pool.token_mint_a, token_a_account);
        let token_b_account_info = account_to_account_info(pool.token_mint_b, token_b_account);

        let accounts = vec![
            program_id_account,
            pool_id_account_info.clone(),
            vault_a_account_info.clone(),
            vault_b_account_info.clone(),
            token_a_account_info.clone(),
            token_b_account_info.clone(),
            tick_array_0_account.clone(),
            tick_array_1_account.clone(),
            tick_array_2_account.clone(),
            oracle_account.clone(),
        ];

        let orca_whirlpool = OrcaWhirlpool::new(&accounts, 0, accounts.len())
            .expect("Failed to create OrcaWhirlpool");

        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");

        eprintln!(
            "Price: {:?}, Inverse Price: {:?}",
            orca_whirlpool.price, orca_whirlpool.inverse_price
        );

        let token_a_account = &accounts[orca_whirlpool.start_index + OrcaWhirlpool::TOKEN_A_IDX];
        let token_b_account = &accounts[orca_whirlpool.start_index + OrcaWhirlpool::TOKEN_B_IDX];
        let mint_decimals_a = get_mint_decimals(token_a_account);
        let mint_decimals_b = get_mint_decimals(token_b_account);
        eprintln!(
            "Mint decimals A: {:?}, B: {:?}",
            mint_decimals_a, mint_decimals_b
        );

        // Test round trip: SOL -> TOKEN -> SOL
        let sol_in = 1_000_000_000; // 1 SOL

        let token_mint = if orca_whirlpool.base_token_pk == sol_mint {
            orca_whirlpool.quote_token_pk
        } else if orca_whirlpool.quote_token_pk == sol_mint {
            orca_whirlpool.base_token_pk
        } else {
            eprintln!("Warning: Pool does not contain SOL, skipping round trip test");
            return;
        };

        let (max_sol_in, max_token_out) = orca_whirlpool.get_max_amounts_in_out(sol_mint).unwrap();
        eprintln!("Max SOL IN: {:?} -> MAX TOKEN OUT: {:?}", max_sol_in as f64 / 1_000_000_000.0, max_token_out as f64 / 1_000_000.0);

        eprintln!("================================================");
        // Step 1: Swap SOL -> TOKEN
        let clock1 = get_clock(&rpc_client).await.unwrap();
        let token_out = orca_whirlpool
            .swap_base_in(&accounts, sol_mint, sol_in, clock1.clone())
            .expect("swap_base_in failed");
        eprintln!(
            "Step 1 (swap_base_in): {} SOL -> {} TOKEN",
            sol_in as f64 / 1_000_000_000.0,
            token_out as f64 / 1_000_000.0,
        );

        let max_sol_in = orca_whirlpool
            .swap_base_out(&accounts, token_mint, token_out, clock1.clone())
            .expect("swap_base_out failed");
        eprintln!(
            "Step 1 (swap_base_out): MAX SOL IN {} -> {} TOKEN OUT",
            max_sol_in as f64 / 1_000_000_000.0,
            token_out as f64 / 1_000_000.0,
        );

        eprintln!("================================================");

        // Step 2: Swap TOKEN -> SOL
        let sol_out = orca_whirlpool
            .swap_base_in(&accounts, token_mint, token_out, clock1.clone())
            .expect("second swap_base_in failed");
        eprintln!(
            "Step 2 (swap_base_in): {} TOKEN -> {} SOL",
            token_out as f64 / 1_000_000.0,
            sol_out as f64 / 1_000_000_000.0,
        );

        let max_token_in = orca_whirlpool
            .swap_base_out(&accounts, sol_mint, sol_out, clock1.clone())
            .expect("second swap_base_out failed");
        eprintln!(
            "Step 2 (swap_base_out): {} MAX TOKEN IN -> {} SOL OUT",
            max_token_in as f64 / 1_000_000.0,
            sol_out as f64 / 1_000_000_000.0
        );

        eprintln!("================================================");
        eprintln!("Round trip completed!");
    }
}
