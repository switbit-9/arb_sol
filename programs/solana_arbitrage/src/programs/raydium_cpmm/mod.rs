// Declare submodules first (these are accessed via super:: from child modules)
pub mod curve;
pub mod error;
pub mod states;
pub mod utils;

// Now import using relative paths from declared modules
use self::curve::calculator::CurveCalculator;
use self::error::ErrorCode;
use self::states::PoolState;
use crate::utils::{
    token::{apply_transfer_fee, apply_transfer_inverse_fee, MintFee},
    utils::read_vault_data,
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

// pub const PROGRAM_ID: Pubkey =
    // Pubkey::from_str_const("CPMDWBwJDtYax9qW7AyRuVC19Cc4L4Vcy4n2BHAbHkCW"); //TO DO: be changed for mainnet
pub const PROGRAM_ID: Pubkey = Pubkey::from_str_const("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");
const SWAP_BASE_IN_DISC: [u8; 8] = [143, 190, 90, 218, 196, 30, 51, 222];
const SWAP_BASE_OUT_DISC: [u8; 8] = [55, 217, 98, 86, 163, 74, 180, 173];
// Static accounts (from static_base, 2 accounts)
pub const S_PROGRAM_ID: usize = 0;
pub const S_VAULT_AUTHORITY: usize = 1;

// Dynamic accounts (from dyn_start, 5 accounts)
pub const D_POOL: usize = 0;
pub const D_BASE_VAULT: usize = 1;
pub const D_QUOTE_VAULT: usize = 2;
pub const D_AMM_CONFIG: usize = 3;
pub const D_OBSERVATION: usize = 4;

pub const MIN_ACCOUNTS: usize = 5;

fn get_price_f64(
    base_vault_amount: u64,
    quote_vault_amount: u64,
    fees_token_0: u64,
    fees_token_1: u64,
) -> Result<f64> {
    let token_0_amount = base_vault_amount
        .checked_sub(fees_token_0)
        .ok_or(ProgramError::InvalidArgument)?;
    let token_1_amount = quote_vault_amount
        .checked_sub(fees_token_1)
        .ok_or(ProgramError::InvalidArgument)?;

    if token_0_amount == 0 || token_1_amount == 0 {
        return Err(ProgramError::InvalidArgument.into());
    }

    Ok(token_1_amount as f64 / token_0_amount as f64)
}

#[derive(Clone)]
pub struct RaydiumCPMM {
    pub pool_id: Pubkey,
    pub base_token_pk: Pubkey,
    pub quote_token_pk: Pubkey,
    pub base_vault_key: Pubkey,
    pub quote_vault_key: Pubkey,
    pub base_vault_amount: u64,
    pub quote_vault_amount: u64,
    pub price: f64,

    pub static_base: usize,
    pub dyn_start: usize,
    pub creator_fee_rate: u64,
    pub trade_fee_rate: u64,
    pub protocol_fee_rate: u64,
    pub fund_fee_rate: u64,
    pub total_fee_numerator: u64,
    pub buy_max_in: u64,
    pub buy_max_out: u64,
    pub sell_max_in: u64,
    pub sell_max_out: u64,
    pub prepared: bool,
    // Pre-computed from PoolState (avoids storing the full 680-byte struct)
    pub fees_token_0: u64,              // protocol + fund + creator fees for token_0
    pub fees_token_1: u64,              // protocol + fund + creator fees for token_1
    pub adjusted_creator_fee_rate: u64, // 0 if !enable_creator_fee
    pub buy_creator_fee_on_input: bool, // is_creator_fee_on_input for ZeroForOne
    pub sell_creator_fee_on_input: bool, // is_creator_fee_on_input for OneForZero
    pub base_is_token_0: bool,          // whether base_vault_key == pool.token_0_vault
}

impl ProgramMeta for RaydiumCPMM {
    fn get_id(&self) -> &Pubkey {
        &PROGRAM_ID
    }

    fn get_pool_id(&self) -> &Pubkey {
        &self.pool_id
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        let inverse = if self.price > 0.0 { 1.0 / self.price } else { 0.0 };
        Ok((self.price, inverse))
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (&self.base_token_pk, &self.quote_token_pk)
    }

    fn name(&self) -> &'static str { "RaydiumCPMM" }

    fn get_fee_factor(&self) -> Result<(f64, f64)> { let f = 1.0 - self.total_fee_numerator as f64 / 1_000_000.0; Ok((f, f)) }

    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        Ok((self.base_vault_amount, self.quote_vault_amount))
    }

    fn get_max_amount_in<'a>(&self, _accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_in) } else { Ok(self.sell_max_in) }
    }

    fn get_max_amount_out<'a>(&self, _accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_out) } else { Ok(self.sell_max_out) }
    }

    fn get_cached_max_amounts(&self, input_mint: Pubkey) -> (u64, u64) {
        if input_mint == self.base_token_pk { (self.buy_max_in, self.buy_max_out) } else { (self.sell_max_in, self.sell_max_out) }
    }

    fn has_output_liquidity(&self, input_mint: Pubkey) -> bool {
        if input_mint == self.base_token_pk {
            self.quote_vault_amount > 0
        } else {
            self.base_vault_amount > 0
        }
    }

    fn fast_quote(&mut self, input_mint: Pubkey, amount_in: u64, _profit_pct: f64) -> Result<(u64, u64)> {
        let (max_in, max_out) = self.get_cached_max_amounts(input_mint);
        let amount_in = amount_in.min(max_in);
        debug_eprintln!("[RAYD CPMM] Fast quote: {:.9} SOL ({}) -> {:.6} tokens ({})", amount_in as f64 / 1_000_000_000.0, amount_in, max_out as f64 / 1_000_000.0, max_out);
        let (input_vault_amount, output_vault_amount) = if input_mint == self.base_token_pk {
            (self.base_vault_amount, self.quote_vault_amount)
        } else {
            (self.quote_vault_amount, self.base_vault_amount)
        };

        let (total_input_token_amount, total_output_token_amount, is_creator_fee_on_input) =
            self.get_swap_amounts(input_mint, input_vault_amount, output_vault_amount);

        let result = CurveCalculator::swap_base_input(
            u128::from(amount_in),
            u128::from(total_input_token_amount),
            u128::from(total_output_token_amount),
            self.trade_fee_rate,
            self.adjusted_creator_fee_rate,
            self.protocol_fee_rate,
            self.fund_fee_rate,
            is_creator_fee_on_input,
        )
        .ok_or(ErrorCode::ZeroTradingTokens)?;

        let out = u64::try_from(result.output_amount).unwrap_or(u64::MAX);
        Ok((amount_in, out.min(max_out)))
    }

    fn swap_base_in<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        _clock: &Clock,
    ) -> Result<u64> {
        let (input_vault_amount, output_vault_amount) = if input_mint == self.base_token_pk {
            (self.base_vault_amount, self.quote_vault_amount)
        } else {
            (self.quote_vault_amount, self.base_vault_amount)
        };

        let transfer_fee = apply_transfer_fee(amount_in, input_transfer_fee);
        let actual_amount_in = amount_in.saturating_sub(transfer_fee);

        let (total_input_token_amount, total_output_token_amount, is_creator_fee_on_input) =
            self.get_swap_amounts(input_mint, input_vault_amount, output_vault_amount);

        let result = CurveCalculator::swap_base_input(
            u128::from(actual_amount_in),
            u128::from(total_input_token_amount),
            u128::from(total_output_token_amount),
            self.trade_fee_rate,
            self.adjusted_creator_fee_rate,
            self.protocol_fee_rate,
            self.fund_fee_rate,
            is_creator_fee_on_input,
        )
        .ok_or(ErrorCode::ZeroTradingTokens)?;

        let amount_out = u64::try_from(result.output_amount).unwrap();
        let transfer_fee_out = apply_transfer_fee(amount_out, output_transfer_fee);
        let amount_out = amount_out
            .checked_sub(transfer_fee_out)
            .ok_or(ErrorCode::MathOverflow)?;

        Ok(amount_out)
    }

    fn swap_base_out<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        _clock: &Clock,
    ) -> Result<u64> {
        // When output_mint != base, input is base (and vice versa)
        let input_mint = if output_mint != self.base_token_pk {
            self.base_token_pk
        } else {
            self.quote_token_pk
        };
        let (input_vault_amount, output_vault_amount) = if input_mint == self.base_token_pk {
            (self.base_vault_amount, self.quote_vault_amount)
        } else {
            (self.quote_vault_amount, self.base_vault_amount)
        };

        let out_fee = apply_transfer_inverse_fee(amount_out, output_transfer_fee);
        let amount_out_with_transfer_fee = amount_out
            .checked_add(out_fee)
            .ok_or(ErrorCode::MathOverflow)?;

        let (total_input_token_amount, total_output_token_amount, is_creator_fee_on_input) =
            self.get_swap_amounts(input_mint, input_vault_amount, output_vault_amount);

        let result = CurveCalculator::swap_base_output(
            u128::from(amount_out_with_transfer_fee),
            u128::from(total_input_token_amount),
            u128::from(total_output_token_amount),
            self.trade_fee_rate,
            self.adjusted_creator_fee_rate,
            self.protocol_fee_rate,
            self.fund_fee_rate,
            is_creator_fee_on_input,
        )
        .ok_or(ErrorCode::ZeroTradingTokens)?;

        let source_amount_swapped = u64::try_from(result.input_amount).unwrap();
        let in_fee =
            apply_transfer_inverse_fee(source_amount_swapped, input_transfer_fee);
        let input_transfer_amount = source_amount_swapped
            .checked_add(in_fee)
            .ok_or(ErrorCode::MathOverflow)?;

        Ok(input_transfer_amount)
    }

    fn invoke_swap_base_in<'a>(
        &mut self,
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
        self.invoke_swap(
            accounts, input_mint, &SWAP_BASE_IN_DISC,
            amount_in, min_amount_out.unwrap_or(1),
            payer, user_mint_1_token_account, user_mint_2_token_account,
            mint_1_account, mint_2_account, mint_1_token_program, mint_2_token_program,
        )
    }

    fn invoke_swap_base_out<'a>(
        &mut self,
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
        self.invoke_swap(
            accounts, input_mint, &SWAP_BASE_OUT_DISC,
            max_amount_in, amount_out.unwrap_or(1),
            payer, user_mint_1_token_account, user_mint_2_token_account,
            mint_1_account, mint_2_account, mint_1_token_program, mint_2_token_program,
        )
    }

    #[cfg(any(test, feature = "debug"))]
    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        msg!("=== Raydium CPMM ===");
        msg!("[static] S0 program_id: {}", accounts[self.static_base + S_PROGRAM_ID].key);
        msg!("[static] S1 vault_authority: {}", accounts[self.static_base + S_VAULT_AUTHORITY].key);
        msg!("[dyn]    D0 pool: {}", accounts[self.dyn_start + D_POOL].key);
        msg!("[dyn]    D1 base_vault: {}", accounts[self.dyn_start + D_BASE_VAULT].key);
        msg!("[dyn]    D2 quote_vault: {}", accounts[self.dyn_start + D_QUOTE_VAULT].key);
        msg!("[dyn]    D3 amm_config: {}", accounts[self.dyn_start + D_AMM_CONFIG].key);
        msg!("[dyn]    D4 observation: {}", accounts[self.dyn_start + D_OBSERVATION].key);
        Ok(())
    }
}

