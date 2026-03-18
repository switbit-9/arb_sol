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
    programs::{PoolKind, ProgramMeta},
};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    account_info::AccountInfo,
    instruction::{AccountMeta, Instruction},
    program::invoke_unchecked,
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
    /// Pre-computed fee factor: 1 - total_fee_numerator/1_000_000
    pub fee_factor: (f64, f64),
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
    fn pool_kind(&self) -> PoolKind { PoolKind::RaydiumCPMM }

    fn get_fee_factor(&self) -> Result<(f64, f64)> { Ok(self.fee_factor) }

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

    fn fast_quote<'a>(&mut self, _accounts: &[AccountInfo<'a>], input_mint: Pubkey, amount_in: u64, _profit_pct: f64) -> Result<(u64, u64)> {
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

        invoke_unchecked(&swap_ix, &accounts_arr)?;
        Ok(())
    }

    /// Read all fee data from the AmmConfig and PoolState accounts.
    /// Returns (trade_fee_rate, creator_fee_rate, protocol_fee_rate, fund_fee_rate,
    ///          fees_token_0, fees_token_1, enable_creator_fee, creator_fee_on,
    ///          base_is_token_0)
    #[inline(always)]
    fn read_fees<'a>(
        amm_config_acc: &AccountInfo<'a>,
        pool_acc: &AccountInfo<'a>,
        base_vault_key: &Pubkey,
    ) -> Result<(u64, u64, u64, u64, u64, u64, bool, u8, bool)> {
        // Read fee rates directly from AmmConfig bytes
        // Layout after 8-byte discriminator: bump(1) + disable_create_pool(1) + index(2) = 4 bytes
        // then trade_fee_rate(8), protocol_fee_rate(8), fund_fee_rate(8), create_pool_fee(8),
        // protocol_owner(32), fund_owner(32), creator_fee_rate(8)
        let config_data = amm_config_acc.try_borrow_data()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        let d = &*config_data;
        let trade_fee_rate = u64::from_le_bytes(d[12..20].try_into().unwrap());
        let protocol_fee_rate = u64::from_le_bytes(d[20..28].try_into().unwrap());
        let fund_fee_rate = u64::from_le_bytes(d[28..36].try_into().unwrap());
        let creator_fee_rate = u64::from_le_bytes(d[108..116].try_into().unwrap());
        drop(config_data);

        // Read accumulated fees and pool flags from PoolState (zero-copy)
        let pool_data = pool_acc.try_borrow_data()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        let pool_size = std::mem::size_of::<PoolState>();
        let pool: &PoolState = bytemuck::from_bytes(&pool_data[8..8 + pool_size]);

        let fees_token_0 = pool.protocol_fees_token_0
            .saturating_add(pool.fund_fees_token_0)
            .saturating_add(pool.creator_fees_token_0);
        let fees_token_1 = pool.protocol_fees_token_1
            .saturating_add(pool.fund_fees_token_1)
            .saturating_add(pool.creator_fees_token_1);
        let enable_creator_fee = pool.enable_creator_fee;
        let creator_fee_on = pool.creator_fee_on;
        let base_is_token_0 = *base_vault_key == pool.token_0_vault;

        Ok((trade_fee_rate, creator_fee_rate, protocol_fee_rate, fund_fee_rate,
            fees_token_0, fees_token_1, enable_creator_fee, creator_fee_on,
            base_is_token_0))
    }

    pub fn new<'a>(
        accounts: &[AccountInfo<'a>],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
    ) -> Result<Self> {
        let pool_acc = &accounts[dyn_start + D_POOL];
        let base_vault = &accounts[dyn_start + D_BASE_VAULT];
        let quote_vault = &accounts[dyn_start + D_QUOTE_VAULT];
        let amm_config_acc = &accounts[dyn_start + D_AMM_CONFIG];

        // Parse vault data (mint + amount) from vault accounts
        let (base_token_pk, base_vault_amount) = read_vault_data(base_vault)?;
        let (quote_token_pk, quote_vault_amount) = read_vault_data(quote_vault)?;

        #[cfg(test)]
        let base_vault_amount = (base_vault_amount as f64 * 0.95) as u64;

        // Read all fee data from AmmConfig + PoolState accounts
        let (trade_fee_rate, creator_fee_rate, protocol_fee_rate, fund_fee_rate,
             fees_token_0, fees_token_1, enable_creator_fee, creator_fee_on,
             base_is_token_0) = Self::read_fees(amm_config_acc, pool_acc, base_vault.key)?;

        let adjusted_creator_fee_rate = if enable_creator_fee { creator_fee_rate } else { 0 };
        let total_fee_numerator = trade_fee_rate + adjusted_creator_fee_rate;

        let price = get_price_f64(base_vault_amount, quote_vault_amount, fees_token_0, fees_token_1)?;

        // ZeroForOne: input is token_0
        let buy_creator_fee_on_input = matches!(creator_fee_on, 0 | 1);
        // OneForZero: input is token_1
        let sell_creator_fee_on_input = matches!(creator_fee_on, 0 | 2);

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
            protocol_fee_rate,
            fund_fee_rate,
            total_fee_numerator,
            fee_factor: { let f = 1.0 - total_fee_numerator as f64 / 1_000_000.0; (f, f) },
            buy_max_in: 0,
            buy_max_out: 0,
            sell_max_in: 0,
            sell_max_out: 0,
            prepared: false,
            fees_token_0,
            fees_token_1,
            adjusted_creator_fee_rate,
            buy_creator_fee_on_input,
            sell_creator_fee_on_input,
            base_is_token_0,
        };
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

    /// Compute deferred fields: cached max amounts.
    /// Called only for instances that participate in a profitable arb path.
    /// Fee data is already read in new() via read_fees().
    pub fn prepare_for_execution<'a>(
        &mut self,
        _accounts: &[AccountInfo<'a>],
    ) {
        if self.prepared {
            return;
        }
        self.prepared = true;

        let (buy_in, buy_out, sell_in, sell_out) =
            Self::compute_cached_max(self.base_vault_amount, self.quote_vault_amount, self.total_fee_numerator, 1_000_000);
        self.buy_max_in = buy_in;
        self.buy_max_out = buy_out;
        self.sell_max_in = sell_in;
        self.sell_max_out = sell_out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::token::MintFee;
    use solana_client::nonblocking::rpc_client::RpcClient;
    use solana_sdk::pubkey::Pubkey as SdkPubkey;

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
            key_static, false, true, lamports, data_vec, owner_static, false, 0,
        )
    }

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
            key_static, false, false, lamports, data, owner_static,
            account.executable, account.rent_epoch,
        )
    }

    fn to_sdk(key: Pubkey) -> SdkPubkey {
        SdkPubkey::try_from(key.to_bytes().as_ref()).unwrap()
    }

    fn get_rpc_client() -> RpcClient {
        let api_key = "f230200b-f911-43c1-a242-4e7b066d0993";
        RpcClient::new(format!("https://mainnet.helius-rpc.com/?api-key={}", api_key))
    }

    async fn get_clock_from_rpc(rpc_client: &RpcClient) -> Clock {
        use anchor_client::solana_sdk::sysvar;
        let clock_account = rpc_client.get_account(&sysvar::clock::ID).await
            .expect("Failed to fetch clock");
        let data = &clock_account.data;
        assert!(data.len() >= 40, "Clock account data too short");
        Clock {
            slot: u64::from_le_bytes(data[0..8].try_into().unwrap()),
            epoch_start_timestamp: i64::from_le_bytes(data[8..16].try_into().unwrap()),
            epoch: u64::from_le_bytes(data[16..24].try_into().unwrap()),
            leader_schedule_epoch: u64::from_le_bytes(data[24..32].try_into().unwrap()),
            unix_timestamp: i64::from_le_bytes(data[32..40].try_into().unwrap()),
        }
    }

    async fn build_from_pool_id(
        pool_id: Pubkey,
    ) -> (RaydiumCPMM, Vec<AccountInfo<'static>>, Clock) {
        let rpc_client = get_rpc_client();

        // Fetch pool account and parse PoolState
        let pool_account = rpc_client.get_account(&to_sdk(pool_id)).await
            .unwrap_or_else(|e| panic!("Failed to fetch pool {}: {}", pool_id, e));
        let pool_state_size = std::mem::size_of::<PoolState>();
        let pool: PoolState = bytemuck::pod_read_unaligned(&pool_account.data[8..8 + pool_state_size]);

        eprintln!("Pool: {}", pool_id);
        eprintln!("  token_0 (base): {}", pool.token_0_mint);
        eprintln!("  token_1 (quote): {}", pool.token_1_mint);

        // Fetch AMM config account (fees are read from it in new() via read_fees())
        let amm_config_raw = rpc_client.get_account(&to_sdk(pool.amm_config)).await
            .expect("Failed to fetch AMM config");

        // Fetch vault accounts
        let vault_0_account = rpc_client.get_account(&to_sdk(pool.token_0_vault)).await
            .expect("Failed to fetch vault 0");
        let vault_1_account = rpc_client.get_account(&to_sdk(pool.token_1_vault)).await
            .expect("Failed to fetch vault 1");

        // Build AccountInfo array
        let pool_id_info = account_to_account_info(pool_id, pool_account);
        let base_vault_info = account_to_account_info(pool.token_0_vault, vault_0_account);
        let quote_vault_info = account_to_account_info(pool.token_1_vault, vault_1_account);
        let amm_config_info = account_to_account_info(pool.amm_config, amm_config_raw);

        let program_id_info = create_mock_account_info_with_data(
            PROGRAM_ID, anchor_lang::solana_program::system_program::id(), None,
        );
        let vault_authority_info = create_mock_account_info_with_data(
            Pubkey::new_unique(), anchor_lang::solana_program::system_program::id(), None,
        );
        let observation_info = create_mock_account_info_with_data(
            pool.observation_key, anchor_lang::solana_program::system_program::id(), None,
        );

        // Layout:
        // Static (static_base=0): [program_id, vault_authority]
        // Dynamic (dyn_start=2): [pool, base_vault, quote_vault, amm_config, observation]
        let accounts = vec![
            program_id_info,         // S0
            vault_authority_info,    // S1
            pool_id_info,            // D0
            base_vault_info,         // D1
            quote_vault_info,        // D2
            amm_config_info,         // D3
            observation_info,        // D4
        ];

        let static_base: usize = 0;
        let dyn_start: usize = 2;
        let dyn_end: usize = accounts.len();

        // Fees are now read directly from accounts in new() via read_fees()
        let mut cpmm = RaydiumCPMM::new(&accounts, static_base, dyn_start, dyn_end)
            .expect("RaydiumCPMM::new failed");

        cpmm.prepare_for_execution(&accounts);

        let clock = get_clock_from_rpc(&rpc_client).await;

        eprintln!("  price: {}", cpmm.price);
        eprintln!("  total_fee_numerator: {}", cpmm.total_fee_numerator);

        (cpmm, accounts, clock)
    }

    #[tokio::test]
    async fn test_cpmm_round_trip() {
        let pool_id = Pubkey::from_str_const("BSh8SjXvauDvNRAzsVGibW1kB8eaaXWJYnf36SC9T7HC");
        let (mut cpmm, accounts, clock) = build_from_pool_id(pool_id).await;

        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let no_fee = MintFee::ZERO;

        // 1. Print all account pubkeys
        eprintln!("\n=== Account Pubkeys ===");
        eprintln!("base_mint        : {}", cpmm.base_token_pk);
        eprintln!("quote_mint       : {}", cpmm.quote_token_pk);
        eprintln!("pool_id          : {}", accounts[2 + D_POOL].key);
        eprintln!("base_vault       : {}", accounts[2 + D_BASE_VAULT].key);
        eprintln!("quote_vault      : {}", accounts[2 + D_QUOTE_VAULT].key);
        eprintln!("amm_config       : {}", accounts[2 + D_AMM_CONFIG].key);
        eprintln!("observation      : {}", accounts[2 + D_OBSERVATION].key);
        eprintln!("program_id       : {}", accounts[S_PROGRAM_ID].key);
        eprintln!("vault_authority  : {}", accounts[S_VAULT_AUTHORITY].key);

        // 2. Prices
        let (price, inverse_price) = cpmm.get_prices().unwrap();
        eprintln!("\n=== Prices ===");
        eprintln!("price            : {}", price);
        eprintln!("inverse_price    : {}", inverse_price);

        // 3. Fees
        let (fee_factor, fee_factor_2) = cpmm.get_fee_factor().unwrap();
        eprintln!("\n=== Fees ===");
        eprintln!("total_fee_num    : {}", cpmm.total_fee_numerator);
        eprintln!("fee_factor       : {}", fee_factor);
        eprintln!("fee_factor_2     : {}", fee_factor_2);

        // 4. Max amounts
        eprintln!("\n=== After prepare_for_execution ===");
        eprintln!("buy_max_in       : {}", cpmm.buy_max_in);
        eprintln!("buy_max_out      : {}", cpmm.buy_max_out);
        eprintln!("sell_max_in      : {}", cpmm.sell_max_in);
        eprintln!("sell_max_out     : {}", cpmm.sell_max_out);

        // 5. Round-trip with start_amount = 1 WSOL
        let start_amount: u64 = 1_000_000_000;

        let other_mint = if cpmm.base_token_pk == sol_mint {
            cpmm.quote_token_pk
        } else {
            cpmm.base_token_pk
        };

        let rpc = get_rpc_client();
        let other_mint_account = rpc.get_account(
            &SdkPubkey::try_from(other_mint.to_bytes().as_ref()).unwrap()
        ).await.unwrap();
        let token_decimals = other_mint_account.data[44] as i32;
        let sol_div = 10f64.powi(9);
        let tok_div = 10f64.powi(token_decimals);

        // Direction 1: SOL -> TOKEN -> SOL
        eprintln!("\n=== Direction 1: SOL -> TOKEN -> SOL ===");
        let token_out = cpmm.swap_base_in(
            &accounts, sol_mint, start_amount, no_fee, no_fee, &clock,
        ).unwrap();
        let max_sol_in = cpmm.swap_base_out(
            &accounts, other_mint, token_out, no_fee, no_fee, &clock,
        ).unwrap();
        eprintln!("AMOUNT_IN {} -> AMOUNT_OUT {} -> MAX_AMOUNT_IN {}", start_amount as f64 / sol_div, token_out as f64 / tok_div, max_sol_in as f64 / sol_div);

        // Direction 2: TOKEN -> SOL -> TOKEN
        eprintln!("\n=== Direction 2: TOKEN -> SOL -> TOKEN ===");
        let sol_out = cpmm.swap_base_in(
            &accounts, other_mint, token_out, no_fee, no_fee, &clock,
        ).unwrap();
        let max_token_in = cpmm.swap_base_out(
            &accounts, sol_mint, sol_out, no_fee, no_fee, &clock,
        ).unwrap();
        eprintln!("AMOUNT_IN {} -> AMOUNT_OUT {} -> MAX_AMOUNT_IN {}", token_out as f64 / tok_div, sol_out as f64 / sol_div, max_token_in as f64 / tok_div);
    }
}