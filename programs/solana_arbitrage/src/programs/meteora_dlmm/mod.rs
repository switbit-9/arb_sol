use super::super::programs::ProgramMeta;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    account_info::next_account_info,
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use anchor_spl::token::spl_token::native_mint;
use dlmm::dlmm::accounts::{BinArray, BinArrayBitmapExtension, LbPair};
use dlmm::quote::quote_exact_in;
use dlmm::token::load_mint;
use std::collections::HashMap;

#[derive(Clone)]
pub struct MeteoraDlmm<'info> {
    pub program_id: AccountInfo<'info>,
    pub pool_id: AccountInfo<'info>,
    pub base_vault: AccountInfo<'info>,
    pub quote_vault: AccountInfo<'info>,
    pub base_token: AccountInfo<'info>,
    pub quote_token: AccountInfo<'info>,
    pub oracle: AccountInfo<'info>,
    pub host_fee_in: AccountInfo<'info>,
    pub memo: AccountInfo<'info>,
    pub event_authority: AccountInfo<'info>,
    pub bitmap_extension: AccountInfo<'info>,
    pub bin_arrays_buy: Option<Vec<AccountInfo<'info>>>,
    pub bin_arrays_sell: Option<Vec<AccountInfo<'info>>>,
}

impl<'info> ProgramMeta for MeteoraDlmm<'info> {
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

    fn swap_base_in(&self, amount_in: u64, clock: Clock) -> Result<u64> {
        self.swap_base_in_impl(amount_in, clock)
    }

    fn swap_base_out(&self, amount_in: u64, clock: Clock) -> Result<u64> {
        self.swap_base_out_impl(amount_in, clock)
    }

    fn invoke_swap_base_in<'a>(
        &self,
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

    fn log_accounts(&self) -> Result<()> {
        msg!(
            "Meteora DLMM accounts: program_id={}, pool_id={}, base_vault={}, quote_vault={}, base_token={}, quote_token={}, oracle={}, host_fee_in={}, memo={}, event_authority={}, bitmap_extension={}",
            self.program_id.key,
            self.pool_id.key,
            self.base_vault.key,
            self.quote_vault.key,
            self.base_token.key,
            self.quote_token.key,
            self.oracle.key,
            self.host_fee_in.key,
            self.memo.key,
            self.event_authority.key,
            self.bitmap_extension.key,
        );
        if let Some(bin_arrays_buy) = &self.bin_arrays_buy {
            for (i, account) in bin_arrays_buy.iter().enumerate() {
                msg!("bin_arrays_buy[{}]={}", i, account.key);
            }
        }
        if let Some(bin_arrays_sell) = &self.bin_arrays_sell {
            for (i, account) in bin_arrays_sell.iter().enumerate() {
                msg!("bin_arrays_sell[{}]={}", i, account.key);
            }
        }
        Ok(())
    }
}

