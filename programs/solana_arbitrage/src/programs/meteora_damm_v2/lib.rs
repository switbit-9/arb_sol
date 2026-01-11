use anchor_lang::prelude::*;

// pub mod utils;  // Module doesn't exist - not needed

use super::super::programs::ProgramMeta;
use anchor_lang::solana_program::{
    account_info::next_account_info,
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};
// Import from damm_v2 module
use crate::programs::meteora_damm_v2::damm_v2::{
    params::swap::TradeDirection,
    state::{fee::FeeMode, Pool},
    utils::activation_handler::ActivationType,
};

pub fn get_current_point(
    activation_type: u8,
    current_slot: u64,
    current_timestamp: u64,
) -> Result<u64> {
    let activation_type =
        ActivationType::try_from(activation_type).map_err(|_| ProgramError::InvalidAccountData)?;

    let current_point = match activation_type {
        ActivationType::Slot => current_slot,
        ActivationType::Timestamp => current_timestamp,
    };

    Ok(current_point)
}

pub struct MeteoraDammV2<'info> {
    pub program_id: AccountInfo<'info>,
    pub pool_id: AccountInfo<'info>,
    pub base_vault: AccountInfo<'info>,
    pub quote_vault: AccountInfo<'info>,
    pub base_token: AccountInfo<'info>,
    pub quote_token: AccountInfo<'info>,
    pub pool_authority: AccountInfo<'info>,
    pub event_authority: AccountInfo<'info>,
    pub referral_token_account: AccountInfo<'info>,
}

impl<'info> ProgramMeta for MeteoraDammV2<'info> {
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
}

