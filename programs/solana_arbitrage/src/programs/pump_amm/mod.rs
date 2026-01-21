use crate::programs::ProgramMeta;
use crate::utils::utils::{amount_with_slippage, parse_token_account};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    account_info::next_account_info,
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};
mod constants;
use anchor_spl::token_interface::TokenAccount;

pub struct PumpAmm<'info> {
    pub accounts: Vec<AccountInfo<'info>>,
    pub program_id: AccountInfo<'info>,
    pub pool_id: AccountInfo<'info>,
    pub base_vault: AccountInfo<'info>,
    pub quote_vault: AccountInfo<'info>,
    pub base_token: AccountInfo<'info>,
    pub quote_token: AccountInfo<'info>,
    pub base_vault_account: TokenAccount,
    pub quote_vault_account: TokenAccount,
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

        let base_vault_account = parse_token_account(base_vault)?;
        let quote_vault_account = parse_token_account(quote_vault)?;

        Ok(PumpAmm {
            accounts: accounts.to_vec(),
            program_id: program_id.clone(),
            pool_id: pool_id.clone(),
            base_vault: base_vault.clone(),
            quote_vault: quote_vault.clone(),
            base_token: base_token.clone(),
            quote_token: quote_token.clone(),
            base_vault_account: base_vault_account.clone(),
            quote_vault_account: quote_vault_account.clone(),
        })
    }

    pub fn get_prices(&self) -> Result<(f64, f64)> {
        // price : Base -> Quote
        // inverse_price : Quote -> Base
        let (token_0_amount, token_1_amount) = (
            self.base_vault_account.amount,
            self.quote_vault_account.amount,
        );
        let price = token_1_amount as f64 / token_0_amount as f64;
        let inverse_price = 1.0 / price;
        Ok((price, inverse_price))
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
        let base_vault_amount = self.base_vault_account.amount as u128;
        let quote_vault_amount = self.quote_vault_account.amount as u128;
        eprintln!("Base vault amount: {:?}", base_vault_amount);
        eprintln!("Quote vault amount: {:?}", quote_vault_amount);

        // Determine direction: Is the user giving the Base token or the Quote token?
        let (input_reserve, output_reserve) = if input_mint == *self.base_token.key {
            // User gives Base -> Receives Quote
            // input_reserve (x) = base_vault, output_reserve (y) = quote_vault
            (base_vault_amount, quote_vault_amount)
        } else {
            // User gives Quote -> Receives Base
            // input_reserve (x) = quote_vault, output_reserve (y) = base_vault
            (quote_vault_amount, base_vault_amount)
        };

        // Constant Product Formula: y_out = y - (x * y) / (x + x_in)
        let numerator = input_reserve
            .checked_mul(output_reserve)
            .ok_or(ProgramError::InvalidArgument)?;

        let denominator = input_reserve
            .checked_add(amount_in as u128)
            .ok_or(ProgramError::InvalidArgument)?;

        let quotient = numerator
            .checked_div(denominator)
            .ok_or(ProgramError::InvalidArgument)?;

        let amount_out = output_reserve
            .checked_sub(quotient)
            .ok_or(ProgramError::InvalidArgument)?;

        // Apply 0.02% fee (multiply by 0.9998)
        let amount_out_after_fee = amount_out
            .checked_mul(9_998)
            .and_then(|x| x.checked_div(10_000))
            .ok_or(ProgramError::InvalidArgument)?;
        
        let final_amount = amount_with_slippage(amount_out_after_fee as u64, 0.02, false);

        Ok(amount_out_after_fee as u64)
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
        _input_mint: Pubkey,
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
        data.extend_from_slice(&amount_out_value.to_le_bytes());
        data.extend_from_slice(&max_amount_in.to_le_bytes());

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
        _input_mint: Pubkey,
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

    // Helper function to create a mock AccountInfo with TokenAccount data
    #[tokio::test]
    async fn test_pump_amm_swap() {
        use anchor_client::Cluster;
        use solana_client::nonblocking::rpc_client::RpcClient;

        // RPC client pointing to mainnet
        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        // Pool ID from mainnet
        let base_vault_key = Pubkey::from_str_const("34xJta85xK71cERHfJuSZiGiUyiRfupaiYndXu4qUbwW");
        let quote_vault_key =
            Pubkey::from_str_const("9KpaUSDcgU4yrRh2yYCaoujTNJA4vCtwGFQx4LksqeF9");
        let base_token_key = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let quote_token_key =
            Pubkey::from_str_const("7xTWEPgGrcRW1GFDLqvi92kjXjuQU2rGdEXx2u8Qsmgk");

        let base_vault_account = fetch_account_info_from_rpc(&rpc_client, base_vault_key).await;
        let quote_vault_account = fetch_account_info_from_rpc(&rpc_client, quote_vault_key).await;
        let base_token_account = fetch_account_info_from_rpc(&rpc_client, base_token_key).await;
        let quote_token_account = fetch_account_info_from_rpc(&rpc_client, quote_token_key).await;

        let program_id = create_mock_account_info(PumpAmm::PROGRAM_ID, system_program::id(), None);
        let pool_id = create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
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

        eprintln!("Base vault account: {:?}", base_vault_key);
        eprintln!("Quote vault account: {:?}", quote_vault_key);
        eprintln!("Base token account: {:?}", base_token_key);
        eprintln!("Quote token account: {:?}", quote_token_key);

        let accounts = vec![
            program_id,                           // 0
            pool_id,                              // 1
            base_vault_account.clone(),           // 2
            quote_vault_account.clone(),          // 3
            base_token_account.clone(),           // 4
            quote_token_account.clone(),          // 5
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

        let prices = pump_amm.get_prices().unwrap();
        let price = prices.0;
        let inverse_price = prices.1;
        eprintln!("Price: {:?}", price);
        eprintln!("Inverse price: {:?}", inverse_price);

        // Test with quote_amount_in = 10_000_000
        let quote_amount_in = 1_000_000_000u64;
        let clock = Clock::default();
        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112"); // Use quote_mint directly since quote_token was moved into accounts
        eprintln!("================================================");
        let amount_out_1 = pump_amm
            .swap_base_in(sol_mint, quote_amount_in, clock)
            .unwrap();
        let (sol_price, inverse_sol_price) = if base_token_key == sol_mint {
            (price, inverse_price)
        } else {
            (inverse_price, price)
        };
        let amount_out_1_v2 = quote_amount_in as f64 * sol_price;
        eprintln!("SOL {:?} -> TOKEN {:?} TOKEN V2: {:?}", quote_amount_in as f64 / 1_000_000_000.0, amount_out_1 as f64 / 1_000_000.0, amount_out_1_v2 as f64 / 1_000_000.0);

        eprintln!("================================================");
        let token_mint = if base_token_key == sol_mint {
            quote_token_key
        } else {
            base_token_key
        };
        let amount_out_2 = pump_amm.swap_base_in_impl(token_mint, amount_out_1_v2 as u64, Clock::default()).unwrap();
        let amount_received_v2_2 = amount_out_1_v2 as f64 * inverse_sol_price;
        eprintln!("TOKEN {:?} -> SOL {:?} SOL V2: {:?}", amount_out_1 as f64 / 1_000_000.0, amount_out_2 as f64 / 1_000_000_000.0, amount_received_v2_2 as f64 / 1_000_000_000.0);

    }

}
