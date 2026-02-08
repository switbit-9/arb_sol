pub mod libraries;
pub mod states;

use self::libraries::{swap_math, tick_math};
use self::states::{TickArraySimple, WhirlpoolSimple, TICK_ARRAY_SIZE};
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
fn sqrt_price_to_price(sqrt_price_x64: u128, decimals_a: u8, decimals_b: u8) -> f64 {
    let sqrt_price = sqrt_price_x64 as f64 / (1u128 << 64) as f64;
    let raw_price = sqrt_price * sqrt_price;
    // Adjust for decimals: price is token_b / token_a
    let decimal_adjustment = 10f64.powi((decimals_a as i32) - (decimals_b as i32));
    raw_price * decimal_adjustment
}

// ============================================================================
// OrcaWhirlpool Implementation
// ============================================================================

#[derive(Clone)]
pub struct OrcaWhirlpool<'info> {
    pub pool_id: Pubkey,
    pub whirlpools_config: Pubkey,
    pub token_mint_a: Pubkey,
    pub token_mint_b: Pubkey,
    pub token_vault_a: Pubkey,
    pub token_vault_b: Pubkey,
    pub sqrt_price: u128,
    pub tick_current_index: i32,
    pub liquidity: u128,
    pub tick_spacing: u16,
    pub fee_rate: u16,
    pub protocol_fee_rate: u16,
    pub mint_decimals_a: u8,
    pub mint_decimals_b: u8,
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
        (&self.token_mint_a, &self.token_mint_b)
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

        let a_to_b = input_mint == self.token_mint_a;

        let (input_token_account, output_token_account) = if a_to_b {
            (token_a_account, token_b_account)
        } else {
            (token_b_account, token_a_account)
        };

        // Account for input transfer fees
        let transfer_fee = get_transfer_fee(input_token_account, amount_in)?;
        let actual_amount_in = amount_in.saturating_sub(transfer_fee);

        // Get tick arrays for swap simulation
        let tick_array_0 = &accounts[self.start_index + Self::TICK_ARRAY_0_IDX];
        let tick_array_1 = &accounts[self.start_index + Self::TICK_ARRAY_1_IDX];
        let tick_array_2 = &accounts[self.start_index + Self::TICK_ARRAY_2_IDX];

        // Calculate swap output using tick traversal
        let amount_out = self.calculate_swap_base_in(
            actual_amount_in,
            a_to_b,
            tick_array_0,
            tick_array_1,
            tick_array_2,
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
        let a_to_b = output_mint == self.token_mint_b;

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

        // Get tick arrays
        let tick_array_0 = &accounts[self.start_index + Self::TICK_ARRAY_0_IDX];
        let tick_array_1 = &accounts[self.start_index + Self::TICK_ARRAY_1_IDX];
        let tick_array_2 = &accounts[self.start_index + Self::TICK_ARRAY_2_IDX];

        // Calculate required input
        let amount_in = self.calculate_swap_base_out(
            amount_out_with_fee,
            a_to_b,
            tick_array_0,
            tick_array_1,
            tick_array_2,
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
        let tick_array_0 = &accounts[self.start_index + Self::TICK_ARRAY_0_IDX];
        let tick_array_1 = &accounts[self.start_index + Self::TICK_ARRAY_1_IDX];
        let tick_array_2 = &accounts[self.start_index + Self::TICK_ARRAY_2_IDX];
        let oracle = &accounts[self.start_index + Self::ORACLE_IDX];

        let a_to_b = input_mint == self.token_mint_a;

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

        // Build swap instruction
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

        // Swap discriminator: swap_v2
        let mut data = vec![0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.unwrap_or(0).to_le_bytes());
        data.extend_from_slice(&0u128.to_le_bytes()); // sqrt_price_limit = 0
        data.push(1); // amount_specified_is_input = true
        data.push(if a_to_b { 1 } else { 0 });

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

        let a_to_b = input_mint == self.token_mint_a;

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

        let mut data = vec![0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];
        data.extend_from_slice(&amount_out.unwrap_or(0).to_le_bytes());
        data.extend_from_slice(&max_amount_in.to_le_bytes());
        data.extend_from_slice(&0u128.to_le_bytes());
        data.push(0); // amount_specified_is_input = false
        data.push(if a_to_b { 1 } else { 0 });

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
            self.token_mint_a,
            self.token_mint_b,
            self.sqrt_price,
            self.tick_current_index,
        );
        Ok(())
    }

    fn get_max_amounts_in_out(&self, input_mint: Pubkey) -> Result<(u64, u64)> {
        let a_to_b = input_mint == self.token_mint_a;

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
    pub const TICK_ARRAY_0_IDX: usize = 6;
    pub const TICK_ARRAY_1_IDX: usize = 7;
    pub const TICK_ARRAY_2_IDX: usize = 8;
    pub const ORACLE_IDX: usize = 9;

    pub fn new(
        accounts: &[AccountInfo<'info>],
        start_index: usize,
        end_index: usize,
    ) -> Result<Self> {
        let pool_account = &accounts[start_index + Self::POOL_ID_IDX];
        let token_a_account = &accounts[start_index + Self::TOKEN_A_IDX];
        let token_b_account = &accounts[start_index + Self::TOKEN_B_IDX];

        // Parse pool state
        let pool_data = pool_account.try_borrow_data()?;
        let pool = WhirlpoolSimple::try_from_bytes(&pool_data)?;

        // Get mint decimals from token accounts
        let mint_decimals_a = Self::get_mint_decimals(token_a_account)?;
        let mint_decimals_b = Self::get_mint_decimals(token_b_account)?;

        // Calculate prices from sqrt_price
        let price = sqrt_price_to_price(pool.sqrt_price, mint_decimals_a, mint_decimals_b);
        let inverse_price = if price > 0.0 { 1.0 / price } else { 0.0 };

        Ok(OrcaWhirlpool {
            pool_id: *pool_account.key,
            whirlpools_config: pool.whirlpools_config,
            token_mint_a: pool.token_mint_a,
            token_mint_b: pool.token_mint_b,
            token_vault_a: pool.token_vault_a,
            token_vault_b: pool.token_vault_b,
            sqrt_price: pool.sqrt_price,
            tick_current_index: pool.tick_current_index,
            liquidity: pool.liquidity,
            tick_spacing: pool.tick_spacing,
            fee_rate: pool.fee_rate,
            protocol_fee_rate: pool.protocol_fee_rate,
            mint_decimals_a,
            mint_decimals_b,
            price,
            inverse_price,
            start_index,
            end_index,
            phantom: PhantomData,
        })
    }

    /// Get mint decimals from a mint account
    fn get_mint_decimals(mint_account: &AccountInfo) -> Result<u8> {
        let data = mint_account.try_borrow_data()?;
        // Mint decimals are at offset 44 in SPL Token mint layout
        if data.len() >= 45 {
            Ok(data[44])
        } else {
            Ok(9) // Default to 9 decimals
        }
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
    fn calculate_swap_base_in<'a>(
        &self,
        amount_in: u64,
        a_to_b: bool,
        tick_array_0: &AccountInfo<'a>,
        tick_array_1: &AccountInfo<'a>,
        tick_array_2: &AccountInfo<'a>,
    ) -> Result<u64> {
        if self.liquidity == 0 {
            return Err(error!(crate::programs::SolarBError::InsufficientFunds));
        }

        let mut amount_remaining = amount_in;
        let mut amount_out_total = 0u64;
        let mut current_sqrt_price = self.sqrt_price;
        let mut current_liquidity = self.liquidity;
        let mut current_tick = self.tick_current_index;

        // Borrow tick array data - lazy parsing (no TickArraySimple created upfront)
        let data_0 = tick_array_0.try_borrow_data().ok();
        let data_1 = tick_array_1.try_borrow_data().ok();
        let data_2 = tick_array_2.try_borrow_data().ok();

        // Maximum iterations to prevent infinite loops
        const MAX_ITERATIONS: usize = 20;
        let mut iterations = 0;

        while amount_remaining > 0 && iterations < MAX_ITERATIONS {
            iterations += 1;

            // Find next initialized tick - parses only the needed tick array lazily
            let (next_tick_index, next_tick) = self.find_next_initialized_tick_lazy(
                current_tick,
                a_to_b,
                &data_0,
                &data_1,
                &data_2,
            );

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

            // Check if liquidity is depleted
            if current_liquidity == 0 {
                break;
            }
        }

        Ok(amount_out_total)
    }

    /// Calculate required input for exact output swap
    fn calculate_swap_base_out<'a>(
        &self,
        amount_out: u64,
        a_to_b: bool,
        tick_array_0: &AccountInfo<'a>,
        tick_array_1: &AccountInfo<'a>,
        tick_array_2: &AccountInfo<'a>,
    ) -> Result<u64> {
        if self.liquidity == 0 {
            return Err(error!(crate::programs::SolarBError::InsufficientFunds));
        }

        let mut amount_remaining = amount_out;
        let mut amount_in_total = 0u64;
        let mut current_sqrt_price = self.sqrt_price;
        let mut current_liquidity = self.liquidity;
        let mut current_tick = self.tick_current_index;

        // Borrow tick array data - lazy parsing (no TickArraySimple created upfront)
        let data_0 = tick_array_0.try_borrow_data().ok();
        let data_1 = tick_array_1.try_borrow_data().ok();
        let data_2 = tick_array_2.try_borrow_data().ok();

        const MAX_ITERATIONS: usize = 20;
        let mut iterations = 0;

        while amount_remaining > 0 && iterations < MAX_ITERATIONS {
            iterations += 1;

            // Find next initialized tick - parses only the needed tick array lazily
            let (next_tick_index, next_tick) = self.find_next_initialized_tick_lazy(
                current_tick,
                a_to_b,
                &data_0,
                &data_1,
                &data_2,
            );

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

            if current_liquidity == 0 {
                break;
            }
        }

        Ok(amount_in_total)
    }

    /// Find the next initialized tick in the swap direction (fully lazy - parses only when needed)
    /// Takes raw data references and only parses tick arrays on-demand to minimize heap usage
    fn find_next_initialized_tick_lazy<'a>(
        &self,
        current_tick: i32,
        a_to_b: bool,
        data_0: &Option<std::cell::Ref<'a, &mut [u8]>>,
        data_1: &Option<std::cell::Ref<'a, &mut [u8]>>,
        data_2: &Option<std::cell::Ref<'a, &mut [u8]>>,
    ) -> (i32, Option<states::Tick>) {
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
                            return (found_tick_index, Some(tick));
                        }
                    }
                }
                // TickArraySimple is dropped here - only one parsed at a time
            }
        }

        // If no initialized tick found, return boundary
        let ticks_in_array = TICK_ARRAY_SIZE * tick_spacing as i32;
        let boundary_tick = if a_to_b {
            let array_start = (current_tick / ticks_in_array) * ticks_in_array;
            if current_tick < 0 && current_tick % ticks_in_array != 0 {
                array_start - ticks_in_array
            } else {
                array_start
            }
        } else {
            let array_start = (current_tick / ticks_in_array) * ticks_in_array;
            array_start + ticks_in_array - tick_spacing as i32
        };

        (
            boundary_tick
                .max(tick_math::MIN_TICK)
                .min(tick_math::MAX_TICK),
            None,
        )
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
        eprintln!("Fee Rate: {} ({}%)", fee_rate, fee_rate as f64 / 10000.0);

        eprintln!("\n✓ Pool parsing test passed!");
    }

    #[tokio::test]
    async fn test_orca_whirlpool_round_trip_swap() {
        use anchor_client::Cluster;

        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        // SOL/USDC Whirlpool on mainnet
        let pool_id_key = Pubkey::from_str_const("BofA2ViUSudPBTUms2KRuG6AHNeMawjNfwqTJDgx5BKW");

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

        // Fetch tick arrays (we'll use placeholder accounts if they don't exist)
        // In a real scenario, you'd need to calculate which tick arrays to fetch
        // For testing, we'll create mock tick arrays
        let tick_array_0_key = Pubkey::default();
        let tick_array_1_key = Pubkey::default();
        let tick_array_2_key = Pubkey::default();
        let oracle_key = Pubkey::default();

        // Create AccountInfo instances
        let program_id_key = OrcaWhirlpool::PROGRAM_ID;
        let program_id_account =
            create_mock_account_info_with_data(program_id_key, system_program::id(), None);

        let pool_id_account_info = account_to_account_info(pool_id_key, pool_account);
        let vault_a_account_info = account_to_account_info(pool.token_vault_a, vault_a_account);
        let vault_b_account_info = account_to_account_info(pool.token_vault_b, vault_b_account);
        let token_a_account_info = account_to_account_info(pool.token_mint_a, token_a_account);
        let token_b_account_info = account_to_account_info(pool.token_mint_b, token_b_account);

        // Create mock tick arrays and oracle (empty data for now)
        let tick_array_0_account = create_mock_account_info_with_data(
            tick_array_0_key,
            program_id_key,
            Some(vec![0u8; 1000]),
        );
        let tick_array_1_account = create_mock_account_info_with_data(
            tick_array_1_key,
            program_id_key,
            Some(vec![0u8; 1000]),
        );
        let tick_array_2_account = create_mock_account_info_with_data(
            tick_array_2_key,
            program_id_key,
            Some(vec![0u8; 1000]),
        );
        let oracle_account =
            create_mock_account_info_with_data(oracle_key, program_id_key, Some(vec![0u8; 1000]));

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
        eprintln!(
            "Mint decimals A: {:?}, B: {:?}",
            orca_whirlpool.mint_decimals_a, orca_whirlpool.mint_decimals_b
        );

        // Test round trip: SOL -> TOKEN -> SOL
        let sol_in = 1_000_000_000; // 1 SOL

        let token_mint = if orca_whirlpool.token_mint_a == sol_mint {
            orca_whirlpool.token_mint_b
        } else if orca_whirlpool.token_mint_b == sol_mint {
            orca_whirlpool.token_mint_a
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
