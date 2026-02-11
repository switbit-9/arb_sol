// Declare submodules first (these are accessed via super:: from child modules)
pub mod curve;
pub mod error;
pub mod states;
pub mod utils;

// Now import using relative paths from declared modules
use self::curve::calculator::CurveCalculator;
use self::error::ErrorCode;
use self::states::{AmmConfig, PoolState, SwapParams};
use crate::utils::{
    token::{get_transfer_fee, get_transfer_inverse_fee},
    utils::parse_token_account,
};
use crate::{
    programs::ProgramMeta,
};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    account_info::AccountInfo,
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use bytemuck;
use std::marker::PhantomData;

fn get_prices(
    pool: &PoolState,
    base_vault_amount: u64,
    quote_vault_amount: u64,
) -> Result<(f64, f64)> {
    // price : Base -> Quote (decimal-adjusted)
    // inverse_price : Quote -> Base (decimal-adjusted)
    let (token_0_amount, token_1_amount) =
        pool.vault_amount_without_fee(base_vault_amount, quote_vault_amount)?;

    if token_0_amount == 0 {
        return Err(ProgramError::InvalidArgument.into());
    }

    // Calculate raw price ratio
    let raw_price = token_1_amount as f64 / token_0_amount as f64;

    if raw_price == 0.0 {
        return Err(ProgramError::InvalidArgument.into());
    }

    // Adjust for decimals: price = raw_price * 10^(base_decimals - quote_decimals)
    // This gives the actual price: how many quote tokens per 1 base token
    let decimal_adjustment =
        10f64.powi((pool.mint_0_decimals as i32) - (pool.mint_1_decimals as i32));
    let price = raw_price * decimal_adjustment;

    let inverse_price = 1.0 / price;
    Ok((price, inverse_price))
}

#[derive(Clone)]
pub struct RaydiumCPMM<'info> {
    // pub program_id: AccountInfo<'info>,
    pub pool_id: Pubkey,
    pub base_token_pk: Pubkey,
    pub quote_token_pk: Pubkey,
    pub base_vault_key: Pubkey,
    pub quote_vault_key: Pubkey,
    pub base_vault_amount: u64,
    pub quote_vault_amount: u64,
    pub price: f64,
    pub inverse_price: f64,

    // pub base_vault_account: TokenAccount,
    // pub quote_vault_account: TokenAccount,
    pub pool: PoolState,
    pub start_index: usize,
    pub end_index: usize,
    // pub amm_config: AccountInfo<'info>,
    // pub observation_key: AccountInfo<'info>,
    // pub authority: AccountInfo<'info>,
    pub creator_fee_rate: u64,
    pub trade_fee_rate: u64,
    pub protocol_fee_rate: u64,
    pub fund_fee_rate: u64,
    pub fee_rate: f64,
    pub phantom: PhantomData<&'info ()>,
}

