use super::super::programs::ProgramMeta;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    account_info::next_account_info, program_error::ProgramError, pubkey::Pubkey,
};
use bytemuck;
// Expose the damm_v2 module
pub mod damm_v2;
// Re-export types from damm_v2 module
pub use damm_v2::curve::{get_spot_price_a_to_b, get_spot_price_b_to_a};
pub use damm_v2::{ActivationType, FeeMode, Pool, TradeDirection};

pub fn get_current_point(
    activation_type: u8,
    current_slot: u64,
    current_timestamp: u64,
) -> Result<u64> {
    use anchor_lang::prelude::*;
    use damm_v2::ActivationType;

    let activation_type =
        ActivationType::try_from(activation_type).map_err(|_| ProgramError::InvalidAccountData)?;

    let current_point = match activation_type {
        ActivationType::Slot => current_slot,
        ActivationType::Timestamp => current_timestamp,
    };

    Ok(current_point)
}

#[derive(Clone)]
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
    pub pool: Pool,
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

    fn get_prices(&self) -> Result<(f64, f64)> {
        self.get_prices_impl()
    }

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
}

impl<'info> MeteoraDammV2<'info> {
    pub const PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");

    pub fn new(accounts: &[AccountInfo<'info>]) -> Result<Self> {
        let mut iter = accounts.iter();
        let program_id: &AccountInfo<'info> = next_account_info(&mut iter)?; // 0
        let pool_id = next_account_info(&mut iter)?; // 1
        let base_vault = next_account_info(&mut iter)?; // 2
        let quote_vault = next_account_info(&mut iter)?; // 3
        let base_token = next_account_info(&mut iter)?; // 4
        let quote_token = next_account_info(&mut iter)?; // 5
        let pool_authority = next_account_info(&mut iter)?; // 6
        let event_authority = next_account_info(&mut iter)?; // 7
        let referral_token_account = next_account_info(&mut iter)?; // 8

        let pool_data = pool_id.try_borrow_data().unwrap();
        let pool: Pool = bytemuck::pod_read_unaligned(&pool_data[8..]);

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
            pool: pool.clone(),
        })
    }

    pub fn get_prices_impl(&self) -> Result<(f64, f64)> {
        // price : token_A -> token_B (A -> B)
        // inverse_price : token_B -> token_A (B -> A)
        let actual_sqrt_price = self.pool.sqrt_price as f64 / (1u128 << 64) as f64;
        let price_b_to_a_base = actual_sqrt_price * actual_sqrt_price; // token_b / token_a in base units
        let price = 1.0 / price_b_to_a_base; // token_a / token_b in base units
        Ok((price_b_to_a_base as f64, price as f64))
    }

    pub fn swap_base_in_impl(
        &self,
        input_mint: Pubkey,
        amount_in: u64,
        clock: Clock,
    ) -> Result<u64> {
        // Determine trade direction based on input_mint
        let trade_direction = if input_mint == self.base_token.key() {
            TradeDirection::AtoB
        } else {
            TradeDirection::BtoA
        };
        let current_timestamp = clock.unix_timestamp as u64;
        let current_slot = clock.slot as u64;

        let current_point =
            get_current_point(self.pool.activation_type, current_slot, current_timestamp)?;

        let has_referral = !self.referral_token_account.key.eq(&Pubkey::default());
        let fee_mode: FeeMode =
            FeeMode::get_fee_mode(self.pool.collect_fee_mode, trade_direction, has_referral)?;

        let results = self.pool.get_swap_result_from_exact_input(
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
        // Determine trade direction based on input_mint
        let trade_direction = if input_mint == self.base_token.key() {
            TradeDirection::AtoB
        } else {
            TradeDirection::BtoA
        };
        let current_timestamp = clock.unix_timestamp as u64;
        let current_slot = clock.slot as u64;

        let current_point =
            get_current_point(self.pool.activation_type, current_slot, current_timestamp)?;

        let has_referral = !self.referral_token_account.key.eq(&Pubkey::default());
        let fee_mode =
            FeeMode::get_fee_mode(self.pool.collect_fee_mode, trade_direction, has_referral)?;
        let results = self.pool.get_swap_result_from_exact_output(
            amount_out,
            &fee_mode,
            trade_direction,
            current_point,
        )?;

        eprintln!("results: {:?}", results);

        // Return the input amount needed to get the desired output
        Ok(results.excluded_fee_input_amount)
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
        use anchor_lang::solana_program::{
            instruction::{AccountMeta, Instruction},
            program::invoke,
        };

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
        use anchor_lang::solana_program::{
            instruction::{AccountMeta, Instruction},
            program::invoke,
        };

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
    use anchor_lang::solana_program::{
        account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, system_program,
    };
    use bytemuck;
    use damm_v2::state::pool::Pool;

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
        let expected_bytes = [
            202, 173, 213, 232, 67, 75, 181, 53, 88, 180, 220, 112, 105, 107, 171, 119, 215, 173,
            214, 67, 75, 181, 53, 88, 180, 220, 112, 105, 107, 171, 119, 215,
        ];
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&expected_bytes);
        let expected_id = Pubkey::new_from_array(arr);
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
        eprintln!("pool activation_point: {}", pool.activation_point);
        eprintln!("pool activation_type: {}", pool.activation_type);
        eprintln!("pool liquidity: {}", pool.liquidity);
        eprintln!("pool pool_status: {}", pool.pool_status);
        eprintln!("pool sqrt_price: {}", pool.sqrt_price);

        // Use actual addresses from pool data for important accounts
        let program_id = MeteoraDammV2::PROGRAM_ID;
        let base_vault = pool.token_a_vault;
        let quote_vault = pool.token_b_vault;
        let base_token = pool.token_a_mint;
        let quote_token = pool.token_b_mint;
        let pool_authority = Pubkey::new_unique(); // This might need to be calculated properly
        let event_authority = Pubkey::new_unique();
        let referral_token_account = Pubkey::default(); // Use default for no referral

        let correct_accounts = vec![
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

        let meteora_correct = MeteoraDammV2::new(&correct_accounts).unwrap();

        let clock = Clock {
            slot: 200000000, // High slot number to ensure activation
            epoch_start_timestamp: 0,
            epoch: 500, // High epoch
            leader_schedule_epoch: 0,
            unix_timestamp: 1700000000, // Recent timestamp (2023)
        };

        // Test with a much smaller amount first
        let amount_in = 1000000; // 0.001 tokens (assuming 9 decimals)
        let input_mint = base_token; // Swap base token in
        let result = meteora_correct.swap_base_in(input_mint, amount_in, clock);
        eprintln!("result: {:?}", result);
        if let Err(ref e) = result {
            eprintln!("Error: {:?}", e);
        }
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
        let input_mint = quote_token; // For swap_base_out, input is quote_token to get base_token out
        let result = meteora.swap_base_out(input_mint, amount_out, clock);

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
        let input_mint = base_token; // Swap base token in
        let result = meteora.swap_base_in(input_mint, amount_in, clock);

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
        let input_mint = base_token; // Swap base token in
        let result = meteora.swap_base_in(input_mint, amount_in, clock);

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

    #[tokio::test]
    async fn test_damm_v2_swap() {
        use anchor_client::Cluster;
        use solana_client::nonblocking::rpc_client::RpcClient;

        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        let pool_id = Pubkey::from_str_const("BHxTthQtTgz3jrDsvdxsaqP6R1KyCCNpg5kDY4NJBaqV");
        let pool_account_info = fetch_account_info_from_rpc(&rpc_client, pool_id).await;

        // Read pool data from AccountInfo in a separate scope to drop the borrow
        let (token_a_mint, token_b_mint, token_a_vault, token_b_vault) = {
            let pool_data: std::cell::Ref<'_, &mut [u8]> =
                pool_account_info.try_borrow_data().unwrap();
            let pool: Pool = bytemuck::pod_read_unaligned(&pool_data[8..]);

            eprintln!("Mint A: {:?}", pool.token_a_mint);
            eprintln!("Mint B: {:?}", pool.token_b_mint);
            eprintln!("Pool A Vault: {:?}", pool.token_a_vault);
            eprintln!("Pool B Vault: {:?}", pool.token_b_vault);
            eprintln!("pool activation_point: {}", pool.activation_point);
            eprintln!("pool activation_type: {}", pool.activation_type);
            eprintln!("pool liquidity: {}", pool.liquidity);
            eprintln!("pool pool_status: {}", pool.pool_status);
            eprintln!("pool sqrt_price: {}", pool.sqrt_price);

            (
                pool.token_a_mint,
                pool.token_b_mint,
                pool.token_a_vault,
                pool.token_b_vault,
            )
        };

        // Create program_id account
        let program_id_account = create_mock_account_info_with_data(
            MeteoraDammV2::PROGRAM_ID,
            system_program::id(),
            None,
        );
        let base_vault_account = fetch_account_info_from_rpc(&rpc_client, token_a_vault).await;
        let quote_vault_account = fetch_account_info_from_rpc(&rpc_client, token_b_vault).await;
        let base_token_account = fetch_account_info_from_rpc(&rpc_client, token_a_mint).await;
        let quote_token_account = fetch_account_info_from_rpc(&rpc_client, token_b_mint).await;

        // Create pool authority and event authority accounts
        let pool_authority = create_mock_account_info_with_data(
            MeteoraDammV2::PROGRAM_ID,
            system_program::id(),
            None,
        );
        let event_authority = create_mock_account_info_with_data(
            MeteoraDammV2::PROGRAM_ID,
            system_program::id(),
            None,
        );
        let referral_token_account = create_mock_account_info_with_data(
            MeteoraDammV2::PROGRAM_ID,
            system_program::id(),
            None,
        );

        let accounts = vec![
            program_id_account,             // 0: program_id
            pool_account_info.clone(),      // 1: pool_id
            base_vault_account.clone(),     // 2: base_vault
            quote_vault_account.clone(),    // 3: quote_vault
            base_token_account.clone(),     // 4: base_token
            quote_token_account.clone(),    // 5: quote_token
            pool_authority.clone(),         // 6: pool_authority
            event_authority.clone(),        // 7: event_authority
            referral_token_account.clone(), // 8: referral_token_account
        ];

        let clock1 = get_clock(&rpc_client).await.unwrap();
        let clock2 = get_clock(&rpc_client).await.unwrap();
        let meteora_damm_v2 = MeteoraDammV2::new(&accounts).unwrap();

        let prices = meteora_damm_v2.get_prices().unwrap();
        let price = prices.0;
        let inverse_price = prices.1;
        eprintln!("price: {:?}", price);
        eprintln!("inverse_price: {:?}", inverse_price);
        eprintln!("================================================");

        let in_sol_amount = 1_000_000_000;
        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let token_mint = if token_a_mint == sol_mint {
            token_b_mint
        } else {
            token_a_mint
        };

        let (sol_price, token_price) = if token_a_mint == sol_mint {
            (price, inverse_price)
        } else {
            (inverse_price, price)
        };
        eprintln!("Sol price: {:?}", sol_price);
        eprintln!("Token price: {:?}", token_price);
        let amount_out = meteora_damm_v2
            .swap_base_in(sol_mint, in_sol_amount, clock1)
            .unwrap();
        let amount_out_v2 = in_sol_amount as f64 * sol_price;
        eprintln!(
            "Step 1: {} SOL -> {} TOKEN / {}",
            in_sol_amount as f64 / 1_000_000_000.0,
            amount_out as f64 / 1_000_000.0,
            amount_out_v2 as f64 / 1_000_000.0
        );
        eprintln!("================================================");

        let token_amount_out = meteora_damm_v2
            .swap_base_in(token_mint, amount_out, clock2)
            .unwrap();
        let token_amount_out_v2 = amount_out as f64 * token_price;
        eprintln!(
            "Step 2: {} TOKEN -> {} SOL / {}",
            amount_out as f64 / 1_000_000.0,
            token_amount_out as f64 / 1_000_000_000.0,
            token_amount_out_v2 as f64 / 1_000_000_000.0
        );
        eprintln!("================================================");
    }
}
