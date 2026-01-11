// Declare submodules first (these are accessed via super:: from child modules)
pub mod curve;
pub mod error;
pub mod states;
pub mod utils;

// Now import using relative paths from declared modules
use self::curve::calculator::CurveCalculator;
use self::curve::calculator::TradeDirection;
use self::error::ErrorCode;
use self::states::{AmmConfig, PoolState, SwapParams};
use self::utils::token::{amount_with_slippage, get_transfer_fee, get_transfer_inverse_fee};
use crate::utils::utils::parse_token_account;
use crate::{
    programs::ProgramMeta,
    // Market,
};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    account_info::{next_account_info, AccountInfo},
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use bytemuck;

pub struct RaydiumCpSwapProgram {}

// =====================
// RaydiumCPMM meta parser
// =====================

#[derive(Clone)]
pub struct RaydiumCPMM<'info> {
    pub accounts: Vec<AccountInfo<'info>>,
    pub program_id: AccountInfo<'info>,
    pub pool_id: AccountInfo<'info>,
    pub base_vault: AccountInfo<'info>,
    pub quote_vault: AccountInfo<'info>,
    pub base_token: AccountInfo<'info>,
    pub quote_token: AccountInfo<'info>,
    // pub amm_config: AccountInfo<'info>,
    // pub observation_key: AccountInfo<'info>,
    // pub authority: AccountInfo<'info>,
}

impl<'info> ProgramMeta for RaydiumCPMM<'info> {
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
        // For swap_base_out, amount_in is actually amount_out desired, input_mint is the input token
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

impl<'info> RaydiumCPMM<'info> {
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

    pub fn swap_base_in_impl(
        &self,
        input_mint: Pubkey,
        amount_in: u64,
        _clock: Clock,
    ) -> Result<u64> {
        let pool_data = self.pool_id.try_borrow_data()?;
        let pool = bytemuck::pod_read_unaligned::<PoolState>(&pool_data[8..]);

        let amm_data = self.accounts[6].try_borrow_data()?;
        let amm_config: AmmConfig = AmmConfig::try_from_bytes(&amm_data)?;

        // Determine input/output vaults and mints
        let (input_vault, output_vault, input_token_account, output_token_account) =
            if input_mint == self.base_token.key() {
                (
                    &self.base_vault,
                    &self.quote_vault,
                    &self.base_token,
                    &self.quote_token,
                )
            } else {
                (
                    &self.quote_vault,
                    &self.base_vault,
                    &self.quote_token,
                    &self.base_token,
                )
            };

        let transfer_fee = get_transfer_fee(input_token_account, amount_in)?;
        let actual_amount_in = amount_in.saturating_sub(transfer_fee);

        // Parse vault amounts
        let input_vault_account = parse_token_account(input_vault)?;
        let output_vault_account = parse_token_account(output_vault)?;

        let SwapParams {
            trade_direction,
            total_input_token_amount,
            total_output_token_amount,
            token_0_price_x64: _,
            token_1_price_x64: _,
            is_creator_fee_on_input,
        } = pool.get_swap_params(
            input_vault.key(),
            output_vault.key(),
            input_vault_account.amount,
            output_vault_account.amount,
        )?;

        let creator_fee_rate = pool.adjust_creator_fee_rate(amm_config.creator_fee_rate);
        let result = CurveCalculator::swap_base_input(
            u128::from(actual_amount_in),
            u128::from(total_input_token_amount),
            u128::from(total_output_token_amount),
            amm_config.trade_fee_rate,
            creator_fee_rate,
            amm_config.protocol_fee_rate,
            amm_config.fund_fee_rate,
            is_creator_fee_on_input,
        )
        .ok_or(ErrorCode::ZeroTradingTokens)?;

        let amount_out = u64::try_from(result.output_amount).unwrap();

        // Get transfer fee for output token based on trade direction
        let output_token_account = match trade_direction {
            TradeDirection::ZeroForOne => {
                // ZeroForOne means token_0 -> token_1, so output is token_1 (quote_token)
                &self.quote_token
            }
            TradeDirection::OneForZero => {
                // OneForZero means token_1 -> token_0, so output is token_0 (base_token)
                &self.base_token
            }
        };
        let transfer_fee = get_transfer_fee(output_token_account, amount_out)?;
        let amount_received = amount_out
            .checked_sub(transfer_fee)
            .ok_or(ErrorCode::MathOverflow)?;
        // calc mint out amount with slippage (0% slippage)
        let minimum_amount_out = amount_with_slippage(amount_received, 0.0, false);

        Ok(minimum_amount_out)
    }

