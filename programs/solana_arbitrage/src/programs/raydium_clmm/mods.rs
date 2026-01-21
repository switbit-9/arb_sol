


use anchor_lang::prelude::*;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    account_info::{next_account_info, AccountInfo},
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use bytemuck;

use crate::programs::raydium_clmm::states::{AmmConfig, PoolState};
use crate::programs::raydium_clmm::swap_v2::swap_internal;

#[derive(Clone)]
pub struct RaydiumCLMM<'info> {
    pub accounts: Vec<AccountInfo<'info>>,
    pub program_id: AccountInfo<'info>,
    pub pool_id: AccountInfo<'info>,
    pub base_vault: AccountInfo<'info>,
    pub quote_vault: AccountInfo<'info>,
    pub base_token: AccountInfo<'info>,
    pub quote_token: AccountInfo<'info>,

}

impl<'info> ProgramMeta for RaydiumCLMM<'info> {
    fn get_id(&self) -> &Pubkey {
        &Self::PROGRAM_ID
    }

    fn get_vaults(&self) -> (&AccountInfo<'_>, &AccountInfo<'_>) {
        unsafe {
            (
                &*(&self.base_vault as *const AccountInfo<'info> as *const AccountInfo<'_>),
                &*(&self.quote_vault as *const AccountInfo<'info> as *const AccountInfo<'_>),
            )
        }
    }

    fn swap_base_in(&self, input_mint: Pubkey, amount_in: u64, clock: Clock) -> Result<u64> {
        self.swap_base_in_impl(input_mint, amount_in, clock)
    }

    fn swap_base_out(&self, input_mint: Pubkey, amount_in: u64, clock: Clock) -> Result<u64> {
        self.swap_base_out_impl(input_mint, amount_in, clock)
    }

    // fn get_prices(&self) -> Result<(f64, f64)> {
    //     self.get_prices_impl()
    // }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (self.base_token.key, self.quote_token.key)
    }

    fn invoke_swap_base_in<'a>(
        &self,
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
        self.invoke_swap_base_in_impl(
            input_mint,
            max_amount_in,
            amount_out,
            payer,
            user_mint_1_token_account,
            user_mint_2_token_account,
            mint_1_account,
            mint_2_account,
            mint_1_token_program,
            mint_2_token_program,
        )
    }

    fn invoke_swap_base_out<'a>(
        &self,
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
        self.invoke_swap_base_out_impl(
            input_mint,
            min_amount_out.unwrap_or(0), // RaydiumCPSwap has reversed params
            amount_in,
            payer,
            user_mint_1_token_account,
            user_mint_2_token_account,
            mint_1_account,
            mint_2_account,
            mint_1_token_program,
            mint_2_token_program,
        )
    }

    fn log_accounts(&self) -> Result<()> {
        msg!(
            "Raydium CPMM accounts: pool={}, base_vault={}, quote_vault={}, base_token={}, quote_token={}",
            self.pool_id.key,
            self.base_vault.key,
            self.quote_vault.key,
            self.base_token.key,
            self.quote_token.key,
        );
        Ok(())
    }
}

impl<'info> RaydiumCLMM<'info> {
    pub const PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("CPMDWBwJDtYax9qW7AyRuVC19Cc4L4Vcy4n2BHAbHkCW");
    pub fn new(accounts: &[AccountInfo<'info>]) -> Result<Self> {
        let mut iter = accounts.iter();
        let program_id = next_account_info(&mut iter)?;
        let pool_id = next_account_info(&mut iter)?;
        let base_vault = next_account_info(&mut iter)?;
        let quote_vault = next_account_info(&mut iter)?;
        let base_token = next_account_info(&mut iter)?;
        let quote_token = next_account_info(&mut iter)?;
        // let amm_config = next_account_info(&mut iter)?;
        // let observation_key = next_account_info(&mut iter)?;

        Ok(RaydiumCPMM {
            accounts: accounts.to_vec(),
            pool_id: pool_id.clone(),
            program_id: program_id.clone(),
            base_vault: base_vault.clone(),
            quote_vault: quote_vault.clone(),
            base_token: base_token.clone(),
            quote_token: quote_token.clone(),
        })
    }

    fn get_bin_arrays_buy(&self) -> Option<Vec<AccountInfo<'info>>> {
        if self.accounts.len() <= 10 {
            return None;
        }

        let remaining = &self.accounts[10..];
        let sol_mint = native_mint::id();

        // Find position of SOL MINT separator
        let sol_mint_pos = remaining.iter().position(|acc| *acc.key == sol_mint);

        match sol_mint_pos {
            Some(pos) => {
                // Split at SOL MINT position - buy arrays are before SOL MINT
                let buy_slice = &remaining[..pos];
                if buy_slice.is_empty() {
                    None
                } else {
                    Some(buy_slice.iter().cloned().collect())
                }
            }
            None => {
                // No SOL MINT found, all remaining are buy arrays
                if remaining.is_empty() {
                    None
                } else {
                    Some(remaining.iter().cloned().collect())
                }
            }
        }
    }

