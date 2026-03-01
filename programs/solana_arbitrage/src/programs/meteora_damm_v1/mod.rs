use crate::programs::ProgramMeta;
use crate::utils::token::get_transfer_fee;
use crate::utils::utils::parse_token_account;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use std::marker::PhantomData;

// ── Pool account data byte offsets (after 8-byte Anchor discriminator) ──
const TOKEN_A_MINT_OFFSET: usize = 32;
const TOKEN_B_MINT_OFFSET: usize = 64;
const TRADE_FEE_NUM_OFFSET: usize = 322;
const TRADE_FEE_DEN_OFFSET: usize = 330;
const PROTOCOL_FEE_NUM_OFFSET: usize = 338;
const PROTOCOL_FEE_DEN_OFFSET: usize = 346;

// ── Vault account data byte offsets (after 8-byte Anchor discriminator) ──
// Vault layout: enabled(1) + bumps(2) + total_amount(8) + token_vault(32) + fee_vault(32)
//   + token_mint(32) + lp_mint(32) + strategies(30*32=960) + base(32) + admin(32) + operator(32)
//   + locked_profit_tracker { last_updated_locked_profit(8) + last_report(8) + locked_profit_degradation(8) }
const VAULT_TOTAL_AMOUNT_OFFSET: usize = 3; // 1 + 2
const VAULT_LAST_UPDATED_LOCKED_PROFIT_OFFSET: usize = 1195; // 3 + 8 + 32*4 + 960 + 32*3
const VAULT_LAST_REPORT_OFFSET: usize = 1203;
const VAULT_LOCKED_PROFIT_DEGRADATION_OFFSET: usize = 1211;
const LOCKED_PROFIT_DEGRADATION_DENOMINATOR: u128 = 1_000_000_000_000;

/// SPL Token Mint supply offset (after COption<Pubkey> mint_authority = 36 bytes)
const MINT_SUPPLY_OFFSET: usize = 36;

fn read_pubkey(data: &[u8], offset: usize) -> Pubkey {
    Pubkey::new_from_array(data[offset..offset + 32].try_into().unwrap())
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

fn read_mint_supply(account: &AccountInfo) -> Result<u64> {
    let data = account.try_borrow_data()?;
    if data.len() < MINT_SUPPLY_OFFSET + 8 {
        return Err(ProgramError::InvalidAccountData.into());
    }
    Ok(read_u64(&data, MINT_SUPPLY_OFFSET))
}

/// Read the vault's unlocked amount from the Vault state account.
/// unlocked_amount = total_amount - locked_profit(current_time)
/// This matches Vault::get_unlocked_amount from the DAMM v1 SDK.
fn read_vault_unlocked_amount(vault_account: &AccountInfo, current_time: u64) -> Result<u64> {
    let data = vault_account.try_borrow_data()?;
    let d = &data[8..]; // skip Anchor discriminator
    let total_amount = read_u64(d, VAULT_TOTAL_AMOUNT_OFFSET);
    let last_updated_locked_profit = read_u64(d, VAULT_LAST_UPDATED_LOCKED_PROFIT_OFFSET);
    let last_report = read_u64(d, VAULT_LAST_REPORT_OFFSET);
    let locked_profit_degradation = read_u64(d, VAULT_LOCKED_PROFIT_DEGRADATION_OFFSET);

    let duration = current_time.saturating_sub(last_report) as u128;
    let locked_fund_ratio = duration.saturating_mul(locked_profit_degradation as u128);

    let locked_profit = if locked_fund_ratio >= LOCKED_PROFIT_DEGRADATION_DENOMINATOR {
        0u64
    } else {
        let lp = (last_updated_locked_profit as u128)
            .checked_mul(LOCKED_PROFIT_DEGRADATION_DENOMINATOR - locked_fund_ratio)
            .and_then(|v| v.checked_div(LOCKED_PROFIT_DEGRADATION_DENOMINATOR))
            .unwrap_or(0);
        lp as u64
    };

    Ok(total_amount.saturating_sub(locked_profit))
}

pub struct MeteoraDammV1<'info> {
    pub pool_id: Pubkey,
    pub base_token_pk: Pubkey,
    pub quote_token_pk: Pubkey,
    pub base_vault_amount: u64,
    pub quote_vault_amount: u64,
    pub price: f64,
    pub inverse_price: f64,
    pub fee_rate: f64,
    pub trade_fee_numerator: u64,
    pub trade_fee_denominator: u64,
    pub protocol_fee_numerator: u64,
    pub protocol_fee_denominator: u64,
    pub start_index: usize,
    pub end_index: usize,
    pub buy_max_in: u64,
    pub buy_max_out: u64,
    pub sell_max_in: u64,
    pub sell_max_out: u64,
    _phantom: PhantomData<&'info ()>,
}