impl RaydiumCPMM {
    /// Inline swap params: subtracts accumulated fees from vault amounts and
    /// determines creator-fee-on-input flag. Replaces the old PoolState::get_swap_params()
    /// + adjust_creator_fee_rate() calls — avoids the unused token_price_x32 computation.
    #[inline(always)]
    fn get_swap_amounts(&self, input_mint: Pubkey, input_vault_amount: u64, output_vault_amount: u64) -> (u64, u64, bool) {
        let is_zero_for_one = (input_mint == self.base_token_pk) == self.base_is_token_0;
        if is_zero_for_one {
            (
                input_vault_amount.saturating_sub(self.fees_token_0),
                output_vault_amount.saturating_sub(self.fees_token_1),
                self.buy_creator_fee_on_input,
            )
        } else {
            (
                input_vault_amount.saturating_sub(self.fees_token_1),
                output_vault_amount.saturating_sub(self.fees_token_0),
                self.sell_creator_fee_on_input,
            )
        }
    }

    /// Unified CPI invoke for both swap_base_in and swap_base_out.
    fn invoke_swap<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        disc: &[u8; 8],
        arg1: u64,
        arg2: u64,
        payer: AccountInfo<'a>,
        user_mint_1_token_account: AccountInfo<'a>,
        user_mint_2_token_account: AccountInfo<'a>,
        mint_1_account: AccountInfo<'a>,
        mint_2_account: AccountInfo<'a>,
        mint_1_token_program: AccountInfo<'a>,
        mint_2_token_program: AccountInfo<'a>,
    ) -> Result<()> {
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let base_vault = &accounts[self.dyn_start + D_BASE_VAULT];
        let quote_vault = &accounts[self.dyn_start + D_QUOTE_VAULT];
        let authority_account = &accounts[self.static_base + S_VAULT_AUTHORITY];
        let amm_config_account = &accounts[self.dyn_start + D_AMM_CONFIG];
        let observation_account = &accounts[self.dyn_start + D_OBSERVATION];

        let (input_vault, output_vault) = if input_mint == self.base_token_pk {
            (base_vault, quote_vault)
        } else {
            (quote_vault, base_vault)
        };

        let (input_token_program, output_token_program, user_input_token_account, user_output_token_account, input_mint_acc, output_mint_acc) = if input_mint == *mint_1_account.key {
            (mint_1_token_program, mint_2_token_program, user_mint_1_token_account, user_mint_2_token_account, mint_1_account, mint_2_account)
        } else {
            (mint_2_token_program, mint_1_token_program, user_mint_2_token_account, user_mint_1_token_account, mint_2_account, mint_1_account)
        };

        let mut data = Vec::with_capacity(24);
        data.extend_from_slice(disc);
        data.extend_from_slice(&arg1.to_le_bytes());
        data.extend_from_slice(&arg2.to_le_bytes());

        let swap_ix = Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(*payer.key, true),
                AccountMeta::new_readonly(*authority_account.key, false),
                AccountMeta::new_readonly(*amm_config_account.key, false),
                AccountMeta::new(*pool_id.key, false),
                AccountMeta::new(*user_input_token_account.key, false),
                AccountMeta::new(*user_output_token_account.key, false),
                AccountMeta::new(*input_vault.key, false),
                AccountMeta::new(*output_vault.key, false),
                AccountMeta::new_readonly(*input_token_program.key, false),
                AccountMeta::new_readonly(*output_token_program.key, false),
                AccountMeta::new_readonly(*input_mint_acc.key, false),
                AccountMeta::new_readonly(*output_mint_acc.key, false),
                AccountMeta::new(*observation_account.key, false),
            ],
            data,
        };

        let accounts_arr = [
            payer,
            authority_account.clone(),
            amm_config_account.clone(),
            pool_id.clone(),
            user_input_token_account,
            user_output_token_account,
            input_vault.clone(),
            output_vault.clone(),
            input_token_program,
            output_token_program,
            input_mint_acc,
            output_mint_acc,
            observation_account.clone(),
        ];

        invoke(&swap_ix, &accounts_arr)?;
        Ok(())
    }

    pub fn new<'a>(
        accounts: &[AccountInfo<'a>],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
        pool_fees: &[u32],
    ) -> Result<Self> {
        let pool_acc = &accounts[dyn_start + D_POOL];
        let base_vault = &accounts[dyn_start + D_BASE_VAULT];
        let quote_vault = &accounts[dyn_start + D_QUOTE_VAULT];

        // Parse vault data (mint + amount) from vault accounts
        let (base_token_pk, base_vault_amount) = read_vault_data(base_vault)?;
        let (quote_token_pk, quote_vault_amount) = read_vault_data(quote_vault)?;

        #[cfg(test)]
        let base_vault_amount = (base_vault_amount as f64 * 0.95) as u64;

        // trade_fee_rate and creator_fee_rate from client-side pool_fees (millionths)
        //trade_fee_rate + creator_fee_rate (if self.enable_creator_fee)
        let trade_fee_rate = pool_fees[0] as u64;
        //creator (if self.enable_creator_fee)
        let creator_fee_rate = pool_fees[1] as u64;
        // protocol_fees_token_0 + fund_fees_token_0 + creator_fees_token_0
        let fees_token_0 = pool_fees[1] as u64;
        // protocol_fees_token_1 + fund_fees_token_1 +1creator_fees_token_0
        let fees_token_1 = pool_fees[2] as u64;

        let total_fee_numerator = trade_fee_rate + creator_fee_rate;

        let price = get_price_f64(base_vault_amount, quote_vault_amount, fees_token_0, fees_token_1)?;


        // Defer pool parsing and max amounts to prepare_for_execution()
        let instance = RaydiumCPMM {
            pool_id: *pool_acc.key,
            base_token_pk,
            quote_token_pk,
            base_vault_key: *base_vault.key,
            quote_vault_key: *quote_vault.key,
            base_vault_amount,
            quote_vault_amount,
            price,
            static_base,
            dyn_start,
            creator_fee_rate,
            trade_fee_rate,
            protocol_fee_rate: 0,
            fund_fee_rate: 0,
            total_fee_numerator,
            buy_max_in: 0,
            buy_max_out: 0,
            sell_max_in: 0,
            sell_max_out: 0,
            prepared: false,
            // Populated in prepare_for_execution via zero-copy read
            fees_token_0: 0,
            fees_token_1: 0,
            adjusted_creator_fee_rate: 0,
            buy_creator_fee_on_input: true,
            sell_creator_fee_on_input: true,
            base_is_token_0: true,
        };
        // instance.log_accounts(accounts)?;
        Ok(instance)
    }

    fn compute_cached_max(base_vault: u64, quote_vault: u64, fee_num: u64, fee_den: u64) -> (u64, u64, u64, u64) {
        fn cp_max(x: u64, y: u64, fee_num: u64, fee_den: u64) -> (u64, u64) {
            let ff = fee_den - fee_num; // fee_factor * fee_den
            if y == 0 || ff == 0 {
                return (0, y);
            }
            // dx = x * 99 * fee_den / ff  (target = y * 99/100, so y/denom - 1 = 99)
            let dx = (x as u128)
                .saturating_mul(99)
                .saturating_mul(fee_den as u128)
                / (ff as u128);
            (dx.min(u64::MAX as u128) as u64, y)
        }
        let (buy_in, buy_out) = cp_max(base_vault, quote_vault, fee_num, fee_den);
        let (sell_in, sell_out) = cp_max(quote_vault, base_vault, fee_num, fee_den);
        (buy_in, buy_out, sell_in, sell_out)
    }

    /// Compute deferred fields: max amounts, transfer fee rates.
    /// Called only for instances that participate in a profitable arb path.
    pub fn prepare_for_execution<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
    ) {
        if self.prepared {
            return;
        }
        self.prepared = true;

        // Zero-copy read: borrow account data, extract only the fields we need,
        // then drop the borrow. No 680-byte PoolState copy.
        let pool_acc = &accounts[self.dyn_start + D_POOL];
        if let Ok(pool_data) = pool_acc.try_borrow_data() {
            let pool_size = std::mem::size_of::<PoolState>();
            let pool: &PoolState = bytemuck::from_bytes(&pool_data[8..8 + pool_size]);

            self.base_is_token_0 = self.base_vault_key == pool.token_0_vault;

            self.fees_token_0 = pool.protocol_fees_token_0
                .saturating_add(pool.fund_fees_token_0)
                .saturating_add(pool.creator_fees_token_0);
            self.fees_token_1 = pool.protocol_fees_token_1
                .saturating_add(pool.fund_fees_token_1)
                .saturating_add(pool.creator_fees_token_1);

            self.adjusted_creator_fee_rate = if pool.enable_creator_fee {
                self.creator_fee_rate
            } else {
                0
            };

            // ZeroForOne: input is token_0
            self.buy_creator_fee_on_input = matches!(pool.creator_fee_on, 0 | 1);
            // OneForZero: input is token_1
            self.sell_creator_fee_on_input = matches!(pool.creator_fee_on, 0 | 2);
        }

        let (buy_in, buy_out, sell_in, sell_out) =
            Self::compute_cached_max(self.base_vault_amount, self.quote_vault_amount, self.total_fee_numerator, 1_000_000);
        self.buy_max_in = buy_in;
        self.buy_max_out = buy_out;
        self.sell_max_in = sell_in;
        self.sell_max_out = sell_out;
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use anchor_lang::prelude::Clock;
//     use anchor_lang::solana_program::{account_info::AccountInfo, pubkey::Pubkey, system_program};
//     use solana_client::nonblocking::rpc_client::RpcClient;
//     use solana_sdk::pubkey::Pubkey as SdkPubkey;

//     // Helper function to create a mock AccountInfo with provided data
//     fn create_mock_account_info_with_data(
//         key: Pubkey,
//         owner: Pubkey,
//         data: Option<Vec<u8>>,
//     ) -> AccountInfo<'static> {
//         let data_vec = data.unwrap_or_else(|| vec![0u8; 8]);
//         let data_vec = Box::leak(Box::new(data_vec));
//         let lamports = Box::leak(Box::new(0u64));
//         let owner_static = Box::leak(Box::new(owner));
//         let key_static = Box::leak(Box::new(key));

//         AccountInfo::new(
//             key_static,
//             false,
//             true,
//             lamports,
//             data_vec,
//             owner_static,
//             false,
//             0,
//         )
//     }

//     // Helper to convert solana_sdk::account::Account to AccountInfo
//     fn account_to_account_info(
//         key: Pubkey,
//         account: solana_sdk::account::Account,
//     ) -> AccountInfo<'static> {
//         let data = Box::leak(Box::new(account.data));
//         let lamports = Box::leak(Box::new(account.lamports));
//         let owner_bytes: [u8; 32] = account.owner.to_bytes();
//         let owner = Pubkey::try_from(owner_bytes.as_ref()).unwrap();
//         let owner_static = Box::leak(Box::new(owner));
//         let key_static = Box::leak(Box::new(key));
//         AccountInfo::new(
//             key_static,
//             false, // is_signer
//             false, // is_writable
//             lamports,
//             data,
//             owner_static,
//             account.executable,
//             account.rent_epoch,
//         )
//     }

//     // Helper function to fetch account from RPC and convert to AccountInfo
//     async fn fetch_account_info_from_rpc(
//         rpc_client: &RpcClient,
//         key: Pubkey,
//     ) -> AccountInfo<'static> {
//         let sdk_pubkey = SdkPubkey::try_from(key.to_bytes().as_ref())
//             .expect("Failed to convert Pubkey to SdkPubkey");
//         let account = rpc_client
//             .get_account(&sdk_pubkey)
//             .await
//             .expect(&format!("Failed to fetch account {}", key));
//         account_to_account_info(key, account)
//     }

//     /// Get on chain clock from RPC
//     async fn get_clock(rpc_client: &RpcClient) -> anyhow::Result<Clock> {
//         use anchor_client::solana_sdk::sysvar;

//         let clock_account = rpc_client.get_account(&sysvar::clock::ID).await?;

//         // Clock from Solana is borsh-serialized with these fields in order:
//         // slot: u64 (8 bytes)
//         // epoch_start_timestamp: i64 (8 bytes)
//         // epoch: u64 (8 bytes)
//         // leader_schedule_epoch: u64 (8 bytes)
//         // unix_timestamp: i64 (8 bytes)
//         // Total: 40 bytes
//         if clock_account.data.len() < 40 {
//             return Err(anyhow::anyhow!(
//                 "Clock account data too short: {} bytes",
//                 clock_account.data.len()
//             ));
//         }

//         let data = &clock_account.data;
//         let slot = u64::from_le_bytes(
//             data[0..8]
//                 .try_into()
//                 .map_err(|_| anyhow::anyhow!("Failed to parse slot"))?,
//         );
//         let epoch_start_timestamp = i64::from_le_bytes(
//             data[8..16]
//                 .try_into()
//                 .map_err(|_| anyhow::anyhow!("Failed to parse epoch_start_timestamp"))?,
//         );
//         let epoch = u64::from_le_bytes(
//             data[16..24]
//                 .try_into()
//                 .map_err(|_| anyhow::anyhow!("Failed to parse epoch"))?,
//         );
//         let leader_schedule_epoch = u64::from_le_bytes(
//             data[24..32]
//                 .try_into()
//                 .map_err(|_| anyhow::anyhow!("Failed to parse leader_schedule_epoch"))?,
//         );
//         let unix_timestamp = i64::from_le_bytes(
//             data[32..40]
//                 .try_into()
//                 .map_err(|_| anyhow::anyhow!("Failed to parse unix_timestamp"))?,
//         );

//         Ok(Clock {
//             slot,
//             epoch_start_timestamp,
//             epoch,
//             leader_schedule_epoch,
//             unix_timestamp,
//         })
//     }

//     #[tokio::test]
//     async fn test_raydium_cpmm_fetch_pool_info() {
//         use anchor_client::Cluster;

//         // RPC client pointing to mainnet
//         let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

//         // Pool ID from mainnet
//         let pool_id_key = Pubkey::from_str_const("21WT1Hs2DpANaGQJncBXV8GHqE1jr7RQNmUKPXCYhrZE");

//         // Fetch pool account
//         let pool_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool_id_key.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();

//         // Parse pool state (skip first 8 bytes which is Anchor discriminator)
//         // PoolState::LEN includes the 8-byte discriminator, so the struct size is LEN - 8
//         let pool_state_size = PoolState::LEN - 8;
//         if pool_account.data.len() < 8 + pool_state_size {
//             panic!(
//                 "Pool account data too short: {} bytes, expected at least {} bytes",
//                 pool_account.data.len(),
//                 8 + pool_state_size
//             );
//         }
//         let pool: PoolState =
//             bytemuck::pod_read_unaligned(&pool_account.data[8..8 + pool_state_size]);

//         // Fetch vault accounts to get amounts
//         let (vault_0_account_opt, vault_1_account_opt, token_0_amount, token_1_amount) = match (
//             rpc_client
//                 .get_account(&SdkPubkey::try_from(pool.token_0_vault.to_bytes().as_ref()).unwrap())
//                 .await,
//             rpc_client
//                 .get_account(&SdkPubkey::try_from(pool.token_1_vault.to_bytes().as_ref()).unwrap())
//                 .await,
//         ) {
//             (Ok(v0), Ok(v1)) => {
//                 // Parse token account amounts (offset 64 for amount in SPL token account)
//                 let t0_amount = if v0.data.len() >= 72 {
//                     u64::from_le_bytes(v0.data[64..72].try_into().unwrap())
//                 } else {
//                     0
//                 };
//                 let t1_amount = if v1.data.len() >= 72 {
//                     u64::from_le_bytes(v1.data[64..72].try_into().unwrap())
//                 } else {
//                     0
//                 };
//                 (Some(v0), Some(v1), t0_amount, t1_amount)
//             }
//             (Err(e0), _) => {
//                 eprintln!(
//                     "Warning: Could not fetch token 0 vault {}: {:?}",
//                     pool.token_0_vault, e0
//                 );
//                 (None, None, 0, 0)
//             }
//             (_, Err(e1)) => {
//                 eprintln!(
//                     "Warning: Could not fetch token 1 vault {}: {:?}",
//                     pool.token_1_vault, e1
//                 );
//                 (None, None, 0, 0)
//             }
//         };

//         // Fetch AMM config
//         let amm_config_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool.amm_config.to_bytes().as_ref()).unwrap())
//             .await;

//         if let Ok(amm_config_account) = amm_config_account {
//             let amm_config: AmmConfig = AmmConfig::try_from_bytes(&amm_config_account.data)
//                 .unwrap_or_else(|_| {
//                     eprintln!("Warning: Failed to deserialize AMM config, using default");
//                     AmmConfig::default()
//                 });

//             eprintln!("\n=== AMM Config ===");
//             eprintln!("Trade Fee Rate: {}", amm_config.trade_fee_rate);
//             eprintln!("Protocol Fee Rate: {}", amm_config.protocol_fee_rate);
//             eprintln!("Fund Fee Rate: {}", amm_config.fund_fee_rate);
//             eprintln!("Creator Fee Rate: {}", amm_config.creator_fee_rate);
//         } else {
//             eprintln!("\nWarning: Could not fetch AMM config account");
//         }

//         // Determine which vault is base and which is quote
//         eprintln!("\n=== Token Information ===");
//         eprintln!("Base Token (Token 0): {}", pool.token_0_mint);
//         eprintln!("Quote Token (Token 1): {}", pool.token_1_mint);

//         // Verify we got valid data
//         assert_ne!(
//             pool.token_0_mint,
//             Pubkey::default(),
//             "Token 0 mint should be set"
//         );
//         assert_ne!(
//             pool.token_1_mint,
//             Pubkey::default(),
//             "Token 1 mint should be set"
//         );
//         assert_ne!(
//             pool.token_0_vault,
//             Pubkey::default(),
//             "Token 0 vault should be set"
//         );
//         assert_ne!(
//             pool.token_1_vault,
//             Pubkey::default(),
//             "Token 1 vault should be set"
//         );

//         // Note: Vault balances might be zero if pool is closed or accounts don't exist
//         if token_0_amount > 0 && token_1_amount > 0 {
//             eprintln!("✓ Pool has active liquidity");
//         } else {
//             eprintln!("⚠ Pool vaults may be empty or accounts not found (pool might be closed)");
//         }
//     }

//     #[tokio::test]
//     async fn test_raydium_cpmm_swap_base_in() {
//         use anchor_client::Cluster;

//         // RPC client pointing to mainnet
//         let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

//         // Pool ID from mainnet
//         let pool_id_key = Pubkey::from_str_const("21WT1Hs2DpANaGQJncBXV8GHqE1jr7RQNmUKPXCYhrZE");

//         eprintln!("Testing swap_base_in for pool: {}", pool_id_key);

//         // Fetch pool account
//         let pool_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool_id_key.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();

//         // Parse pool state
//         // Parse pool state (skip first 8 bytes which is Anchor discriminator)
//         let pool_state_size = PoolState::LEN - 8;
//         if pool_account.data.len() < 8 + pool_state_size {
//             panic!(
//                 "Pool account data too short: {} bytes, expected at least {} bytes",
//                 pool_account.data.len(),
//                 8 + pool_state_size
//             );
//         }
//         let pool: PoolState =
//             bytemuck::pod_read_unaligned(&pool_account.data[8..8 + pool_state_size]);

//         // Fetch vault accounts
//         let vault_0_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool.token_0_vault.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();
//         let vault_1_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool.token_1_vault.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();

//         // Fetch mint accounts
//         let mint_0_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool.token_0_mint.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();
//         let mint_1_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool.token_1_mint.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();

//         // Fetch AMM config
//         let amm_config_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool.amm_config.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();

//         // Get clock
//         let clock = get_clock(&rpc_client).await.unwrap();

//         // Extract vault amounts before converting to AccountInfo (they get moved)
//         let base_vault_amount = if vault_0_account.data.len() >= 72 {
//             u64::from_le_bytes(vault_0_account.data[64..72].try_into().unwrap())
//         } else {
//             0
//         };
//         let quote_vault_amount = if vault_1_account.data.len() >= 72 {
//             u64::from_le_bytes(vault_1_account.data[64..72].try_into().unwrap())
//         } else {
//             0
//         };

//         // Convert accounts to AccountInfo
//         let pool_id_account_info = account_to_account_info(pool_id_key, pool_account);
//         let base_vault = account_to_account_info(pool.token_0_vault, vault_0_account);
//         let quote_vault = account_to_account_info(pool.token_1_vault, vault_1_account);
//         let base_token = account_to_account_info(pool.token_0_mint, mint_0_account);
//         let quote_token = account_to_account_info(pool.token_1_mint, mint_1_account);

//         // Create program_id account
//         let program_id_key = RaydiumCPMM::PROGRAM_ID;
//         let program_id_account =
//             create_mock_account_info_with_data(program_id_key, system_program::id(), None);

//         // Create vault_authority mock
//         let vault_authority_account =
//             create_mock_account_info_with_data(Pubkey::new_unique(), system_program::id(), None);

//         // Create accounts array: [statics] [dynamics]
//         // Statics: S0=program_id, S1=vault_authority
//         // Dynamics: D0=pool, D1=base_vault, D2=quote_vault, D3=amm_config
//         let static_base = 0;
//         let dyn_start = 2;
//         let accounts = vec![
//             program_id_account,                                           // S0: program_id
//             vault_authority_account,                                      // S1: vault_authority
//             pool_id_account_info.clone(),                                 // D0: pool
//             base_vault.clone(),                                           // D1: base_vault
//             quote_vault.clone(),                                          // D2: quote_vault
//             account_to_account_info(pool.amm_config, amm_config_account), // D3: amm_config
//         ];
//         let dyn_end = accounts.len();

//         // Create RaydiumCPMM instance
//         let mut raydium_cpmm =
//             RaydiumCPMM::new(&accounts, static_base, dyn_start, dyn_end, &[], &[0, 0]).expect("Failed to create RaydiumCPMM");

//         // Test swap_base_in with a small amount
//         // Use 1% of the smaller vault balance to avoid large price impact

//         eprintln!("Base vault amount: {}", base_vault_amount);
//         eprintln!("Quote vault amount: {}", quote_vault_amount);

//         // Use 0.1% of base vault as input (swap base in = input base token, get quote token out)
//         let amount_in = base_vault_amount / 1000;

//         // Adjust based on decimals - if decimals are high, we might need larger amounts
//         let amount_in_adjusted = if pool.mint_0_decimals >= 9 {
//             amount_in.max(1_000_000) // At least 0.001 tokens for 9 decimals
//         } else {
//             amount_in.max(1000) // At least 1000 base units
//         };

//         eprintln!(
//             "Testing swap_base_in with amount_in: {}",
//             amount_in_adjusted
//         );

//         let input_mint = *base_token.key; // Swap base token in
//         let result = raydium_cpmm.swap_base_in(&accounts, input_mint, amount_in_adjusted, MintFee::ZERO, MintFee::ZERO, &clock);

//         match result {
//             Ok(amount_out) => {
//                 eprintln!("✓ swap_base_in succeeded!");
//                 eprintln!("  Input: {} base tokens", amount_in_adjusted);
//                 eprintln!("  Output: {} quote tokens", amount_out);
//                 assert!(amount_out > 0, "Output amount should be greater than 0");

//                 // Verify output is reasonable (should be proportional to reserves)
//                 let expected_ratio = (quote_vault_amount as f64) / (base_vault_amount as f64);
//                 let actual_ratio = (amount_out as f64) / (amount_in_adjusted as f64);
//                 eprintln!("  Expected price ratio: {:.6}", expected_ratio);
//                 eprintln!("  Actual price ratio: {:.6}", actual_ratio);
//             }
//             Err(e) => {
//                 eprintln!("✗ swap_base_in failed: {:?}", e);
//                 panic!("swap_base_in should succeed with valid pool data");
//             }
//         }
//     }

//     #[tokio::test]
//     async fn test_raydium_cpmm_swap_base_out() {
//         use anchor_client::Cluster;

//         // RPC client pointing to mainnet
//         let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

//         // Pool ID from mainnet
//         let pool_id_key = Pubkey::from_str_const("21WT1Hs2DpANaGQJncBXV8GHqE1jr7RQNmUKPXCYhrZE");

//         eprintln!("Testing swap_base_out for pool: {}", pool_id_key);

//         // Fetch pool account
//         let pool_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool_id_key.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();

//         // Parse pool state
//         // Parse pool state (skip first 8 bytes which is Anchor discriminator)
//         let pool_state_size = PoolState::LEN - 8;
//         if pool_account.data.len() < 8 + pool_state_size {
//             panic!(
//                 "Pool account data too short: {} bytes, expected at least {} bytes",
//                 pool_account.data.len(),
//                 8 + pool_state_size
//             );
//         }
//         let pool: PoolState =
//             bytemuck::pod_read_unaligned(&pool_account.data[8..8 + pool_state_size]);

//         // Fetch vault accounts
//         let vault_0_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool.token_0_vault.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();
//         let vault_1_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool.token_1_vault.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();

//         // Fetch mint accounts
//         let mint_0_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool.token_0_mint.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();
//         let mint_1_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool.token_1_mint.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();

//         // Fetch AMM config
//         let amm_config_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool.amm_config.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();

//         // Get clock
//         let clock = get_clock(&rpc_client).await.unwrap();

//         // Extract vault amounts before converting to AccountInfo (they get moved)
//         let base_vault_amount = if vault_0_account.data.len() >= 72 {
//             u64::from_le_bytes(vault_0_account.data[64..72].try_into().unwrap())
//         } else {
//             0
//         };
//         let quote_vault_amount = if vault_1_account.data.len() >= 72 {
//             u64::from_le_bytes(vault_1_account.data[64..72].try_into().unwrap())
//         } else {
//             0
//         };

//         // Convert accounts to AccountInfo
//         let pool_id_account_info = account_to_account_info(pool_id_key, pool_account);
//         let base_vault = account_to_account_info(pool.token_0_vault, vault_0_account);
//         let quote_vault = account_to_account_info(pool.token_1_vault, vault_1_account);
//         let base_token = account_to_account_info(pool.token_0_mint, mint_0_account);
//         let quote_token = account_to_account_info(pool.token_1_mint, mint_1_account);

//         // Create program_id account
//         let program_id_key = RaydiumCPMM::PROGRAM_ID;
//         let program_id_account =
//             create_mock_account_info_with_data(program_id_key, system_program::id(), None);

//         // Create vault_authority mock
//         let vault_authority_account =
//             create_mock_account_info_with_data(Pubkey::new_unique(), system_program::id(), None);

//         // Create accounts array: [statics] [dynamics]
//         let static_base = 0;
//         let dyn_start = 2;
//         let accounts = vec![
//             program_id_account,                                           // S0: program_id
//             vault_authority_account,                                      // S1: vault_authority
//             pool_id_account_info.clone(),                                 // D0: pool
//             base_vault.clone(),                                           // D1: base_vault
//             quote_vault.clone(),                                          // D2: quote_vault
//             account_to_account_info(pool.amm_config, amm_config_account), // D3: amm_config
//         ];
//         let dyn_end = accounts.len();

//         // Create RaydiumCPMM instance
//         let mut raydium_cpmm = RaydiumCPMM::new(&accounts, static_base, dyn_start, dyn_end, &[], &[0, 0]).expect("Failed to create RaydiumCPMM");

//         // Test swap_base_out with desired output amount

//         eprintln!("Base vault amount: {}", base_vault_amount);
//         eprintln!("Quote vault amount: {}", quote_vault_amount);

//         // For swap_base_out, we specify desired output amount
//         // Use 0.1% of quote vault as desired output (we want quote tokens out, so we input base tokens)
//         let amount_out_desired = quote_vault_amount / 1000;

//         // Adjust based on decimals
//         let amount_out_adjusted = if pool.mint_1_decimals >= 9 {
//             amount_out_desired.max(1_000_000) // At least 0.001 tokens for 9 decimals
//         } else {
//             amount_out_desired.max(1000) // At least 1000 base units
//         };

//         eprintln!(
//             "Testing swap_base_out with amount_out_desired: {}",
//             amount_out_adjusted
//         );

//         // swap_base_out takes the desired output amount and returns required input
//         // input_mint is the token we're putting in (base token) to get quote token out
//         let input_mint = *base_token.key;
//         let result = raydium_cpmm.swap_base_out(&accounts, input_mint, amount_out_adjusted, MintFee::ZERO, MintFee::ZERO, &clock);

//         match result {
//             Ok(amount_in_required) => {
//                 eprintln!("✓ swap_base_out succeeded!");
//                 eprintln!("  Desired Output: {} quote tokens", amount_out_adjusted);
//                 eprintln!("  Required Input: {} base tokens", amount_in_required);
//                 assert!(
//                     amount_in_required > 0,
//                     "Required input amount should be greater than 0"
//                 );

//                 // Verify the required input is reasonable
//                 let expected_ratio = (base_vault_amount as f64) / (quote_vault_amount as f64);
//                 let actual_ratio = (amount_in_required as f64) / (amount_out_adjusted as f64);
//                 eprintln!("  Expected price ratio: {:.6}", expected_ratio);
//                 eprintln!("  Actual price ratio: {:.6}", actual_ratio);
//             }
//             Err(e) => {
//                 eprintln!("✗ swap_base_out failed: {:?}", e);
//                 panic!("swap_base_out should succeed with valid pool data");
//             }
//         }
//     }
//     pub fn deserialize_anchor_account<T: AccountDeserialize>(
//         account: &solana_sdk::account::Account,
//     ) -> Result<T> {
//         let mut data: &[u8] = &account.data;
//         T::try_deserialize(&mut data).map_err(Into::into)
//     }
//     #[tokio::test]
//     async fn test_raydium_cpmm_round_trip_swap() {
//         use anchor_client::Cluster;

//         // RPC client pointing to mainnet
//         let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

//         // Pool ID from mainnet
//         let pool_id_key = Pubkey::from_str_const("C9U2Ksk6KKWvLEeo5yUQ7Xu46X7NzeBJtd9PBfuXaUSM");

//         // Fetch all necessary accounts (same as previous tests)
//         let pool_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool_id_key.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();

//         // Parse pool state (skip first 8 bytes which is Anchor discriminator)
//         let pool_state_size = PoolState::LEN - 8;
//         if pool_account.data.len() < 8 + pool_state_size {
//             panic!(
//                 "Pool account data too short: {} bytes, expected at least {} bytes",
//                 pool_account.data.len(),
//                 8 + pool_state_size
//             );
//         }
//         // PoolState is a ZeroCopy type, so use bytemuck instead of AccountDeserialize
//         let pool: PoolState =
//             bytemuck::pod_read_unaligned(&pool_account.data[8..8 + pool_state_size]);

//         let vault_0_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool.token_0_vault.to_bytes().as_ref()).unwrap())
//             .await;
//         let vault_1_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool.token_1_vault.to_bytes().as_ref()).unwrap())
//             .await;

//         eprintln!("pool: {:?}", pool_id_key);
//         eprintln!("base vault: {:?}", pool.token_0_vault);
//         eprintln!("quote: {:?}", pool.token_1_vault);
//         eprintln!("base mint: {:?}", pool.token_0_mint);
//         eprintln!("quote mint: {:?}", pool.token_1_mint);
//         eprintln!("amm config: {:?}", pool.amm_config);
//         eprintln!("lp mint: {:?}", pool.lp_mint);

//         if vault_0_account.is_err() || vault_1_account.is_err() {
//             eprintln!("Warning: Could not fetch vault accounts. Pool may be closed or accounts may not exist.");
//             eprintln!("Vault 0 fetch: {:?}", vault_0_account.as_ref().err());
//             eprintln!("Vault 1 fetch: {:?}", vault_1_account.as_ref().err());
//             return;
//         }

//         let vault_0_account = vault_0_account.unwrap();
//         let vault_1_account = vault_1_account.unwrap();

//         let mint_0_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool.token_0_mint.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();
//         let mint_1_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool.token_1_mint.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();

//         let amm_config_account = rpc_client
//             .get_account(&SdkPubkey::try_from(pool.amm_config.to_bytes().as_ref()).unwrap())
//             .await
//             .unwrap();

//         // Extract vault amounts before converting to AccountInfo (they get moved)
//         let base_vault_amount = if vault_0_account.data.len() >= 72 {
//             u64::from_le_bytes(vault_0_account.data[64..72].try_into().unwrap())
//         } else {
//             0
//         };

//         let pool_id_account_info = account_to_account_info(pool_id_key, pool_account);
//         let base_vault = account_to_account_info(pool.token_0_vault, vault_0_account);
//         let quote_vault = account_to_account_info(pool.token_1_vault, vault_1_account);
//         let base_token = account_to_account_info(pool.token_0_mint, mint_0_account);
//         let quote_token = account_to_account_info(pool.token_1_mint, mint_1_account);
//         let amm_config = account_to_account_info(pool.amm_config, amm_config_account);

//         let program_id_key = RaydiumCPMM::PROGRAM_ID;
//         let program_id_account =
//             create_mock_account_info_with_data(program_id_key, system_program::id(), None);

//         // Create vault_authority mock
//         let vault_authority_account =
//             create_mock_account_info_with_data(Pubkey::new_unique(), system_program::id(), None);

//         // Create accounts array: [statics] [dynamics]
//         let static_base = 0;
//         let dyn_start = 2;
//         let accounts = vec![
//             program_id_account,                // S0: program_id
//             vault_authority_account,            // S1: vault_authority
//             pool_id_account_info.clone(),       // D0: pool
//             base_vault.clone(),                 // D1: base_vault
//             quote_vault.clone(),                // D2: quote_vault
//             amm_config.clone(),                 // D3: amm_config
//         ];
//         let dyn_end = accounts.len();

//         let mut raydium_cpmm =
//             RaydiumCPMM::new(&accounts, static_base, dyn_start, dyn_end, &[], &[0, 0]).expect("Failed to create RaydiumCPMM");

//         let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");

//         eprintln!(
//             "Price: {:?}, {:?}",
//             raydium_cpmm.price, raydium_cpmm.inverse_price
//         );
//         eprintln!("Base decimals: {:?}, Quote decimals: {:?}", pool.mint_0_decimals, pool.mint_1_decimals);

//         // Test round trip: base -> quote -> base
//         let sol_in = 100_000_000_000; // 0.01% of pool

//         let token_mint = if *base_token.key == sol_mint {
//             *quote_token.key
//         } else {
//             *base_token.key
//         };
//         eprintln!("================================================");
//         // Step 1: Swap base -> quote
//         let clock1 = get_clock(&rpc_client).await.unwrap();
//         eprintln!("sol_in: {:?}", sol_in);
//         let token_out = raydium_cpmm
//             .swap_base_in(&accounts, sol_mint, sol_in, MintFee::ZERO, MintFee::ZERO, &clock1)
//             .expect("swap_base_in failed");
//         eprintln!(
//             "Step 1 (swap_base_in): {} SOL -> {} TOKEN",
//             sol_in as f64 / 1_000_000_000.0,
//             token_out as f64 / 1_000_000.0,
//         );
//         let max_sol_in = raydium_cpmm
//             .swap_base_out(&accounts, token_mint, token_out, MintFee::ZERO, MintFee::ZERO, &clock1)
//             .expect("swap_base_out failed");
//         eprintln!(
//             "Step 1 (swap_base_out): MAX SOL IN {} -> {} TOKEN OUT",
//             max_sol_in as f64 / 1_000_000_000.0,
//             token_out as f64 / 1_000_000.0,
//         );

//         eprintln!("================================================");

//         let sol_out = raydium_cpmm
//             .swap_base_in(&accounts, token_mint, token_out, MintFee::ZERO, MintFee::ZERO, &clock1)
//             .expect("second swap_base_in failed");
//         eprintln!(
//             "Step 2 (swap_base_in): {} TOKEN -> {} SOL",
//             token_out as f64 / 1_000_000.0,
//             sol_out as f64 / 1_000_000_000.0,
//         );
//         let max_token_in = raydium_cpmm
//             .swap_base_out(&accounts, sol_mint, sol_out, MintFee::ZERO, MintFee::ZERO, &clock1)
//             .expect("second swap_base_out failed");
//         eprintln!(
//             "Step 2 (swap_base_out): {} MAX TOKEN IN -> {} SOL OUT",
//             max_token_in as f64 / 1_000_000.0,
//             sol_out as f64 / 1_000_000_000.0
//         );
//     }
// }