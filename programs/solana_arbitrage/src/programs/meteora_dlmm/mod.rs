use crate::programs::programs::ProgramMeta;
use crate::programs::SolarBError;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use anchor_spl::token::spl_token::native_mint;
use dlmm::constants::FEE_PRECISION;
use dlmm::dlmm::accounts::{BinArrayBitmapExtension, LbPair};
use dlmm::dlmm::types::Bin;
use dlmm::extensions::{BinExtension, LbPairExtension};
use dlmm::math::price_math::get_price_from_id;
use dlmm::quote::{get_active_bin_array, quote_exact_in, quote_exact_out};

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

/// Extract bin arrays for buying from accounts starting at index 11
/// Structure: [fixed accounts] [bin_arrays_buy...] [SOL_MINT] [bin_arrays_sell...]
fn get_bin_arrays_buy<'a>(
    accounts: &[AccountInfo<'a>],
    start_index: usize,
    end_index: usize,
) -> Option<Vec<AccountInfo<'a>>> {
    if end_index - start_index <= 11 {
        return None;
    }

    let remaining = &accounts[start_index + 11..end_index];
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

/// Extract bin arrays for selling from accounts starting at index 11
/// Structure: [fixed accounts] [bin_arrays_buy...] [SOL_MINT] [bin_arrays_sell...]
fn get_bin_arrays_sell<'a>(
    accounts: &[AccountInfo<'a>],
    start_index: usize,
    end_index: usize,
) -> Option<Vec<AccountInfo<'a>>> {
    if end_index - start_index <= 11 {
        return None;
    }

    let remaining = &accounts[start_index + 11..end_index];
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

use std::marker::PhantomData;

#[derive(Clone)]
pub struct MeteoraDlmm<'info> {
    // pub pool_id: AccountInfo<'info>,
    // pub quote_vault: AccountInfo<'info>,
    pub pool_id: Pubkey,
    pub base_token_pk: Pubkey,
    pub quote_token_pk: Pubkey,
    pub lb_pair: LbPair,
    pub active_bin: Bin,
    pub lb_price: u128,
    pub price: f64,
    pub inverse_price: f64,
    pub start_index: usize,
    pub end_index: usize,
    pub fee_rate: f64,
    // pub bin_arrays: Option<Vec<AccountInfo<'info>>>,
    // pub oracle: AccountInfo<'info>,
    // pub host_fee_in: AccountInfo<'info>,
    // pub memo: AccountInfo<'info>,
    // pub event_authority: AccountInfo<'info>,
    // pub bitmap_extension: AccountInfo<'info>,
    // pub bin_arrays_buy: Option<Vec<AccountInfo<'info>>>,
    // pub bin_arrays_sell: Option<Vec<AccountInfo<'info>>>,
    pub phantom: PhantomData<&'info ()>,
}

impl<'info> ProgramMeta for MeteoraDlmm<'info> {
    fn get_id(&self) -> &Pubkey {
        &Self::PROGRAM_ID
    }

    fn get_pool_id(&self) -> &Pubkey {
        &self.pool_id
    }