impl<'info> ProgramMeta for RaydiumCPMM<'info> {
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
        (&self.base_token_pk, &self.quote_token_pk)
    }

    fn swap_base_in<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        _clock: Clock,
    ) -> Result<u64> {
        let base_token_account = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
        let quote_token_account = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];

        // Determine input/output vaults and mints
        let (
            input_vault_key,
            output_vault_key,
            input_vault_amount,
            output_vault_amount,
            input_token_account,
            output_token_account,
        ) = if input_mint == self.base_token_pk {
            (
                self.base_vault_key,
                self.quote_vault_key,
                &self.base_vault_amount,
                &self.quote_vault_amount,
                base_token_account,
                quote_token_account,
            )
        } else {
            (
                self.quote_vault_key,
                self.base_vault_key,
                &self.quote_vault_amount,
                &self.base_vault_amount,
                quote_token_account,
                base_token_account,
            )
        };

        let transfer_fee = get_transfer_fee(input_token_account, amount_in)?;
        let actual_amount_in = amount_in.saturating_sub(transfer_fee);

        let SwapParams {
            trade_direction,
            total_input_token_amount,
            total_output_token_amount,
            token_0_price_x64: _,
            token_1_price_x64: _,
            is_creator_fee_on_input,
        } = self.pool.get_swap_params(
            input_vault_key,
            output_vault_key,
            *input_vault_amount,
            *output_vault_amount,
        )?;

        let creator_fee_rate = self.pool.adjust_creator_fee_rate(self.creator_fee_rate);
        let result = CurveCalculator::swap_base_input(
            u128::from(actual_amount_in),
            u128::from(total_input_token_amount),
            u128::from(total_output_token_amount),
            self.trade_fee_rate,
            creator_fee_rate,
            self.protocol_fee_rate,
            self.fund_fee_rate,
            is_creator_fee_on_input,
        )
        .ok_or(ErrorCode::ZeroTradingTokens)?;

        let amount_out = u64::try_from(result.output_amount).unwrap();
        let transfer_fee = get_transfer_fee(output_token_account, amount_out)?;
        let amount_out = amount_out
            .checked_sub(transfer_fee)
            .ok_or(ErrorCode::MathOverflow)?;

        Ok(amount_out)
    }

    fn swap_base_out<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        _clock: Clock,
    ) -> Result<u64> {
        let amm_data = accounts[self.start_index + 6].try_borrow_data()?;
        let amm_config: AmmConfig = AmmConfig::try_from_bytes(&amm_data)?;

        let base_token_account = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
        let quote_token_account = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];

        // Determine output mint from input mint
        // Determine input/output vaults and mints
        let (
            input_vault_key,
            output_vault_key,
            input_vault_amount,
            output_vault_amount,
            input_token_account,
            output_token_account,
        ) = if output_mint != self.base_token_pk {
            (
                self.base_vault_key,
                self.quote_vault_key,
                &self.base_vault_amount,
                &self.quote_vault_amount,
                base_token_account,
                quote_token_account,
            )
        } else {
            (
                self.quote_vault_key,
                self.base_vault_key,
                &self.quote_vault_amount,
                &self.base_vault_amount,
                quote_token_account,
                base_token_account,
            )
        };

        let out_transfer_fee = get_transfer_inverse_fee(output_token_account, amount_out)?;
        let amount_out_with_transfer_fee = amount_out
            .checked_add(out_transfer_fee)
            .ok_or(ErrorCode::MathOverflow)?;

        let SwapParams {
            trade_direction: _,
            total_input_token_amount,
            total_output_token_amount,
            token_0_price_x64: _,
            token_1_price_x64: _,
            is_creator_fee_on_input,
        } = self.pool.get_swap_params(
            input_vault_key,
            output_vault_key,
            *input_vault_amount,
            *output_vault_amount,
        )?;

        let creator_fee_rate = self
            .pool
            .adjust_creator_fee_rate(amm_config.creator_fee_rate);
        let result = CurveCalculator::swap_base_output(
            u128::from(amount_out_with_transfer_fee),
            u128::from(total_input_token_amount),
            u128::from(total_output_token_amount),
            amm_config.trade_fee_rate,
            creator_fee_rate,
            amm_config.protocol_fee_rate,
            amm_config.fund_fee_rate,
            is_creator_fee_on_input,
        )
        .ok_or(ErrorCode::ZeroTradingTokens)?;

        let source_amount_swapped = u64::try_from(result.input_amount).unwrap();
        let amount_in_transfer_fee =
            get_transfer_inverse_fee(input_token_account, source_amount_swapped)?;
        let input_transfer_amount = source_amount_swapped
            .checked_add(amount_in_transfer_fee)
            .ok_or(ErrorCode::MathOverflow)?;

        Ok(input_transfer_amount)
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
        let base_vault = &accounts[self.start_index + Self::BASE_VAULT_IDX];
        let quote_vault = &accounts[self.start_index + Self::QUOTE_VAULT_IDX];
        let authority_account = &accounts[self.start_index + Self::VAULT_AUTHORITY_IDX];
        let amm_config_account = &accounts[self.start_index + Self::AMM_CONFIG_IDX];
        let observation_account = &accounts[self.start_index + Self::OBSERVATION_KEY_IDX];

        let (
            input_token_program,
            output_token_program,
            user_input_token_account,
            user_output_token_account,
            input_vault,
            output_vault,
            input_mint,
            output_mint,
        ) = if input_mint == self.base_token_pk {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
                base_vault,
                quote_vault,
                mint_1_account,
                mint_2_account,
            )
        } else {
            (
                mint_2_token_program,
                mint_1_token_program,
                user_mint_2_token_account,
                user_mint_1_token_account,
                base_vault,
                quote_vault,
                mint_2_account,
                mint_1_account,
            )
        };

        let metas = vec![
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*authority_account.key, false),
            AccountMeta::new_readonly(self.pool.amm_config, false),
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new(*user_input_token_account.key, false),
            AccountMeta::new(*user_output_token_account.key, false),
            AccountMeta::new(*input_vault.key, false),
            AccountMeta::new(*output_vault.key, false),
            AccountMeta::new_readonly(*input_token_program.key, false),
            AccountMeta::new_readonly(*output_token_program.key, false),
            AccountMeta::new_readonly(*input_mint.key, false),
            AccountMeta::new_readonly(*output_mint.key, false),
            AccountMeta::new(*observation_account.key, false),
        ];
        let mut data = vec![143, 190, 90, 218, 196, 30, 51, 222];
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.unwrap_or(0).to_le_bytes());

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data,
        };

        // Collect all required accounts for invoke
        // Order must match metas exactly!
        let accounts_vec: Vec<AccountInfo<'a>> = vec![
            payer.clone(),
            authority_account.clone(),
            amm_config_account.clone(),
            pool_id.clone(),
            user_input_token_account.clone(),
            user_output_token_account.clone(),
            input_vault.clone(),
            output_vault.clone(),
            input_token_program.clone(),
            output_token_program.clone(),
            input_mint.clone(),
            output_mint.clone(),
            observation_account.clone(),
        ];

        // Cast entire vector to AccountInfo<'a> for invoke
        unsafe {
            let accounts_slice: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts_slice)?;
        }
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
        let base_vault = &accounts[self.start_index + Self::BASE_VAULT_IDX];
        let quote_vault = &accounts[self.start_index + Self::QUOTE_VAULT_IDX];
        let authority_account = &accounts[self.start_index + Self::VAULT_AUTHORITY_IDX];
        let amm_config_account = &accounts[self.start_index + Self::AMM_CONFIG_IDX];
        let observation_account = &accounts[self.start_index + Self::OBSERVATION_KEY_IDX];

        let (
            input_token_program,
            output_token_program,
            user_input_token_account,
            user_output_token_account,
            input_vault,
            output_vault,
            input_mint,
            output_mint,
        ) = if input_mint == self.base_token_pk {
            (
                mint_2_token_program,
                mint_1_token_program,
                user_mint_2_token_account,
                user_mint_1_token_account,
                quote_vault,
                base_vault,
                mint_2_account,
                mint_1_account,
            )
        } else {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
                quote_vault,
                base_vault,
                mint_1_account,
                mint_2_account,
            )
        };

        let metas = vec![
            AccountMeta::new(*payer.key, true),
            AccountMeta::new(*authority_account.key, false),
            AccountMeta::new(self.pool.amm_config, false),
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new(*user_input_token_account.key, false),
            AccountMeta::new(*user_output_token_account.key, false),
            AccountMeta::new(*input_vault.key, false),
            AccountMeta::new(*output_vault.key, false),
            AccountMeta::new_readonly(*input_token_program.key, false),
            AccountMeta::new_readonly(*output_token_program.key, false),
            AccountMeta::new_readonly(*input_mint.key, false),
            AccountMeta::new_readonly(*output_mint.key, false),
            AccountMeta::new(self.pool.observation_key, false),
        ];
        let mut data = vec![55, 217, 98, 86, 163, 74, 180, 173];
        data.extend_from_slice(&max_amount_in.to_le_bytes());
        data.extend_from_slice(&amount_out.unwrap_or(0).to_le_bytes());

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data,
        };

        // Collect all required accounts for invoke
        // Order must match metas exactly!
        let accounts_vec: Vec<AccountInfo<'a>> = vec![
        payer.clone(), 
        authority_account.clone(),
        amm_config_account.clone(),
        pool_id.clone(),
        user_input_token_account.clone(),
        user_output_token_account.clone(),
        input_vault.clone(),
        output_vault.clone(),
        input_token_program.clone(),
        output_token_program.clone(),
        input_mint.clone(),
        output_mint.clone(),
        observation_account.clone(),
        ];
        // Cast entire vector to AccountInfo<'a> for invoke
        unsafe {
            let accounts_slice: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts_slice)?;
        }
        Ok(())
    }

    fn log_accounts<'a>(&self, _accounts: &[AccountInfo<'a>]) -> Result<()> {
        msg!(
            "Raydium CPMM accounts: pool={}, base_vault={}, quote_vault={}, base_token={}, quote_token={}",
            self.pool_id,
            self.base_vault_key,
            self.quote_vault_key,
            self.base_token_pk,
            self.quote_token_pk,
        );
        Ok(())
    }
}