impl<'info> MeteoraDammV2<'info> {
    pub const PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");
    pub fn new(accounts: &[AccountInfo<'info>]) -> Result<Self> {
        let mut iter = accounts.iter();
        let program_id = next_account_info(&mut iter)?; // 0
        let pool_id = next_account_info(&mut iter)?; // 1
        let base_vault = next_account_info(&mut iter)?; // 2
        let quote_vault = next_account_info(&mut iter)?; // 3
        let base_token = next_account_info(&mut iter)?; // 4
        let quote_token = next_account_info(&mut iter)?; // 5
        let pool_authority = next_account_info(&mut iter)?; // 6
        let event_authority = next_account_info(&mut iter)?; // 7
        let referral_token_account = next_account_info(&mut iter)?; // 8

        Ok(MeteoraDammV2 {
            program_id: program_id.clone(),
            pool_id: pool_id.clone(),
            base_vault: base_vault.clone(),
            quote_vault: quote_vault.clone(),
            base_token: base_token.clone(),
            quote_token: quote_token.clone(),
            pool_authority: pool_authority.clone(),
            event_authority: event_authority.clone(),
            referral_token_account: referral_token_account.clone(),
        })
    }

    pub fn log_accounts(&self) -> Result<()> {
        msg!(
            "Meteora DAMM v2 accounts: pool={}, base_vault={}, quote_vault={}, base_token={}, quote_token={}, pool_authority={}, event_authority={}, referral_token_account={}",
            self.pool_id.key,
            self.base_vault.key,
            self.quote_vault.key,
            self.base_token.key,
            self.quote_token.key,
            self.pool_authority.key,
            self.event_authority.key,
            self.referral_token_account.key,
        );
        Ok(())
    }

    pub fn swap_base_in_impl(
        &self,
        input_mint: Pubkey,
        amount_in: u64,
        clock: Clock,
    ) -> Result<u64> {
        let data = self.pool_id.try_borrow_data()?;
        let pool: &Pool = bytemuck::try_from_bytes::<Pool>(&data[8..])
            .map_err(|_| ProgramError::InvalidAccountData)?;

        // Determine trade direction based on input_mint
        let trade_direction = if input_mint == self.base_token.key() {
            TradeDirection::AtoB
        } else {
            TradeDirection::BtoA
        };
        let current_timestamp = clock.unix_timestamp as u64;
        let current_slot = clock.slot as u64;

        let current_point =
            get_current_point(pool.activation_type, current_slot, current_timestamp)?;

        let has_referral = !self.referral_token_account.key.eq(&Pubkey::default());
        let fee_mode = FeeMode::get_fee_mode(pool.collect_fee_mode, trade_direction, has_referral)?;
        let results = pool.get_swap_result_from_exact_input(
            amount_in,
            &fee_mode,
            trade_direction,
            current_point,
        )?;

        Ok(results.output_amount)
    }

    pub fn swap_base_out_impl(
        &self,
        input_mint: Pubkey,
        amount_out: u64,
        clock: Clock,
    ) -> Result<u64> {
        let data = self.pool_id.try_borrow_data()?;
        let pool: &Pool = bytemuck::try_from_bytes::<Pool>(&data[8..])
            .map_err(|_| ProgramError::InvalidAccountData)?;

        // Determine trade direction based on input_mint
        let trade_direction = if input_mint == self.base_token.key() {
            TradeDirection::AtoB
        } else {
            TradeDirection::BtoA
        };
        let current_timestamp = clock.unix_timestamp as u64;
        let current_slot = clock.slot as u64;

        let current_point =
            get_current_point(pool.activation_type, current_slot, current_timestamp)?;

        let has_referral = !self.referral_token_account.key.eq(&Pubkey::default());
        let fee_mode = FeeMode::get_fee_mode(pool.collect_fee_mode, trade_direction, has_referral)?;
        let results = pool.get_swap_result_from_exact_input(
            amount_out,
            &fee_mode,
            trade_direction,
            current_point,
        )?;

        // Return the input amount needed to get the desired output
        Ok(results.output_amount)
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

        let amount_out_value = amount_out.unwrap_or(0);
        let metas = vec![
            AccountMeta::new_readonly(*self.pool_authority.key, false),
            AccountMeta::new(*self.pool_id.key, false),
            AccountMeta::new(*user_quote_token_account.key, false),
            AccountMeta::new(*user_base_token_account.key, false),
            AccountMeta::new(*self.base_vault.key, false),
            AccountMeta::new(*self.quote_vault.key, false),
            AccountMeta::new_readonly(*self.base_token.key, false),
            AccountMeta::new_readonly(*self.quote_token.key, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*base_token_program.key, false),
            AccountMeta::new_readonly(*quote_token_program.key, false),
            AccountMeta::new_readonly(*self.referral_token_account.key, false),
            AccountMeta::new_readonly(*self.event_authority.key, false),
            AccountMeta::new_readonly(*self.program_id.key, false),
        ];

        let mut data = vec![0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];
        data.extend_from_slice(&max_amount_in.to_le_bytes());
        data.extend_from_slice(&amount_out_value.to_le_bytes());

        let swap_ix = Instruction {
            program_id: *self.program_id.key,
            accounts: metas,
            data,
        };

        // Collect AccountInfo into a vector and use unsafe to cast lifetimes
        // This is safe because 'a outlives 'info in practice when called from execute_arbitrage_path
        let mut accounts_vec: Vec<AccountInfo<'info>> = vec![
            self.pool_authority.to_account_info(),
            self.pool_id.to_account_info(),
            self.base_vault.to_account_info(),
            self.quote_vault.to_account_info(),
            self.base_token.to_account_info(),
            self.quote_token.to_account_info(),
            self.referral_token_account.to_account_info(),
            self.event_authority.to_account_info(),
            self.program_id.to_account_info(),
        ];
        // Cast parameter AccountInfo<'a> to AccountInfo<'info> to add to vector
        accounts_vec
            .push(unsafe { std::mem::transmute(user_quote_token_account.to_account_info()) });
        accounts_vec
            .push(unsafe { std::mem::transmute(user_base_token_account.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(payer.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(base_token_program.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(quote_token_program.to_account_info()) });

        // Cast entire vector to AccountInfo<'a> for invoke
        unsafe {
            let accounts: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts)?;
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

        let min_amount_out_value = min_amount_out.unwrap_or(0);
        let metas = vec![
            AccountMeta::new_readonly(*self.pool_authority.key, false),
            AccountMeta::new(*self.pool_id.key, false),
            AccountMeta::new(*user_base_token_account.key, false),
            AccountMeta::new(*user_quote_token_account.key, false),
            AccountMeta::new(*self.base_vault.key, false),
            AccountMeta::new(*self.quote_vault.key, false),
            AccountMeta::new_readonly(*self.base_token.key, false),
            AccountMeta::new_readonly(*self.quote_token.key, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*base_token_program.key, false),
            AccountMeta::new_readonly(*quote_token_program.key, false),
            AccountMeta::new_readonly(*self.referral_token_account.key, false),
            AccountMeta::new_readonly(*self.event_authority.key, false),
            AccountMeta::new_readonly(*self.program_id.key, false),
        ];
        let mut data = vec![0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out_value.to_le_bytes());

        let swap_ix = Instruction {
            program_id: *self.program_id.key,
            accounts: metas,
            data,
        };

        // Collect AccountInfo into a vector and use unsafe to cast lifetimes
        let mut accounts_vec: Vec<AccountInfo<'info>> = vec![
            self.pool_authority.to_account_info(),
            self.pool_id.to_account_info(),
            self.base_vault.to_account_info(),
            self.quote_vault.to_account_info(),
            self.base_token.to_account_info(),
            self.quote_token.to_account_info(),
            self.referral_token_account.to_account_info(),
            self.event_authority.to_account_info(),
            self.program_id.to_account_info(),
        ];
        accounts_vec
            .push(unsafe { std::mem::transmute(user_base_token_account.to_account_info()) });
        accounts_vec
            .push(unsafe { std::mem::transmute(user_quote_token_account.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(payer.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(base_token_program.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(quote_token_program.to_account_info()) });

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
    use anchor_client::solana_sdk::clock::Clock;
    use anchor_client::{solana_client::nonblocking::rpc_client::RpcClient, Cluster};
    use anchor_lang::solana_program::{
        account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, system_program,
    };
    use bytemuck;
    use cp_amm::state::Pool;

    // Helper function to create a mock AccountInfo
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

    // Helper function to create a Pool from actual pool data
    // Pool data from pool_data.txt (Python bytes literal converted to Rust)
    fn create_test_pool() -> Pool {
        // Actual pool data bytes (from pool_data.txt, skipping 8-byte discriminator)
        // This is the raw pool account data starting after the discriminator
        let pool_data_bytes = include_bytes!("pool_data.bin");

        // Skip the 8-byte discriminator and deserialize the Pool
        let pool: Pool = bytemuck::pod_read_unaligned(&pool_data_bytes[8..]);
        pool
    }

    #[test]
    fn test_get_current_point_slot() {
        let activation_type = 0u8; // Slot
        let current_slot = 1000u64;
        let current_timestamp = 1234567890u64;

        let result = get_current_point(activation_type, current_slot, current_timestamp).unwrap();
        assert_eq!(result, current_slot);
    }

    #[test]
    fn test_get_current_point_timestamp() {
        let activation_type = 1u8; // Timestamp
        let current_slot = 1000u64;
        let current_timestamp = 1234567890u64;

        let result = get_current_point(activation_type, current_slot, current_timestamp).unwrap();
        assert_eq!(result, current_timestamp);
    }

    #[test]
    fn test_get_current_point_invalid_type() {
        let activation_type = 255u8; // Invalid
        let current_slot = 1000u64;
        let current_timestamp = 1234567890u64;

        let result = get_current_point(activation_type, current_slot, current_timestamp);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), ProgramError::InvalidAccountData.into());
    }

    #[test]
    fn test_meteora_damm_v2_program_id() {
        let expected_id = Pubkey::from_str_const("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");
        assert_eq!(MeteoraDammV2::PROGRAM_ID, expected_id);
    }

    #[test]
    fn test_meteora_damm_v2_new_insufficient_accounts() {
        let accounts = vec![];
        let result = MeteoraDammV2::new(&accounts);
        assert!(result.is_err());
    }

    #[test]
    fn test_meteora_damm_v2_new_sufficient_accounts() {
        let program_id = Pubkey::new_unique();
        let pool_id = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let base_token = Pubkey::new_unique();
        let quote_token = Pubkey::new_unique();
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        let referral_token_account = Pubkey::new_unique();

        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),
            create_mock_account_info(pool_id, system_program::id(), None),
            create_mock_account_info(base_vault, system_program::id(), None),
            create_mock_account_info(quote_vault, system_program::id(), None),
            create_mock_account_info(base_token, system_program::id(), None),
            create_mock_account_info(quote_token, system_program::id(), None),
            create_mock_account_info(pool_authority, system_program::id(), None),
            create_mock_account_info(event_authority, system_program::id(), None),
            create_mock_account_info(referral_token_account, system_program::id(), None),
        ];

        let result = MeteoraDammV2::new(&accounts);
        assert!(result.is_ok());

        let meteora = result.unwrap();
        assert_eq!(*meteora.program_id.key, program_id);
        assert_eq!(*meteora.pool_id.key, pool_id);
        assert_eq!(*meteora.base_vault.key, base_vault);
        assert_eq!(*meteora.quote_vault.key, quote_vault);
    }

    #[test]
    fn test_swap_base_in_basic() {
        let pool = create_test_pool();
        let pool_bytes = bytemuck::bytes_of(&pool);

        // Create pool account with 8-byte discriminator + pool data
        let mut pool_data = vec![0u8; 8];
        pool_data.extend_from_slice(pool_bytes);

        let pool_id = Pubkey::new_unique();
        let pool_account = create_mock_account_info(pool_id, system_program::id(), Some(pool_data));

        let program_id = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let base_token = Pubkey::new_unique();
        let quote_token = Pubkey::new_unique();
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        let referral_token_account = Pubkey::new_unique();

        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),
            pool_account.clone(),
            create_mock_account_info(base_vault, system_program::id(), None),
            create_mock_account_info(quote_vault, system_program::id(), None),
            create_mock_account_info(base_token, system_program::id(), None),
            create_mock_account_info(quote_token, system_program::id(), None),
            create_mock_account_info(pool_authority, system_program::id(), None),
            create_mock_account_info(event_authority, system_program::id(), None),
            create_mock_account_info(referral_token_account, system_program::id(), None),
        ];

        let meteora = MeteoraDammV2::new(&accounts).unwrap();
        let data = meteora.pool_id.try_borrow_data().unwrap();
        let pool: Pool = bytemuck::pod_read_unaligned(&data[8..]);

        eprintln!("pool: {:?}", pool.token_a_mint);
        eprintln!("pool: {:?}", pool.token_b_mint);
        eprintln!("pool: {:?}", pool.token_a_vault);
        eprintln!("pool: {:?}", pool.token_b_vault);

        let clock = Clock {
            slot: 1000,
            epoch_start_timestamp: 0,
            epoch: 0,
            leader_schedule_epoch: 0,
            unix_timestamp: 1234567890,
        };

        // Test with a small amount
        let amount_in = 187734691202; // 1 token (assuming 6 decimals)
        let result = meteora.swap_base_in(amount_in, clock);

        // Should succeed and return some output amount
        assert!(result.is_ok());
        let output_amount = result.unwrap();
        assert!(output_amount > 0);
        eprintln!("Result {:?}", output_amount);
    }