    fn swap_base_in<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        clock: Clock,
    ) -> Result<u64> {
        self.swap_base_in_impl(accounts, input_mint, amount_in, clock)
    }

    fn swap_base_out<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        clock: Clock,
    ) -> Result<u64> {
        self.swap_base_out_impl(accounts, output_mint, amount_out, clock)
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        Ok((self.price, self.inverse_price))
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (&self.base_token_pk, &self.quote_token_pk)
    }

    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        let program_id = accounts[self.start_index + Self::PROGRAM_ID_IDX].key;
        let pool_id = accounts[self.start_index + Self::POOL_ID_IDX].key;
        let base_vault = accounts[self.start_index + Self::BASE_VAULT_IDX].key;
        let quote_vault = accounts[self.start_index + Self::QUOTE_VAULT_IDX].key;
        let base_token = accounts[self.start_index + Self::BASE_TOKEN_IDX].key;
        let quote_token = accounts[self.start_index + Self::QUOTE_TOKEN_IDX].key;
        let oracle = accounts[self.start_index + Self::ORACLE_IDX].key;
        let host_fee_in = accounts[self.start_index + Self::HOST_FEE_IN_IDX].key;
        let memo = accounts[self.start_index + Self::MEMO_IDX].key;
        let event_authority = accounts[self.start_index + Self::EVENT_AUTHORITY_IDX].key;
        let bitmap_extension = accounts[self.start_index + Self::BITMAP_EXTENSION_IDX].key;
        let bin_arrays_buy = get_bin_arrays_buy(accounts, self.start_index, self.end_index);
        let bin_arrays_sell = get_bin_arrays_sell(accounts, self.start_index, self.end_index);

        msg!("Meteora Dlmm Accounts:");
        msg!("  program_id: {}", program_id);
        msg!("  pool_id: {}", pool_id);
        msg!("  base_vault: {}", base_vault);
        msg!("  quote_vault: {}", quote_vault);
        msg!("  base_token: {}", base_token);
        msg!("  quote_token: {}", quote_token);
        msg!("  oracle: {}", oracle);
        msg!("  host_fee_in: {}", host_fee_in);
        msg!("  memo: {}", memo);
        msg!("  event_authority: {}", event_authority);
        msg!("  bitmap_extension: {}", bitmap_extension);
        msg!("  bin_arrays_buy: {:?}", bin_arrays_buy);
        msg!("  bin_arrays_sell: {:?}", bin_arrays_sell);
        Ok(())
    }


    fn get_max_amounts_in_out(&self, input_mint: Pubkey) -> Result<(u64, u64)> {
        let max_amount_in = self.get_max_amount_in(input_mint)?;
        let max_amount_out = self.get_max_amount_out(input_mint)?;
        Ok((max_amount_in, max_amount_out))
    }


        fn invoke_swap_base_in<'a>(
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
        self.invoke_swap_base_in_impl(
            accounts,
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
        self.invoke_swap_base_out_impl(
            accounts,
            input_mint,
            amount_in,
            min_amount_out,
            payer,
            user_mint_1_token_account,
            user_mint_2_token_account,
            mint_1_account,
            mint_2_account,
            mint_1_token_program,
            mint_2_token_program,
        )
    }

}

impl<'info> MeteoraDlmm<'info> {
    pub const PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");
    pub const PROGRAM_ID_IDX: usize = 0;
    pub const POOL_ID_IDX: usize = 1;
    pub const BASE_VAULT_IDX: usize = 2;
    pub const QUOTE_VAULT_IDX: usize = 3;
    pub const BASE_TOKEN_IDX: usize = 4;
    pub const QUOTE_TOKEN_IDX: usize = 5;
    pub const ORACLE_IDX: usize = 6;
    pub const HOST_FEE_IN_IDX: usize = 7;
    pub const MEMO_IDX: usize = 8;
    pub const EVENT_AUTHORITY_IDX: usize = 9;
    pub const BITMAP_EXTENSION_IDX: usize = 10;

    #[inline(never)]
    pub fn new(
        accounts: &[AccountInfo<'info>],
        start_index: usize,
        end_index: usize,
    ) -> Result<Self> {
        // Clone accounts we need before using the Vec
        let pool_id = accounts[start_index + Self::POOL_ID_IDX].clone();
        let base_token: AccountInfo<'_> = accounts[start_index + Self::BASE_TOKEN_IDX].clone();
        let quote_token = accounts[start_index + Self::QUOTE_TOKEN_IDX].clone();
        let (lb_pair, lb_price) = {
            // Borrow data from the cloned pool_id, not from the Vec
            let pool_data = pool_id.try_borrow_data()?;
            let slice = &pool_data[8..];
            let lb: LbPair = bytemuck::pod_read_unaligned(slice);
            let pr: u128 = get_price_from_id(lb.active_id, lb.bin_step)
                .map_err(|_| error!(SolarBError::InsufficientAccounts))?;
            (lb, pr)
        };

        let bin_arrays_buy = get_bin_arrays_buy(&accounts, start_index, end_index);
        let mut active_bin = get_active_bin_array(
            *pool_id.key,
            &lb_pair,
            None,
            true,
            bin_arrays_buy.unwrap_or_default(),
        )
        .unwrap();

        let (price, inverse_price) = get_prices(lb_price)?;

        let fee_rate = compute_fee_rate(&lb_pair).map_err(|_| error!(SolarBError::InsufficientAccounts))?;

        Ok(MeteoraDlmm {
            // pool_id,
            // base_vault: base_vault.clone(),
            // quote_vault: quote_vault.clone(),
            base_token_pk: *base_token.key,
            quote_token_pk: *quote_token.key,
            pool_id: *pool_id.key,
            lb_pair,
            active_bin,
            lb_price,
            price,
            inverse_price,
            fee_rate,
            start_index: start_index,
            end_index: end_index,
            // oracle: oracle.clone(),
            // host_fee_in: host_fee_in.clone(),
            // memo: memo.clone(),
            // event_authority: event_authority.clone(),
            // bitmap_extension: bin_array_bitmap_extension.clone(),
            // bin_arrays_buy: bin_arrays_buy.clone(),
            // bin_arrays_sell: bin_arrays_sell.clone(),
            phantom: PhantomData,
        })
    }

    fn log_accounts_impl<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        let program_id = accounts[self.start_index + Self::PROGRAM_ID_IDX].key;
        let pool_id = accounts[self.start_index + Self::POOL_ID_IDX].key;
        let base_vault = accounts[self.start_index + Self::BASE_VAULT_IDX].key;
        let quote_vault = accounts[self.start_index + Self::QUOTE_VAULT_IDX].key;
        let base_token = accounts[self.start_index + Self::BASE_TOKEN_IDX].key;
        let quote_token = accounts[self.start_index + Self::QUOTE_TOKEN_IDX].key;
        let oracle = accounts[self.start_index + Self::ORACLE_IDX].key;
        let host_fee_in = accounts[self.start_index + Self::HOST_FEE_IN_IDX].key;
        let memo = accounts[self.start_index + Self::MEMO_IDX].key;
        let event_authority = accounts[self.start_index + Self::EVENT_AUTHORITY_IDX].key;
        let bitmap_extension = accounts[self.start_index + Self::BITMAP_EXTENSION_IDX].key;
        let bin_arrays_buy = get_bin_arrays_buy(accounts, self.start_index, self.end_index);
        let bin_arrays_sell = get_bin_arrays_sell(accounts, self.start_index, self.end_index);

        msg!("Meteora Dlmm Accounts:");
        msg!("  program_id: {}", program_id);
        msg!("  pool_id: {}", pool_id);
        msg!("  base_vault: {}", base_vault);
        msg!("  quote_vault: {}", quote_vault);
        msg!("  base_token: {}", base_token);
        msg!("  quote_token: {}", quote_token);
        msg!("  oracle: {}", oracle);
        msg!("  host_fee_in: {}", host_fee_in);
        msg!("  memo: {}", memo);
        msg!("  event_authority: {}", event_authority);
        msg!("  bitmap_extension: {}", bitmap_extension);
        msg!("  bin_arrays_buy: {:?}", bin_arrays_buy);
        msg!("  bin_arrays_sell: {:?}", bin_arrays_sell);
        Ok(())
    }