impl<'info> RaydiumCPMM<'info> {
    // pub const PROGRAM_ID: Pubkey =
        // Pubkey::from_str_const("CPMDWBwJDtYax9qW7AyRuVC19Cc4L4Vcy4n2BHAbHkCW"); //TO DO: be changed for mainnet
    pub const PROGRAM_ID: Pubkey = Pubkey::from_str_const("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");
    pub const PROGRAM_ID_IDX: usize = 0;
    pub const POOL_ID_IDX: usize = 1;
    pub const BASE_VAULT_IDX: usize = 2;
    pub const QUOTE_VAULT_IDX: usize = 3;
    pub const BASE_TOKEN_IDX: usize = 4;
    pub const QUOTE_TOKEN_IDX: usize = 5;
    pub const AMM_CONFIG_IDX: usize = 6;
    pub const OBSERVATION_KEY_IDX: usize = 7;
    pub const VAULT_AUTHORITY_IDX: usize = 8;

    pub fn new(
        accounts: &[AccountInfo<'info>],
        start_index: usize,
        end_index: usize,
    ) -> Result<Self> {
        let pool_id = accounts[start_index + Self::POOL_ID_IDX].clone(); // 1
        let base_vault = accounts[start_index + Self::BASE_VAULT_IDX].clone(); // 2
        let quote_vault = accounts[start_index + Self::QUOTE_VAULT_IDX].clone(); // 3

        // Parse vault amounts
        let base_vault_amount = parse_token_account(&base_vault)?.amount;
        let quote_vault_amount = parse_token_account(&quote_vault)?.amount;

        let amm_data = accounts[start_index + Self::AMM_CONFIG_IDX].try_borrow_data()?;
        let amm_config: AmmConfig = AmmConfig::try_from_bytes(&amm_data)?;

        let pool_data = pool_id.try_borrow_data()?;
        let pool = bytemuck::pod_read_unaligned::<PoolState>(&pool_data[8..]);

        let (price, inverse_price) = get_prices(&pool, base_vault_amount, quote_vault_amount)?;

        Ok(RaydiumCPMM {
            pool_id: *pool_id.key,
            base_token_pk: pool.token_0_mint,
            quote_token_pk: pool.token_1_mint,
            base_vault_key: pool.token_0_vault,
            quote_vault_key: pool.token_1_vault,
            base_vault_amount: base_vault_amount,
            quote_vault_amount: quote_vault_amount,
            price: price,
            inverse_price: inverse_price,
            pool: pool,
            start_index,
            end_index,
            creator_fee_rate: amm_config.creator_fee_rate,
            trade_fee_rate: amm_config.trade_fee_rate,
            protocol_fee_rate: amm_config.protocol_fee_rate,
            fund_fee_rate: amm_config.fund_fee_rate,
            fee_rate: (amm_config.trade_fee_rate + pool.adjust_creator_fee_rate(amm_config.creator_fee_rate)) as f64 / 1_000_000.0,
            phantom: PhantomData,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::prelude::Clock;
    use anchor_lang::solana_program::{account_info::AccountInfo, pubkey::Pubkey, system_program};
    use solana_client::nonblocking::rpc_client::RpcClient;
    use solana_sdk::pubkey::Pubkey as SdkPubkey;

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
        rpc_client: &RpcClient,
        key: Pubkey,
    ) -> AccountInfo<'static> {
        let sdk_pubkey = SdkPubkey::try_from(key.to_bytes().as_ref())
            .expect("Failed to convert Pubkey to SdkPubkey");
        let account = rpc_client
            .get_account(&sdk_pubkey)
            .await
            .expect(&format!("Failed to fetch account {}", key));
        account_to_account_info(key, account)
    }

    /// Get on chain clock from RPC
    async fn get_clock(rpc_client: &RpcClient) -> anyhow::Result<Clock> {
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

    #[tokio::test]
    async fn test_raydium_cpmm_fetch_pool_info() {
        use anchor_client::Cluster;

        // RPC client pointing to mainnet
        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        // Pool ID from mainnet
        let pool_id_key = Pubkey::from_str_const("21WT1Hs2DpANaGQJncBXV8GHqE1jr7RQNmUKPXCYhrZE");

        // Fetch pool account
        let pool_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool_id_key.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();

        // Parse pool state (skip first 8 bytes which is Anchor discriminator)
        // PoolState::LEN includes the 8-byte discriminator, so the struct size is LEN - 8
        let pool_state_size = PoolState::LEN - 8;
        if pool_account.data.len() < 8 + pool_state_size {
            panic!(
                "Pool account data too short: {} bytes, expected at least {} bytes",
                pool_account.data.len(),
                8 + pool_state_size
            );
        }
        let pool: PoolState =
            bytemuck::pod_read_unaligned(&pool_account.data[8..8 + pool_state_size]);

        // Fetch vault accounts to get amounts
        let (vault_0_account_opt, vault_1_account_opt, token_0_amount, token_1_amount) = match (
            rpc_client
                .get_account(&SdkPubkey::try_from(pool.token_0_vault.to_bytes().as_ref()).unwrap())
                .await,
            rpc_client
                .get_account(&SdkPubkey::try_from(pool.token_1_vault.to_bytes().as_ref()).unwrap())
                .await,
        ) {
            (Ok(v0), Ok(v1)) => {
                // Parse token account amounts (offset 64 for amount in SPL token account)
                let t0_amount = if v0.data.len() >= 72 {
                    u64::from_le_bytes(v0.data[64..72].try_into().unwrap())
                } else {
                    0
                };
                let t1_amount = if v1.data.len() >= 72 {
                    u64::from_le_bytes(v1.data[64..72].try_into().unwrap())
                } else {
                    0
                };
                (Some(v0), Some(v1), t0_amount, t1_amount)
            }
            (Err(e0), _) => {
                eprintln!(
                    "Warning: Could not fetch token 0 vault {}: {:?}",
                    pool.token_0_vault, e0
                );
                (None, None, 0, 0)
            }
            (_, Err(e1)) => {
                eprintln!(
                    "Warning: Could not fetch token 1 vault {}: {:?}",
                    pool.token_1_vault, e1
                );
                (None, None, 0, 0)
            }
        };

        // Fetch AMM config
        let amm_config_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.amm_config.to_bytes().as_ref()).unwrap())
            .await;

        if let Ok(amm_config_account) = amm_config_account {
            let amm_config: AmmConfig = AmmConfig::try_from_bytes(&amm_config_account.data)
                .unwrap_or_else(|_| {
                    eprintln!("Warning: Failed to deserialize AMM config, using default");
                    AmmConfig::default()
                });

            eprintln!("\n=== AMM Config ===");
            eprintln!("Trade Fee Rate: {}", amm_config.trade_fee_rate);
            eprintln!("Protocol Fee Rate: {}", amm_config.protocol_fee_rate);
            eprintln!("Fund Fee Rate: {}", amm_config.fund_fee_rate);
            eprintln!("Creator Fee Rate: {}", amm_config.creator_fee_rate);
        } else {
            eprintln!("\nWarning: Could not fetch AMM config account");
        }

        // Determine which vault is base and which is quote
        eprintln!("\n=== Token Information ===");
        eprintln!("Base Token (Token 0): {}", pool.token_0_mint);
        eprintln!("Quote Token (Token 1): {}", pool.token_1_mint);

        // Verify we got valid data
        assert_ne!(
            pool.token_0_mint,
            Pubkey::default(),
            "Token 0 mint should be set"
        );
        assert_ne!(
            pool.token_1_mint,
            Pubkey::default(),
            "Token 1 mint should be set"
        );
        assert_ne!(
            pool.token_0_vault,
            Pubkey::default(),
            "Token 0 vault should be set"
        );
        assert_ne!(
            pool.token_1_vault,
            Pubkey::default(),
            "Token 1 vault should be set"
        );

        // Note: Vault balances might be zero if pool is closed or accounts don't exist
        if token_0_amount > 0 && token_1_amount > 0 {
            eprintln!("✓ Pool has active liquidity");
        } else {
            eprintln!("⚠ Pool vaults may be empty or accounts not found (pool might be closed)");
        }
    }

    #[tokio::test]
    async fn test_raydium_cpmm_swap_base_in() {
        use anchor_client::Cluster;

        // RPC client pointing to mainnet
        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        // Pool ID from mainnet
        let pool_id_key = Pubkey::from_str_const("21WT1Hs2DpANaGQJncBXV8GHqE1jr7RQNmUKPXCYhrZE");

        eprintln!("Testing swap_base_in for pool: {}", pool_id_key);

        // Fetch pool account
        let pool_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool_id_key.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();

        // Parse pool state
        // Parse pool state (skip first 8 bytes which is Anchor discriminator)
        let pool_state_size = PoolState::LEN - 8;
        if pool_account.data.len() < 8 + pool_state_size {
            panic!(
                "Pool account data too short: {} bytes, expected at least {} bytes",
                pool_account.data.len(),
                8 + pool_state_size
            );
        }
        let pool: PoolState =
            bytemuck::pod_read_unaligned(&pool_account.data[8..8 + pool_state_size]);

        // Fetch vault accounts
        let vault_0_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_0_vault.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();
        let vault_1_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_1_vault.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();

        // Fetch mint accounts
        let mint_0_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_0_mint.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();
        let mint_1_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_1_mint.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();

        // Fetch AMM config
        let amm_config_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.amm_config.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();

        // Get clock
        let clock = get_clock(&rpc_client).await.unwrap();

        // Extract vault amounts before converting to AccountInfo (they get moved)
        let base_vault_amount = if vault_0_account.data.len() >= 72 {
            u64::from_le_bytes(vault_0_account.data[64..72].try_into().unwrap())
        } else {
            0
        };
        let quote_vault_amount = if vault_1_account.data.len() >= 72 {
            u64::from_le_bytes(vault_1_account.data[64..72].try_into().unwrap())
        } else {
            0
        };

        // Convert accounts to AccountInfo
        let pool_id_account_info = account_to_account_info(pool_id_key, pool_account);
        let base_vault = account_to_account_info(pool.token_0_vault, vault_0_account);
        let quote_vault = account_to_account_info(pool.token_1_vault, vault_1_account);
        let base_token = account_to_account_info(pool.token_0_mint, mint_0_account);
        let quote_token = account_to_account_info(pool.token_1_mint, mint_1_account);

        // Create program_id account
        let program_id_key = RaydiumCPMM::PROGRAM_ID;
        let program_id_account =
            create_mock_account_info_with_data(program_id_key, system_program::id(), None);

        // Create accounts array - must match the order expected by RaydiumCPMM::new
        let accounts = vec![
            program_id_account,                                           // 0: program_id
            pool_id_account_info.clone(),                                 // 1: pool_id
            base_vault.clone(),                                           // 2: base_vault
            quote_vault.clone(),                                          // 3: quote_vault
            base_token.clone(),                                           // 4: base_token
            quote_token.clone(),                                          // 5: quote_token
            account_to_account_info(pool.amm_config, amm_config_account), // 6: amm_config
        ];

        // Create RaydiumCPMM instance
        let raydium_cpmm =
            RaydiumCPMM::new(&accounts, 0, accounts.len()).expect("Failed to create RaydiumCPMM");

        // Test swap_base_in with a small amount
        // Use 1% of the smaller vault balance to avoid large price impact

        eprintln!("Base vault amount: {}", base_vault_amount);
        eprintln!("Quote vault amount: {}", quote_vault_amount);

        // Use 0.1% of base vault as input (swap base in = input base token, get quote token out)
        let amount_in = base_vault_amount / 1000;

        // Adjust based on decimals - if decimals are high, we might need larger amounts
        let amount_in_adjusted = if pool.mint_0_decimals >= 9 {
            amount_in.max(1_000_000) // At least 0.001 tokens for 9 decimals
        } else {
            amount_in.max(1000) // At least 1000 base units
        };

        eprintln!(
            "Testing swap_base_in with amount_in: {}",
            amount_in_adjusted
        );

        let input_mint = *base_token.key; // Swap base token in
        let result = raydium_cpmm.swap_base_in(&accounts, input_mint, amount_in_adjusted, clock);

        match result {
            Ok(amount_out) => {
                eprintln!("✓ swap_base_in succeeded!");
                eprintln!("  Input: {} base tokens", amount_in_adjusted);
                eprintln!("  Output: {} quote tokens", amount_out);
                assert!(amount_out > 0, "Output amount should be greater than 0");

                // Verify output is reasonable (should be proportional to reserves)
                let expected_ratio = (quote_vault_amount as f64) / (base_vault_amount as f64);
                let actual_ratio = (amount_out as f64) / (amount_in_adjusted as f64);
                eprintln!("  Expected price ratio: {:.6}", expected_ratio);
                eprintln!("  Actual price ratio: {:.6}", actual_ratio);
            }
            Err(e) => {
                eprintln!("✗ swap_base_in failed: {:?}", e);
                panic!("swap_base_in should succeed with valid pool data");
            }
        }
    }

    #[tokio::test]
    async fn test_raydium_cpmm_swap_base_out() {
        use anchor_client::Cluster;

        // RPC client pointing to mainnet
        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        // Pool ID from mainnet
        let pool_id_key = Pubkey::from_str_const("21WT1Hs2DpANaGQJncBXV8GHqE1jr7RQNmUKPXCYhrZE");

        eprintln!("Testing swap_base_out for pool: {}", pool_id_key);

        // Fetch pool account
        let pool_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool_id_key.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();

        // Parse pool state
        // Parse pool state (skip first 8 bytes which is Anchor discriminator)
        let pool_state_size = PoolState::LEN - 8;
        if pool_account.data.len() < 8 + pool_state_size {
            panic!(
                "Pool account data too short: {} bytes, expected at least {} bytes",
                pool_account.data.len(),
                8 + pool_state_size
            );
        }
        let pool: PoolState =
            bytemuck::pod_read_unaligned(&pool_account.data[8..8 + pool_state_size]);

        // Fetch vault accounts
        let vault_0_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_0_vault.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();
        let vault_1_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_1_vault.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();

        // Fetch mint accounts
        let mint_0_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_0_mint.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();
        let mint_1_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_1_mint.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();

        // Fetch AMM config
        let amm_config_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.amm_config.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();

        // Get clock
        let clock = get_clock(&rpc_client).await.unwrap();

        // Extract vault amounts before converting to AccountInfo (they get moved)
        let base_vault_amount = if vault_0_account.data.len() >= 72 {
            u64::from_le_bytes(vault_0_account.data[64..72].try_into().unwrap())
        } else {
            0
        };
        let quote_vault_amount = if vault_1_account.data.len() >= 72 {
            u64::from_le_bytes(vault_1_account.data[64..72].try_into().unwrap())
        } else {
            0
        };

        // Convert accounts to AccountInfo
        let pool_id_account_info = account_to_account_info(pool_id_key, pool_account);
        let base_vault = account_to_account_info(pool.token_0_vault, vault_0_account);
        let quote_vault = account_to_account_info(pool.token_1_vault, vault_1_account);
        let base_token = account_to_account_info(pool.token_0_mint, mint_0_account);
        let quote_token = account_to_account_info(pool.token_1_mint, mint_1_account);

        // Create program_id account
        let program_id_key = RaydiumCPMM::PROGRAM_ID;
        let program_id_account =
            create_mock_account_info_with_data(program_id_key, system_program::id(), None);

        // Create accounts array
        let accounts = vec![
            program_id_account,
            pool_id_account_info.clone(),
            base_vault.clone(),
            quote_vault.clone(),
            base_token.clone(),
            quote_token.clone(),
            account_to_account_info(pool.amm_config, amm_config_account),
        ];

        // Create RaydiumCPMM instance
        let raydium_cpmm = RaydiumCPMM::new(&accounts, 0, accounts.len()).expect("Failed to create RaydiumCPMM");

        // Test swap_base_out with desired output amount

        eprintln!("Base vault amount: {}", base_vault_amount);
        eprintln!("Quote vault amount: {}", quote_vault_amount);

        // For swap_base_out, we specify desired output amount
        // Use 0.1% of quote vault as desired output (we want quote tokens out, so we input base tokens)
        let amount_out_desired = quote_vault_amount / 1000;

        // Adjust based on decimals
        let amount_out_adjusted = if pool.mint_1_decimals >= 9 {
            amount_out_desired.max(1_000_000) // At least 0.001 tokens for 9 decimals
        } else {
            amount_out_desired.max(1000) // At least 1000 base units
        };

        eprintln!(
            "Testing swap_base_out with amount_out_desired: {}",
            amount_out_adjusted
        );

        // swap_base_out takes the desired output amount and returns required input
        // input_mint is the token we're putting in (base token) to get quote token out
        let input_mint = *base_token.key;
        let result = raydium_cpmm.swap_base_out(&accounts, input_mint, amount_out_adjusted, clock);

        match result {
            Ok(amount_in_required) => {
                eprintln!("✓ swap_base_out succeeded!");
                eprintln!("  Desired Output: {} quote tokens", amount_out_adjusted);
                eprintln!("  Required Input: {} base tokens", amount_in_required);
                assert!(
                    amount_in_required > 0,
                    "Required input amount should be greater than 0"
                );

                // Verify the required input is reasonable
                let expected_ratio = (base_vault_amount as f64) / (quote_vault_amount as f64);
                let actual_ratio = (amount_in_required as f64) / (amount_out_adjusted as f64);
                eprintln!("  Expected price ratio: {:.6}", expected_ratio);
                eprintln!("  Actual price ratio: {:.6}", actual_ratio);
            }
            Err(e) => {
                eprintln!("✗ swap_base_out failed: {:?}", e);
                panic!("swap_base_out should succeed with valid pool data");
            }
        }
    }
    pub fn deserialize_anchor_account<T: AccountDeserialize>(
        account: &solana_sdk::account::Account,
    ) -> Result<T> {
        let mut data: &[u8] = &account.data;
        T::try_deserialize(&mut data).map_err(Into::into)
    }
    #[tokio::test]
    async fn test_raydium_cpmm_round_trip_swap() {
        use anchor_client::Cluster;

        // RPC client pointing to mainnet
        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        // Pool ID from mainnet
        let pool_id_key = Pubkey::from_str_const("C9U2Ksk6KKWvLEeo5yUQ7Xu46X7NzeBJtd9PBfuXaUSM");

        // Fetch all necessary accounts (same as previous tests)
        let pool_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool_id_key.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();

        // Parse pool state (skip first 8 bytes which is Anchor discriminator)
        let pool_state_size = PoolState::LEN - 8;
        if pool_account.data.len() < 8 + pool_state_size {
            panic!(
                "Pool account data too short: {} bytes, expected at least {} bytes",
                pool_account.data.len(),
                8 + pool_state_size
            );
        }
        // PoolState is a ZeroCopy type, so use bytemuck instead of AccountDeserialize
        let pool: PoolState =
            bytemuck::pod_read_unaligned(&pool_account.data[8..8 + pool_state_size]);

        let vault_0_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_0_vault.to_bytes().as_ref()).unwrap())
            .await;
        let vault_1_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_1_vault.to_bytes().as_ref()).unwrap())
            .await;

        eprintln!("pool: {:?}", pool_id_key);
        eprintln!("base vault: {:?}", pool.token_0_vault);
        eprintln!("quote: {:?}", pool.token_1_vault);
        eprintln!("base mint: {:?}", pool.token_0_mint);
        eprintln!("quote mint: {:?}", pool.token_1_mint);
        eprintln!("amm config: {:?}", pool.amm_config);
        eprintln!("lp mint: {:?}", pool.lp_mint);

        if vault_0_account.is_err() || vault_1_account.is_err() {
            eprintln!("Warning: Could not fetch vault accounts. Pool may be closed or accounts may not exist.");
            eprintln!("Vault 0 fetch: {:?}", vault_0_account.as_ref().err());
            eprintln!("Vault 1 fetch: {:?}", vault_1_account.as_ref().err());
            return;
        }

        let vault_0_account = vault_0_account.unwrap();
        let vault_1_account = vault_1_account.unwrap();

        let mint_0_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_0_mint.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();
        let mint_1_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_1_mint.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();

        let amm_config_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.amm_config.to_bytes().as_ref()).unwrap())
            .await
            .unwrap();

        // Extract vault amounts before converting to AccountInfo (they get moved)
        let base_vault_amount = if vault_0_account.data.len() >= 72 {
            u64::from_le_bytes(vault_0_account.data[64..72].try_into().unwrap())
        } else {
            0
        };

        let pool_id_account_info = account_to_account_info(pool_id_key, pool_account);
        let base_vault = account_to_account_info(pool.token_0_vault, vault_0_account);
        let quote_vault = account_to_account_info(pool.token_1_vault, vault_1_account);
        let base_token = account_to_account_info(pool.token_0_mint, mint_0_account);
        let quote_token = account_to_account_info(pool.token_1_mint, mint_1_account);
        let amm_config = account_to_account_info(pool.amm_config, amm_config_account);

        let program_id_key = RaydiumCPMM::PROGRAM_ID;
        let program_id_account =
            create_mock_account_info_with_data(program_id_key, system_program::id(), None);

        let accounts = vec![
            program_id_account,
            pool_id_account_info.clone(),
            base_vault.clone(),
            quote_vault.clone(),
            base_token.clone(),
            quote_token.clone(),
            amm_config.clone(),
        ];

        let raydium_cpmm =
            RaydiumCPMM::new(&accounts, 0, accounts.len()).expect("Failed to create RaydiumCPMM");

        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");

        eprintln!(
            "Price: {:?}, {:?}",
            raydium_cpmm.price, raydium_cpmm.inverse_price
        );
        eprintln!("Base decimals: {:?}, Quote decimals: {:?}", pool.mint_0_decimals, pool.mint_1_decimals);

        // Test round trip: base -> quote -> base
        let sol_in = 100_000_000_000; // 0.01% of pool

        let token_mint = if *base_token.key == sol_mint {
            *quote_token.key
        } else {
            *base_token.key
        };
        eprintln!("================================================");
        // Step 1: Swap base -> quote
        let clock1 = get_clock(&rpc_client).await.unwrap();
        eprintln!("sol_in: {:?}", sol_in);
        let token_out = raydium_cpmm
            .swap_base_in(&accounts, sol_mint, sol_in, clock1.clone())
            .expect("swap_base_in failed");
        eprintln!(
            "Step 1 (swap_base_in): {} SOL -> {} TOKEN",
            sol_in as f64 / 1_000_000_000.0,
            token_out as f64 / 1_000_000.0,
        );
        let max_sol_in = raydium_cpmm
            .swap_base_out(&accounts, token_mint, token_out, clock1.clone())
            .expect("swap_base_out failed");
        eprintln!(
            "Step 1 (swap_base_out): MAX SOL IN {} -> {} TOKEN OUT",
            max_sol_in as f64 / 1_000_000_000.0,
            token_out as f64 / 1_000_000.0,
        );

        eprintln!("================================================");

        let sol_out = raydium_cpmm
            .swap_base_in(&accounts, token_mint, token_out, clock1.clone())
            .expect("second swap_base_in failed");
        eprintln!(
            "Step 2 (swap_base_in): {} TOKEN -> {} SOL",
            token_out as f64 / 1_000_000.0,
            sol_out as f64 / 1_000_000_000.0,
        );
        let max_token_in = raydium_cpmm
            .swap_base_out(&accounts, sol_mint, sol_out, clock1.clone())
            .expect("second swap_base_out failed");
        eprintln!(
            "Step 2 (swap_base_out): {} MAX TOKEN IN -> {} SOL OUT",
            max_token_in as f64 / 1_000_000.0,
            sol_out as f64 / 1_000_000_000.0
        );
    }
}
