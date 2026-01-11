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
use dlmm::dlmm::accounts::{BinArrayBitmapExtension, LbPair};
use dlmm::pda;
use dlmm::quote::quote_exact_in;
use dlmm::token::load_mint;

#[derive(Clone)]
pub struct MeteoraDlmm<'info> {
    pub accounts: Vec<AccountInfo<'info>>,
    pub program_id: AccountInfo<'info>,
    pub pool_id: AccountInfo<'info>,
    pub base_vault: AccountInfo<'info>,
    pub quote_vault: AccountInfo<'info>,
    pub base_token: AccountInfo<'info>,
    pub quote_token: AccountInfo<'info>,
    // pub oracle: AccountInfo<'info>,
    // pub host_fee_in: AccountInfo<'info>,
    // pub memo: AccountInfo<'info>,
    // pub event_authority: AccountInfo<'info>,
    // pub bitmap_extension: AccountInfo<'info>,
    // pub bin_arrays_buy: Option<Vec<AccountInfo<'info>>>,
    // pub bin_arrays_sell: Option<Vec<AccountInfo<'info>>>,
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

    fn swap_base_in(&self, input_mint: Pubkey, amount_in: u64, clock: Clock) -> Result<u64> {
        self.swap_base_in_impl(input_mint, amount_in, clock)
    }

    fn swap_base_out(&self, input_mint: Pubkey, amount_in: u64, clock: Clock) -> Result<u64> {
        self.swap_base_in_impl(input_mint, amount_in, clock)
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
        let stored_accounts = self.accounts.clone();
        let program_id = &stored_accounts[0];
        let pool_id = &stored_accounts[1];
        let base_vault = &stored_accounts[2];
        let quote_vault = &stored_accounts[3];
        let base_token = &stored_accounts[4];
        let quote_token = &stored_accounts[5];
        let oracle = &stored_accounts[6];
        let host_fee_in = &stored_accounts[7];
        let memo = &stored_accounts[8];
        let event_authority = &stored_accounts[9];
        let bitmap_extension = &stored_accounts[10];

        msg!("Program ID: {}", program_id.key);
        msg!("Pool ID: {}", pool_id.key);
        msg!("Base Vault: {}", base_vault.key);
        msg!("Quote Vault: {}", quote_vault.key);
        msg!("Base Token: {}", base_token.key);
        msg!("Quote Token: {}", quote_token.key);
        msg!("Oracle: {}", oracle.key);
        msg!("Host Fee In: {}", host_fee_in.key);
        msg!("Memo: {}", memo.key);
        msg!("Event Authority: {}", event_authority.key);
        msg!("Bitmap Extension: {}", bitmap_extension.key);

        let bin_arrays_buy = self.get_bin_arrays_buy();
        if let Some(bin_arrays) = bin_arrays_buy {
            msg!("Found {} buy bin arrays", bin_arrays.len());
            for (idx, account) in bin_arrays.iter().enumerate() {
                msg!("Buy bin array [{}]: {}", idx, account.key);
            }
        } else {
            msg!("No buy bin found");
        }

        let bin_arrays_sell = self.get_bin_arrays_sell();
        if let Some(bin_arrays) = bin_arrays_sell {
            msg!("Found {} sell bin arrays", bin_arrays.len());
            for (idx, account) in bin_arrays.iter().enumerate() {
                msg!("Sell bin array [{}]: {}", idx, account.key);
            }
        } else {
            msg!("No sell bin found");
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
                                                         // let oracle = next_account_info(&mut iter)?; // 6
                                                         // let host_fee_in = next_account_info(&mut iter)?; // 7
                                                         // let memo = next_account_info(&mut iter)?; // 8
                                                         // let event_authority = next_account_info(&mut iter)?; // 9
                                                         // let bin_array_bitmap_extension = next_account_info(&mut iter)?; // 10

        // Handle bin_arrays: they are split by SOL MINT account
        // Structure: [fixed accounts] [bin_arrays_buy...] [SOL_MINT] [bin_arrays_sell...]
        // We've consumed 11 accounts (0-10), so remaining start at index 11
        // let bin_arrays_buy = self.get_bin_arrays_buy();
        // let bin_arrays_sell = self.get_bin_arrays_sell();

        Ok(MeteoraDlmm {
            accounts: accounts.to_vec(),
            program_id: program_id.clone(),
            pool_id: pool_id.clone(),
            base_vault: base_vault.clone(),
            quote_vault: quote_vault.clone(),
            base_token: base_token.clone(),
            quote_token: quote_token.clone(),
            // oracle: oracle.clone(),
            // host_fee_in: host_fee_in.clone(),
            // memo: memo.clone(),
            // event_authority: event_authority.clone(),
            // bitmap_extension: bin_array_bitmap_extension.clone(),
            // bin_arrays_buy: bin_arrays_buy.clone(),
            // bin_arrays_sell: bin_arrays_sell.clone(),
        })
    }

    /// Extract bin arrays for buying from accounts starting at index 11
    /// Structure: [fixed accounts] [bin_arrays_buy...] [SOL_MINT] [bin_arrays_sell...]
    fn get_bin_arrays_buy(&self) -> Option<Vec<AccountInfo<'info>>> {
        if self.accounts.len() <= 11 {
            return None;
        }

        let remaining = &self.accounts[11..];
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
    fn get_bin_arrays_sell(&self) -> Option<Vec<AccountInfo<'info>>> {
        if self.accounts.len() <= 11 {
            return None;
        }

        let remaining = &self.accounts[11..];
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
        self.log_accounts()?;
        msg!("2");
        let pool_data = self.pool_id.try_borrow_data()?;
        if pool_data.len() < 8 {
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
        let pool_id_state: LbPair = bytemuck::pod_read_unaligned(pool_data_slice);
        let pool_id_key = *self.pool_id.key;

        let swap_for_y = input_mint == pool_id_state.token_x_mint;

        // Deserialize bitmap extension if available
        let bitmap_extension_account = &self.accounts[10];
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

        // Keep bin_array_accounts alive in the same scope where it's used
        let bin_arrays = self.get_bin_arrays_buy().unwrap_or_default();

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
                    pool_id_key,
                    &pool_id_state,
                    amount_in,
                    swap_for_y, // swap_for_y
                    bin_arrays,
                    bitmap_extension.as_ref(),
                    &clock,
                    mint_x_ref,
                    mint_y_ref,
                )
            }
        }
        .map_err(|_e| {
            anchor_lang::error::Error::from(anchor_lang::error::ErrorCode::ConstraintOwner)
        })?;
        Ok(quote.amount_out)
    }

    pub fn swap_base_out_impl(
        &self,
        input_mint: Pubkey,
        amount_in: u64,
        clock: Clock,
    ) -> Result<u64> {
        self.log_accounts()?;
        msg!("2");
        let pool_data = self.pool_id.try_borrow_data()?;
        if pool_data.len() < 8 {
            return Err(anchor_lang::error::Error::from(
                anchor_lang::error::ErrorCode::AccountDiscriminatorNotFound,
            ));
        }
        msg!("3");
        let pool_data_slice = &pool_data[8..];
        let lb_pair_size = std::mem::size_of::<LbPair>();
        if pool_data_slice.len() < lb_pair_size {
            return Err(anchor_lang::error::Error::from(
                anchor_lang::error::ErrorCode::AccountDiscriminatorNotFound,
            ));
        }
        msg!("4");
        let lb_pair_state: LbPair = bytemuck::pod_read_unaligned(pool_data_slice);
        let lb_pair_key = *self.pool_id.key;

        let swap_for_y = input_mint == lb_pair_state.token_x_mint;

        // Deserialize bitmap extension if available
        let bitmap_extension_account = &self.accounts[10];
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
        msg!("5");
        // For swap_base_out: we want base token OUT, so we're putting quote token IN
        // This means we're swapping FOR base token (X), so swap_for_y = false
        // For swap_for_y = false, we need BUY arrays (arrays to the right)
        // Keep bin_array_accounts alive in the same scope where it's used
        let bin_arrays = self.get_bin_arrays_buy().unwrap_or_default();
        msg!(
            "5.1: Using {} buy bin arrays for swap_base_out",
            bin_arrays.len()
        );
        for account in &bin_arrays {
            msg!("b: {}", account.key);
        }
        msg!("6");
        // Helper to load mints and call quote_exact_in, working around lifetime variance
        // Safe because InterfaceAccount just wraps AccountInfo and we're only changing
        // the lifetime annotation, not the actual data or memory layout
        let quote = {
            // Work around lifetime variance: cast references to AccountInfo to match expected lifetime
            let base_token_ref: &AccountInfo<'info> =
                unsafe { &*(&self.base_token as *const AccountInfo<'info>) };
            let quote_token_ref: &AccountInfo<'info> =
                unsafe { &*(&self.quote_token as *const AccountInfo<'info>) };

            msg!(
                "6.1: Loading mints - base_token: {}, quote_token: {}",
                base_token_ref.key,
                quote_token_ref.key
            );
            let mint_x_account = load_mint(base_token_ref).map_err(|e| {
                msg!(
                    "ERROR loading base_token mint {}: {:?}",
                    base_token_ref.key,
                    e
                );
                anchor_lang::error::Error::from(anchor_lang::error::ErrorCode::ConstraintOwner)
            })?;
            let mint_y_account = load_mint(quote_token_ref).map_err(|e| {
                msg!(
                    "ERROR loading quote_token mint {}: {:?}",
                    quote_token_ref.key,
                    e
                );
                anchor_lang::error::Error::from(anchor_lang::error::ErrorCode::ConstraintOwner)
            })?;
            msg!("6.2: Mints loaded successfully");

            unsafe {
                let mint_x_ref: &InterfaceAccount<'_, anchor_spl::token_interface::Mint> =
                    &*(&mint_x_account
                        as *const InterfaceAccount<'_, anchor_spl::token_interface::Mint>);
                let mint_y_ref: &InterfaceAccount<'_, anchor_spl::token_interface::Mint> =
                    &*(&mint_y_account
                        as *const InterfaceAccount<'_, anchor_spl::token_interface::Mint>);
                msg!(
                    "7: Calling quote_exact_in with amount_in={}, swap_for_y=false",
                    amount_in
                );
                quote_exact_in(
                    lb_pair_key,
                    &lb_pair_state,
                    amount_in,
                    swap_for_y, // swap_for_y = false means swapping FOR X (base token), so we need buy arrays
                    bin_arrays,
                    bitmap_extension.as_ref(),
                    &clock,
                    mint_x_ref,
                    mint_y_ref,
                )
            }
        }
        .map_err(|e| {
            msg!("ERROR in quote_exact_in: {:?}", e);
            // Try to preserve the original error if possible, otherwise use ConstraintOwner
            anchor_lang::error::Error::from(anchor_lang::error::ErrorCode::ConstraintOwner)
        })?;
        msg!("8: quote success, amount_out={}", quote.amount_out);
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
        msg!("1");

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

        // Get stored accounts from self.accounts - these are the accounts stored in the struct
        let stored_accounts = self.accounts.clone();
        let program_id_stored = &stored_accounts[0];
        let pool_id = &stored_accounts[1];
        let base_vault = &stored_accounts[2];
        let quote_vault = &stored_accounts[3];
        let base_token = &stored_accounts[4];
        let quote_token = &stored_accounts[5];
        let oracle = &stored_accounts[6];
        let host_fee_in = &stored_accounts[7];
        let memo = &stored_accounts[8];
        let event_authority = &stored_accounts[9];
        let bitmap_extension = &stored_accounts[10];
        msg!("pool_id: {}", pool_id.key);
        msg!("bitmap_extension: {}", bitmap_extension.key);
        msg!("base_vault: {}", base_vault.key);
        msg!("quote_vault: {}", quote_vault.key);
        msg!("user_base_token_account: {}", user_base_token_account.key);
        msg!("user_quote_token_account: {}", user_quote_token_account.key);
        msg!("base_token: {}", base_token.key);
        msg!("quote_token: {}", quote_token.key);
        msg!("oracle: {}", oracle.key);
        msg!("host_fee_in: {}", host_fee_in.key);
        msg!("memo: {}", memo.key);
        msg!("event_authority: {}", event_authority.key);
        msg!("bitmap_extension: {}", bitmap_extension.key);
        let mut metas = vec![
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new_readonly(*bitmap_extension.key, false),
            AccountMeta::new(*base_vault.key, false),
            AccountMeta::new(*quote_vault.key, false),
            AccountMeta::new(*user_base_token_account.key, false),
            AccountMeta::new(*user_quote_token_account.key, false),
            AccountMeta::new_readonly(*base_token.key, false),
            AccountMeta::new_readonly(*quote_token.key, false),
            AccountMeta::new_readonly(*oracle.key, false),
            AccountMeta::new(*host_fee_in.key, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*base_token_program.key, false),
            AccountMeta::new_readonly(*quote_token_program.key, false),
            AccountMeta::new_readonly(*memo.key, false),
            AccountMeta::new_readonly(*event_authority.key, false),
            AccountMeta::new_readonly(Self::PROGRAM_ID, false),
        ];
        // Add bin arrays (buy arrays for swap_base_in)
        let bin_arrays = self.get_bin_arrays_buy();
        if let Some(bin_arrays) = bin_arrays {
            for account in bin_arrays {
                msg!("b: {}", account.key);
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
            memo.clone(),              // 13: memo (readonly)
            event_authority.clone(),   // 14: event_authority (readonly)
            program_id_stored.clone(), // 15: program_id (readonly)
        ];
        // Add bin arrays (buy arrays for swap_base_in)
        let bin_arrays = self.get_bin_arrays_buy();
        if let Some(bin_arrays) = bin_arrays {
            for account in bin_arrays {
                accounts_vec.push(account);
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
        msg!("1");
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

        // Get stored accounts from self.accounts - these are the accounts stored in the struct
        let stored_accounts = self.accounts.clone();
        let program_id_stored = &stored_accounts[0];
        let pool_id = &stored_accounts[1];
        let base_vault = &stored_accounts[2];
        let quote_vault = &stored_accounts[3];
        let base_token = &stored_accounts[4];
        let quote_token = &stored_accounts[5];
        let oracle = &stored_accounts[6];
        let host_fee_in = &stored_accounts[7];
        let memo = &stored_accounts[8];
        let event_authority = &stored_accounts[9];
        let bitmap_extension = &stored_accounts[10];

        msg!("pool_id: {}", pool_id.key);
        msg!("bitmap_extension: {}", bitmap_extension.key);
        msg!("base_vault: {}", base_vault.key);
        msg!("quote_vault: {}", quote_vault.key);
        msg!("user_base_token_account: {}", user_base_token_account.key);
        msg!("user_quote_token_account: {}", user_quote_token_account.key);
        msg!("base_token: {}", base_token.key);
        msg!("quote_token: {}", quote_token.key);
        msg!("oracle: {}", oracle.key);
        msg!("host_fee_in: {}", host_fee_in.key);
        msg!("memo: {}", memo.key);
        msg!("event_authority: {}", event_authority.key);
        msg!("bitmap_extension: {}", bitmap_extension.key);

        let mut metas = vec![
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new(*bitmap_extension.key, false),
            AccountMeta::new(*base_vault.key, false),
            AccountMeta::new(*quote_vault.key, false),
            AccountMeta::new(*user_base_token_account.key, false),
            AccountMeta::new(*user_quote_token_account.key, false),
            AccountMeta::new_readonly(*base_token.key, false),
            AccountMeta::new_readonly(*quote_token.key, false),
            AccountMeta::new_readonly(*oracle.key, false),
            AccountMeta::new(*host_fee_in.key, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*base_token_program.key, false),
            AccountMeta::new_readonly(*quote_token_program.key, false),
            AccountMeta::new_readonly(*memo.key, false),
            AccountMeta::new_readonly(*event_authority.key, false),
            AccountMeta::new_readonly(Self::PROGRAM_ID, false),
        ];
        // Add bin arrays (sell arrays for swap_base_out)
        let bin_arrays = self.get_bin_arrays_sell();
        if let Some(bin_arrays) = bin_arrays {
            for account in bin_arrays {
                msg!("b: {}", account.key);
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
            memo.clone(),              // 13: memo (readonly)
            event_authority.clone(),   // 14: event_authority (readonly)
            program_id_stored.clone(), // 15: program_id (readonly)
        ];
        // Add bin arrays (sell arrays for swap_base_out)
        let bin_arrays = self.get_bin_arrays_sell();
        if let Some(bin_arrays) = bin_arrays {
            for account in bin_arrays {
                accounts_vec.push(account);
            }
        }

        unsafe {
            let accounts: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts)?;
        }
        Ok(())
    }
}

// Use the function from dlmm::quote module instead of duplicating
pub use dlmm::quote::get_bin_array_pubkeys_for_swap;

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::prelude::{Clock, InterfaceAccount};
    use anchor_lang::solana_program::{account_info::AccountInfo, pubkey::Pubkey, system_program};
    use anchor_spl::token_interface::Mint;
    use dlmm::dlmm::accounts::BinArray;
    use dlmm::{self, lb_pair};

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

    #[tokio::test]
    async fn test_dlmm_swap_quote_exact_in() {
        use anchor_client::Cluster;
        use solana_client::nonblocking::rpc_client::RpcClient;
        use std::collections::HashMap;
        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");

        // RPC client. No gPA is required.
        let rpc_client = RpcClient::new(Cluster::Devnet.url().to_string());

        let pool_id = Pubkey::from_str_const("FT8ueq7bP7DpBoP6b3QSsos3TkRY9JYCbGLCLKA3tgUn");

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
            dlmm::quote::get_bin_array_pubkeys_for_swap(pool_id, &lb_pair, None, true, 3).unwrap();

        // Get 3 bin arrays to the right the from active bin
        let right_bin_array_pubkeys =
            dlmm::quote::get_bin_array_pubkeys_for_swap(pool_id, &lb_pair, None, false, 3).unwrap();

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
        let bitmap_extension_account =
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

        let clock1 = get_clock(&rpc_client).await.unwrap();
        let clock_2 = clock1.clone();
        let clock_3 = clock1.clone();

        // Create MeteoraDlmm instance
        let meteora_dlmm = MeteoraDlmm::new(&accounts).unwrap();

        // 1 SOL -> USDC
        let in_sol_amount = 1_000_000_000;

        // Determine swap_for_y: if SOL is token_x, we swap X for Y (swap_for_y = true)
        // If SOL is token_y, we swap Y for X (swap_for_y = false)
        let swap_for_y = lb_pair.token_x_mint == sol_mint;
        eprintln!("swap_for_y: {:?}", swap_for_y);

        let amount_out = meteora_dlmm
            .swap_base_in(sol_mint, in_sol_amount, clock1)
            .unwrap();

        // Step 2: Swap quote -> base (reverse swap)
        let other_mint = if token_y_mint_key != sol_mint {
            token_y_mint_key
        } else {
            token_x_mint_key
        };

        let amount_out_2 = meteora_dlmm
            .swap_base_out(other_mint, amount_out, clock_2)
            .unwrap();
        eprintln!(
            "Step 1: {} SOL -> {} TOKEN",
            in_sol_amount as f64 / 1_000_000_000.0,
            amount_out as f64 / 1_000_000.0
        );
        eprintln!(
            "Step 2: {} TOKEN -> {} SOL",
            amount_out as f64 / 1_000_000.0,
            amount_out_2 as f64 / 1_000_000_000.0
        );

        // Use the already combined bin_array_all_infos for quote_exact_in
        // Clone it since we need it later for the second quote call
        // let bin_arrays_vec_for_quote: Vec<AccountInfo> = bin_array_all_infos.clone();

        // const BIN_SIZE: usize = 144;
        // const ESTIMATED_BIN_ARRAY_SIZE: usize = 56 + (70 * 144); // header + bins

        // eprintln!(
        //     "Calling quote_exact_in with {} bin arrays",
        //     bin_arrays_vec_for_quote.len()
        // );
        // eprintln!("Stack usage in quote_exact_in:");
        // eprintln!("  - Each bin array index read: 8 bytes (i64)");
        // eprintln!("  - Each bin read: {} bytes (Bin struct)", BIN_SIZE);
        // eprintln!(
        //     "  - NO full BinArray deserialization (~{} bytes avoided per array)",
        //     ESTIMATED_BIN_ARRAY_SIZE
        // );
        // eprintln!(
        //     "  - Estimated max stack usage per iteration: {} bytes (well under 4KB limit)",
        //     8 + BIN_SIZE
        // );

        // let quote_result = dlmm::quote_exact_in(
        //     pool_id,
        //     &lb_pair,
        //     in_sol_amount,
        //     swap_for_y,
        //     bin_arrays_vec_for_quote,
        //     None,
        //     &clock_3,
        //     &mint_x_interface,
        //     &mint_y_interface,
        // )
        // .unwrap();

        // let amount_out_2 = quote_result.amount_out;

        // eprintln!("1 SOL -> {:?} TOKEN", amount_out_2);

        // // For TOKEN -> SOL: if SOL is token_x, we swap Y for X (swap_for_y = false)
        // // If SOL is token_y, we swap X for Y (swap_for_y = true)
        // let swap_for_y_reverse = !swap_for_y;

        // // For reverse swap, input is the token we got from first swap (amount_out_2)
        // // Determine which token that is based on swap direction
        // let reverse_input_mint = if swap_for_y {
        //     lb_pair.token_y_mint // First swap was X->Y, so reverse is Y->X
        // } else {
        //     lb_pair.token_x_mint // First swap was Y->X, so reverse is X->Y
        // };
        // if swap_for_y_reverse {
        //     let quote_result = meteora_dlmm
        //         .swap_base_in(reverse_input_mint, amount_out_2, clock2)
        //         .unwrap();
        //     eprintln!(
        //         "{:?} TOKEN -> {:?} SOL",
        //         amount_out_2,
        //         quote_result as f64 / 1_000_000_000.0
        //     );
        // } else {
        //     let quote_result = meteora_dlmm
        //         .swap_base_out(reverse_input_mint, amount_out_2, clock2)
        //         .unwrap();
        //     eprintln!(
        //         "{:?} TOKEN -> {:?} SOL",
        //         amount_out_2,
        //         quote_result as f64 / 1_000_000_000.0
        //     );
        // }

        // // Fetch clock again for the quote call (clock2 was moved in swap_base_in/swap_base_out)
        // let clock3 = get_clock(&rpc_client).await.unwrap();

        // // Use the already combined bin_array_all_infos for the second quote call
        // let bin_arrays_vec_for_quote2: Vec<AccountInfo> = bin_array_all_infos.clone();

        // let quote_result = dlmm::quote_exact_in(
        //     pool_id,
        //     &lb_pair,
        //     amount_out_2,
        //     swap_for_y_reverse,
        //     bin_arrays_vec_for_quote2,
        //     None,
        //     &clock3,
        //     &mint_x_interface,
        //     &mint_y_interface,
        // )
        // .unwrap();

        // eprintln!(
        //     "{:?} TOKEN -> {:?} SOL",
        //     amount_out_2,
        //     quote_result.amount_out as f64 / 1_000_000_000.0
        // );
    }
}