    fn get_prices_impl(&self) -> Result<(f64, f64)> {
        Ok((self.price, self.inverse_price))
    }

    fn get_amount_after_fee(self, amount_in: u64) -> Result<u64> {
        let fee = self
            .lb_pair
            .compute_fee_from_amount(amount_in)
            .map_err(|_| error!(SolarBError::FeeOverflow))?;
        let amount_in_after_fee = amount_in.checked_sub(fee).ok_or(SolarBError::FeeOverflow)?;
        Ok(amount_in_after_fee)
    }

    pub fn swap_base_in_impl<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        clock: Clock,
    ) -> Result<u64> {
        let swap_for_y = input_mint == self.lb_pair.token_x_mint;
        // Deserialize bitmap extension if available
        let bitmap_extension_account = &accounts[self.start_index + 10];
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
        let bin_arrays = if swap_for_y {
            // Keep bin_array_accounts alive in the same scope where it's used
            let bin_arrays: Vec<AccountInfo<'_>> =
                get_bin_arrays_buy(accounts, self.start_index, self.end_index).unwrap_or_default();
            bin_arrays
        } else {
            let bin_arrays: Vec<AccountInfo<'_>> =
                get_bin_arrays_sell(accounts, self.start_index, self.end_index).unwrap_or_default();
            bin_arrays
        };

        let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
        let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];

        let quote = {
            quote_exact_in(
                *accounts[self.start_index + 1].key,
                &self.lb_pair,
                amount_in,
                swap_for_y, // swap_for_y
                bin_arrays,
                bitmap_extension.as_ref(),
                &clock,
                &base_token,
                &quote_token,
            )
        }
        .map_err(|e| {
            let error_msg = format!("ERROR in quote_exact_in: {}", e);
            eprintln!("{}", error_msg);
            msg!("{}", error_msg);
            // Preserve the original error message using ProgramError::Custom
            anchor_lang::error::Error::from(ProgramError::Custom(2004))
        })?;
        Ok(quote.amount_out)
    }

    pub fn swap_base_out_impl<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        clock: Clock,
    ) -> Result<u64> {
        let swap_for_y = input_mint == self.lb_pair.token_x_mint;

        eprintln!("swap_for_y: {:?}, {}", swap_for_y, amount_in);
        // Deserialize bitmap extension if available
        let bitmap_extension_account = &accounts[self.start_index + 10];
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
        let bin_arrays = if !swap_for_y {
            // Keep bin_array_accounts alive in the same scope where it's used
            let bin_arrays: Vec<AccountInfo<'_>> =
                get_bin_arrays_buy(accounts, self.start_index, self.end_index).unwrap_or_default();
            bin_arrays
        } else {
            let bin_arrays: Vec<AccountInfo<'_>> =
                get_bin_arrays_sell(accounts, self.start_index, self.end_index).unwrap_or_default();
            bin_arrays
        };

        let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
        let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];

        let quote = {
            quote_exact_out(
                *accounts[self.start_index + 1].key,
                &self.lb_pair,
                amount_in,
                swap_for_y, // swap_for_y = false means swapping FOR X (base token), so we need buy arrays
                bin_arrays,
                bitmap_extension.as_ref(),
                &clock,
                &base_token,
                &quote_token,
            )
        }
        .map_err(|e| {
            let error_msg = format!("ERROR in quote_exact_in: {}", e);
            eprintln!("{}", error_msg);
            msg!("{}", error_msg);
            // Preserve the original error message using ProgramError::Custom
            anchor_lang::error::Error::from(ProgramError::Custom(2004))
        })?;
        Ok(quote.amount_in)
    }

    pub fn invoke_swap_base_in_impl<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
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

        let amount_out_value = amount_out.unwrap_or(0);

        // Get stored accounts from self.accounts - these are the accounts stored in the struct
        let program_id = &accounts[self.start_index + 0];
        let pool_id = &accounts[self.start_index + 1];
        let base_vault = &accounts[self.start_index + 2];
        let quote_vault = &accounts[self.start_index + 3];
        let base_token = &accounts[self.start_index + 4];
        let quote_token = &accounts[self.start_index + 5];
        let oracle = &accounts[self.start_index + 6];
        let host_fee_in = &accounts[self.start_index + 7];
        let memo = &accounts[self.start_index + 8];
        let event_authority = &accounts[self.start_index + 9];
        let bitmap_extension = &accounts[self.start_index + 10];

        let swap_for_y = input_mint == *base_token.key;

        let bin_arrays = if swap_for_y {
            // Keep bin_array_accounts alive in the same scope where it's used
            let bin_arrays: Vec<AccountInfo<'_>> =
                get_bin_arrays_buy(accounts, self.start_index, self.end_index).unwrap_or_default();
            bin_arrays
        } else {
            let bin_arrays: Vec<AccountInfo<'_>> =
                get_bin_arrays_sell(accounts, self.start_index, self.end_index).unwrap_or_default();
            bin_arrays
        };

        let mut metas = vec![
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new_readonly(*bitmap_extension.key, false),
            AccountMeta::new(*base_vault.key, false),
            AccountMeta::new(*quote_vault.key, false),
            AccountMeta::new(*user_base_token_account.key, false),
            AccountMeta::new(*user_quote_token_account.key, false),
            AccountMeta::new_readonly(*base_token.key, false),
            AccountMeta::new_readonly(*quote_token.key, false),
            AccountMeta::new(*oracle.key, false),
            AccountMeta::new(*host_fee_in.key, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*base_token_program.key, false),
            AccountMeta::new_readonly(*quote_token_program.key, false),
            AccountMeta::new_readonly(*memo.key, false),
            AccountMeta::new_readonly(*event_authority.key, false),
            AccountMeta::new_readonly(Self::PROGRAM_ID, false),
        ];
        // Add bin arrays (buy arrays for swap_base_in)
        for account in bin_arrays.clone() {
            metas.push(AccountMeta::new(*account.key, false));
        }

        let mut data = vec![43, 215, 247, 132, 137, 60, 243, 81]; // TODO: Add proper instruction discriminator
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&amount_out_value.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes()); // Empty vec

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data,
        };

        // Collect AccountInfo into a vector and use unsafe to cast lifetimes
        // Order must match metas order exactly
        let mut accounts_vec: Vec<AccountInfo<'a>> = vec![
            pool_id.clone(),          // 0: pool_id
            bitmap_extension.clone(), // 1: bitmap_extension (readonly)
            base_vault.clone(),       // 2: base_vault
            quote_vault.clone(),      // 3: quote_vault
            unsafe { std::mem::transmute(user_base_token_account.to_account_info()) }, // 4: user_base_token_account
            unsafe { std::mem::transmute(user_quote_token_account.to_account_info()) }, // 5: user_quote_token_account
            base_token.clone(),  // 6: base_token (readonly)
            quote_token.clone(), // 7: quote_token (readonly)
            oracle.clone(),      // 8: oracle (readonly)
            host_fee_in.clone(), // 9: host_fee_in
            unsafe { std::mem::transmute(payer.to_account_info()) }, // 10: payer (signer)
            unsafe { std::mem::transmute(base_token_program.to_account_info()) }, // 11: base_token_program (readonly)
            unsafe { std::mem::transmute(quote_token_program.to_account_info()) }, // 12: quote_token_program (readonly)
            memo.clone(),            // 13: memo (readonly)
            event_authority.clone(), // 14: event_authority (readonly)
            program_id.clone(),      // 15: program_id (readonly)
        ];
        // Add bin arrays (buy arrays for swap_base_in)
        for account in bin_arrays {
            accounts_vec.push(account);
        }

        unsafe {
            let accounts: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts)?;
        }
        Ok(())
    }

    pub fn invoke_swap_base_out_impl<'a>(
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

        // Get stored accounts from self.accounts - these are the accounts stored in the struct
        let program_id = &accounts[self.start_index + 0];
        let pool_id = &accounts[self.start_index + 1];
        let base_vault = &accounts[self.start_index + 2];
        let quote_vault = &accounts[self.start_index + 3];
        let base_token = &accounts[self.start_index + 4];
        let quote_token = &accounts[self.start_index + 5];
        let oracle = &accounts[self.start_index + 6];
        let host_fee_in = &accounts[self.start_index + 7];
        let memo = &accounts[self.start_index + 8];
        let event_authority = &accounts[self.start_index + 9];
        let bitmap_extension = &accounts[self.start_index + 10];

        let swap_for_y = input_mint == *base_token.key;

        let bin_arrays: Vec<AccountInfo<'_>> = if swap_for_y {
            // Keep bin_array_accounts alive in the same scope where it's used
            let bin_arrays: Vec<AccountInfo<'_>> =
                get_bin_arrays_buy(accounts, self.start_index, self.end_index).unwrap_or_default();
            bin_arrays
        } else {
            let bin_arrays: Vec<AccountInfo<'_>> =
                get_bin_arrays_sell(accounts, self.start_index, self.end_index).unwrap_or_default();
            bin_arrays
        };

        let mut metas = vec![
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new(*bitmap_extension.key, false),
            AccountMeta::new(*base_vault.key, false),
            AccountMeta::new(*quote_vault.key, false),
            AccountMeta::new(*user_base_token_account.key, false),
            AccountMeta::new(*user_quote_token_account.key, false),
            AccountMeta::new_readonly(*base_token.key, false),
            AccountMeta::new_readonly(*quote_token.key, false),
            AccountMeta::new(*oracle.key, false),
            AccountMeta::new(*host_fee_in.key, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*base_token_program.key, false),
            AccountMeta::new_readonly(*quote_token_program.key, false),
            AccountMeta::new_readonly(*memo.key, false),
            AccountMeta::new_readonly(*event_authority.key, false),
            AccountMeta::new_readonly(Self::PROGRAM_ID, false),
        ];
        // Add bin arrays (sell arrays for swap_base_out)

        for account in bin_arrays.clone() {
            metas.push(AccountMeta::new(*account.key, false));
        }

        // swap2 instruction discriminator: [65, 75, 63, 76, 235, 91, 91, 136]
        let mut data = vec![65, 75, 63, 76, 235, 91, 91, 136];
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out_value.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes()); // Empty vec

        // RemainingAccountsInfo: { slices: Vec<RemainingAccountsSlice> }
        // For basic swaps without transfer hooks, slices is empty
        // Serialize Vec length as u32 (Anchor uses u32 for Vec length)
        data.extend_from_slice(&0u32.to_le_bytes()); // Empty vec

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data,
        };

        // Collect AccountInfo into a vector and use unsafe to cast lifetimes
        // Order must match metas order exactly
        let mut accounts_vec: Vec<AccountInfo<'a>> = vec![
            pool_id.clone(),          // 0: pool_id
            bitmap_extension.clone(), // 1: bitmap_extension
            base_vault.clone(),       // 2: base_vault
            quote_vault.clone(),      // 3: quote_vault
            unsafe { std::mem::transmute(user_base_token_account.to_account_info()) }, // 4: user_base_token_account
            unsafe { std::mem::transmute(user_quote_token_account.to_account_info()) }, // 5: user_quote_token_account
            base_token.clone(),  // 6: base_token (readonly)
            quote_token.clone(), // 7: quote_token (readonly)
            oracle.clone(),      // 8: oracle (readonly)
            host_fee_in.clone(), // 9: host_fee_in
            unsafe { std::mem::transmute(payer.to_account_info()) }, // 10: payer (signer)
            unsafe { std::mem::transmute(base_token_program.to_account_info()) }, // 11: base_token_program (readonly)
            unsafe { std::mem::transmute(quote_token_program.to_account_info()) }, // 12: quote_token_program (readonly)
            memo.clone(),            // 13: memo (readonly)
            event_authority.clone(), // 14: event_authority (readonly)
            program_id.clone(),      // 15: program_id (readonly)
        ];
        // Add bin arrays (sell arrays for swap_base_out)
        for account in bin_arrays {
            accounts_vec.push(account);
        }

        unsafe {
            let accounts: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts)?;
        }
        Ok(())
    }

    // fn get_amount_in(self, amount_out: u64, price: u128, input_mint: Pubkey) -> Result<u64> {
    //     let swap_for_y = *self.base_token.key == input_mint;
    //     // Price is scaled by 2^64, so we need proper scaling calculations
    //     if swap_for_y {
    //         // amount_in = (amount_out << 64) / price (with ceiling)
    //         let scale = 1u128
    //             .checked_shl(SCALE_OFFSET as u32)
    //             .ok_or(ProgramError::InvalidArgument)?;
    //         let numerator = (amount_out as u128)
    //             .checked_mul(scale)
    //             .ok_or(ProgramError::InvalidArgument)?;
    //         let amount_in = numerator
    //             .checked_add(price)
    //             .and_then(|x| x.checked_sub(1))
    //             .and_then(|x| x.checked_div(price))
    //             .ok_or(ProgramError::InvalidArgument)?;
    //         Ok(amount_in as u64)
    //     } else {
    //         // amount_in = (amount_out * price) >> 64 (with ceiling)
    //         let numerator = (amount_out as u128)
    //             .checked_mul(price)
    //             .ok_or(ProgramError::InvalidArgument)?;
    //         let scale = 1u128
    //             .checked_shl(SCALE_OFFSET as u32)
    //             .ok_or(ProgramError::InvalidArgument)?;
    //         let amount_in = numerator
    //             .checked_add(scale)
    //             .and_then(|x| x.checked_sub(1))
    //             .and_then(|x| Some(x >> SCALE_OFFSET))
    //             .ok_or(ProgramError::InvalidArgument)?;
    //         Ok(amount_in as u64)
    //     }
    // }

    fn get_amount_out(self, amount_in: u64, input_mint: Pubkey) -> Result<u64> {
        let swap_for_y = self.base_token_pk == input_mint;
        let price = self.lb_price;
        let fee = self
            .lb_pair
            .compute_fee_from_amount(amount_in)
            .map_err(|e| {
                msg!("compute_fee_from_amount error: {}", e);
                ProgramError::InvalidArgument
            })?;
        // let fee_rate = fee as f64 / amount_in as f64;
        // eprintln!("fee: {} - {}", fee, fee_rate);
        let amount_in_after_fee = amount_in
            .checked_sub(fee)
            .ok_or(ProgramError::InvalidArgument)?;
        // Price is scaled by 2^64, so we need proper scaling calculations
        // This matches Bin::get_amount_out implementation
        let amount_out = if swap_for_y {
            // amount_out = (price * amount_in_after_fee) >> 64 (with floor)
            let numerator = price
                .checked_mul(amount_in_after_fee as u128)
                .ok_or(ProgramError::InvalidArgument)?;
            (numerator >> SCALE_OFFSET) as u64
        } else {
            // amount_out = (amount_in_after_fee << 64) / price (with floor)
            let scale = 1u128
                .checked_shl(SCALE_OFFSET as u32)
                .ok_or(ProgramError::InvalidArgument)?;
            let numerator = (amount_in_after_fee as u128)
                .checked_mul(scale)
                .ok_or(ProgramError::InvalidArgument)?;
            numerator
                .checked_div(price)
                .ok_or(ProgramError::InvalidArgument)? as u64
        };
        Ok(amount_out)
    }

    // pub fn get_amount_out_from_amount_in(
    //     &self,
    //     price: u128,
    //     amount_in: u64,
    //     input_mint: Pubkey,
    // ) -> Result<u128> {
    //     let swap_for_y = self.base_token_pk == input_mint;
    //     let swap_result = self
    //         .active_bin
    //         .clone()
    //         .swap(amount_in, price, swap_for_y, &self.lb_pair, None)
    //         .map_err(|_| error!(SolarBError::InsufficientAccounts))?;
    //     Ok(swap_result.amount_out as u128)
    // }

    // pub fn get_amount_in_from_amount_out(
    //     &self,
    //     price: u128,
    //     amount_out: u64,
    //     input_mint: Pubkey,
    // ) -> Result<u128> {
    //     let swap_for_y = self.base_token_pk == input_mint;
    //     let amount_in = Bin::get_amount_in(amount_out, price, swap_for_y)
    //         .map_err(|_| error!(SolarBError::InsufficientAccounts))?;
    //     let fee = self
    //         .lb_pair
    //         .compute_fee(amount_out)
    //         .map_err(|_| error!(SolarBError::TransferFeeCalculationError))?;
    //     let total_amount_in = amount_in
    //         .checked_add(fee)
    //         .ok_or_else(|| error!(SolarBError::TransferFeeCalculationError))?;
    //     Ok(total_amount_in as u128)
    //     // total_amount_in = amount_in + transfer_fee
    // }

    // Gives always the maxmimum amount in of the token provided
    pub fn get_max_amount_in(&self, input_mint: Pubkey) -> Result<u64> {
        let price = self.lb_price;
        let swap_for_y = self.base_token_pk == input_mint;
        let amount: u64 = self
            .active_bin
            .get_max_amount_in(price, swap_for_y)
            .map_err(|_| error!(SolarBError::InsufficientAccounts))?;
        Ok(amount as u64)
    }

    // Gives always the maxmimum amount out of the contrary token provided
    pub fn get_max_amount_out(&self, input_mint: Pubkey) -> Result<u64> {
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
    use dlmm;
    use dlmm::dlmm::accounts::BinArray;
    use dlmm::quote::get_bin_array_pubkeys_for_swap;

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

    async fn build_test_scenario() -> (MeteoraDlmm<'static>, Vec<AccountInfo<'static>>) {
        use anchor_client::Cluster;
        use dlmm::pda;
        use solana_client::nonblocking::rpc_client::RpcClient;
        use std::collections::HashMap;

        let cluster: Cluster = Cluster::Mainnet;
        let rpc_client = RpcClient::new(cluster.url().to_string());

        let pool_id: Pubkey = if cluster == Cluster::Mainnet {
            Pubkey::from_str_const("LJGCprfvx4qZVXktL24CLArGwzpAsQXjq5AQFa5w6WT")
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

        let left_bin_array_pubkeys =
            get_bin_array_pubkeys_for_swap(pool_id, &lb_pair, None, true, 5).unwrap();

        // Get more bin arrays to the right (sell arrays) - increase from 5 to handle larger swaps
        let right_bin_array_pubkeys =
            get_bin_array_pubkeys_for_swap(pool_id, &lb_pair, None, false, 10)
                .unwrap();

        eprintln!("mint_x_account: {:?}", token_x_mint_key);
        eprintln!("mint_y_account: {:?}", token_y_mint_key);
        eprintln!("reserve_x: {:?}", base_vault_key);
        eprintln!("reserve_y: {:?}", quote_vault_key);
        eprintln!("oracle: {:?}", oracle_key);
        eprintln!("bitmap_extension: {:?}", bitmap_extension_key);
        for key in left_bin_array_pubkeys.clone() {
            eprintln!("left_bin_array: {:?}", key);
        }
        for key in right_bin_array_pubkeys.clone() {
            eprintln!("right_bin_array: {:?}", key);
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
        let base_token_account = fetch_account_info_from_rpc(&rpc_client, token_x_mint_key).await;
        let quote_token_account = fetch_account_info_from_rpc(&rpc_client, token_y_mint_key).await;
        let oracle_account = fetch_account_info_from_rpc(&rpc_client, lb_pair.oracle).await;
        // Derive bitmap extension PDA
        let bitmap_extension_account: AccountInfo<'static> =
            try_fetch_account_info_from_rpc(&rpc_client, bitmap_extension_key)
                .await
                .unwrap_or_else(|| program_id_account.clone());

        // host_fee_in, memo, and event_authority are not fields on LbPair - use placeholder accounts
        // These are optional accounts used in swap instructions
        let host_fee_in_key = Pubkey::default(); // Placeholder - can be zero for swaps without host fee
        let memo_key = anchor_spl::associated_token::ID; // Use a placeholder key for memo (not critical for quote)
        let (event_authority_key, _) = pda::derive_event_authority_pda();

        let host_fee_in_account =
            create_mock_account_info_with_data(host_fee_in_key, system_program::id(), None);
        let memo_account = create_mock_account_info_with_data(memo_key, system_program::id(), None);
        let event_authority_account =
            create_mock_account_info_with_data(event_authority_key, system_program::id(), None);

        eprintln!("program_id_account: {:?}", program_id_account.key);
        let mut accounts = vec![
            program_id_account,       // 0: program_id (required by MeteoraDlmm::new)
            pool_id_account_info,     // 1: pool_id
            base_vault_account,       // 2: base_vault
            quote_vault_account,      // 3: quote_vault
            base_token_account,       // 4: base_token
            quote_token_account,      // 5: quote_token
            oracle_account,           // 6: oracle
            host_fee_in_account,      // 7: host_fee_in
            memo_account,             // 8: memo
            event_authority_account,  // 9: event_authority
            bitmap_extension_account, // 10: bitmap_extension
        ];

        // Add bin array accounts: buy arrays, then SOL MINT separator, then sell arrays
        accounts.extend(bin_array_buy_infos);
        // Add SOL MINT as separator - fetch it from RPC
        let sol_mint_key = anchor_spl::token::spl_token::native_mint::id();
        let sol_mint_account_info = fetch_account_info_from_rpc(&rpc_client, sol_mint_key).await;
        accounts.push(sol_mint_account_info);
        accounts.extend(bin_array_sell_infos);
        let meteora_dlmm = MeteoraDlmm::new(&accounts, 0, accounts.len()).unwrap();
        (meteora_dlmm, accounts)
    }

    #[tokio::test]
    async fn test_dlmm_swap_quote_exact_in() {
        use anchor_client::Cluster;
        use solana_client::nonblocking::rpc_client::RpcClient;
        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let (meteora_dlmm, accounts) = build_test_scenario().await;
        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());
        let clock1 = get_clock(&rpc_client).await.unwrap();
        let clock_2 = clock1.clone();
        let clock_3 = clock1.clone();

        // Create MeteoraDlmm instance
        // let (token_a_decimal, token_b_decimal) = if token_x_mint_key == wsol {
        //     (9, 6) // token_a is SOL (9 decimals), token_b is likely USDC/USDT (6 decimals)
        // } else if token_y_mint_key == wsol {
        //     (6, 9) // token_a is likely USDC/USDT (6 decimals), token_b is SOL (9 decimals)
        // } else {
        //     // Neither is SOL, default to common case (e.g., USDC/USDT pair)
        //     (6, 6)
        // };

        let prices = meteora_dlmm.get_prices().unwrap();
        let price = prices.0;
        let inverse_price = prices.1;
        eprintln!("price: {:?}", price);

        let (sol_price, token_price) = if meteora_dlmm.base_token_pk == sol_mint {
            (price, inverse_price)
        } else {
            (inverse_price, price)
        };
        eprintln!("sol_price: {:?}", sol_price);
        eprintln!("token_price: {:?}", token_price);

        // 1 SOL -> USDC
        let in_sol_amount = 1_000;

        // Determine swap_for_y: if SOL is token_x, we swap X for Y (swap_for_y = true)
        // If SOL is token_y, we swap Y for X (swap_for_y = false)
        let swap_for_y = meteora_dlmm.lb_pair.token_x_mint == sol_mint;
        eprintln!("swap_for_y: {:?}", swap_for_y);

        let amount_out = meteora_dlmm
            .swap_base_in(&accounts, sol_mint, in_sol_amount, clock1)
            .unwrap();
        let amount_out_v2 = in_sol_amount as f64 * inverse_price;
        let amount_out_v2_2 = meteora_dlmm
            .clone()
            .get_amount_out(in_sol_amount, sol_mint)
            .unwrap();

        eprintln!(
            "Step 1: {} SOL -> {} TOKEN / {} / {} ",
            in_sol_amount as f64 / 1_000_000_000.0,
            amount_out as f64 / 1_000_000.0,
            amount_out_v2 as f64 / 1_000_000.0,
            amount_out_v2_2 as f64 / 1_000_000.0,
        );
        eprintln!("================================================");
        // Step 2: Swap quote -> base (reverse swap)
        let other_mint = if meteora_dlmm.quote_token_pk != sol_mint {
            meteora_dlmm.quote_token_pk
        } else {
            meteora_dlmm.base_token_pk
        };

        let token_amount_out = meteora_dlmm
            .swap_base_out(&accounts, other_mint, amount_out, clock_2)
            .unwrap();
        let token_amount_out_v2 = amount_out as f64 * token_price;
        let token_amount_out_v2_2 = meteora_dlmm
            .clone()
            .get_amount_out(amount_out, other_mint)
            .unwrap();

        eprintln!(
            "Step 2: {} TOKEN -> SOL {} / {} / {}",
            amount_out as f64 / 1_000_000.0,
            token_amount_out as f64 / 1_000_000_000.0,
            token_amount_out_v2 as f64 / 1_000_000_000.0,
            token_amount_out_v2_2 as f64 / 1_000_000_000.0
        );
    }
}