    #[test]
    fn test_swap_base_out_basic() {
        let pool = create_test_pool();
        let pool_bytes = bytemuck::bytes_of(&pool);

        let mut pool_data = vec![0u8; 8];
        pool_data.extend_from_slice(pool_bytes);

        let pool_id = Pubkey::new_unique();
        let pool_account = create_mock_account_info(pool_id, system_program::id(), Some(pool_data));

        let program_id = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let base_token = Pubkey::new_unique();
        let quote_token = Pubkey::new_unique();
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        let referral_token_account = Pubkey::new_unique();

        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),
            pool_account.clone(),
            create_mock_account_info(base_vault, system_program::id(), None),
            create_mock_account_info(quote_vault, system_program::id(), None),
            create_mock_account_info(base_token, system_program::id(), None),
            create_mock_account_info(quote_token, system_program::id(), None),
            create_mock_account_info(pool_authority, system_program::id(), None),
            create_mock_account_info(event_authority, system_program::id(), None),
            create_mock_account_info(referral_token_account, system_program::id(), None),
        ];

        let meteora = MeteoraDammV2::new(&accounts).unwrap();
        let data = meteora.pool_id.try_borrow_data().unwrap();
        let pool: Pool = bytemuck::pod_read_unaligned(&data[8..]);

        eprintln!("pool: {:?}", pool.token_a_mint);
        eprintln!("pool: {:?}", pool.token_b_mint);

        let clock = Clock {
            slot: 1000,
            epoch_start_timestamp: 0,
            epoch: 0,
            leader_schedule_epoch: 0,
            unix_timestamp: 1234567890,
        };

        // Test with a small amount (desired output amount)
        let amount_out = 1_000_000_000; // Desired output amount
        let result = meteora.swap_base_out(amount_out, clock);

        // Should succeed and return some output amount
        assert!(result.is_ok());
        let output_amount = result.unwrap();
        assert!(output_amount > 0);
        eprintln!("Result {:?}", output_amount);
    }

    #[test]
    fn test_swap_base_in_with_referral() {
        let pool = create_test_pool();
        let pool_bytes = bytemuck::bytes_of(&pool);

        let mut pool_data = vec![0u8; 8];
        pool_data.extend_from_slice(pool_bytes);

        let pool_id = Pubkey::new_unique();
        let pool_account = create_mock_account_info(pool_id, system_program::id(), Some(pool_data));

        let program_id = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let base_token = Pubkey::new_unique();
        let quote_token = Pubkey::new_unique();
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        // Use a non-default referral token account
        let referral_token_account = Pubkey::new_unique();

        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),
            pool_account.clone(),
            create_mock_account_info(base_vault, system_program::id(), None),
            create_mock_account_info(quote_vault, system_program::id(), None),
            create_mock_account_info(base_token, system_program::id(), None),
            create_mock_account_info(quote_token, system_program::id(), None),
            create_mock_account_info(pool_authority, system_program::id(), None),
            create_mock_account_info(event_authority, system_program::id(), None),
            create_mock_account_info(referral_token_account, system_program::id(), None),
        ];

        let meteora = MeteoraDammV2::new(&accounts).unwrap();

        let clock = Clock {
            slot: 1000,
            epoch_start_timestamp: 0,
            epoch: 0,
            leader_schedule_epoch: 0,
            unix_timestamp: 1234567890,
        };

        let amount_in = 1_000_000;
        let result = meteora.swap_base_in(amount_in, clock);

        // Should succeed even with referral
        assert!(result.is_ok());
    }

    #[test]
    fn test_swap_base_in_with_default_referral() {
        let pool = create_test_pool();
        let pool_bytes = bytemuck::bytes_of(&pool);

        let mut pool_data = vec![0u8; 8];
        pool_data.extend_from_slice(pool_bytes);

        let pool_id = Pubkey::new_unique();
        let pool_account = create_mock_account_info(pool_id, system_program::id(), Some(pool_data));

        let program_id = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let base_token = Pubkey::new_unique();
        let quote_token = Pubkey::new_unique();
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        // Use default (zero) referral token account
        let referral_token_account = Pubkey::default();

        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),
            pool_account.clone(),
            create_mock_account_info(base_vault, system_program::id(), None),
            create_mock_account_info(quote_vault, system_program::id(), None),
            create_mock_account_info(base_token, system_program::id(), None),
            create_mock_account_info(quote_token, system_program::id(), None),
            create_mock_account_info(pool_authority, system_program::id(), None),
            create_mock_account_info(event_authority, system_program::id(), None),
            create_mock_account_info(referral_token_account, system_program::id(), None),
        ];

        let meteora = MeteoraDammV2::new(&accounts).unwrap();

        let clock = Clock {
            slot: 1000,
            epoch_start_timestamp: 0,
            epoch: 0,
            leader_schedule_epoch: 0,
            unix_timestamp: 1234567890,
        };

        let amount_in = 1_000_000;
        let result = meteora.swap_base_in(amount_in, clock);

        // Should succeed without referral
        assert!(result.is_ok());
    }

    #[test]
    fn test_program_meta_implementation() {
        let program_id = MeteoraDammV2::PROGRAM_ID;
        let pool_id = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let base_token = Pubkey::new_unique();
        let quote_token = Pubkey::new_unique();
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        let referral_token_account = Pubkey::new_unique();

        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),
            create_mock_account_info(pool_id, system_program::id(), None),
            create_mock_account_info(base_vault, system_program::id(), None),
            create_mock_account_info(quote_vault, system_program::id(), None),
            create_mock_account_info(base_token, system_program::id(), None),
            create_mock_account_info(quote_token, system_program::id(), None),
            create_mock_account_info(pool_authority, system_program::id(), None),
            create_mock_account_info(event_authority, system_program::id(), None),
            create_mock_account_info(referral_token_account, system_program::id(), None),
        ];

        let meteora = MeteoraDammV2::new(&accounts).unwrap();

        // Test ProgramMeta trait implementation
        let id = meteora.get_id();
        assert_eq!(*id, MeteoraDammV2::PROGRAM_ID);

        let (vault1, vault2) = meteora.get_vaults();
        assert_eq!(*vault1.key, *meteora.base_vault.key);
        assert_eq!(*vault2.key, *meteora.quote_vault.key);
    }
}