impl<'info> MeteoraDlmm<'info> {
    pub const PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");
    pub fn new(accounts: &[AccountInfo<'info>]) -> Result<Self> {
        let mut iter = accounts.iter();
        let program_id = next_account_info(&mut iter)?; // 0
        let pool_id = next_account_info(&mut iter)?; // 1
        let base_vault = next_account_info(&mut iter)?; // 2
        let quote_vault = next_account_info(&mut iter)?; // 3
        let base_token = next_account_info(&mut iter)?; // 4
        let quote_token = next_account_info(&mut iter)?; // 5
        let oracle = next_account_info(&mut iter)?; // 6
        let host_fee_in = next_account_info(&mut iter)?; // 7
        let memo = next_account_info(&mut iter)?; // 8
        let event_authority = next_account_info(&mut iter)?; // 9
        let bin_array_bitmap_extension = next_account_info(&mut iter)?; // 10

        // Handle bin_arrays: they are split by SOL MINT account
        // Structure: [fixed accounts] [bin_arrays_buy...] [SOL_MINT] [bin_arrays_sell...]
        // We've consumed 11 accounts (0-10), so remaining start at index 11
        // NOTE: Temporarily disabled bin_arrays to avoid AccountInfo cloning issues
        // let (bin_arrays_buy, bin_arrays_sell) = (None, None);
        let (bin_arrays_buy, bin_arrays_sell) = if accounts.len() > 11 {
            let remaining = &accounts[11..];
            let sol_mint = native_mint::id();

            // Find position of SOL MINT separator
            let sol_mint_pos = remaining.iter().position(|acc| *acc.key == sol_mint);

            match sol_mint_pos {
                Some(pos) => {
                    // Split at SOL MINT position
                    let buy_slice = &remaining[..pos];
                    let after_sol = &remaining[pos + 1..]; // Skip SOL MINT itself
                    (
                        if buy_slice.is_empty() {
                            None
                        } else {
                            // Create Vec by cloning AccountInfo - safe since all have same lifetime
                            Some(buy_slice.iter().cloned().collect())
                        },
                        if after_sol.is_empty() {
                            None
                        } else {
                            Some(after_sol.iter().cloned().collect())
                        },
                    )
                }
                None => {
                    // No SOL MINT found, all remaining are buy arrays
                    (
                        if remaining.is_empty() {
                            None
                        } else {
                            Some(remaining.iter().cloned().collect())
                        },
                        None,
                    )
                }
            }
        } else {
            (None, None)
        };

        Ok(MeteoraDlmm {
            program_id: program_id.clone(),
            pool_id: pool_id.clone(),
            base_vault: base_vault.clone(),
            quote_vault: quote_vault.clone(),
            base_token: base_token.clone(),
            quote_token: quote_token.clone(),
            oracle: oracle.clone(),
            host_fee_in: host_fee_in.clone(),
            memo: memo.clone(),
            event_authority: event_authority.clone(),
            bitmap_extension: bin_array_bitmap_extension.clone(),
            bin_arrays_buy: bin_arrays_buy.clone(),
            bin_arrays_sell: bin_arrays_sell.clone(),
        })
    }

    pub fn swap_base_in_impl(&self, amount_in: u64, clock: Clock) -> Result<u64> {
        let pool_data = self.pool_id.try_borrow_data()?;
        if pool_data.len() < 8 {
            msg!("Pool ID account data too short: {} bytes", pool_data.len());
            return Err(anchor_lang::error::Error::from(
                anchor_lang::error::ErrorCode::AccountDiscriminatorNotFound,
            ));
        }
        let pool_data_slice = &pool_data[8..];
        let lb_pair_size = std::mem::size_of::<LbPair>();
        if pool_data_slice.len() < lb_pair_size {
            msg!(
                "Pool ID data too short for LbPair: {} bytes (expected {})",
                pool_data_slice.len(),
                lb_pair_size
            );
            return Err(anchor_lang::error::Error::from(
                anchor_lang::error::ErrorCode::AccountDiscriminatorNotFound,
            ));
        }
        let pool_id_state: LbPair = bytemuck::pod_read_unaligned(pool_data_slice);
        let pool_id_key = *self.pool_id.key;

        // Deserialize bitmap extension if available
        let bitmap_extension: Option<BinArrayBitmapExtension> =
            if *self.bitmap_extension.key == Self::PROGRAM_ID {
                None
            } else if self.bitmap_extension.data_len() > 8 {
                Some(bytemuck::pod_read_unaligned(
                    &self.bitmap_extension.try_borrow_data()?[8..],
                ))
            } else {
                None
            };

        // Deserialize bin arrays into HashMap (only buy arrays, using commons types)
        let bin_arrays: HashMap<Pubkey, BinArray> = self
            .bin_arrays_buy
            .as_ref()
            .into_iter()
            .flatten()
            .filter_map(|account_info| {
                let data = account_info.try_borrow_data().ok()?;
                if data.len() < 8 {
                    return None;
                }
                let bin_array_data = &data[8..];
                let bin_array_size = std::mem::size_of::<BinArray>();
                if bin_array_data.len() < bin_array_size {
                    return None;
                }
                let bin_array = bytemuck::pod_read_unaligned(bin_array_data);
                Some((*account_info.key, bin_array))
            })
            .collect();

        let swap_for_y = true;

        // Helper to load mints and call quote_exact_in, working around lifetime variance
        // Safe because InterfaceAccount just wraps AccountInfo and we're only changing
        // the lifetime annotation, not the actual data or memory layout
        let quote = {
            // Work around lifetime variance: cast references to AccountInfo to match expected lifetime
            let base_token_ref: &AccountInfo<'info> =
                unsafe { &*(&self.base_token as *const AccountInfo<'info>) };
            let quote_token_ref: &AccountInfo<'info> =
                unsafe { &*(&self.quote_token as *const AccountInfo<'info>) };

            let mint_x_account = load_mint(base_token_ref).map_err(|e| {
                msg!("Failed to load mint X: {:?}", e);
                anchor_lang::error::Error::from(anchor_lang::error::ErrorCode::ConstraintOwner)
            })?;
            let mint_y_account = load_mint(quote_token_ref).map_err(|e| {
                msg!("Failed to load mint Y: {:?}", e);
                anchor_lang::error::Error::from(anchor_lang::error::ErrorCode::ConstraintOwner)
            })?;

            unsafe {
                let mint_x_ref: &InterfaceAccount<'_, anchor_spl::token_interface::Mint> =
                    &*(&mint_x_account
                        as *const InterfaceAccount<'_, anchor_spl::token_interface::Mint>);
                let mint_y_ref: &InterfaceAccount<'_, anchor_spl::token_interface::Mint> =
                    &*(&mint_y_account
                        as *const InterfaceAccount<'_, anchor_spl::token_interface::Mint>);
                quote_exact_in(
                    pool_id_key,
                    &pool_id_state,
                    amount_in,
                    swap_for_y,
                    bin_arrays,
                    bitmap_extension.as_ref(),
                    &clock,
                    mint_x_ref,
                    mint_y_ref,
                )
            }
        }
        .map_err(|e| {
            msg!("Quote error: {:?}", e);
            anchor_lang::error::Error::from(anchor_lang::error::ErrorCode::ConstraintOwner)
        })?;
        Ok(quote.amount_out)
    }

    pub fn swap_base_out_impl(&self, amount_in: u64, clock: Clock) -> Result<u64> {
        let pool_data = self.pool_id.try_borrow_data()?;
        if pool_data.len() < 8 {
            msg!("Pool ID account data too short: {} bytes", pool_data.len());
            return Err(anchor_lang::error::Error::from(
                anchor_lang::error::ErrorCode::AccountDiscriminatorNotFound,
            ));
        }
        let pool_data_slice = &pool_data[8..];
        let lb_pair_size = std::mem::size_of::<LbPair>();
        if pool_data_slice.len() < lb_pair_size {
            return Err(anchor_lang::error::Error::from(
                anchor_lang::error::ErrorCode::AccountDiscriminatorNotFound,
            ));
        }
        let lb_pair_state: LbPair = bytemuck::pod_read_unaligned(pool_data_slice);
        let lb_pair_key = *self.pool_id.key;

        // Deserialize bitmap extension if available
        let bitmap_extension: Option<BinArrayBitmapExtension> =
            if *self.bitmap_extension.key == Self::PROGRAM_ID {
                None
            } else if self.bitmap_extension.data_len() > 8 {
                Some(bytemuck::pod_read_unaligned(
                    &self.bitmap_extension.try_borrow_data()?[8..],
                ))
            } else {
                None
            };

        // Deserialize bin arrays into HashMap (only sell arrays, using commons types)
        let bin_arrays: HashMap<Pubkey, BinArray> = self
            .bin_arrays_sell
            .as_ref()
            .into_iter()
            .flatten()
            .filter_map(|account_info| {
                let data = account_info.try_borrow_data().ok()?;
                if data.len() < 8 {
                    return None;
                }
                let bin_array_data = &data[8..];
                let bin_array_size = std::mem::size_of::<BinArray>();
                if bin_array_data.len() < bin_array_size {
                    return None;
                }
                let bin_array = bytemuck::pod_read_unaligned(bin_array_data);
                Some((*account_info.key, bin_array))
            })
            .collect();

        let swap_for_y = false;

        // Helper to load mints and call quote_exact_in, working around lifetime variance
        // Safe because InterfaceAccount just wraps AccountInfo and we're only changing
        // the lifetime annotation, not the actual data or memory layout
        let quote = {
            // Work around lifetime variance: cast references to AccountInfo to match expected lifetime
            let base_token_ref: &AccountInfo<'info> =
                unsafe { &*(&self.base_token as *const AccountInfo<'info>) };
            let quote_token_ref: &AccountInfo<'info> =
                unsafe { &*(&self.quote_token as *const AccountInfo<'info>) };

            let mint_x_account = load_mint(base_token_ref).map_err(|_e| {
                anchor_lang::error::Error::from(anchor_lang::error::ErrorCode::ConstraintOwner)
            })?;
            let mint_y_account = load_mint(quote_token_ref).map_err(|_e| {
                anchor_lang::error::Error::from(anchor_lang::error::ErrorCode::ConstraintOwner)
            })?;

            unsafe {
                let mint_x_ref: &InterfaceAccount<'_, anchor_spl::token_interface::Mint> =
                    &*(&mint_x_account
                        as *const InterfaceAccount<'_, anchor_spl::token_interface::Mint>);
                let mint_y_ref: &InterfaceAccount<'_, anchor_spl::token_interface::Mint> =
                    &*(&mint_y_account
                        as *const InterfaceAccount<'_, anchor_spl::token_interface::Mint>);
                quote_exact_in(
                    lb_pair_key,
                    &lb_pair_state,
                    amount_in,
                    swap_for_y,
                    bin_arrays,
                    bitmap_extension.as_ref(),
                    &clock,
                    mint_x_ref,
                    mint_y_ref,
                )
            }
        }
        .map_err(|e| {
            msg!("Quote error: {:?}", e);
            anchor_lang::error::Error::from(anchor_lang::error::ErrorCode::ConstraintOwner)
        })?;
        Ok(quote.amount_out)
    }

    pub fn invoke_swap_base_in_impl<'a>(
        &self,
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
        ) = if mint_1_account.key == self.base_token.key {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
            )
        } else if mint_2_account.key == self.base_token.key {
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

        let mut metas = vec![
            AccountMeta::new(*self.pool_id.key, false),
            AccountMeta::new_readonly(*self.bitmap_extension.key, false),
            AccountMeta::new(*self.base_vault.key, false),
            AccountMeta::new(*self.quote_vault.key, false),
            AccountMeta::new(*user_base_token_account.key, false),
            AccountMeta::new(*user_quote_token_account.key, false),
            AccountMeta::new_readonly(*self.base_token.key, false),
            AccountMeta::new_readonly(*self.quote_token.key, false),
            AccountMeta::new_readonly(*self.oracle.key, false), // ?
            AccountMeta::new(*self.host_fee_in.key, false),     // ?
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*base_token_program.key, false),
            AccountMeta::new_readonly(*quote_token_program.key, false),
            AccountMeta::new_readonly(*self.memo.key, false),
            AccountMeta::new_readonly(*self.event_authority.key, false),
            AccountMeta::new_readonly(Self::PROGRAM_ID, false),
        ];
        // Add bin arrays (buy arrays for swap_base_in)
        if let Some(bin_arrays) = &self.bin_arrays_buy {
            for account in bin_arrays {
                metas.push(AccountMeta::new(*account.key, false));
            }
        }

        let mut data = vec![43, 215, 247, 132, 137, 60, 243, 81]; // TODO: Add proper instruction discriminator
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&amount_out_value.to_le_bytes());

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data,
        };

        // Collect AccountInfo into a vector and use unsafe to cast lifetimes
        // Order must match metas order exactly
        let mut accounts_vec: Vec<AccountInfo<'info>> = vec![
            self.pool_id.to_account_info(),          // 0: pool_id
            self.bitmap_extension.to_account_info(), // 1: bitmap_extension (readonly)
            self.base_vault.to_account_info(),       // 2: base_vault
            self.quote_vault.to_account_info(),      // 3: quote_vault
            unsafe { std::mem::transmute(user_base_token_account.to_account_info()) }, // 4: user_base_token_account
            unsafe { std::mem::transmute(user_quote_token_account.to_account_info()) }, // 5: user_quote_token_account
            self.base_token.to_account_info(), // 6: base_token (readonly)
            self.quote_token.to_account_info(), // 7: quote_token (readonly)
            self.oracle.to_account_info(),     // 8: oracle (readonly)
            self.host_fee_in.to_account_info(), // 9: host_fee_in
            unsafe { std::mem::transmute(payer.to_account_info()) }, // 10: payer (signer)
            unsafe { std::mem::transmute(base_token_program.to_account_info()) }, // 11: base_token_program (readonly)
            unsafe { std::mem::transmute(quote_token_program.to_account_info()) }, // 12: quote_token_program (readonly)
            self.memo.to_account_info(),            // 13: memo (readonly)
            self.event_authority.to_account_info(), // 14: event_authority (readonly)
            self.program_id.to_account_info(),      // 15: program_id (readonly)
        ];
        // Add bin arrays (buy arrays for swap_base_in)
        if let Some(bin_arrays) = &self.bin_arrays_buy {
            for account in bin_arrays {
                accounts_vec.push(account.to_account_info());
            }
        }

        unsafe {
            let accounts: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts)?;
        }
        Ok(())
    }

    pub fn invoke_swap_base_out_impl<'a>(
        &self,
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
        ) = if mint_1_account.key == self.base_token.key {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
            )
        } else if mint_2_account.key == self.base_token.key {
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

        let mut metas = vec![
            AccountMeta::new(*self.pool_id.key, false),
            AccountMeta::new(*self.bitmap_extension.key, false),
            AccountMeta::new(*self.base_vault.key, false),
            AccountMeta::new(*self.quote_vault.key, false),
            AccountMeta::new(*user_base_token_account.key, false),
            AccountMeta::new(*user_quote_token_account.key, false),
            AccountMeta::new_readonly(*self.base_token.key, false),
            AccountMeta::new_readonly(*self.quote_token.key, false),
            AccountMeta::new_readonly(*self.oracle.key, false),
            AccountMeta::new(*self.host_fee_in.key, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*base_token_program.key, false),
            AccountMeta::new_readonly(*quote_token_program.key, false),
            AccountMeta::new_readonly(*self.memo.key, false),
            AccountMeta::new_readonly(*self.event_authority.key, false),
            AccountMeta::new_readonly(Self::PROGRAM_ID, false),
        ];
        // Add bin arrays (sell arrays for swap_base_out)
        if let Some(bin_arrays) = &self.bin_arrays_sell {
            for account in bin_arrays {
                metas.push(AccountMeta::new(*account.key, false));
            }
        }

        let mut data = vec![43, 215, 247, 132, 137, 60, 243, 81];
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out_value.to_le_bytes());

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data,
        };

        // Collect AccountInfo into a vector and use unsafe to cast lifetimes
        // Order must match metas order exactly
        let mut accounts_vec: Vec<AccountInfo<'info>> = vec![
            self.pool_id.to_account_info(),          // 0: pool_id
            self.bitmap_extension.to_account_info(), // 1: bitmap_extension
            self.base_vault.to_account_info(),       // 2: base_vault
            self.quote_vault.to_account_info(),      // 3: quote_vault
            unsafe { std::mem::transmute(user_base_token_account.to_account_info()) }, // 4: user_base_token_account
            unsafe { std::mem::transmute(user_quote_token_account.to_account_info()) }, // 5: user_quote_token_account
            self.base_token.to_account_info(), // 6: base_token (readonly)
            self.quote_token.to_account_info(), // 7: quote_token (readonly)
            self.oracle.to_account_info(),     // 8: oracle (readonly)
            self.host_fee_in.to_account_info(), // 9: host_fee_in
            unsafe { std::mem::transmute(payer.to_account_info()) }, // 10: payer (signer)
            unsafe { std::mem::transmute(base_token_program.to_account_info()) }, // 11: base_token_program (readonly)
            unsafe { std::mem::transmute(quote_token_program.to_account_info()) }, // 12: quote_token_program (readonly)
            self.memo.to_account_info(),            // 13: memo (readonly)
            self.event_authority.to_account_info(), // 14: event_authority (readonly)
            self.program_id.to_account_info(),      // 15: program_id (readonly)
        ];
        // Add bin arrays (sell arrays for swap_base_out)
        if let Some(bin_arrays) = &self.bin_arrays_sell {
            for account in bin_arrays {
                accounts_vec.push(account.to_account_info());
            }
        }

        unsafe {
            let accounts: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::prelude::{Clock, InterfaceAccount};
    use anchor_lang::solana_program::{account_info::AccountInfo, pubkey::Pubkey, system_program};
    use anchor_spl::token_interface::Mint;
    use dlmm;

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

    #[tokio::test]
    async fn test_dlmm_swap_quote_exact_in() {
        use anchor_client::Cluster;
        use solana_client::nonblocking::rpc_client::RpcClient;
        use std::collections::HashMap;

        // RPC client. No gPA is required.
        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        let sol_usdc = Pubkey::from_str_const("Cgnuirsk5dQ9Ka1Grnru7J8YW1sYncYUjiXvYxT7G4iZ");

        let pool_id_account = rpc_client.get_account(&sol_usdc).await.unwrap();

        let pool_id: dlmm::dlmm::accounts::LbPair =
            bytemuck::pod_read_unaligned(&pool_id_account.data[8..]);

        eprintln!("base_token: {:?}", pool_id.token_x_mint);
        eprintln!("quote_token: {:?}", pool_id.token_y_mint);

        let mut mint_accounts = rpc_client
            .get_multiple_accounts(&[pool_id.token_x_mint, pool_id.token_y_mint])
            .await
            .unwrap();

        let mint_x_account = mint_accounts[0].take().unwrap();
        let mint_y_account = mint_accounts[1].take().unwrap();

        // 3 bin arrays to left, and right is enough to cover most of the swap, and stay under 1.4m CU constraint.
        // Get 3 bin arrays to the left from the active bin
        let left_bin_array_pubkeys =
            dlmm::get_bin_array_pubkeys_for_swap(sol_usdc, &pool_id, None, true, 3).unwrap();

        // Get 3 bin arrays to the right the from active bin
        let right_bin_array_pubkeys =
            dlmm::get_bin_array_pubkeys_for_swap(sol_usdc, &pool_id, None, false, 3).unwrap();

        // Fetch bin arrays separately for buy and sell
        let all_bin_array_pubkeys: Vec<Pubkey> = left_bin_array_pubkeys
            .iter()
            .chain(right_bin_array_pubkeys.iter())
            .cloned()
            .collect();

        let bin_array_accounts = rpc_client
            .get_multiple_accounts(&all_bin_array_pubkeys)
            .await
            .unwrap();

        // Process left arrays (buy) and right arrays (sell) separately
        let mut bin_array_buy_infos = Vec::new();
        let mut bin_array_sell_infos = Vec::new();
        let mut bin_arrays_buy_map = HashMap::new();
        let mut bin_arrays_sell_map = HashMap::new();
        let mut bin_arrays_combined = HashMap::new(); // Combined for quote function

        // Process left arrays (buy)
        for (account_opt, key) in bin_array_accounts
            .iter()
            .take(left_bin_array_pubkeys.len())
            .zip(left_bin_array_pubkeys.iter())
        {
            if let Some(account) = account_opt {
                let bin_array = bytemuck::pod_read_unaligned::<dlmm::dlmm::accounts::BinArray>(
                    &account.data[8..],
                );
                let account_info = account_to_account_info(*key, account.clone());
                bin_array_buy_infos.push(account_info);
                bin_arrays_buy_map.insert(*key, bin_array);
                bin_arrays_combined.insert(*key, bin_array);
            }
        }

        // Process right arrays (sell)
        for (account_opt, key) in bin_array_accounts
            .iter()
            .skip(left_bin_array_pubkeys.len())
            .zip(right_bin_array_pubkeys.iter())
        {
            if let Some(account) = account_opt {
                let bin_array = bytemuck::pod_read_unaligned::<dlmm::dlmm::accounts::BinArray>(
                    &account.data[8..],
                );
                let account_info = account_to_account_info(*key, account.clone());
                bin_array_sell_infos.push(account_info);
                bin_arrays_sell_map.insert(*key, bin_array);
                bin_arrays_combined.insert(*key, bin_array);
            }
        }

        // Use combined map for quote function
        let bin_arrays = bin_arrays_combined;

        // Derive vault PDAs
        let (base_vault_key, _) = dlmm::derive_reserve_pda(pool_id.token_x_mint, sol_usdc);
        let (quote_vault_key, _) = dlmm::derive_reserve_pda(pool_id.token_y_mint, sol_usdc);

        // Derive other PDAs
        let (oracle_key, _) = dlmm::derive_oracle_pda(sol_usdc);
        let (bitmap_extension_key, _) = dlmm::derive_bin_array_bitmap_extension(sol_usdc);
        let (event_authority_key, _) = dlmm::derive_event_authority_pda();

        // Use placeholder keys for optional accounts
        let host_fee_in_key = Pubkey::new_unique();
        let memo_key = Pubkey::new_unique();

        // Convert RPC accounts to AccountInfo
        eprintln!(
            "pool_id_account.data.len() before conversion: {}",
            pool_id_account.data.len()
        );
        let pool_id_account_info = account_to_account_info(sol_usdc, pool_id_account);
        let pool_id_data_len = pool_id_account_info.data_len();
        eprintln!(
            "pool_id_account_info.data_len() after conversion: {}",
            pool_id_data_len
        );
        let base_vault = fetch_account_info_from_rpc(&rpc_client, base_vault_key).await;
        let quote_vault = fetch_account_info_from_rpc(&rpc_client, quote_vault_key).await;
        let base_token = account_to_account_info(pool_id.token_x_mint, mint_x_account);
        let quote_token = account_to_account_info(pool_id.token_y_mint, mint_y_account);
        let oracle = fetch_account_info_from_rpc(&rpc_client, oracle_key).await;
        let bitmap_extension = fetch_account_info_from_rpc(&rpc_client, bitmap_extension_key).await;

        // Create mock accounts for optional fields
        let host_fee_in =
            create_mock_account_info_with_data(host_fee_in_key, system_program::id(), None);
        let memo = create_mock_account_info_with_data(memo_key, system_program::id(), None);
        let event_authority =
            create_mock_account_info_with_data(event_authority_key, system_program::id(), None);

        // Create program_id account (first account in MeteoraDlmm::new)
        let program_id_key = MeteoraDlmm::PROGRAM_ID;
        let program_id_account =
            create_mock_account_info_with_data(program_id_key, system_program::id(), None);

        let mut accounts = vec![
            program_id_account,   // 0: program_id (required by MeteoraDlmm::new)
            pool_id_account_info, // 1: pool_id
            base_vault,           // 2: base_vault
            quote_vault,          // 3: quote_vault
            base_token,           // 4: base_token
            quote_token,          // 5: quote_token
            oracle,               // 6: oracle
            host_fee_in,          // 7: host_fee_in
            memo,                 // 8: memo
            event_authority,      // 9: event_authority
            bitmap_extension,     // 10: bitmap_extension
        ];

        // Add bin array accounts: buy arrays, then SOL MINT separator, then sell arrays
        accounts.extend(bin_array_buy_infos);
        // Add SOL MINT as separator - fetch it from RPC
        let sol_mint_key = anchor_spl::token::spl_token::native_mint::id();
        let sol_mint_account_info = fetch_account_info_from_rpc(&rpc_client, sol_mint_key).await;
        accounts.push(sol_mint_account_info);
        accounts.extend(bin_array_sell_infos);

        let meteora_dlmm = MeteoraDlmm::new(&accounts).unwrap();

        // 1 SOL -> USDC
        let in_sol_amount = 1_000_000_000;
        let clock1 = get_clock(&rpc_client).await.unwrap();

        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");

        // Determine swap_for_y: if SOL is token_x, we swap X for Y (swap_for_y = true)
        // If SOL is token_y, we swap Y for X (swap_for_y = false)
        let swap_for_y = pool_id.token_x_mint == sol_mint;
        eprintln!("clock11: {:?}", swap_for_y);

        if swap_for_y {
            let quote_result = meteora_dlmm.swap_base_in(in_sol_amount, clock1).unwrap();
            eprintln!("1 SOL -> {:?} TOKEN", quote_result as f64);
        } else {
            let quote_result = meteora_dlmm.swap_base_out(in_sol_amount, clock1).unwrap();
            eprintln!("1 SOL -> {:?} TOKEN", quote_result as f64);
        }
        eprintln!("clock2: {:?}", swap_for_y);

        // Fetch mint accounts again for the second quote call
        let mut mint_accounts2 = rpc_client
            .get_multiple_accounts(&[pool_id.token_x_mint, pool_id.token_y_mint])
            .await
            .unwrap();
        let mint_x_account2 = mint_accounts2[0].take().unwrap();
        let mint_y_account2 = mint_accounts2[1].take().unwrap();

        let clock2 = get_clock(&rpc_client).await.unwrap();

        let mint_x_interface = account_to_interface_mint(mint_x_account2, pool_id.token_x_mint);
        let mint_y_interface = account_to_interface_mint(mint_y_account2, pool_id.token_y_mint);

        let quote_result = dlmm::quote_exact_in(
            sol_usdc,
            &pool_id,
            in_sol_amount,
            swap_for_y,
            bin_arrays.clone(),
            None,
            &clock2,
            &mint_x_interface,
            &mint_y_interface,
        )
        .unwrap();

        let amount_out_2 = quote_result.amount_out;

        eprintln!("1 SOL -> {:?} TOKEN", amount_out_2);

        // For TOKEN -> SOL: if SOL is token_x, we swap Y for X (swap_for_y = false)
        // If SOL is token_y, we swap X for Y (swap_for_y = true)
        let swap_for_y_reverse = !swap_for_y;

        if swap_for_y_reverse {
            let quote_result = meteora_dlmm.swap_base_in(amount_out_2, clock2).unwrap();
            eprintln!(
                "{:?} TOKEN -> {:?} SOL",
                amount_out_2,
                quote_result as f64 / 1_000_000_000.0
            );
        } else {
            let quote_result = meteora_dlmm.swap_base_out(amount_out_2, clock2).unwrap();
            eprintln!(
                "{:?} TOKEN -> {:?} SOL",
                amount_out_2,
                quote_result as f64 / 1_000_000_000.0
            );
        }

        // Fetch clock again for the quote call (clock2 was moved in swap_base_in/swap_base_out)
        let clock3 = get_clock(&rpc_client).await.unwrap();

        let quote_result = dlmm::quote_exact_in(
            sol_usdc,
            &pool_id,
            amount_out_2,
            swap_for_y_reverse,
            bin_arrays.clone(),
            None,
            &clock3,
            &mint_x_interface,
            &mint_y_interface,
        )
        .unwrap();

        eprintln!(
            "{:?} TOKEN -> {:?} SOL",
            amount_out_2,
            quote_result.amount_out as f64 / 1_000_000_000.0
        );
    }
}