impl<'info> ProgramMeta for MeteoraDammV1<'info> {
    fn get_id(&self) -> &Pubkey {
        &Self::PROGRAM_ID
    }

    fn get_pool_id(&self) -> &Pubkey {
        &self.pool_id
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (&self.base_token_pk, &self.quote_token_pk)
    }

    fn name(&self) -> &'static str { "MeteoraDammV1" }

    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        Ok((self.base_vault_amount, self.quote_vault_amount))
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        Ok((self.price, self.inverse_price))
    }

    fn get_fee_factor(&self) -> Result<(f64, f64)> {
        // Symmetric fee: same rate for both A->B and B->A
        let f = 1.0 - self.fee_rate;
        Ok((f, f))
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

    fn swap_base_in<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        _clock: &Clock,
    ) -> Result<u64> {
        let (reserve_in, reserve_out) = if input_mint == self.base_token_pk {
            (self.base_vault_amount as u128, self.quote_vault_amount as u128)
        } else {
            (self.quote_vault_amount as u128, self.base_vault_amount as u128)
        };

        let token_a_mint = &accounts[self.start_index + Self::TOKEN_A_MINT_IDX];
        let token_b_mint = &accounts[self.start_index + Self::TOKEN_B_MINT_IDX];

        let (token_in_mint, token_out_mint) = if input_mint == self.base_token_pk {
            (token_a_mint, token_b_mint)
        } else {
            (token_b_mint, token_a_mint)
        };

        // Apply transfer fee on input
        let transfer_fee_in = get_transfer_fee(token_in_mint, amount_in)?;
        let amount_in_after_transfer = amount_in.checked_sub(transfer_fee_in).unwrap();

        // Calculate total trade fee: fee = amount * fee_num / fee_den
        let trade_fee = self.calculate_trade_fee(amount_in_after_transfer as u128)?;

        // Protocol fee is a subset of trade fee
        let protocol_fee = self.calculate_protocol_fee(trade_fee)?;

        // Amount after protocol fee goes into the pool
        let amount_after_protocol = (amount_in_after_transfer as u128)
            .checked_sub(protocol_fee)
            .ok_or(ProgramError::InvalidArgument)?;

        // Trade fee (minus protocol portion) is deducted from the effective swap amount
        let net_trade_fee = trade_fee
            .checked_sub(protocol_fee)
            .ok_or(ProgramError::InvalidArgument)?;
        let effective_in = amount_after_protocol
            .checked_sub(net_trade_fee)
            .ok_or(ProgramError::InvalidArgument)?;

        // Constant product: out = reserve_out * effective_in / (reserve_in + effective_in)
        let numerator = reserve_out
            .checked_mul(effective_in)
            .ok_or(ProgramError::InvalidArgument)?;
        let denominator = reserve_in
            .checked_add(effective_in)
            .ok_or(ProgramError::InvalidArgument)?;
        let amount_out = numerator
            .checked_div(denominator)
            .ok_or(ProgramError::InvalidArgument)?;

        let amount_out_u64 = u64::try_from(amount_out).map_err(|_| ProgramError::InvalidArgument)?;

        // Apply transfer fee on output
        let transfer_fee_out = get_transfer_fee(token_out_mint, amount_out_u64)?;
        let amount_out_final = amount_out_u64.checked_sub(transfer_fee_out).unwrap();

        Ok(amount_out_final)
    }

    fn swap_base_out<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        _clock: &Clock,
    ) -> Result<u64> {
        let (reserve_in, reserve_out) = if output_mint == self.base_token_pk {
            // Output is A, input is B
            (self.quote_vault_amount as u128, self.base_vault_amount as u128)
        } else {
            // Output is B, input is A
            (self.base_vault_amount as u128, self.quote_vault_amount as u128)
        };

        let token_a_mint = &accounts[self.start_index + Self::TOKEN_A_MINT_IDX];
        let token_b_mint = &accounts[self.start_index + Self::TOKEN_B_MINT_IDX];

        let (token_in_mint, token_out_mint) = if output_mint == self.base_token_pk {
            (token_b_mint, token_a_mint)
        } else {
            (token_a_mint, token_b_mint)
        };

        // Add transfer fee to desired output
        let transfer_fee_out = get_transfer_fee(token_out_mint, amount_out)?;
        let amount_out_before_transfer = (amount_out as u128)
            .checked_add(transfer_fee_out as u128)
            .ok_or(ProgramError::InvalidArgument)?;

        // Inverse constant product: in = reserve_in * out / (reserve_out - out) + 1 (round up)
        let numerator = reserve_in
            .checked_mul(amount_out_before_transfer)
            .ok_or(ProgramError::InvalidArgument)?;
        let denominator = reserve_out
            .checked_sub(amount_out_before_transfer)
            .ok_or(ProgramError::InvalidArgument)?;
        let effective_in = numerator
            .checked_div(denominator)
            .ok_or(ProgramError::InvalidArgument)?
            .checked_add(1)
            .ok_or(ProgramError::InvalidArgument)?;

        // Add back trade fee: amount_before_fee = effective_in / (1 - fee_rate)
        let amount_before_fee = if self.trade_fee_denominator > self.trade_fee_numerator {
            let denom = self.trade_fee_denominator - self.trade_fee_numerator;
            effective_in
                .checked_mul(self.trade_fee_denominator as u128)
                .ok_or(ProgramError::InvalidArgument)?
                .checked_div(denom as u128)
                .ok_or(ProgramError::InvalidArgument)?
                .checked_add(1)
                .ok_or(ProgramError::InvalidArgument)?
        } else {
            effective_in
        };

        let amount_in_u64 =
            u64::try_from(amount_before_fee).map_err(|_| ProgramError::InvalidArgument)?;

        // Add transfer fee on input
        let transfer_fee_in = get_transfer_fee(token_in_mint, amount_in_u64)?;
        let total_in = amount_in_u64
            .checked_add(transfer_fee_in)
            .ok_or(ProgramError::InvalidArgument)?;

        Ok(total_in)
    }

    fn invoke_swap_base_in<'a>(
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
        let (user_source_token, user_destination_token) =
            if input_mint == *mint_1_account.key {
                (user_mint_1_token_account, user_mint_2_token_account)
            } else {
                (user_mint_2_token_account, user_mint_1_token_account)
            };

        // Protocol fee account depends on input token direction
        let protocol_token_fee = if input_mint == self.base_token_pk {
            &accounts[self.start_index + Self::PROTOCOL_TOKEN_A_FEE_IDX]
        } else {
            &accounts[self.start_index + Self::PROTOCOL_TOKEN_B_FEE_IDX]
        };

        let pool = &accounts[self.start_index + Self::POOL_IDX];
        let a_vault = &accounts[self.start_index + Self::A_VAULT_IDX];
        let b_vault = &accounts[self.start_index + Self::B_VAULT_IDX];
        let a_token_vault = &accounts[self.start_index + Self::A_TOKEN_VAULT_IDX];
        let b_token_vault = &accounts[self.start_index + Self::B_TOKEN_VAULT_IDX];
        let a_vault_lp_mint = &accounts[self.start_index + Self::A_VAULT_LP_MINT_IDX];
        let b_vault_lp_mint = &accounts[self.start_index + Self::B_VAULT_LP_MINT_IDX];
        let a_vault_lp = &accounts[self.start_index + Self::A_VAULT_LP_IDX];
        let b_vault_lp = &accounts[self.start_index + Self::B_VAULT_LP_IDX];
        let vault_program = &accounts[self.start_index + Self::VAULT_PROGRAM_IDX];
        let token_program = &accounts[self.start_index + Self::TOKEN_PROGRAM_IDX];

        let minimum_out = amount_out.unwrap_or(0);

        // Swap instruction account layout (from SDK swap.rs)
        let metas = [
            AccountMeta::new(*pool.key, false),                  // pool
            AccountMeta::new(*user_source_token.key, false),     // user_source_token
            AccountMeta::new(*user_destination_token.key, false),// user_destination_token
            AccountMeta::new(*a_vault.key, false),               // a_vault
            AccountMeta::new(*b_vault.key, false),               // b_vault
            AccountMeta::new(*a_token_vault.key, false),         // a_token_vault
            AccountMeta::new(*b_token_vault.key, false),         // b_token_vault
            AccountMeta::new(*a_vault_lp_mint.key, false),       // a_vault_lp_mint
            AccountMeta::new(*b_vault_lp_mint.key, false),       // b_vault_lp_mint
            AccountMeta::new(*a_vault_lp.key, false),            // a_vault_lp
            AccountMeta::new(*b_vault_lp.key, false),            // b_vault_lp
            AccountMeta::new(*protocol_token_fee.key, false),    // protocol_token_fee
            AccountMeta::new(*payer.key, true),                  // user (signer)
            AccountMeta::new_readonly(*vault_program.key, false),// vault_program
            AccountMeta::new_readonly(*token_program.key, false),// token_program
        ];

        // Anchor swap discriminator: sha256("global:swap")[..8]
        let mut data = vec![0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];
        data.extend_from_slice(&max_amount_in.to_le_bytes());
        data.extend_from_slice(&minimum_out.to_le_bytes());

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas.to_vec(),
            data,
        };

        let accounts_arr = [
            pool.clone(),
            unsafe { std::mem::transmute(user_source_token.to_account_info()) },
            unsafe { std::mem::transmute(user_destination_token.to_account_info()) },
            a_vault.clone(),
            b_vault.clone(),
            a_token_vault.clone(),
            b_token_vault.clone(),
            a_vault_lp_mint.clone(),
            b_vault_lp_mint.clone(),
            a_vault_lp.clone(),
            b_vault_lp.clone(),
            protocol_token_fee.clone(),
            unsafe { std::mem::transmute(payer.to_account_info()) },
            vault_program.clone(),
            token_program.clone(),
        ];

        unsafe {
            let accounts_slice: &[AccountInfo<'a>] = std::mem::transmute(accounts_arr.as_slice());
            invoke(&swap_ix, accounts_slice)?;
        }
        Ok(())
    }

    fn invoke_swap_base_out<'a>(
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
        // DAMM v1 has a single swap instruction (in_amount + minimum_out_amount)
        self.invoke_swap_base_in(
            accounts,
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

    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        msg!("=== Meteora DAMM V1 ===");
        msg!("0 program_id: {}", accounts[self.start_index + Self::PROGRAM_ID_IDX].key);
        msg!("1 pool: {}", accounts[self.start_index + Self::POOL_IDX].key);
        msg!("2 a_vault: {}", accounts[self.start_index + Self::A_VAULT_IDX].key);
        msg!("3 b_vault: {}", accounts[self.start_index + Self::B_VAULT_IDX].key);
        msg!("4 a_token_vault: {}", accounts[self.start_index + Self::A_TOKEN_VAULT_IDX].key);
        msg!("5 b_token_vault: {}", accounts[self.start_index + Self::B_TOKEN_VAULT_IDX].key);
        msg!("6 a_vault_lp_mint: {}", accounts[self.start_index + Self::A_VAULT_LP_MINT_IDX].key);
        msg!("7 b_vault_lp_mint: {}", accounts[self.start_index + Self::B_VAULT_LP_MINT_IDX].key);
        msg!("8 a_vault_lp: {}", accounts[self.start_index + Self::A_VAULT_LP_IDX].key);
        msg!("9 b_vault_lp: {}", accounts[self.start_index + Self::B_VAULT_LP_IDX].key);
        msg!("10 token_a_mint: {}", accounts[self.start_index + Self::TOKEN_A_MINT_IDX].key);
        msg!("11 token_b_mint: {}", accounts[self.start_index + Self::TOKEN_B_MINT_IDX].key);
        msg!("12 protocol_token_a_fee: {}", accounts[self.start_index + Self::PROTOCOL_TOKEN_A_FEE_IDX].key);
        msg!("13 protocol_token_b_fee: {}", accounts[self.start_index + Self::PROTOCOL_TOKEN_B_FEE_IDX].key);
        msg!("14 vault_program: {}", accounts[self.start_index + Self::VAULT_PROGRAM_IDX].key);
        msg!("15 token_program: {}", accounts[self.start_index + Self::TOKEN_PROGRAM_IDX].key);
        Ok(())
    }
}