    pub fn swap_base_out_impl(
        &self,
        input_mint: Pubkey,
        amount_out: u64,
        _clock: Clock,
    ) -> Result<u64> {
        let pool_data = self.pool_id.try_borrow_data()?;
        let pool = bytemuck::pod_read_unaligned::<PoolState>(&pool_data[8..]);

        let amm_data = self.accounts[6].try_borrow_data()?;
        let amm_config: AmmConfig = AmmConfig::try_from_bytes(&amm_data)?;

        // Determine output mint from input mint
        let output_mint = if input_mint == self.base_token.key() {
            self.quote_token.key()
        } else {
            self.base_token.key()
        };

        // Get transfer fee for output token (inverse calculation)
        let output_token_account = if output_mint == self.base_token.key() {
            &self.base_token
        } else {
            &self.quote_token
        };
        let out_transfer_fee = get_transfer_inverse_fee(output_token_account, amount_out)?;
        let amount_out_with_transfer_fee = amount_out
            .checked_add(out_transfer_fee)
            .ok_or(ErrorCode::MathOverflow)?;

        // Determine input/output vaults and mints
        // For swap_base_out, input_mint determines the direction
        let (input_vault, output_vault) = if input_mint == self.base_token.key() {
            (&self.base_vault, &self.quote_vault)
        } else {
            (&self.quote_vault, &self.base_vault)
        };

        // Parse vault amounts
        let input_vault_account = parse_token_account(input_vault)?;
        let output_vault_account = parse_token_account(output_vault)?;

        let SwapParams {
            trade_direction: _,
            total_input_token_amount,
            total_output_token_amount,
            token_0_price_x64: _,
            token_1_price_x64: _,
            is_creator_fee_on_input,
        } = pool.get_swap_params(
            input_vault.key(),
            output_vault.key(),
            input_vault_account.amount,
            output_vault_account.amount,
        )?;

        let creator_fee_rate = pool.adjust_creator_fee_rate(amm_config.creator_fee_rate);
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

        // Get transfer inverse fee for input token (we need to send more to account for fees)
        let input_token_account = if input_mint == self.base_token.key() {
            &self.base_token
        } else {
            &self.quote_token
        };
        let amount_in_transfer_fee =
            get_transfer_inverse_fee(input_token_account, source_amount_swapped)?;

        let input_transfer_amount = source_amount_swapped
            .checked_add(amount_in_transfer_fee)
            .ok_or(ErrorCode::MathOverflow)?;
        // calc max in with slippage (0% slippage)
        let max_amount_in = amount_with_slippage(input_transfer_amount, 0.0, true);

        Ok(max_amount_in)
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
            input_token_program,
            output_token_program,
            user_input_token_account,
            user_output_token_account,
            input_vault,
            output_vault,
            input_mint,
            output_mint,
        ) = if mint_1_account.key() == self.base_token.key() {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
                &self.base_vault,
                &self.quote_vault,
                mint_1_account,
                mint_2_account,
            )
        } else if mint_2_account.key() == self.base_token.key() {
            (
                mint_2_token_program,
                mint_1_token_program,
                user_mint_2_token_account,
                user_mint_1_token_account,
                &self.base_vault,
                &self.quote_vault,
                mint_2_account,
                mint_1_account,
            )
        } else {
            return Err(ProgramError::InvalidAccountData.into());
        };

