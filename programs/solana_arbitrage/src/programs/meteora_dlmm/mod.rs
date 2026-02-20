use crate::programs::programs::ProgramMeta;
use crate::programs::SolarBError;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use dlmm::constants::FEE_PRECISION;
use dlmm::dlmm::accounts::{BinArray, BinArrayBitmapExtension, LbPair};
use dlmm::dlmm::types::Bin;
use dlmm::extensions::{BinArrayExtension, BinExtension, LbPairExtension};
use dlmm::math::price_math::get_price_from_id;
use dlmm::pda::derive_bin_array_pda;
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

use std::marker::PhantomData;

#[derive(Clone)]
pub struct MeteoraDlmm<'info> {
    pub pool_id: Pubkey,
    pub base_token_pk: Pubkey,
    pub quote_token_pk: Pubkey,
    pub lb_pair: Box<LbPair>,
    pub active_bin: Bin,
    pub lb_price: u128,
    pub price: f64,
    pub inverse_price: f64,
    pub start_index: usize,
    pub end_index: usize,
    pub fee_rate: f64,
    /// Total token X in the active bin array (sum of all 70 bins)
    pub bin_array_total_x: u64,
    /// Total token Y in the active bin array (sum of all 70 bins)
    pub bin_array_total_y: u64,
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
        let swap_for_y = input_mint == self.lb_pair.token_x_mint;
        // Deserialize bitmap extension if available
        let bitmap_extension_account = &accounts[self.start_index + Self::BITMAP_EXTENSION_IDX];
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
            vec![
                accounts[self.start_index + Self::BUY_BIN_1_IDX].clone(),
                accounts[self.start_index + Self::BUY_BIN_2_IDX].clone(),
            ]
        } else {
            vec![
                accounts[self.start_index + Self::SELL_BIN_1_IDX].clone(),
                accounts[self.start_index + Self::SELL_BIN_2_IDX].clone(),
            ]
        };

        let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
        let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];

        let quote = {
            quote_exact_in(
                *accounts[self.start_index + Self::POOL_ID_IDX].key,
                &self.lb_pair,
                amount_in,
                swap_for_y,
                bin_arrays,
                bitmap_extension.as_ref(),
                &clock,
                &base_token,
                &quote_token,
            )
        }
        .map_err(|e| {
            let error_msg = format!("ERROR in quote_exact_in: {}", e);
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!("{}", error_msg);
            msg!("{}", error_msg);
            anchor_lang::error::Error::from(ProgramError::Custom(2004))
        })?;
        Ok(quote.amount_out)
    }

    fn swap_base_out<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        clock: Clock,
    ) -> Result<u64> {
        let swap_for_y = output_mint == self.lb_pair.token_y_mint;

        // Deserialize bitmap extension if available
        let bitmap_extension_account = &accounts[self.start_index + Self::BITMAP_EXTENSION_IDX];
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
            vec![
                accounts[self.start_index + Self::BUY_BIN_1_IDX].clone(),
                accounts[self.start_index + Self::BUY_BIN_2_IDX].clone(),
            ]
        } else {
            vec![
                accounts[self.start_index + Self::SELL_BIN_1_IDX].clone(),
                accounts[self.start_index + Self::SELL_BIN_2_IDX].clone(),
            ]
        };

        let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
        let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];

        let quote = {
            quote_exact_out(
                *accounts[self.start_index + Self::POOL_ID_IDX].key,
                &self.lb_pair,
                amount_out,
                swap_for_y,
                bin_arrays,
                bitmap_extension.as_ref(),
                &clock,
                &base_token,
                &quote_token,
            )
        }
        .map_err(|e| {
            let error_msg = format!("ERROR in quote_exact_out: {}", e);
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!("{}", error_msg);
            msg!("{}", error_msg);
            anchor_lang::error::Error::from(ProgramError::Custom(2004))
        })?;
        Ok(quote.amount_in)
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        Ok((self.price, self.inverse_price))
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (&self.base_token_pk, &self.quote_token_pk)
    }

    fn name(&self) -> &'static str { "MeteoraDLMM" }

    fn get_max_amount_in(&self, mint: Pubkey) -> Result<u64> {
        MeteoraDlmm::get_max_amount_in(self, mint)
    }

    fn get_max_amount_out(&self, mint: Pubkey) -> Result<u64> {
        MeteoraDlmm::get_max_amount_out(self, mint)
    }

    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        msg!("=== Meteora DLMM ===");
        msg!("0 program_id: {}", accounts[self.start_index + Self::PROGRAM_ID_IDX].key);
        msg!("1 pool_id: {}", accounts[self.start_index + Self::POOL_ID_IDX].key);
        msg!("2 base_vault: {}", accounts[self.start_index + Self::BASE_VAULT_IDX].key);
        msg!("3 quote_vault: {}", accounts[self.start_index + Self::QUOTE_VAULT_IDX].key);
        msg!("4 base_token: {}", accounts[self.start_index + Self::BASE_TOKEN_IDX].key);
        msg!("5 quote_token: {}", accounts[self.start_index + Self::QUOTE_TOKEN_IDX].key);
        msg!("6 oracle: {}", accounts[self.start_index + Self::ORACLE_IDX].key);
        msg!("7 host_fee_in: {}", accounts[self.start_index + Self::HOST_FEE_IN_IDX].key);
        msg!("8 event_authority: {}", accounts[self.start_index + Self::EVENT_AUTHORITY_IDX].key);
        msg!("9 bitmap_extension: {}", accounts[self.start_index + Self::BITMAP_EXTENSION_IDX].key);
        msg!("10 buy_bin_1: {}", accounts[self.start_index + Self::BUY_BIN_1_IDX].key);
        msg!("11 buy_bin_2: {}", accounts[self.start_index + Self::BUY_BIN_2_IDX].key);
        msg!("12 sell_bin_1: {}", accounts[self.start_index + Self::SELL_BIN_1_IDX].key);
        msg!("13 sell_bin_2: {}", accounts[self.start_index + Self::SELL_BIN_2_IDX].key);
        Ok(())
    }


    fn get_fee_factor(&self) -> Result<(f64, f64)> {
        let f = 1.0 - self.fee_rate;
        Ok((f, f))
    }

    fn get_max_amounts_in_out(&self, input_mint: Pubkey) -> Result<(u64, u64)> {
        let max_amount_in = self.get_max_amount_in_active_bin(input_mint)?;
        let max_amount_out = self.get_max_amount_out_active_bin(input_mint)?;
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

        let (user_token_in, user_token_out) = if input_mint == self.base_token_pk {
            (user_base_token_account, user_quote_token_account)
        } else {
            (user_quote_token_account, user_base_token_account)
        };

        let amount_out_value = 0 as u64;

        // Get stored accounts from self.accounts - these are the accounts stored in the struct
        let program_id = &accounts[self.start_index + Self::PROGRAM_ID_IDX];
        let pool_id = &accounts[self.start_index + Self::POOL_ID_IDX];
        let base_vault = &accounts[self.start_index + Self::BASE_VAULT_IDX];
        let quote_vault = &accounts[self.start_index + Self::QUOTE_VAULT_IDX];
        let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
        let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];
        let oracle = &accounts[self.start_index + Self::ORACLE_IDX];
        let host_fee_in = &accounts[self.start_index + Self::HOST_FEE_IN_IDX];
        let memo = &accounts[3];
        let event_authority = &accounts[self.start_index + Self::EVENT_AUTHORITY_IDX];
        let bitmap_extension = &accounts[self.start_index + Self::BITMAP_EXTENSION_IDX];

        // msg!("program_id: {:?}", program_id.key);
        // msg!("pool_id: {:?}", pool_id.key);
        // msg!("base_vault: {:?}", base_vault.key);
        // msg!("quote_vault: {:?}", quote_vault.key);
        // msg!("base_token: {:?}", base_token.key);
        // msg!("quote_token: {:?}", quote_token.key);
        // msg!("oracle: {:?}", oracle.key);
        // msg!("host_fee_in: {:?}", host_fee_in.key);
        // msg!("memo: {:?}", memo.key);
        // msg!("event_authority: {:?}", event_authority.key);
        // msg!("bitmap_extension: {:?}", bitmap_extension.key);

        let swap_for_y = input_mint == *base_token.key;

        let (bin_array_1, bin_array_2) = if swap_for_y {
            (
                accounts[self.start_index + Self::BUY_BIN_1_IDX].clone(),
                accounts[self.start_index + Self::BUY_BIN_2_IDX].clone(),
            )
        } else {
            (
                accounts[self.start_index + Self::SELL_BIN_1_IDX].clone(),
                accounts[self.start_index + Self::SELL_BIN_2_IDX].clone(),
            )
        };

        let mut metas = Vec::with_capacity(18);
        metas.push(AccountMeta::new(*pool_id.key, false));
        metas.push(if *bitmap_extension.key == Self::PROGRAM_ID {
            AccountMeta::new_readonly(*bitmap_extension.key, false)
        } else {
            AccountMeta::new(*bitmap_extension.key, false)
        });
        metas.push(AccountMeta::new(*base_vault.key, false));
        metas.push(AccountMeta::new(*quote_vault.key, false));
        metas.push(AccountMeta::new(*user_token_in.key, false));
        metas.push(AccountMeta::new(*user_token_out.key, false));
        metas.push(AccountMeta::new_readonly(*base_token.key, false));
        metas.push(AccountMeta::new_readonly(*quote_token.key, false));
        metas.push(AccountMeta::new(*oracle.key, false));
        metas.push(AccountMeta::new(*host_fee_in.key, false));
        metas.push(AccountMeta::new(*payer.key, true));
        metas.push(AccountMeta::new_readonly(*base_token_program.key, false));
        metas.push(AccountMeta::new_readonly(*quote_token_program.key, false));
        metas.push(AccountMeta::new_readonly(*memo.key, false));
        metas.push(AccountMeta::new_readonly(*event_authority.key, false));
        metas.push(AccountMeta::new_readonly(Self::PROGRAM_ID, false));
        metas.push(AccountMeta::new(*bin_array_1.key, false));
        metas.push(AccountMeta::new(*bin_array_2.key, false));

        // swap2 instruction discriminator (SwapExactIn)
        let mut data = vec![65, 75, 63, 76, 235, 91, 91, 136];
        data.extend_from_slice(&max_amount_in.to_le_bytes());
        data.extend_from_slice(&amount_out_value.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes()); // Empty vec (remaining accounts slices)
        data.extend_from_slice(&0u32.to_le_bytes()); // Empty vec (remaining accounts info)

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data,
        };

        // Collect AccountInfo into a vector and use unsafe to cast lifetimes
        // Order must match metas order exactly
        let mut accounts_vec: Vec<AccountInfo<'a>> = Vec::with_capacity(18);
        accounts_vec.push(pool_id.clone());
        accounts_vec.push(bitmap_extension.clone());
        accounts_vec.push(base_vault.clone());
        accounts_vec.push(quote_vault.clone());
        accounts_vec.push(unsafe { std::mem::transmute(user_token_in.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(user_token_out.to_account_info()) });
        accounts_vec.push(base_token.clone());
        accounts_vec.push(quote_token.clone());
        accounts_vec.push(oracle.clone());
        accounts_vec.push(host_fee_in.clone());
        accounts_vec.push(unsafe { std::mem::transmute(payer.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(base_token_program.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(quote_token_program.to_account_info()) });
        accounts_vec.push(memo.clone());
        accounts_vec.push(event_authority.clone());
        accounts_vec.push(program_id.clone());
        accounts_vec.push(bin_array_1.clone());
        accounts_vec.push(bin_array_2.clone());

        unsafe {
            let accounts: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts)?;
        }
        Ok(())
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
        let memo = &accounts[3];
        let event_authority = &accounts[self.start_index + 9];
        let bitmap_extension = &accounts[self.start_index + 10];

        let swap_for_y = input_mint == *base_token.key;

        let (bin_array_1, bin_array_2) = if swap_for_y {
            (
                accounts[self.start_index + Self::BUY_BIN_1_IDX].clone(),
                accounts[self.start_index + Self::BUY_BIN_2_IDX].clone(),
            )
        } else {
            (
                accounts[self.start_index + Self::SELL_BIN_1_IDX].clone(),
                accounts[self.start_index + Self::SELL_BIN_2_IDX].clone(),
            )
        };

        let mut metas = Vec::with_capacity(18);
        metas.push(AccountMeta::new(*pool_id.key, false));
        metas.push(AccountMeta::new(*bitmap_extension.key, false));
        metas.push(AccountMeta::new(*base_vault.key, false));
        metas.push(AccountMeta::new(*quote_vault.key, false));
        metas.push(AccountMeta::new(*user_base_token_account.key, false));
        metas.push(AccountMeta::new(*user_quote_token_account.key, false));
        metas.push(AccountMeta::new_readonly(*base_token.key, false));
        metas.push(AccountMeta::new_readonly(*quote_token.key, false));
        metas.push(AccountMeta::new(*oracle.key, false));
        metas.push(AccountMeta::new(*host_fee_in.key, false));
        metas.push(AccountMeta::new(*payer.key, true));
        metas.push(AccountMeta::new_readonly(*base_token_program.key, false));
        metas.push(AccountMeta::new_readonly(*quote_token_program.key, false));
        metas.push(AccountMeta::new_readonly(*memo.key, false));
        metas.push(AccountMeta::new_readonly(*event_authority.key, false));
        metas.push(AccountMeta::new_readonly(Self::PROGRAM_ID, false));
        metas.push(AccountMeta::new(*bin_array_1.key, false));
        metas.push(AccountMeta::new(*bin_array_2.key, false));

        // swap_exact_out2 instruction discriminator (SwapExactOut)
        let mut data = vec![43, 215, 247, 132, 137, 60, 243, 81];
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out_value.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes()); // Empty vec (remaining accounts slices)
        data.extend_from_slice(&0u32.to_le_bytes()); // Empty vec (remaining accounts info)

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data,
        };

        // Collect AccountInfo into a vector and use unsafe to cast lifetimes
        // Order must match metas order exactly
        let accounts_vec: Vec<AccountInfo<'a>> = vec![
            pool_id.clone(),
            bitmap_extension.clone(),
            base_vault.clone(),
            quote_vault.clone(),
            unsafe { std::mem::transmute(user_base_token_account.to_account_info()) },
            unsafe { std::mem::transmute(user_quote_token_account.to_account_info()) },
            base_token.clone(),
            quote_token.clone(),
            oracle.clone(),
            host_fee_in.clone(),
            unsafe { std::mem::transmute(payer.to_account_info()) },
            unsafe { std::mem::transmute(base_token_program.to_account_info()) },
            unsafe { std::mem::transmute(quote_token_program.to_account_info()) },
            memo.clone(),
            event_authority.clone(),
            program_id.clone(),
            bin_array_1.clone(),
            bin_array_2.clone(),
        ];

        unsafe {
            let accounts: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts)?;
        }
        Ok(())
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
    // pub const MEMO_IDX: usize = 8;
    pub const EVENT_AUTHORITY_IDX: usize = 8;
    pub const BITMAP_EXTENSION_IDX: usize = 9;
    pub const BUY_BIN_1_IDX: usize = 10;
    pub const BUY_BIN_2_IDX: usize = 11;
    pub const SELL_BIN_1_IDX: usize = 12;
    pub const SELL_BIN_2_IDX: usize = 13;

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
            // Box immediately to avoid large LbPair on the stack
            let lb: Box<LbPair> = Box::new(bytemuck::pod_read_unaligned(slice));
            let pr: u128 = get_price_from_id(lb.active_id, lb.bin_step)
                .map_err(|_| error!(SolarBError::InsufficientAccounts))?;
            (lb, pr)
        };

        let bitmap_extension_account = &accounts[start_index + Self::BITMAP_EXTENSION_IDX];
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

        // Fixed layout: [buy_bin_1, buy_bin_2, sell_bin_1, sell_bin_2]
        let all_bin_arrays = vec![
            accounts[start_index + Self::BUY_BIN_1_IDX].clone(),
            accounts[start_index + Self::BUY_BIN_2_IDX].clone(),
            accounts[start_index + Self::SELL_BIN_1_IDX].clone(),
            accounts[start_index + Self::SELL_BIN_2_IDX].clone(),
        ];

        let active_bin = get_active_bin_array(
            *pool_id.key,
            &lb_pair,
            bitmap_extension.as_ref(),
            true,
            all_bin_arrays,
        )
        .map_err(|_| error!(SolarBError::InsufficientAccounts))?;
        let (price, inverse_price) = get_prices(lb_price)?;

        // Sum liquidity across all 70 bins in the active bin's bin array
        let (bin_array_total_x, bin_array_total_y) = {
            const BIN_ARRAY_HEADER_SIZE: usize = 56;
            const BIN_SIZE: usize = 144;
            const MAX_BIN_PER_ARRAY: usize = 70;

            let active_bin_array_idx = BinArray::bin_id_to_bin_array_index(lb_pair.active_id)
                .map_err(|_| error!(SolarBError::InsufficientAccounts))?;
            let active_bin_array_pda = derive_bin_array_pda(*pool_id.key, active_bin_array_idx as i64).0;

            let remaining = &accounts[start_index + Self::BUY_BIN_1_IDX..start_index + Self::SELL_BIN_2_IDX + 1];
            let mut total_x: u64 = 0;
            let mut total_y: u64 = 0;

            if let Some(bin_array_account) = remaining.iter().find(|acc| *acc.key == active_bin_array_pda) {
                let data = bin_array_account.try_borrow_data()?;
                for i in 0..MAX_BIN_PER_ARRAY {
                    let offset = BIN_ARRAY_HEADER_SIZE + i * BIN_SIZE;
                    if offset + 16 > data.len() { break; }
                    let amount_x = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                    let amount_y = u64::from_le_bytes(data[offset + 8..offset + 16].try_into().unwrap());
                    total_x = total_x.saturating_add(amount_x);
                    total_y = total_y.saturating_add(amount_y);
                }
            }

            (total_x, total_y)
        };

        let fee_rate = compute_fee_rate(&lb_pair).map_err(|_| error!(SolarBError::InsufficientAccounts))?;
        let instance = MeteoraDlmm {
            base_token_pk: *base_token.key,
            quote_token_pk: *quote_token.key,
            pool_id: *pool_id.key,
            lb_pair,
            active_bin,
            lb_price,
            price,
            inverse_price,
            fee_rate,
            start_index,
            end_index,
            bin_array_total_x,
            bin_array_total_y,
            phantom: PhantomData,
        };
        // instance.log_accounts(accounts)?;
        Ok(instance)
    }

    // Max input amount across the active bin array (approximate, uses active bin price)
    pub fn get_max_amount_in(&self, input_mint: Pubkey) -> Result<u64> {
        let swap_for_y = self.base_token_pk == input_mint;
        if swap_for_y {
            // Input X, output Y. Max X in ≈ total_y / price
            Ok((self.bin_array_total_y as f64 * self.inverse_price) as u64)
        } else {
            // Input Y, output X. Max Y in ≈ total_x * price
            Ok((self.bin_array_total_x as f64 * self.price) as u64)
        }
    }

    // Max output amount across the active bin array
    pub fn get_max_amount_out(&self, input_mint: Pubkey) -> Result<u64> {
        let swap_for_y = self.base_token_pk == input_mint;
        if swap_for_y {
            Ok(self.bin_array_total_y)
        } else {
            Ok(self.bin_array_total_x)
        }
    }

    // Max input amount in the current active bin only (no price movement)
    pub fn get_max_amount_in_active_bin(&self, input_mint: Pubkey) -> Result<u64> {
        let swap_for_y = self.base_token_pk == input_mint;
        let amount: u64 = self
            .active_bin
            .get_max_amount_in(self.lb_price, swap_for_y)
            .map_err(|_| error!(SolarBError::InsufficientAccounts))?;
        Ok(amount)
    }

    // Max output amount in the current active bin only (no price movement)
    pub fn get_max_amount_out_active_bin(&self, input_mint: Pubkey) -> Result<u64> {
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
            Pubkey::from_str_const("7eexH14UjhNxJe6zTT3f1Vb1E8iACsBMVaWheDEmxdT2")
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

        // Get exactly 2 buy bin arrays and 2 sell bin arrays (fixed layout)
        let left_bin_array_pubkeys =
            get_bin_array_pubkeys_for_swap(pool_id, &lb_pair, None, true, 2).unwrap();

        let right_bin_array_pubkeys =
            get_bin_array_pubkeys_for_swap(pool_id, &lb_pair, None, false, 2)
                .unwrap();

        debug_eprintln!("mint_x_account: {:?}", token_x_mint_key);
        debug_eprintln!("mint_y_account: {:?}", token_y_mint_key);
        debug_eprintln!("reserve_x: {:?}", base_vault_key);
        debug_eprintln!("reserve_y: {:?}", quote_vault_key);
        debug_eprintln!("oracle: {:?}", oracle_key);
        debug_eprintln!("bitmap_extension: {:?}", bitmap_extension_key);
        for key in left_bin_array_pubkeys.clone() {
            debug_eprintln!("left_bin_array: {:?}", key);
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

        debug_eprintln!("program_id_account: {:?}", program_id_account.key);
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

        // Fixed layout: [buy_bin_1, buy_bin_2, sell_bin_1, sell_bin_2]
        assert!(bin_array_buy_infos.len() >= 2, "need at least 2 buy bin arrays");
        assert!(bin_array_sell_infos.len() >= 2, "need at least 2 sell bin arrays");
        accounts.push(bin_array_buy_infos[0].clone());
        accounts.push(bin_array_buy_infos[1].clone());
        accounts.push(bin_array_sell_infos[0].clone());
        accounts.push(bin_array_sell_infos[1].clone());
        let meteora_dlmm = MeteoraDlmm::new(&accounts, 0, accounts.len()).unwrap();
        (meteora_dlmm, accounts)
    }

    #[tokio::test]
    async fn test_dlmm_swap_base_in_both_directions() {
        use anchor_client::Cluster;
        use solana_client::nonblocking::rpc_client::RpcClient;
        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let (meteora_dlmm, accounts) = build_test_scenario().await;
        let rpc_client: RpcClient = RpcClient::new(Cluster::Mainnet.url().to_string());
        let clock = get_clock(&rpc_client).await.unwrap();

        let (price, inverse_price) = meteora_dlmm.get_prices().unwrap();
        debug_eprintln!("price (Y per X): {:?}", price);
        debug_eprintln!("inverse_price (X per Y): {:?}", inverse_price);

        let other_mint = if meteora_dlmm.base_token_pk == sol_mint {
            meteora_dlmm.quote_token_pk
        } else {
            meteora_dlmm.base_token_pk
        };

        // Step 1: swap_base_in SOL -> TOKEN (1 SOL in, get tokens out)
        let in_sol_amount: u64 = 1_000_000_000;
        let tokens_out = meteora_dlmm
            .swap_base_in(&accounts, sol_mint, in_sol_amount, clock.clone())
            .unwrap();
        debug_eprintln!(
            "swap_base_in: {} SOL -> {} TOKEN",
            in_sol_amount as f64 / 1e9,
            tokens_out as f64 / 1e6,
        );

        let max_sol_in = meteora_dlmm.swap_base_out(&accounts, other_mint, tokens_out, clock.clone()).unwrap();
        debug_eprintln!(
            "swap_base_out: {} TOKEN -> {} SOL",
            tokens_out as f64 / 1e6,
            max_sol_in as f64 / 1e9,
        );

        // Step 2: swap_base_in TOKEN -> SOL (reverse direction)
        let sol_out = meteora_dlmm
            .swap_base_in(&accounts, other_mint, tokens_out, clock.clone())
            .unwrap();
        debug_eprintln!(
            "swap_base_in: {} TOKEN -> {} SOL",
            tokens_out as f64 / 1e6,
            sol_out as f64 / 1e9,
        );

        let max_token_in = meteora_dlmm.swap_base_out(&accounts, sol_mint, sol_out, clock.clone()).unwrap();
        debug_eprintln!(
            "swap_base_out: {} SOL -> {} TOKEN",
            sol_out as f64 / 1e9,
            max_token_in as f64 / 1e6,
        );
        // But shouldn't lose more than ~5% in a reasonable pool
        let loss_pct = 1.0 - (sol_out as f64 / in_sol_amount as f64);
        debug_eprintln!("round-trip loss: {:.4}%", loss_pct * 100.0);
        assert!(
            loss_pct < 0.05,
            "round-trip loss too high: {:.4}%",
            loss_pct * 100.0
        );
    }

    #[tokio::test]
    async fn test_dlmm_swap_base_out_both_directions() {
        use anchor_client::Cluster;
        use solana_client::nonblocking::rpc_client::RpcClient;
        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let (meteora_dlmm, accounts) = build_test_scenario().await;
        let rpc_client: RpcClient = RpcClient::new(Cluster::Mainnet.url().to_string());
        let clock = get_clock(&rpc_client).await.unwrap();

        let other_mint = if meteora_dlmm.base_token_pk == sol_mint {
            meteora_dlmm.quote_token_pk
        } else {
            meteora_dlmm.base_token_pk
        };

        // First get a reference amount: how many tokens do we get for 1 SOL?
        let in_sol_amount: u64 = 1_000_000_000;
        let tokens_from_base_in = meteora_dlmm
            .swap_base_in(&accounts, sol_mint, in_sol_amount, clock.clone())
            .unwrap();
        debug_eprintln!(
            "reference swap_base_in: {} SOL -> {} TOKEN",
            in_sol_amount as f64 / 1e9,
            tokens_from_base_in as f64 / 1e6,
        );

        // swap_base_out: "I want exactly tokens_from_base_in tokens, how much SOL do I need?"
        // output_mint = other_mint (the token), amount_out = tokens_from_base_in
        let sol_needed = meteora_dlmm
            .swap_base_out(&accounts, other_mint, tokens_from_base_in, clock.clone())
            .unwrap();
        debug_eprintln!(
            "swap_base_out: need {} SOL to get {} TOKEN",
            sol_needed as f64 / 1e9,
            tokens_from_base_in as f64 / 1e6,
        );
        assert!(sol_needed > 0, "swap_base_out should require input");
        // For the same direction (SOL->TOKEN), exact_out should require
        // the same or slightly more SOL than exact_in provided
        let ratio = sol_needed as f64 / in_sol_amount as f64;
        debug_eprintln!("base_out/base_in ratio: {:.6}", ratio);
        assert!(
            ratio > 0.95 && ratio < 1.05,
            "swap_base_out should be close to swap_base_in: ratio={}",
            ratio,
        );

        // swap_base_out reverse: "I want 1 SOL out, how many tokens do I need?"
        let desired_sol_out: u64 = 1_000_000_000;
        let tokens_needed = meteora_dlmm
            .swap_base_out(&accounts, sol_mint, desired_sol_out, clock.clone())
            .unwrap();
        debug_eprintln!(
            "swap_base_out: need {} TOKEN to get {} SOL",
            tokens_needed as f64 / 1e6,
            desired_sol_out as f64 / 1e9,
        );
        assert!(tokens_needed > 0, "swap_base_out reverse should require input");
    }

    #[tokio::test]
    async fn test_dlmm_round_trip_consistency() {
        use anchor_client::Cluster;
        use solana_client::nonblocking::rpc_client::RpcClient;
        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let (meteora_dlmm, accounts) = build_test_scenario().await;
        let rpc_client: RpcClient = RpcClient::new(Cluster::Mainnet.url().to_string());
        let clock = get_clock(&rpc_client).await.unwrap();

        let other_mint = if meteora_dlmm.base_token_pk == sol_mint {
            meteora_dlmm.quote_token_pk
        } else {
            meteora_dlmm.base_token_pk
        };

        let (fee_factor, inverse_fee_factor) = meteora_dlmm.get_fee_factor().unwrap();
        debug_eprintln!("fee_factor: {:.6}, inverse: {:.6}", fee_factor, inverse_fee_factor);

        // swap_base_in: 1 SOL -> tokens
        let in_sol_amount: u64 = 1_000_000_000;
        let tokens_out = meteora_dlmm
            .swap_base_in(&accounts, sol_mint, in_sol_amount, clock.clone())
            .unwrap();

        // swap_base_out: to get those same tokens, how much SOL is needed?
        let sol_needed_for_same_tokens = meteora_dlmm
            .swap_base_out(&accounts, other_mint, tokens_out, clock.clone())
            .unwrap();

        debug_eprintln!(
            "swap_base_in:  {} SOL -> {} TOKEN",
            in_sol_amount as f64 / 1e9,
            tokens_out as f64 / 1e6,
        );
        debug_eprintln!(
            "swap_base_out: {} SOL needed for {} TOKEN",
            sol_needed_for_same_tokens as f64 / 1e9,
            tokens_out as f64 / 1e6,
        );

        // For the same direction (SOL->TOKEN), exact_out should require
        // the same or slightly more SOL than exact_in provided
        assert!(
            sol_needed_for_same_tokens >= in_sol_amount
                || (in_sol_amount - sol_needed_for_same_tokens) < in_sol_amount / 100,
            "exact_out should require ~same or more input: needed={}, provided={}",
            sol_needed_for_same_tokens,
            in_sol_amount,
        );
    }
}
