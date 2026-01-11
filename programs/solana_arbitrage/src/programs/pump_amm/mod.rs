use crate::programs::ProgramMeta;
use crate::utils::utils::parse_token_account;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    account_info::next_account_info,
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};
mod constants;

pub struct PumpAmm<'info> {
    pub accounts: Vec<AccountInfo<'info>>,
    pub program_id: AccountInfo<'info>,
    pub pool_id: AccountInfo<'info>,
    pub base_vault: AccountInfo<'info>,
    pub quote_vault: AccountInfo<'info>,
    pub base_token: AccountInfo<'info>,
    pub quote_token: AccountInfo<'info>,
}

impl<'info> ProgramMeta for PumpAmm<'info> {
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

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (self.base_token.key, self.quote_token.key)
    }

    fn swap_base_in(&self, input_mint: Pubkey, amount_in: u64, clock: Clock) -> Result<u64> {
        self.swap_base_in_impl(input_mint, amount_in, clock)
    }

    fn swap_base_out(&self, input_mint: Pubkey, amount_in: u64, clock: Clock) -> Result<u64> {
        self.swap_base_out_impl(input_mint, amount_in, clock)
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
            "Pump AMM accounts: program_id={}, pool_id={}, base_vault={}, quote_vault={}, base_token={}, quote_token={}",
            self.program_id.key,
            self.pool_id.key,
            self.base_vault.key,
            self.quote_vault.key,
            self.base_token.key,
            self.quote_token.key,
        );
        Ok(())
    }
}