impl<'info> MeteoraDammV1<'info> {
    pub const PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB");

    // Account indices
    pub const PROGRAM_ID_IDX: usize = 0;
    pub const POOL_IDX: usize = 1;
    pub const A_VAULT_IDX: usize = 2;
    pub const B_VAULT_IDX: usize = 3;
    pub const A_TOKEN_VAULT_IDX: usize = 4;
    pub const B_TOKEN_VAULT_IDX: usize = 5;
    pub const A_VAULT_LP_MINT_IDX: usize = 6;
    pub const B_VAULT_LP_MINT_IDX: usize = 7;
    pub const A_VAULT_LP_IDX: usize = 8;
    pub const B_VAULT_LP_IDX: usize = 9;
    pub const TOKEN_A_MINT_IDX: usize = 10;
    pub const TOKEN_B_MINT_IDX: usize = 11;
    pub const PROTOCOL_TOKEN_A_FEE_IDX: usize = 12;
    pub const PROTOCOL_TOKEN_B_FEE_IDX: usize = 13;
    pub const VAULT_PROGRAM_IDX: usize = 14;
    pub const TOKEN_PROGRAM_IDX: usize = 15;

    pub const MIN_ACCOUNTS: usize = 16;

    pub fn new(
        accounts: &[AccountInfo<'info>],
        start_index: usize,
        end_index: usize,
        clock: &Clock,
    ) -> Result<Self> {
        require!(
            end_index - start_index >= Self::MIN_ACCOUNTS,
            crate::programs::SolarBError::InsufficientAccounts
        );
        require!(
            end_index <= accounts.len(),
            crate::programs::SolarBError::InsufficientAccounts
        );

        let pool_account = &accounts[start_index + Self::POOL_IDX];
        let pool_data = pool_account.try_borrow_data()?;

        // Parse Pool state from Borsh-serialized data (after 8-byte Anchor discriminator)
        let d = &pool_data[8..];

        let token_a_mint = read_pubkey(d, TOKEN_A_MINT_OFFSET);
        let token_b_mint = read_pubkey(d, TOKEN_B_MINT_OFFSET);
        let trade_fee_numerator = read_u64(d, TRADE_FEE_NUM_OFFSET);
        let trade_fee_denominator = read_u64(d, TRADE_FEE_DEN_OFFSET);
        let protocol_fee_numerator = read_u64(d, PROTOCOL_FEE_NUM_OFFSET);
        let protocol_fee_denominator = read_u64(d, PROTOCOL_FEE_DEN_OFFSET);

        drop(pool_data);

        // Compute effective reserves using vault unlocked amount (total_amount - locked_profit):
        // effective_amount = vault_unlocked_amount * pool_vault_lp_amount / vault_lp_supply
        let a_vault = &accounts[start_index + Self::A_VAULT_IDX];
        let b_vault = &accounts[start_index + Self::B_VAULT_IDX];
        let a_vault_lp_mint = &accounts[start_index + Self::A_VAULT_LP_MINT_IDX];
        let b_vault_lp_mint = &accounts[start_index + Self::B_VAULT_LP_MINT_IDX];
        let a_vault_lp = &accounts[start_index + Self::A_VAULT_LP_IDX];
        let b_vault_lp = &accounts[start_index + Self::B_VAULT_LP_IDX];

        let current_time: u64 = clock.unix_timestamp as u64;
        let vault_a_unlocked = read_vault_unlocked_amount(a_vault, current_time)?;
        let vault_b_unlocked = read_vault_unlocked_amount(b_vault, current_time)?;
        let pool_a_lp_amount = parse_token_account(a_vault_lp)?.amount;
        let pool_b_lp_amount = parse_token_account(b_vault_lp)?.amount;
        let vault_a_lp_supply = read_mint_supply(a_vault_lp_mint)?;
        let vault_b_lp_supply = read_mint_supply(b_vault_lp_mint)?;

        let base_vault_amount = if vault_a_lp_supply > 0 {
            ((vault_a_unlocked as u128)
                .checked_mul(pool_a_lp_amount as u128)
                .unwrap_or(0))
            .checked_div(vault_a_lp_supply as u128)
            .unwrap_or(0) as u64
        } else {
            0
        };

        let quote_vault_amount = if vault_b_lp_supply > 0 {
            ((vault_b_unlocked as u128)
                .checked_mul(pool_b_lp_amount as u128)
                .unwrap_or(0))
            .checked_div(vault_b_lp_supply as u128)
            .unwrap_or(0) as u64
        } else {
            0
        };

        let fee_rate = if trade_fee_denominator > 0 {
            trade_fee_numerator as f64 / trade_fee_denominator as f64
        } else {
            0.0
        };

        let (price, inverse_price) = if base_vault_amount > 0 && quote_vault_amount > 0 {
            let p = quote_vault_amount as f64 / base_vault_amount as f64;
            (p, 1.0 / p)
        } else {
            (0.0, 0.0)
        };

        let (buy_max_in, buy_max_out, sell_max_in, sell_max_out) =
            Self::compute_cached_max(base_vault_amount, quote_vault_amount, fee_rate);

        let instance = MeteoraDammV1 {
            pool_id: *pool_account.key,
            base_token_pk: token_a_mint,
            quote_token_pk: token_b_mint,
            base_vault_amount,
            quote_vault_amount,
            price,
            inverse_price,
            fee_rate,
            trade_fee_numerator,
            trade_fee_denominator,
            protocol_fee_numerator,
            protocol_fee_denominator,
            start_index,
            end_index,
            buy_max_in,
            buy_max_out,
            sell_max_in,
            sell_max_out,
            _phantom: PhantomData,
        };
        // instance.log_accounts(accounts)?;
        Ok(instance)
    }

    /// Symmetric CP max amounts: computed once during init.
    fn compute_cached_max(base_vault: u64, quote_vault: u64, fee_rate: f64) -> (u64, u64, u64, u64) {
        fn cp_max(x: f64, y: f64, fee_factor: f64) -> (u64, u64) {
            let target = y * 0.99;
            if target <= 0.0 || y <= target || fee_factor <= 0.0 {
                return (0, y as u64);
            }
            let denom = y - target;
            let dx = (x / fee_factor) * ((y / denom) - 1.0);
            (dx.max(0.0).min(u64::MAX as f64) as u64, y as u64)
        }
        let ff = 1.0 - fee_rate;
        let (buy_in, buy_out) = cp_max(base_vault as f64, quote_vault as f64, ff);
        let (sell_in, sell_out) = cp_max(quote_vault as f64, base_vault as f64, ff);
        (buy_in, buy_out, sell_in, sell_out)
    }

    /// Calculate trade fee: fee = amount * trade_fee_numerator / trade_fee_denominator
    /// Returns minimum 1 if amount > 0 and fee_numerator > 0
    fn calculate_trade_fee(&self, amount: u128) -> Result<u128> {
        if self.trade_fee_numerator == 0 || amount == 0 {
            return Ok(0);
        }
        let fee = amount
            .checked_mul(self.trade_fee_numerator as u128)
            .ok_or(ProgramError::InvalidArgument)?
            .checked_div(self.trade_fee_denominator as u128)
            .ok_or(ProgramError::InvalidArgument)?;
        Ok(if fee == 0 { 1 } else { fee })
    }

    /// Calculate protocol fee as a portion of the trade fee
    fn calculate_protocol_fee(&self, trade_fee: u128) -> Result<u128> {
        if self.protocol_fee_numerator == 0 || trade_fee == 0 {
            return Ok(0);
        }
        let fee = trade_fee
            .checked_mul(self.protocol_fee_numerator as u128)
            .ok_or(ProgramError::InvalidArgument)?
            .checked_div(self.protocol_fee_denominator as u128)
            .ok_or(ProgramError::InvalidArgument)?;
        Ok(if fee == 0 { 1 } else { fee })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_spl::token::Token;

    /// Create a mock mint AccountInfo owned by the SPL Token program (no transfer fee).
    fn mock_spl_mint(key: &'static mut Pubkey, owner: &'static mut Pubkey) -> AccountInfo<'static> {
        // Minimal SPL Token mint: 82 bytes
        let data = Box::leak(Box::new(vec![0u8; 82]));
        let lamports = Box::leak(Box::new(1_000_000u64));
        *owner = Token::id();
        AccountInfo::new(key, false, false, lamports, data, owner, false, 0)
    }

    fn make_damm_v1(
        base_vault: u64,
        quote_vault: u64,
        fee_num: u64,
        fee_den: u64,
        proto_num: u64,
        proto_den: u64,
    ) -> MeteoraDammV1<'static> {
        let base_token_pk = Pubkey::new_unique();
        let quote_token_pk = Pubkey::new_unique();
        let fee_rate = if fee_den > 0 {
            fee_num as f64 / fee_den as f64
        } else {
            0.0
        };
        let price = quote_vault as f64 / base_vault as f64;
        let (buy_max_in, buy_max_out, sell_max_in, sell_max_out) =
            MeteoraDammV1::compute_cached_max(base_vault, quote_vault, fee_rate);
        MeteoraDammV1 {
            pool_id: Pubkey::new_unique(),
            base_token_pk,
            quote_token_pk,
            base_vault_amount: base_vault,
            quote_vault_amount: quote_vault,
            price,
            inverse_price: 1.0 / price,
            fee_rate,
            trade_fee_numerator: fee_num,
            trade_fee_denominator: fee_den,
            protocol_fee_numerator: proto_num,
            protocol_fee_denominator: proto_den,
            start_index: 0,
            end_index: 16,
            buy_max_in,
            buy_max_out,
            sell_max_in,
            sell_max_out,
            _phantom: PhantomData,
        }
    }