        // Load pool state to get amm_config and authority
        let pool_data = self.pool_id.try_borrow_data()?;
        let pool = bytemuck::pod_read_unaligned::<PoolState>(&pool_data[8..]);
        let amm_config_key = pool.amm_config;
        let authority_key = pool.pool_creator; // Or derive from pool_id if needed

        // Get observation_key from pool state
        let observation_key_key = pool.observation_key;

        let amount_out_value = amount_out.unwrap_or(0);
        let metas = vec![
            AccountMeta::new(*payer.key, true),
            AccountMeta::new(authority_key, false),
            AccountMeta::new(amm_config_key, false),
            AccountMeta::new(*self.pool_id.key, false),
            AccountMeta::new(*user_input_token_account.key, false),
            AccountMeta::new(*user_output_token_account.key, false),
            AccountMeta::new(*input_vault.key, false),
            AccountMeta::new(*output_vault.key, false),
            AccountMeta::new_readonly(*input_token_program.key, false),
            AccountMeta::new_readonly(*output_token_program.key, false),
            AccountMeta::new_readonly(*input_mint.key, false),
            AccountMeta::new_readonly(*output_mint.key, false),
            AccountMeta::new(observation_key_key, false),
        ];
        let mut data = vec![143, 190, 90, 218, 196, 30, 51, 222];
        data.extend_from_slice(&max_amount_in.to_le_bytes());
        data.extend_from_slice(&amount_out_value.to_le_bytes());

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data,
        };

        // Collect all required accounts for invoke
        // Order must match metas exactly!
        let mut accounts_vec: Vec<AccountInfo<'info>> = vec![self.pool_id.clone()];

        // Add accounts from function parameters (cast from 'a to 'info)
        accounts_vec.push(unsafe { std::mem::transmute(payer.to_account_info()) });
        accounts_vec
            .push(unsafe { std::mem::transmute(user_input_token_account.to_account_info()) });
        accounts_vec
            .push(unsafe { std::mem::transmute(user_output_token_account.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(input_vault.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(output_vault.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(input_token_program.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(output_token_program.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(input_mint.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(output_mint.to_account_info()) });

        // Cast entire vector to AccountInfo<'a> for invoke
        unsafe {
            let accounts_slice: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts_slice)?;
        }
        Ok(())
    }

    pub fn invoke_swap_base_out_impl<'a>(
        &self,
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
        let (
            input_token_program,
            output_token_program,
            user_input_token_account,
            user_output_token_account,
            input_vault,
            output_vault,
            input_mint,
            output_mint,
        ) = if mint_1_account.key() == self.base_token.key() {
            (
                mint_2_token_program,
                mint_1_token_program,
                user_mint_2_token_account,
                user_mint_1_token_account,
                &self.quote_vault,
                &self.base_vault,
                mint_2_account,
                mint_1_account,
            )
        } else if mint_2_account.key() == self.base_token.key() {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
                &self.quote_vault,
                &self.base_vault,
                mint_1_account,
                mint_2_account,
            )
        } else {
            return Err(ProgramError::InvalidAccountData.into());
        };

        // Load pool state to get amm_config and authority
        let pool_data = self.pool_id.try_borrow_data()?;
        let pool = bytemuck::pod_read_unaligned::<PoolState>(&pool_data[8..]);
        let amm_config_key = pool.amm_config;
        let authority_key = pool.pool_creator;
        let observation_key_key = pool.observation_key;

        let metas = vec![
            AccountMeta::new(*payer.key, true),
            AccountMeta::new(authority_key, false),
            AccountMeta::new(amm_config_key, false),
            AccountMeta::new(*self.pool_id.key, false),
            AccountMeta::new(*user_input_token_account.key, false),
            AccountMeta::new(*user_output_token_account.key, false),
            AccountMeta::new(*input_vault.key, false),
            AccountMeta::new(*output_vault.key, false),
            AccountMeta::new_readonly(*input_token_program.key, false),
            AccountMeta::new_readonly(*output_token_program.key, false),
            AccountMeta::new_readonly(*input_mint.key, false),
            AccountMeta::new_readonly(*output_mint.key, false),
            AccountMeta::new(observation_key_key, false),
        ];
        let mut data = vec![55, 217, 98, 86, 163, 74, 180, 173];
        data.extend_from_slice(&amount_out.to_le_bytes());
        data.extend_from_slice(&max_amount_in.to_le_bytes());

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data,
        };

        // Collect all required accounts for invoke
        // Order must match metas exactly!
        let mut accounts_vec: Vec<AccountInfo<'info>> = vec![self.pool_id.clone()];

        // Add accounts from function parameters (cast from 'a to 'info)
        accounts_vec.push(unsafe { std::mem::transmute(payer.to_account_info()) });
        accounts_vec
            .push(unsafe { std::mem::transmute(user_input_token_account.to_account_info()) });
        accounts_vec
            .push(unsafe { std::mem::transmute(user_output_token_account.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(input_vault.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(output_vault.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(input_token_program.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(output_token_program.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(input_mint.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(output_mint.to_account_info()) });

        // Cast entire vector to AccountInfo<'a> for invoke
        unsafe {
            let accounts_slice: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts_slice)?;
        }
        Ok(())
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
        let raydium_cpmm = RaydiumCPMM::new(&accounts).expect("Failed to create RaydiumCPMM");

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
        let result = raydium_cpmm.swap_base_in(input_mint, amount_in_adjusted, clock);

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
        let raydium_cpmm = RaydiumCPMM::new(&accounts).expect("Failed to create RaydiumCPMM");

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
        let result = raydium_cpmm.swap_base_out(input_mint, amount_out_adjusted, clock);

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
        let pool_id_key = Pubkey::from_str_const("Q2sPHPdUWFMg7M7wwrQKLrn619cAucfRsmhVJffodSp");

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
        eprintln!("base: {:?}", pool.token_0_vault);
        eprintln!("quote: {:?}", pool.token_1_vault);
        let vault_0_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_0_vault.to_bytes().as_ref()).unwrap())
            .await;
        let vault_1_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool.token_1_vault.to_bytes().as_ref()).unwrap())
            .await;

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

        let raydium_cpmm = RaydiumCPMM::new(&accounts).expect("Failed to create RaydiumCPMM");

        // Get initial vault amounts

        // Test round trip: base -> quote -> base
        let initial_amount = 1_000_000_000; // 0.01% of pool
        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");

        let amount_in_adjusted = if pool.mint_0_decimals >= 9 {
            initial_amount.max(1_000_000)
        } else {
            initial_amount.max(1000)
        };

        // Step 1: Swap base -> quote
        let clock1 = get_clock(&rpc_client).await.unwrap();
        let clock2 = clock1.clone();
        let step1_result = raydium_cpmm.swap_base_in(sol_mint, amount_in_adjusted, clock1);

        if step1_result.is_err() {
            eprintln!("Step 1 swap failed: {:?}", step1_result.as_ref().err());
            eprintln!("Amount in: {}", amount_in_adjusted);
            eprintln!("Base vault amount: {}", base_vault_amount);
            eprintln!("Pool token 0 mint: {}", pool.token_0_mint);
            eprintln!("Pool token 1 mint: {}", pool.token_1_mint);
            eprintln!("Base token key: {}", base_token.key);
            eprintln!("Quote token key: {}", quote_token.key);
        }
        assert!(step1_result.is_ok(), "First swap should succeed");
        let quote_received = step1_result.unwrap();
        eprintln!(
            "Step 1: {} SOL -> {} TOKEN",
            amount_in_adjusted as f64 / 1_000_000_000.0,
            quote_received as f64 / 1_000_000.0
        );

        // Step 2: Swap quote -> base (reverse swap)
        let other_mint = if *quote_token.key != sol_mint {
            *quote_token.key
        } else {
            *base_token.key
        };
        let step2_result = raydium_cpmm.swap_base_in(other_mint, quote_received, clock2);
        let base_received = step2_result.unwrap();
        eprintln!(
            "Step 2: {} TOKEN -> {} SOL",
            quote_received as f64 / 1_000_000.0,
            base_received as f64 / 1_000_000_000.0
        );
    }
}