impl<'info> PumpAmm<'info> {
    pub const PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
    pub fn new(accounts: &[AccountInfo<'info>]) -> Result<Self> {
        let mut iter = accounts.iter();
        let program_id = next_account_info(&mut iter)?; // 0
        let pool_id = next_account_info(&mut iter)?; // 1
        let base_vault = next_account_info(&mut iter)?; // 2
        let quote_vault = next_account_info(&mut iter)?; // 3
        let base_token = next_account_info(&mut iter)?; // 4
        let quote_token = next_account_info(&mut iter)?; // 5

        Ok(PumpAmm {
            accounts: accounts.to_vec(),
            program_id: program_id.clone(),
            pool_id: pool_id.clone(),
            base_vault: base_vault.clone(),
            quote_vault: quote_vault.clone(),
            base_token: base_token.clone(),
            quote_token: quote_token.clone(),
        })
    }

    pub fn parse_vaults(&self) -> Result<(u128, u128)> {
        let base_vault = parse_token_account(&self.base_vault)?;
        let quote_vault = parse_token_account(&self.quote_vault)?;
        Ok((base_vault.amount as u128, quote_vault.amount as u128))
    }

    /// Calculate base output amount for a given quote input amount
    /// Formula: base_amount_out = base_reserve - (base_reserve * quote_reserve) / (quote_reserve + quote_amount_in)
    /// Then applies 0.02% fee (multiply by 0.9998)
    pub fn swap_base_in_impl(
        &self,
        input_mint: Pubkey,
        amount_in: u64,
        _clock: Clock,
    ) -> Result<u64> {
        // Get reserves from vaults
        let base_vault_account = parse_token_account(&self.base_vault)?;
        let quote_vault_account = parse_token_account(&self.quote_vault)?;
        let base_reserve = base_vault_account.amount as u128;
        let quote_reserve = quote_vault_account.amount as u128;

        // quote_amount_in is the input parameter (amount_in)
        // base_amount_out = base_reserve - (base_reserve * quote_reserve) / (quote_reserve + quote_amount_in)
        let numerator = base_reserve
            .checked_mul(quote_reserve)
            .ok_or(ProgramError::InvalidArgument)?;
        let denominator = quote_reserve
            .checked_add(amount_in as u128)
            .ok_or(ProgramError::InvalidArgument)?;
        let quotient = numerator
            .checked_div(denominator)
            .ok_or(ProgramError::InvalidArgument)?;
        let base_amount_out = base_reserve
            .checked_sub(quotient)
            .ok_or(ProgramError::InvalidArgument)?;

        // Apply 0.02% fee → multiply by 0.9998 (use integer arithmetic: * 9998 / 10000)
        let base_amount_out_after_fee = base_amount_out
            .checked_mul(9_998)
            .and_then(|x| x.checked_div(10_000))
            .ok_or(ProgramError::InvalidArgument)?;

        Ok(base_amount_out_after_fee as u64)
    }

    /// Calculate base output amount for a given quote input amount
    /// Formula: base_amount_out = base_reserve - (base_reserve * quote_reserve) / (quote_reserve + quote_amount_in)
    /// Then applies lp_fee (0.2%), protocol_fee (0.05%), and multiplies by 1.0023
    pub fn swap_base_out_impl(
        &self,
        input_mint: Pubkey,
        amount_in: u64,
        _clock: Clock,
    ) -> Result<u64> {
        // Get reserves from vaults
        let base_vault_account = parse_token_account(&self.base_vault)?;
        let quote_vault_account = parse_token_account(&self.quote_vault)?;
        let base_reserve = base_vault_account.amount as u128;
        let quote_reserve = quote_vault_account.amount as u128;
        // quote_amount_out = quote_reserve - (base_reserve * quote_reserve) / (base_reserve + base_amount_in)

        // let base_reserve = 114912171739565u128;
        // let quote_reserve = 12070053361u128;

        let numerator = base_reserve
            .checked_mul(quote_reserve)
            .ok_or(ProgramError::InvalidArgument)?;
        let denominator = base_reserve
            .checked_add(amount_in as u128)
            .ok_or(ProgramError::InvalidArgument)?;
        let quotient = numerator
            .checked_div(denominator)
            .ok_or(ProgramError::InvalidArgument)?;
        let quote_amount_out = quote_reserve
            .checked_sub(quotient)
            .ok_or(ProgramError::InvalidArgument)?;

        // lp_fee = int(quote_amount_out * 0.002) (0.2%)
        let lp_fee = quote_amount_out
            .checked_mul(2)
            .and_then(|x| x.checked_div(1_000))
            .ok_or(ProgramError::InvalidArgument)?;

        // protocol_fee = int(quote_amount_out * 0.0005) (0.05%)
        let protocol_fee = quote_amount_out
            .checked_mul(5)
            .and_then(|x| x.checked_div(10_000))
            .ok_or(ProgramError::InvalidArgument)?;

        // fees = lp_fee + protocol_fee
        let fees = lp_fee
            .checked_add(protocol_fee)
            .ok_or(ProgramError::InvalidArgument)?;

        // quote_amount_out - fees
        let quote_after_fees = quote_amount_out
            .checked_sub(fees)
            .ok_or(ProgramError::InvalidArgument)?;

        // Multiply by 1.0023 (use integer arithmetic: * 10023 / 10000)
        let final_amount = quote_after_fees
            .checked_mul(10_023)
            .and_then(|x| x.checked_div(10_000))
            .ok_or(ProgramError::InvalidArgument)?;

        Ok(final_amount as u64)
    }

    pub fn invoke_swap_base_in_impl<'a>(
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

        // Get stored accounts from self.get_accounts() - these are the accounts stored in the struct
        let stored_accounts = self.accounts.clone();
        let program_id_stored = &stored_accounts[0];
        let pool_id = &stored_accounts[1];
        let base_vault = &stored_accounts[2];
        let quote_vault = &stored_accounts[3];
        let base_token = &stored_accounts[4];
        let quote_token = &stored_accounts[5];
        let protocol_fee_recipient = &stored_accounts[6];
        let protocol_fee_token_account = &stored_accounts[7];
        let event_authority = &stored_accounts[8];
        let fee_config = &stored_accounts[9];
        let fee_program = &stored_accounts[10];
        let user_volume_accumulator = &stored_accounts[11];
        let pump_amm_global = &stored_accounts[12];
        let system_program = &stored_accounts[13];
        let associated_token_instruction_program = &stored_accounts[14];
        let global_vol_accumulator = &stored_accounts[15];

        // Extract optional vault_ata and vault_authority if present
        let (vault_ata, vault_authority) = if stored_accounts.len() >= 18 {
            (Some(&stored_accounts[16]), Some(&stored_accounts[17]))
        } else {
            (None, None)
        };

        let amount_out_value = amount_out.unwrap_or(0);
        let mut metas = vec![
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*pump_amm_global.key, false),
            AccountMeta::new_readonly(*base_token.key, false),
            AccountMeta::new_readonly(*quote_token.key, false),
            AccountMeta::new(*user_base_token_account.key, false),
            AccountMeta::new(*user_quote_token_account.key, false),
            AccountMeta::new(*base_vault.key, false),
            AccountMeta::new(*quote_vault.key, false),
            AccountMeta::new_readonly(*protocol_fee_recipient.key, false),
            AccountMeta::new(*protocol_fee_token_account.key, false),
            AccountMeta::new_readonly(*base_token_program.key, false),
            AccountMeta::new_readonly(*quote_token_program.key, false),
            AccountMeta::new_readonly(*system_program.key, false),
            AccountMeta::new_readonly(*associated_token_instruction_program.key, false),
            AccountMeta::new_readonly(*event_authority.key, false),
            AccountMeta::new_readonly(Self::PROGRAM_ID, false),
        ];
        if let (Some(vault_ata_acc), Some(vault_authority_acc)) = (vault_ata, vault_authority) {
            metas.push(AccountMeta::new(*vault_ata_acc.key, false));
            metas.push(AccountMeta::new_readonly(*vault_authority_acc.key, false));
        }
        metas.push(AccountMeta::new_readonly(
            *global_vol_accumulator.key,
            false,
        ));
        metas.push(AccountMeta::new(*user_volume_accumulator.key, false));
        metas.push(AccountMeta::new_readonly(*fee_config.key, false));
        metas.push(AccountMeta::new_readonly(*fee_program.key, false));