    /// Build a mock accounts slice with SPL Token mints at TOKEN_A_MINT_IDX and TOKEN_B_MINT_IDX.
    fn build_mock_accounts() -> Vec<AccountInfo<'static>> {
        let mut accounts = Vec::new();
        for _ in 0..MeteoraDammV1::MIN_ACCOUNTS {
            let key = Box::leak(Box::new(Pubkey::new_unique()));
            let owner = Box::leak(Box::new(Pubkey::default()));
            let data = Box::leak(Box::new(vec![0u8; 8]));
            let lamports = Box::leak(Box::new(0u64));
            accounts.push(AccountInfo::new(
                key, false, false, lamports, data, owner, false, 0,
            ));
        }
        // Replace mint slots with SPL Token-owned mints (transfer fee = 0)
        let key_a = Box::leak(Box::new(Pubkey::new_unique()));
        let owner_a = Box::leak(Box::new(Pubkey::default()));
        accounts[MeteoraDammV1::TOKEN_A_MINT_IDX] = mock_spl_mint(key_a, owner_a);

        let key_b = Box::leak(Box::new(Pubkey::new_unique()));
        let owner_b = Box::leak(Box::new(Pubkey::default()));
        accounts[MeteoraDammV1::TOKEN_B_MINT_IDX] = mock_spl_mint(key_b, owner_b);