    fn get_bin_arrays_sell(&self) -> Option<Vec<AccountInfo<'info>>> {
        if self.accounts.len() <= 10 {
            return None;
        }

        let remaining = &self.accounts[10..];
        let sol_mint = native_mint::id();

        // Find position of SOL MINT separator
        let sol_mint_pos = remaining.iter().position(|acc| *acc.key == sol_mint);

        match sol_mint_pos {
            Some(pos) => {
                // Split at SOL MINT position - sell arrays are after SOL MINT
                let after_sol = &remaining[pos + 1..]; // Skip SOL MINT itself
                if after_sol.is_empty() {
                    None
                } else {
                    Some(after_sol.iter().cloned().collect())
                }
            }
            None => {
                // No SOL MINT found, no sell arrays
                None
            }
        }
    }

    pub fn swap_base_in_impl(
        &self,
        input_mint: Pubkey,
        amount_in: u64,
        clock: Clock,
    ) -> Result<u64> {
        let pool_data = self.pool_id.try_borrow_data()?;
        let pool = bytemuck::pod_read_unaligned::<PoolState>(&pool_data[8..]);
    }

    pub fn swap_base_out_impl(
        &self,
        input_mint: Pubkey,
        amount_in: u64,
        clock: Clock,
    ) -> Result<u64> {
        
        let pool_data = self.pool_id.try_borrow_data()?;
        let pool = bytemuck::pod_read_unaligned::<PoolState>(&pool_data[8..]);

        let amm_config = self.accounts[6].try_borrow_data()?;
        let amm_config = bytemuck::pod_read_unaligned::<AmmConfig>(&amm_config[8..]);

        let zero_for_one = input_mint == pool.token_mint_0;

        let tick_arrays = if zero_for_one {
            // Keep bin_array_accounts alive in the same scope where it's used
            let tick_arrays: Vec<AccountInfo<'_>> = self.get_bin_arrays_buy().unwrap_or_default();
            tick_arrays
        } else {
            let tick_arrays: Vec<AccountInfo<'_>> = self.get_bin_arrays_sell().unwrap_or_default();
            tick_arrays
        };


        let sqrt_price_limit_x64 = Some(pool.sqrt_price_x64);

        let (amount_0, amount_1) = swap_internal(
            amm_config,
            pool_state,
            tick_array_states,
            &mut ctx.observation_state.load_mut()?,
            &tickarray_bitmap_extension,
            amount_specified,
            if sqrt_price_limit_x64 == 0 {
                if zero_for_one {
                    tick_math::MIN_SQRT_PRICE_X64 + 1
                } else {
                    tick_math::MAX_SQRT_PRICE_X64 - 1
                }
            } else {
                sqrt_price_limit_x64
            },
            zero_for_one,
            is_base_input,
            oracle::block_timestamp(),
        )?;

    }


    pub fn invoke_swap_base_in_impl<'a>(
        &self,
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
        Ok(())
    }

    pub fn invoke_swap_base_out_impl<'a>(
        &self,
        _input_mint: Pubkey,
        amount_out: u64,
        max_amount_in: u64,
        payer: AccountInfo<'a>,
        user_mint_1_token_account: AccountInfo<'a>,
        user_mint_2_token_account: AccountInfo<'a>,
        mint_1_account: AccountInfo<'a>,
        mint_2_account: AccountInfo<'a>,
        mint_1_token_program: AccountInfo<'a>,
        mint_2_token_program: AccountInfo<'a>,
    ) -> Result<()> {
        Ok(())
    }
}