        let mut data = vec![0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea];
        data.extend_from_slice(&max_amount_in.to_le_bytes());
        data.extend_from_slice(&amount_out_value.to_le_bytes());

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data,
        };
        // Order must match metas exactly!
        let mut accounts: Vec<AccountInfo<'info>> = vec![
            pool_id.clone(),                                         // 0: writable
            unsafe { std::mem::transmute(payer.to_account_info()) }, // 1: writable, signer
            pump_amm_global.clone(),                                 // 2: readonly
            base_token.clone(),                                      // 3: readonly
            quote_token.clone(),                                     // 4: readonly
            unsafe { std::mem::transmute(user_base_token_account.to_account_info()) }, // 5: writable
            unsafe { std::mem::transmute(user_quote_token_account.to_account_info()) }, // 6: writable
            base_vault.clone(),                 // 7: writable
            quote_vault.clone(),                // 8: writable
            protocol_fee_recipient.clone(),     // 9: readonly
            protocol_fee_token_account.clone(), // 10: writable
            unsafe { std::mem::transmute(base_token_program.to_account_info()) }, // 11: readonly
            unsafe { std::mem::transmute(quote_token_program.to_account_info()) }, // 12: readonly
            system_program.clone(),             // 13: readonly
            associated_token_instruction_program.clone(), // 14: readonly
            event_authority.clone(),            // 15: readonly
            program_id_stored.clone(),          // 16: readonly (PROGRAM_ID)
        ];

        if let (Some(vault_ata_acc), Some(vault_authority_acc)) = (vault_ata, vault_authority) {
            accounts.push(vault_ata_acc.clone());
            accounts.push(vault_authority_acc.clone());
        }

        accounts.push(global_vol_accumulator.clone());
        accounts.push(user_volume_accumulator.clone());
        accounts.push(fee_config.clone());
        accounts.push(fee_program.clone());

        // Cast entire vector to AccountInfo<'a> for invoke
        unsafe {
            let accounts_slice: &[AccountInfo<'a>] = std::mem::transmute(accounts.as_slice());
            invoke(&swap_ix, accounts_slice)?;
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

        // Get stored accounts from self.get_accounts() - these are the accounts stored in the struct
        let stored_accounts = self.accounts.clone();
        let program_id_stored = &stored_accounts[0];
        let pool_id = &stored_accounts[1];
        let base_vault = &stored_accounts[2];
        let quote_vault = &stored_accounts[3];
        let base_token = &stored_accounts[4];
        let quote_token = &stored_accounts[5];
        let protocol_fee_recipient = &stored_accounts[6];
        let protocol_fee_token_account = &stored_accounts[7];
        let event_authority = &stored_accounts[8];
        let fee_config = &stored_accounts[9];
        let fee_program = &stored_accounts[10];
        let user_volume_accumulator = &stored_accounts[11];
        let pump_amm_global = &stored_accounts[12];
        let system_program = &stored_accounts[13];
        let associated_token_instruction_program = &stored_accounts[14];
        let global_vol_accumulator = &stored_accounts[15];

        // Extract optional vault_ata and vault_authority if present
        let (vault_ata, vault_authority) = if stored_accounts.len() >= 18 {
            (Some(&stored_accounts[16]), Some(&stored_accounts[17]))
        } else {
            (None, None)
        };

        // Note: payer, user_base_token_account, user_quote_token_account, base_token_program, quote_token_program
        // are function parameters (already available from lines 442-463)

        let min_amount_out_value = min_amount_out.unwrap_or(0);
        let mut metas = vec![
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*pump_amm_global.key, false),
            AccountMeta::new_readonly(*base_token.key, false),
            AccountMeta::new_readonly(*quote_token.key, false),
            AccountMeta::new(*user_base_token_account.key, false),
            AccountMeta::new(*user_quote_token_account.key, false),
            AccountMeta::new(*base_vault.key, false),
            AccountMeta::new(*quote_vault.key, false),
            AccountMeta::new_readonly(*protocol_fee_recipient.key, false),
            AccountMeta::new(*protocol_fee_token_account.key, false),
            AccountMeta::new_readonly(*base_token_program.key, false),
            AccountMeta::new_readonly(*quote_token_program.key, false),
            AccountMeta::new_readonly(*system_program.key, false),
            AccountMeta::new_readonly(*associated_token_instruction_program.key, false),
            AccountMeta::new_readonly(*event_authority.key, false),
            AccountMeta::new_readonly(*self.program_id.key, false),
        ];
        if let (Some(vault_ata_acc), Some(vault_authority_acc)) = (vault_ata, vault_authority) {
            metas.push(AccountMeta::new(*vault_ata_acc.key, false));
            metas.push(AccountMeta::new_readonly(*vault_authority_acc.key, false));
        }
        metas.push(AccountMeta::new_readonly(
            *global_vol_accumulator.key,
            false,
        ));
        metas.push(AccountMeta::new(*user_volume_accumulator.key, false));
        metas.push(AccountMeta::new_readonly(*fee_config.key, false));
        metas.push(AccountMeta::new_readonly(*fee_program.key, false));

        let mut data = vec![0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad];
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out_value.to_le_bytes());

        let swap_ix = Instruction {
            program_id: *self.program_id.key,
            accounts: metas,
            data,
        };

        // Order must match metas exactly!
        let mut accounts: Vec<AccountInfo<'info>> = vec![
            pool_id.clone(),                                         // 0: writable
            unsafe { std::mem::transmute(payer.to_account_info()) }, // 1: writable, signer
            pump_amm_global.clone(),                                 // 2: readonly
            base_token.clone(),                                      // 3: readonly
            quote_token.clone(),                                     // 4: readonly
            unsafe { std::mem::transmute(user_base_token_account.to_account_info()) }, // 5: writable
            unsafe { std::mem::transmute(user_quote_token_account.to_account_info()) }, // 6: writable
            base_vault.clone(),                 // 7: writable
            quote_vault.clone(),                // 8: writable
            protocol_fee_recipient.clone(),     // 9: readonly
            protocol_fee_token_account.clone(), // 10: writable
            unsafe { std::mem::transmute(base_token_program.to_account_info()) }, // 11: readonly
            unsafe { std::mem::transmute(quote_token_program.to_account_info()) }, // 12: readonly
            system_program.clone(),             // 13: readonly
            associated_token_instruction_program.clone(), // 14: readonly
            event_authority.clone(),            // 15: readonly
            program_id_stored.clone(),          // 16: readonly (PROGRAM_ID)
        ];

        if let (Some(vault_ata_acc), Some(vault_authority_acc)) = (vault_ata, vault_authority) {
            accounts.push(vault_ata_acc.clone()); // 17: writable
            accounts.push(vault_authority_acc.clone()); // 18: readonly
        }
        accounts.push(global_vol_accumulator.clone());
        accounts.push(user_volume_accumulator.clone());
        accounts.push(fee_config.clone()); // 21 or 19: readonly
        accounts.push(fee_program.clone()); // 22 or 20: readonly

        // Cast entire vector to AccountInfo<'a> for invoke
        unsafe {
            let accounts_slice: &[AccountInfo<'a>] = std::mem::transmute(accounts.as_slice());
            invoke(&swap_ix, accounts_slice)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::program_pack::Pack;
    use anchor_lang::solana_program::{account_info::AccountInfo, pubkey::Pubkey, system_program};
    use anchor_spl::token::spl_token::state::Account;

    // Helper function to create a mock AccountInfo with TokenAccount data
    fn create_mock_token_account_info(
        key: Pubkey,
        mint: Pubkey,
        amount: u64,
        owner: Pubkey,
        pool_data: Option<Vec<u8>>,
    ) -> AccountInfo<'static> {
        let data_vec = if let Some(provided_data) = pool_data {
            // Use provided data if available
            provided_data
        } else {
            // Manually construct SPL token account bytes in Pack format
            // We'll create minimal valid data and use unpack/pack to ensure correctness
            let mut data = vec![0u8; Account::LEN];
            let mut offset = 0;

            // mint (32 bytes)
            data[offset..offset + 32].copy_from_slice(&mint.to_bytes());
            offset += 32;

            // owner (32 bytes)
            data[offset..offset + 32].copy_from_slice(&owner.to_bytes());
            offset += 32;

            // amount (8 bytes, little-endian)
            data[offset..offset + 8].copy_from_slice(&amount.to_le_bytes());
            offset += 8;

            // delegate: COption::None = [0, 0, 0, 0] (4 bytes, already zero)
            offset += 4;

            // state: Initialized = 1 (1 byte)
            data[offset] = 1;
            offset += 1;

            // is_native: COption::None = [0, 0, 0, 0] (4 bytes, already zero)
            offset += 4;

            // delegated_amount: 0 (8 bytes, already zero)
            offset += 8;

            // close_authority: COption::None = [0, 0, 0, 0] (4 bytes, already zero)
            // Remaining bytes are padding (already zero)

            // The manually constructed data should be valid Pack format
            // TokenAccount::try_deserialize wraps Account::unpack, so this should work
            data
        };

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

    // Helper function to create a minimal mock AccountInfo
    fn create_mock_account_info(
        key: Pubkey,
        owner: Pubkey,
        account_data: Option<Vec<u8>>,
    ) -> AccountInfo<'static> {
        let data = if let Some(provided_data) = account_data {
            Box::leak(Box::new(provided_data))
        } else {
            Box::leak(Box::new(Vec::new()))
        };
        let lamports = Box::leak(Box::new(0u64));
        let owner_static = Box::leak(Box::new(owner));
        let key_static = Box::leak(Box::new(key));

        AccountInfo::new(
            key_static,
            false,
            false,
            lamports,
            data,
            owner_static,
            false,
            0,
        )
    }

    #[test]
    fn test_parse_vaults() {
        let base_mint = Pubkey::from_str_const("55ESNd1C5XYfJCHnnYD1t4jMdDK91hh2HaGkPQSXpump");
        let quote_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let base_token_program =
            Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
        let quote_token_program =
            Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

        let base_vault_key = Pubkey::new_unique();
        let quote_vault_key = Pubkey::new_unique();

        // Pool data from pool_data.txt
        let base_pool_data = Some(b"<\x84C\xc56\x10\x11+\xc8\x934m\x94\x13\xf3\xc2\xd1\xda\xd1\x87\xa5j\t]\x13\x93\x186UL#\x0f\n\xe4'\xeb\xf9U\x7f1\xb9\xf7I\xeb\xc2\xd96B\xd8\xd6i\xfch\xb9<\xb2\xa02\x96\x0b\xf5\x1a\x1d\xd9/\xaa\rY\xd6S\x03\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x07\x00\x00\x00".to_vec());

        let quote_pool_data = Some(b"\x06\x9b\x88W\xfe\xab\x81\x84\xfbh\x7fcF\x18\xc05\xda\xc49\xdc\x1a\xeb;U\x98\xa0\xf0\x00\x00\x00\x00\x01\n\xe4'\xeb\xf9U\x7f1\xb9\xf7I\xeb\xc2\xd96B\xd8\xd6i\xfch\xb9<\xb2\xa02\x96\x0b\xf5\x1a\x1d\xd9|\xa1\xd4f\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x01\x01\x00\x00\x00\xf0\x1d\x1f\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec());

        // Pass pool_data to vault accounts
        let base_vault_info = create_mock_token_account_info(
            base_vault_key,
            base_mint,
            1_000_000_000, // 1 token with 9 decimals
            base_token_program,
            base_pool_data, // Pass base_pool_data to base_vault_info
        );

        let quote_vault_info = create_mock_token_account_info(
            quote_vault_key,
            quote_mint,
            100_000_000, // 100 tokens with 6 decimals
            quote_token_program,
            quote_pool_data, // Pass quote_pool_data to quote_vault_info
        );

        // Create pool_id account (no pool data needed since it's applied to vault accounts)
        let pool_id = create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let base_token = create_mock_account_info(base_mint, system_program::id(), None);
        let quote_token = create_mock_account_info(quote_mint, system_program::id(), None);
        let protocol_fee_recipient =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let protocol_fee_token_account =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let event_authority =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let coin_creator =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let vault_ata = create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let vault_authority =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let fee_config = create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let fee_program =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);

        let accounts = vec![
            pool_id,
            base_vault_info,
            quote_vault_info,
            base_token,
            quote_token,
            protocol_fee_recipient,
            protocol_fee_token_account,
            event_authority,
            coin_creator,
            vault_ata,
            vault_authority,
            fee_config,
            fee_program,
        ];

        let pump_amm = PumpAmm::new(&accounts).unwrap();

        // Debug: Print what data is in the vault accounts
        let base_vault_data = accounts[1].try_borrow_data().unwrap();
        let quote_vault_data = accounts[2].try_borrow_data().unwrap();
        eprintln!("Base vault data length: {}", base_vault_data.len());
        eprintln!("Quote vault data length: {}", quote_vault_data.len());

        // Token account amount is at offset 64 (32 bytes mint + 32 bytes owner)
        if base_vault_data.len() >= 72 {
            let base_amount_bytes = &base_vault_data[64..72];
            let base_amount_parsed = u64::from_le_bytes(base_amount_bytes.try_into().unwrap());
            eprintln!(
                "Base vault amount bytes (offset 64-72): {:?}",
                base_amount_bytes
            );
            eprintln!("Base vault amount parsed as u64: {}", base_amount_parsed);
        }

        if quote_vault_data.len() >= 72 {
            let quote_amount_bytes = &quote_vault_data[64..72];
            let quote_amount_parsed = u64::from_le_bytes(quote_amount_bytes.try_into().unwrap());
            eprintln!(
                "Quote vault amount bytes (offset 64-72): {:?}",
                quote_amount_bytes
            );
            eprintln!("Quote vault amount parsed as u64: {}", quote_amount_parsed);
        }

        let (base_amount, quote_amount) = pump_amm.parse_vaults().unwrap();
        eprintln!(
            "Parsed base_amount: {}, quote_amount: {}",
            base_amount, quote_amount
        );

        assert_eq!(base_amount, 936_605_012_306_479);
        assert_eq!(quote_amount, 18_905_080_188);
    }

    #[test]
    fn test_pump_amm_get_swap_base_in_amount() {
        // Setup: base_reserve = 1_000_000_000, quote_reserve = 100_000_000
        // Input: quote_amount_in = 10_000_000 (10 tokens)
        // Expected: base_amount_out = base_reserve - (base_reserve * quote_reserve) / (quote_reserve + quote_amount_in)
        //          = 1_000_000_000 - (1_000_000_000 * 100_000_000) / (100_000_000 + 10_000_000)
        //          = 1_000_000_000 - 100_000_000_000_000_000 / 110_000_000
        //          = 1_000_000_000 - 909_090_909
        //          = 90_909_091
        // After 0.02% fee: 90_909_091 * 9998 / 10000 = 90_727_272

        let base_mint = Pubkey::new_unique();
        let quote_mint = Pubkey::new_unique();
        let token_program = Pubkey::new_unique();

        let base_vault_key = Pubkey::new_unique();
        let quote_vault_key = Pubkey::new_unique();

        let base_pool_data = Some(b"<\x84C\xc56\x10\x11+\xc8\x934m\x94\x13\xf3\xc2\xd1\xda\xd1\x87\xa5j\t]\x13\x93\x186UL#\x0f\n\xe4'\xeb\xf9U\x7f1\xb9\xf7I\xeb\xc2\xd96B\xd8\xd6i\xfch\xb9<\xb2\xa02\x96\x0b\xf5\x1a\x1d\xd9/\xaa\rY\xd6S\x03\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x07\x00\x00\x00".to_vec());

        let quote_pool_data = Some(b"\x06\x9b\x88W\xfe\xab\x81\x84\xfbh\x7fcF\x18\xc05\xda\xc49\xdc\x1a\xeb;U\x98\xa0\xf0\x00\x00\x00\x00\x01\n\xe4'\xeb\xf9U\x7f1\xb9\xf7I\xeb\xc2\xd96B\xd8\xd6i\xfch\xb9<\xb2\xa02\x96\x0b\xf5\x1a\x1d\xd9|\xa1\xd4f\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x01\x01\x00\x00\x00\xf0\x1d\x1f\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec());

        // Use actual reserves from pool_data: base_reserve = 936605012306479, quote_reserve = 18905080188
        let base_vault_info = create_mock_token_account_info(
            base_vault_key,
            base_mint,
            936_605_012_306_479,
            token_program,
            base_pool_data, // Pass base_pool_data to base_vault_info
        );

        let quote_vault_info = create_mock_token_account_info(
            quote_vault_key,
            quote_mint,
            18_905_080_188,
            token_program,
            quote_pool_data, // Pass quote_pool_data to quote_vault_info
        );

        let program_id = create_mock_account_info(PumpAmm::PROGRAM_ID, system_program::id(), None);
        let pool_id = create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let base_token = create_mock_account_info(base_mint, system_program::id(), None);
        let quote_token = create_mock_account_info(quote_mint, system_program::id(), None);
        let protocol_fee_recipient =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let protocol_fee_token_account =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let event_authority =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let fee_config = create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let fee_program =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let user_volume_accumulator =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let pump_amm_global =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let system_program_account =
            create_mock_account_info(system_program::id(), system_program::id(), None);
        let associated_token_instruction_program =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let global_vol_accumulator =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let vault_ata = create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let vault_authority =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);

        let accounts = vec![
            program_id,                           // 0
            pool_id,                              // 1
            base_vault_info,                      // 2
            quote_vault_info,                     // 3
            base_token,                           // 4
            quote_token,                          // 5
            protocol_fee_recipient,               // 6
            protocol_fee_token_account,           // 7
            event_authority,                      // 8
            fee_config,                           // 9
            fee_program,                          // 10
            user_volume_accumulator,              // 11
            pump_amm_global,                      // 12
            system_program_account,               // 13
            associated_token_instruction_program, // 14
            global_vol_accumulator,               // 15
            vault_ata,                            // 16
            vault_authority,                      // 17
        ];

        let pump_amm = PumpAmm::new(&accounts).unwrap();

        // Test with quote_amount_in = 10_000_000
        let quote_amount_in = 10_000_000u64;
        let clock = Clock::default();
        let input_mint = quote_mint; // Use quote_mint directly since quote_token was moved into accounts
        let result = pump_amm
            .swap_base_in(input_mint, quote_amount_in, clock)
            .unwrap();
        eprintln!("TOKEN AMOUNT OUT: {:?}", result);

        // Manual calculation for verification using actual reserves from pool_data
        // base_reserve = 936605012306479, quote_reserve = 18905080188 (from pool_data)
        let base_reserve = 936_605_012_306_479u128;
        let quote_reserve = 18_905_080_188u128;
        let numerator = base_reserve * quote_reserve;
        let denominator = quote_reserve + quote_amount_in as u128;
        let quotient = numerator / denominator;
        let base_amount_out = base_reserve - quotient;
        let expected = (base_amount_out * 9_998 / 10_000) as u64;

        assert_eq!(result, expected);
        assert!(result > 0);
    }

    #[test]
    fn test_pump_amm_swap_base_sol_base() {
        // Setup: base_reserve = 1_000_000_000, quote_reserve = 100_000_000
        // Input: base_amount_in = 10_000_000
        // Expected: quote_amount_out = quote_reserve - (base_reserve * quote_reserve) / (base_reserve + base_amount_in)
        //          = 100_000_000 - (1_000_000_000 * 100_000_000) / (1_000_000_000 + 10_000_000)
        //          = 100_000_000 - 100_000_000_000_000_000 / 1_010_000_000
        //          = 100_000_000 - 99_009_900
        //          = 990_100
        // lp_fee = 990_100 * 0.002 = 1_980
        // protocol_fee = 990_100 * 0.0005 = 495
        // fees = 1_980 + 495 = 2_475
        // quote_after_fees = 990_100 - 2_475 = 987_625
        // final = 987_625 * 1.0023 = 989_896

        let base_mint = Pubkey::new_unique();
        let quote_mint = Pubkey::new_unique();
        let token_program = Pubkey::new_unique();

        let base_vault_key = Pubkey::new_unique();
        let quote_vault_key = Pubkey::new_unique();

        // Use pool_data for this test
        let quote_pool_data = Some(b"<\x84C\xc56\x10\x11+\xc8\x934m\x94\x13\xf3\xc2\xd1\xda\xd1\x87\xa5j\t]\x13\x93\x186UL#\x0f\n\xe4'\xeb\xf9U\x7f1\xb9\xf7I\xeb\xc2\xd96B\xd8\xd6i\xfch\xb9<\xb2\xa02\x96\x0b\xf5\x1a\x1d\xd9/\xaa\rY\xd6S\x03\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x07\x00\x00\x00".to_vec());

        let base_pool_data = Some(b"\x06\x9b\x88W\xfe\xab\x81\x84\xfbh\x7fcF\x18\xc05\xda\xc49\xdc\x1a\xeb;U\x98\xa0\xf0\x00\x00\x00\x00\x01\n\xe4'\xeb\xf9U\x7f1\xb9\xf7I\xeb\xc2\xd96B\xd8\xd6i\xfch\xb9<\xb2\xa02\x96\x0b\xf5\x1a\x1d\xd9|\xa1\xd4f\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x01\x01\x00\x00\x00\xf0\x1d\x1f\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec());

        let base_vault_info = create_mock_token_account_info(
            base_vault_key,
            base_mint,
            1_000_000_000,
            token_program,
            base_pool_data, // Pass base_pool_data to base_vault_info
        );

        let quote_vault_info = create_mock_token_account_info(
            quote_vault_key,
            quote_mint,
            100_000_000,
            token_program,
            quote_pool_data, // Pass quote_pool_data to quote_vault_info
        );

        let program_id = create_mock_account_info(PumpAmm::PROGRAM_ID, system_program::id(), None);
        let pool_id = create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let base_token = create_mock_account_info(base_mint, system_program::id(), None);
        let quote_token = create_mock_account_info(quote_mint, system_program::id(), None);
        let protocol_fee_recipient =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let protocol_fee_token_account =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let event_authority =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let fee_config = create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let fee_program =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let user_volume_accumulator =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let pump_amm_global =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let system_program_account =
            create_mock_account_info(system_program::id(), system_program::id(), None);
        let associated_token_instruction_program =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let global_vol_accumulator =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let vault_ata = create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let vault_authority =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);

        let accounts = vec![
            program_id,                           // 0
            pool_id,                              // 1
            base_vault_info,                      // 2
            quote_vault_info,                     // 3
            base_token,                           // 4
            quote_token,                          // 5
            protocol_fee_recipient,               // 6
            protocol_fee_token_account,           // 7
            event_authority,                      // 8
            fee_config,                           // 9
            fee_program,                          // 10
            user_volume_accumulator,              // 11
            pump_amm_global,                      // 12
            system_program_account,               // 13
            associated_token_instruction_program, // 14
            global_vol_accumulator,               // 15
            vault_ata,                            // 16
            vault_authority,                      // 17
        ];

        let pump_amm = PumpAmm::new(&accounts).unwrap();

        // Test with base_amount_in = 10_000_000
        let base_amount_in = 1_000_000_000u64;
        msg!("base_amount_in: {:?}", base_amount_in / 1_000_000_000);
        let clock = Clock::default();
        let input_mint = base_mint; // Use base_mint directly since base_token was moved into accounts
        let result = pump_amm
            .swap_base_out(input_mint, base_amount_in, clock)
            .unwrap();
        eprintln!(
            "{:?} SOL -> {:?} TOKEN",
            base_amount_in as f64 / 1_000_000_000.0,
            result as f64 / 1_000_000_000.0,
        );

        // Test with base_amount_in = 10_000_000
        let base_amount_in = result;
        let clock = Clock::default();
        let input_mint = quote_mint; // Use quote_mint directly since quote_token was moved into accounts
        let result = pump_amm
            .swap_base_in(input_mint, base_amount_in, clock)
            .unwrap();
        eprintln!(
            "{:?} TOKEN -> {:?} SOL",
            base_amount_in as f64 / 1_000_000_000.0,
            result as f64 / 1_000_000_000.0,
        );
        // Manual calculation for verification using actual reserves from pool_data
        let base_reserve = 936_605_012_306_479u128;
        let quote_reserve = 18_905_080_188u128;
        let numerator = base_reserve * quote_reserve;
        let denominator = base_reserve + base_amount_in as u128;
        let quotient = numerator / denominator;
        let quote_amount_out = quote_reserve - quotient;

        let lp_fee = quote_amount_out * 2 / 1_000;
        let protocol_fee = quote_amount_out * 5 / 10_000;
        let fees = lp_fee + protocol_fee;
        let quote_after_fees = quote_amount_out - fees;
        let expected = (quote_after_fees * 10_023 / 10_000) as u64;

        assert_eq!(result, expected);
        assert!(result > 0);
    }

    #[test]
    fn test_pump_amm_swap_base_sol_quote() {
        // Setup: base_reserve = 1_000_000_000, quote_reserve = 100_000_000
        // Input: base_amount_in = 10_000_000
        // Expected: quote_amount_out = quote_reserve - (base_reserve * quote_reserve) / (base_reserve + base_amount_in)
        //          = 100_000_000 - (1_000_000_000 * 100_000_000) / (1_000_000_000 + 10_000_000)
        //          = 100_000_000 - 100_000_000_000_000_000 / 1_010_000_000
        //          = 100_000_000 - 99_009_900
        //          = 990_100
        // lp_fee = 990_100 * 0.002 = 1_980
        // protocol_fee = 990_100 * 0.0005 = 495
        // fees = 1_980 + 495 = 2_475
        // quote_after_fees = 990_100 - 2_475 = 987_625
        // final = 987_625 * 1.0023 = 989_896

        let base_mint = Pubkey::new_unique();
        let quote_mint = Pubkey::new_unique();
        let token_program = Pubkey::new_unique();

        let base_vault_key = Pubkey::new_unique();
        let quote_vault_key = Pubkey::new_unique();

        // Use pool_data for this test
        let base_pool_data = Some(b"<\x84C\xc56\x10\x11+\xc8\x934m\x94\x13\xf3\xc2\xd1\xda\xd1\x87\xa5j\t]\x13\x93\x186UL#\x0f\n\xe4'\xeb\xf9U\x7f1\xb9\xf7I\xeb\xc2\xd96B\xd8\xd6i\xfch\xb9<\xb2\xa02\x96\x0b\xf5\x1a\x1d\xd9/\xaa\rY\xd6S\x03\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x07\x00\x00\x00".to_vec());

        let quote_pool_data = Some(b"\x06\x9b\x88W\xfe\xab\x81\x84\xfbh\x7fcF\x18\xc05\xda\xc49\xdc\x1a\xeb;U\x98\xa0\xf0\x00\x00\x00\x00\x01\n\xe4'\xeb\xf9U\x7f1\xb9\xf7I\xeb\xc2\xd96B\xd8\xd6i\xfch\xb9<\xb2\xa02\x96\x0b\xf5\x1a\x1d\xd9|\xa1\xd4f\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x01\x01\x00\x00\x00\xf0\x1d\x1f\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec());

        let base_vault_info = create_mock_token_account_info(
            base_vault_key,
            base_mint,
            1_000_000_000,
            token_program,
            base_pool_data, // Pass base_pool_data to base_vault_info
        );

        let quote_vault_info = create_mock_token_account_info(
            quote_vault_key,
            quote_mint,
            100_000_000,
            token_program,
            quote_pool_data, // Pass quote_pool_data to quote_vault_info
        );

        let program_id = create_mock_account_info(PumpAmm::PROGRAM_ID, system_program::id(), None);
        let pool_id = create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let base_token = create_mock_account_info(base_mint, system_program::id(), None);
        let quote_token = create_mock_account_info(quote_mint, system_program::id(), None);
        let protocol_fee_recipient =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let protocol_fee_token_account =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let event_authority =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let fee_config = create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let fee_program =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let user_volume_accumulator =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let pump_amm_global =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let system_program_account =
            create_mock_account_info(system_program::id(), system_program::id(), None);
        let associated_token_instruction_program =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let global_vol_accumulator =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let vault_ata = create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let vault_authority =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);

        let accounts = vec![
            program_id,                           // 0
            pool_id,                              // 1
            base_vault_info,                      // 2
            quote_vault_info,                     // 3
            base_token,                           // 4
            quote_token,                          // 5
            protocol_fee_recipient,               // 6
            protocol_fee_token_account,           // 7
            event_authority,                      // 8
            fee_config,                           // 9
            fee_program,                          // 10
            user_volume_accumulator,              // 11
            pump_amm_global,                      // 12
            system_program_account,               // 13
            associated_token_instruction_program, // 14
            global_vol_accumulator,               // 15
            vault_ata,                            // 16
            vault_authority,                      // 17
        ];

        let pump_amm = PumpAmm::new(&accounts).unwrap();

        // Test with base_amount_in = 10_000_000
        let base_amount_in = 1_000_000_000u64;
        msg!("base_amount_in: {:?}", base_amount_in / 1_000_000_000);
        let clock = Clock::default();
        let input_mint = base_mint; // Use base_mint directly since base_token was moved into accounts
        let result = pump_amm
            .swap_base_in(input_mint, base_amount_in, clock)
            .unwrap();
        eprintln!(
            "{:?} SOL -> {:?} TOKEN",
            base_amount_in as f64 / 1_000_000_000.0,
            result as f64 / 1_000_000_000.0,
        );

        // Test with base_amount_in = 10_000_000
        let base_amount_in = result;
        let clock = Clock::default();
        let input_mint = quote_mint; // Use quote_mint directly since quote_token was moved into accounts
        let result = pump_amm
            .swap_base_out(input_mint, base_amount_in, clock)
            .unwrap();
        eprintln!(
            "{:?} TOKEN -> {:?} SOL",
            base_amount_in as f64 / 1_000_000_000.0,
            result as f64 / 1_000_000_000.0,
        );
        // Manual calculation for verification using actual reserves from pool_data
        let base_reserve = 936_605_012_306_479u128;
        let quote_reserve = 18_905_080_188u128;
        let numerator = base_reserve * quote_reserve;
        let denominator = base_reserve + base_amount_in as u128;
        let quotient = numerator / denominator;
        let quote_amount_out = quote_reserve - quotient;

        let lp_fee = quote_amount_out * 2 / 1_000;
        let protocol_fee = quote_amount_out * 5 / 10_000;
        let fees = lp_fee + protocol_fee;
        let quote_after_fees = quote_amount_out - fees;
        let expected = (quote_after_fees * 10_023 / 10_000) as u64;

        assert_eq!(result, expected);
        assert!(result > 0);
    }

    #[test]
    fn test_get_swap_base_in_amount_zero_input() {
        let base_mint = Pubkey::new_unique();
        let quote_mint = Pubkey::new_unique();
        let token_program = Pubkey::new_unique();

        // Use pool_data for this test as well
        let base_pool_data = Some(b"<\x84C\xc56\x10\x11+\xc8\x934m\x94\x13\xf3\xc2\xd1\xda\xd1\x87\xa5j\t]\x13\x93\x186UL#\x0f\n\xe4'\xeb\xf9U\x7f1\xb9\xf7I\xeb\xc2\xd96B\xd8\xd6i\xfch\xb9<\xb2\xa02\x96\x0b\xf5\x1a\x1d\xd9/\xaa\rY\xd6S\x03\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x07\x00\x00\x00".to_vec());

        let quote_pool_data = Some(b"\x06\x9b\x88W\xfe\xab\x81\x84\xfbh\x7fcF\x18\xc05\xda\xc49\xdc\x1a\xeb;U\x98\xa0\xf0\x00\x00\x00\x00\x01\n\xe4'\xeb\xf9U\x7f1\xb9\xf7I\xeb\xc2\xd96B\xd8\xd6i\xfch\xb9<\xb2\xa02\x96\x0b\xf5\x1a\x1d\xd9|\xa1\xd4f\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x01\x01\x00\x00\x00\xf0\x1d\x1f\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec());

        let base_vault_info = create_mock_token_account_info(
            Pubkey::new_unique(),
            base_mint,
            1_000_000_000,
            token_program,
            base_pool_data, // Use pool_data
        );

        let quote_vault_info = create_mock_token_account_info(
            Pubkey::new_unique(),
            quote_mint,
            100_000_000,
            token_program,
            quote_pool_data, // Use pool_data
        );

        let accounts = vec![
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None),
            base_vault_info,
            quote_vault_info,
            create_mock_account_info(base_mint, system_program::id(), None),
            create_mock_account_info(quote_mint, system_program::id(), None),
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None),
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None),
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None),
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None),
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None),
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None),
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None),
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None),
        ];

        let pump_amm = PumpAmm::new(&accounts).unwrap();

        // Zero input should result in zero output
        let clock = Clock::default();
        let input_mint = base_mint;
        let result = pump_amm.swap_base_in(input_mint, 0, clock).unwrap();
        assert_eq!(result, 0);
    }
}