        accounts
    }

    fn dummy_clock() -> Clock {
        Clock {
            slot: 0,
            epoch_start_timestamp: 0,
            epoch: 0,
            leader_schedule_epoch: 0,
            unix_timestamp: 0,
        }
    }

    /// Round-trip: swap_base_in(X) → Y, then swap_base_out(Y) → X'
    /// X' should be >= X (swap_base_out rounds up).
    #[test]
    fn test_round_trip_base_to_quote() {
        let damm = make_damm_v1(
            1_000_000_000, // 1B base
            2_000_000_000, // 2B quote
            25,            // 0.25% fee
            10_000,
            1, // 20% protocol fee
            5,
        );
        let accounts = build_mock_accounts();
        let clock = dummy_clock();
        let input_mint = damm.base_token_pk;

        for &amount_in in &[1_000u64, 100_000, 1_000_000, 10_000_000, 100_000_000] {
            let amount_out = damm
                .swap_base_in(&accounts, input_mint, amount_in, &clock)
                .unwrap();
            assert!(amount_out > 0, "swap_base_in should produce non-zero output");

            // Reverse: how much input is needed to get `amount_out` of quote?
            let needed_in = damm
                .swap_base_out(&accounts, damm.quote_token_pk, amount_out, &clock)
                .unwrap();

            // swap_base_in floors the output, so reversing may differ by ±tolerance
            let tolerance = (amount_in as f64 * 0.001) as u64 + 2;
            let diff = if needed_in >= amount_in {
                needed_in - amount_in
            } else {
                amount_in - needed_in
            };
            assert!(
                diff <= tolerance,
                "Round-trip (base→quote): needed_in={}, amount_in={}, diff={}, tolerance={}",
                needed_in,
                amount_in,
                diff,
                tolerance
            );
        }
    }

    #[test]
    fn test_round_trip_quote_to_base() {
        let damm = make_damm_v1(
            1_000_000_000,
            2_000_000_000,
            25,
            10_000,
            1,
            5,
        );
        let accounts = build_mock_accounts();
        let clock = dummy_clock();
        let input_mint = damm.quote_token_pk;

        for &amount_in in &[1_000u64, 100_000, 1_000_000, 10_000_000, 100_000_000] {
            let amount_out = damm
                .swap_base_in(&accounts, input_mint, amount_in, &clock)
                .unwrap();
            assert!(amount_out > 0);

            // Reverse direction: output is base token
            let needed_in = damm
                .swap_base_out(&accounts, damm.base_token_pk, amount_out, &clock)
                .unwrap();

            // swap_base_in floors the output, so reversing the floored output
            // may need slightly less input. Allow ±tolerance.
            let tolerance = (amount_in as f64 * 0.001) as u64 + 2;
            let diff = if needed_in >= amount_in {
                needed_in - amount_in
            } else {
                amount_in - needed_in
            };
            assert!(
                diff <= tolerance,
                "Round-trip (quote→base): needed_in={}, amount_in={}, diff={}, tolerance={}",
                needed_in,
                amount_in,
                diff,
                tolerance
            );
        }
    }

    /// Forward consistency: swap_base_out(Y) → X, then swap_base_in(X) → Y'
    /// Y' should be >= Y (we over-estimate the input).
    #[test]
    fn test_round_trip_out_then_in_base_to_quote() {
        let damm = make_damm_v1(
            500_000_000,
            1_000_000_000,
            30,
            10_000,
            0, // no protocol fee
            1,
        );
        let accounts = build_mock_accounts();
        let clock = dummy_clock();

        for &desired_out in &[1_000u64, 50_000, 1_000_000, 10_000_000] {
            // How much base do I need to get `desired_out` quote?
            let needed_in = damm
                .swap_base_out(&accounts, damm.quote_token_pk, desired_out, &clock)
                .unwrap();
            assert!(needed_in > 0);

            // Now actually swap that much base in
            let actual_out = damm
                .swap_base_in(&accounts, damm.base_token_pk, needed_in, &clock)
                .unwrap();

            // We should get at least as much as we wanted
            assert!(
                actual_out >= desired_out,
                "Out-then-in: actual_out ({}) should be >= desired_out ({})",
                actual_out,
                desired_out
            );
        }
    }

    /// Zero-fee pool round-trip should be tighter.
    #[test]
    fn test_round_trip_zero_fee() {
        let damm = make_damm_v1(
            1_000_000_000,
            1_000_000_000,
            0, // no fee
            1,
            0,
            1,
        );
        let accounts = build_mock_accounts();
        let clock = dummy_clock();

        for &amount_in in &[1_000u64, 1_000_000, 50_000_000] {
            let amount_out = damm
                .swap_base_in(&accounts, damm.base_token_pk, amount_in, &clock)
                .unwrap();

            let needed_in = damm
                .swap_base_out(&accounts, damm.quote_token_pk, amount_out, &clock)
                .unwrap();

            // With zero fees, needed_in should be exactly amount_in or amount_in+1 (rounding)
            assert!(
                needed_in >= amount_in && needed_in <= amount_in + 1,
                "Zero-fee round-trip: needed_in={}, amount_in={}",
                needed_in,
                amount_in
            );
        }
    }

    /// Verify fee calculation helpers.
    #[test]
    fn test_calculate_trade_fee() {
        let damm = make_damm_v1(1_000_000, 1_000_000, 25, 10_000, 0, 1);
        // 25/10000 = 0.25%
        assert_eq!(damm.calculate_trade_fee(10_000).unwrap(), 25);
        assert_eq!(damm.calculate_trade_fee(0).unwrap(), 0);
        // Small amounts: minimum fee of 1
        assert_eq!(damm.calculate_trade_fee(1).unwrap(), 1);
    }

    #[test]
    fn test_calculate_protocol_fee() {
        let damm = make_damm_v1(1_000_000, 1_000_000, 25, 10_000, 1, 5);
        // protocol = 1/5 = 20% of trade fee
        assert_eq!(damm.calculate_protocol_fee(100).unwrap(), 20);
        assert_eq!(damm.calculate_protocol_fee(0).unwrap(), 0);
        // Small fee: minimum 1
        assert_eq!(damm.calculate_protocol_fee(1).unwrap(), 1);
    }

    /// Constant product invariant: after swap, k' >= k (fees mean k grows).
    #[test]
    fn test_constant_product_invariant() {
        let damm = make_damm_v1(1_000_000_000, 2_000_000_000, 25, 10_000, 1, 5);
        let accounts = build_mock_accounts();
        let clock = dummy_clock();

        let k_before = damm.base_vault_amount as u128 * damm.quote_vault_amount as u128;

        let amount_in: u64 = 10_000_000;
        let amount_out = damm
            .swap_base_in(&accounts, damm.base_token_pk, amount_in, &clock)
            .unwrap();

        let new_base = damm.base_vault_amount as u128 + amount_in as u128;
        let new_quote = damm.quote_vault_amount as u128 - amount_out as u128;
        let k_after = new_base * new_quote;

        assert!(
            k_after >= k_before,
            "k should not decrease: before={}, after={}",
            k_before,
            k_after
        );
    }
}
