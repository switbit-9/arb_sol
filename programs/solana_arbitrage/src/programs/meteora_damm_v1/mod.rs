use crate::programs::ProgramMeta;
use crate::utils::token::{apply_transfer_fee, MintFee};
use crate::utils::utils::read_token_amount;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};

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

pub struct MeteoraDammV1 {
    pub pool_id: Pubkey,
    pub base_token_pk: Pubkey,
    pub quote_token_pk: Pubkey,
    pub base_vault_amount: u64,
    pub quote_vault_amount: u64,
    pub price: f64,
    pub trade_fee_numerator: u64,
    pub trade_fee_denominator: u64,
    pub protocol_fee_numerator: u64,
    pub protocol_fee_denominator: u64,
    pub static_base: usize,
    pub dyn_start: usize,
    pub buy_max_in: u64,
    pub buy_max_out: u64,
    pub sell_max_in: u64,
    pub sell_max_out: u64,
    pub prepared: bool,
}

impl ProgramMeta for MeteoraDammV1 {
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
        let inverse = if self.price > 0.0 { 1.0 / self.price } else { 0.0 };
        Ok((self.price, inverse))
    }

    fn get_fee_factor(&self) -> Result<(f64, f64)> {
        let f = 1.0 - self.trade_fee_numerator as f64 / self.trade_fee_denominator as f64;
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

    fn fast_quote(&mut self, input_mint: Pubkey, amount_in: u64, _profit_pct: f64) -> Result<(u64, u64)> {
        let (max_in, max_out) = self.get_cached_max_amounts(input_mint);
        let amount_in = amount_in.min(max_in);

        let (reserve_in, reserve_out) = if input_mint == self.base_token_pk {
            (self.base_vault_amount as u128, self.quote_vault_amount as u128)
        } else {
            (self.quote_vault_amount as u128, self.base_vault_amount as u128)
        };

        // Trade fee (same as calculate_trade_fee but inline)
        let trade_fee = if self.trade_fee_numerator == 0 || amount_in == 0 {
            0u128
        } else {
            let f = (amount_in as u128)
                .saturating_mul(self.trade_fee_numerator as u128)
                / (self.trade_fee_denominator as u128);
            if f == 0 { 1 } else { f }
        };

        // Protocol fee is a subset of trade fee
        let protocol_fee = if self.protocol_fee_numerator == 0 || trade_fee == 0 {
            0u128
        } else {
            let f = trade_fee
                .saturating_mul(self.protocol_fee_numerator as u128)
                / (self.protocol_fee_denominator as u128);
            if f == 0 { 1 } else { f }
        };

        let amount_after_protocol = (amount_in as u128).saturating_sub(protocol_fee);
        let net_trade_fee = trade_fee.saturating_sub(protocol_fee);
        let effective_in = amount_after_protocol.saturating_sub(net_trade_fee);

        // Constant product: out = reserve_out * effective_in / (reserve_in + effective_in)
        let denominator = reserve_in.saturating_add(effective_in);
        if denominator == 0 { return Ok((amount_in, 0)); }
        let out = reserve_out.saturating_mul(effective_in) / denominator;
        let out = out.min(u64::MAX as u128) as u64;
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
        let (reserve_in, reserve_out) = if input_mint == self.base_token_pk {
            (self.base_vault_amount as u128, self.quote_vault_amount as u128)
        } else {
            (self.quote_vault_amount as u128, self.base_vault_amount as u128)
        };

        let transfer_fee_in = apply_transfer_fee(amount_in, input_transfer_fee);
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

        let transfer_fee_out = apply_transfer_fee(amount_out_u64, output_transfer_fee);
        let amount_out_final = amount_out_u64.checked_sub(transfer_fee_out).unwrap();

        Ok(amount_out_final)
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
        let (reserve_in, reserve_out) = if output_mint == self.base_token_pk {
            (self.quote_vault_amount as u128, self.base_vault_amount as u128)
        } else {
            (self.base_vault_amount as u128, self.quote_vault_amount as u128)
        };

        let transfer_fee_out = apply_transfer_fee(amount_out, output_transfer_fee);
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

        let transfer_fee_in = apply_transfer_fee(amount_in_u64, input_transfer_fee);
        let total_in = amount_in_u64
            .checked_add(transfer_fee_in)
            .ok_or(ProgramError::InvalidArgument)?;

        Ok(total_in)
    }

    fn invoke_swap_base_in<'a>(
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
        let (user_source_token, user_destination_token) =
            if input_mint == *mint_1_account.key {
                (user_mint_1_token_account, user_mint_2_token_account)
            } else {
                (user_mint_2_token_account, user_mint_1_token_account)
            };

        // Protocol fee account depends on input token direction
        let protocol_token_fee = if input_mint == self.base_token_pk {
            &accounts[self.dyn_start + Self::D_PROTO_FEE_A]
        } else {
            &accounts[self.dyn_start + Self::D_PROTO_FEE_B]
        };

        let pool = &accounts[self.dyn_start + Self::D_POOL];
        let a_vault = &accounts[self.dyn_start + Self::D_A_VAULT];
        let b_vault = &accounts[self.dyn_start + Self::D_B_VAULT];
        let a_token_vault = &accounts[self.dyn_start + Self::D_A_TOKEN_VAULT];
        let b_token_vault = &accounts[self.dyn_start + Self::D_B_TOKEN_VAULT];
        let a_vault_lp_mint = &accounts[self.dyn_start + Self::D_A_VAULT_LP_MINT];
        let b_vault_lp_mint = &accounts[self.dyn_start + Self::D_B_VAULT_LP_MINT];
        let a_vault_lp = &accounts[self.dyn_start + Self::D_A_VAULT_LP];
        let b_vault_lp = &accounts[self.dyn_start + Self::D_B_VAULT_LP];
        let vault_program = &accounts[self.dyn_start + Self::D_VAULT_PROGRAM];
        let token_program = &accounts[self.dyn_start + Self::D_TOKEN_PROGRAM];

        let minimum_out = amount_out.unwrap_or(0);

        // Swap instruction account layout (from SDK swap.rs)
        let metas = vec![
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
        let mut data = [0u8; 24];
        data[..8].copy_from_slice(&Self::SWAP_DISC);
        data[8..16].copy_from_slice(&max_amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&minimum_out.to_le_bytes());

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data: data.to_vec(),
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

    #[cfg(any(test, feature = "debug"))]
    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        msg!("=== Meteora DAMM V1 ===");
        msg!("[S] 0 program_id: {}", accounts[self.static_base + Self::S_PROGRAM_ID].key);
        msg!("[D] 0 pool: {}", accounts[self.dyn_start + Self::D_POOL].key);
        msg!("[D] 1 a_vault: {}", accounts[self.dyn_start + Self::D_A_VAULT].key);
        msg!("[D] 2 b_vault: {}", accounts[self.dyn_start + Self::D_B_VAULT].key);
        msg!("[D] 3 a_token_vault: {}", accounts[self.dyn_start + Self::D_A_TOKEN_VAULT].key);
        msg!("[D] 4 b_token_vault: {}", accounts[self.dyn_start + Self::D_B_TOKEN_VAULT].key);
        msg!("[D] 5 a_vault_lp_mint: {}", accounts[self.dyn_start + Self::D_A_VAULT_LP_MINT].key);
        msg!("[D] 6 b_vault_lp_mint: {}", accounts[self.dyn_start + Self::D_B_VAULT_LP_MINT].key);
        msg!("[D] 7 a_vault_lp: {}", accounts[self.dyn_start + Self::D_A_VAULT_LP].key);
        msg!("[D] 8 b_vault_lp: {}", accounts[self.dyn_start + Self::D_B_VAULT_LP].key);
        msg!("[D] 9 proto_fee_a: {}", accounts[self.dyn_start + Self::D_PROTO_FEE_A].key);
        msg!("[D] 10 proto_fee_b: {}", accounts[self.dyn_start + Self::D_PROTO_FEE_B].key);
        msg!("[D] 11 vault_program: {}", accounts[self.dyn_start + Self::D_VAULT_PROGRAM].key);
        msg!("[D] 12 token_program: {}", accounts[self.dyn_start + Self::D_TOKEN_PROGRAM].key);
        msg!("token_a_mint (from pool): {}", self.base_token_pk);
        msg!("token_b_mint (from pool): {}", self.quote_token_pk);
        Ok(())
    }
}

impl MeteoraDammV1 {
    pub const PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB");
    const SWAP_DISC: [u8; 8] = [0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];

    // ── Static account indices (from static_base, 1 account) ──
    pub const S_PROGRAM_ID: usize = 0;

    // ── Dynamic account indices (from dyn_start, 13 accounts) ──
    pub const D_POOL: usize = 0;
    pub const D_A_VAULT: usize = 1;
    pub const D_B_VAULT: usize = 2;
    pub const D_A_TOKEN_VAULT: usize = 3;
    pub const D_B_TOKEN_VAULT: usize = 4;
    pub const D_A_VAULT_LP_MINT: usize = 5;
    pub const D_B_VAULT_LP_MINT: usize = 6;
    pub const D_A_VAULT_LP: usize = 7;
    pub const D_B_VAULT_LP: usize = 8;
    pub const D_PROTO_FEE_A: usize = 9;
    pub const D_PROTO_FEE_B: usize = 10;
    pub const D_VAULT_PROGRAM: usize = 11;
    pub const D_TOKEN_PROGRAM: usize = 12;

    pub const MIN_ACCOUNTS: usize = 13;

    pub fn new<'a>(
        accounts: &[AccountInfo<'a>],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
        clock: &Clock,
        pool_fees: &[u32],
    ) -> Result<Self> {
        require!(
            dyn_end - dyn_start >= Self::MIN_ACCOUNTS,
            crate::programs::SolarBError::InsufficientAccounts
        );
        require!(
            dyn_end <= accounts.len(),
            crate::programs::SolarBError::InsufficientAccounts
        );

        let pool_account = &accounts[dyn_start + Self::D_POOL];
        let pool_data = pool_account.try_borrow_data()?;

        // Parse Pool state from Borsh-serialized data (after 8-byte Anchor discriminator)
        let d = &pool_data[8..];

        let token_a_mint = read_pubkey(d, TOKEN_A_MINT_OFFSET);
        let token_b_mint = read_pubkey(d, TOKEN_B_MINT_OFFSET);
        // trade_fee from client-side pool_fees[0] (millionths), reconstruct as num/den
        let trade_fee_numerator = pool_fees[0] as u64;
        let trade_fee_denominator = 1_000_000u64;
        let protocol_fee_numerator = 0u64;
        let protocol_fee_denominator = 1u64;

        drop(pool_data);

        // Compute effective reserves using vault unlocked amount (total_amount - locked_profit):
        // effective_amount = vault_unlocked_amount * pool_vault_lp_amount / vault_lp_supply
        let a_vault = &accounts[dyn_start + Self::D_A_VAULT];
        let b_vault = &accounts[dyn_start + Self::D_B_VAULT];
        let a_vault_lp_mint = &accounts[dyn_start + Self::D_A_VAULT_LP_MINT];
        let b_vault_lp_mint = &accounts[dyn_start + Self::D_B_VAULT_LP_MINT];
        let a_vault_lp = &accounts[dyn_start + Self::D_A_VAULT_LP];
        let b_vault_lp = &accounts[dyn_start + Self::D_B_VAULT_LP];

        let current_time: u64 = clock.unix_timestamp as u64;
        let vault_a_unlocked = read_vault_unlocked_amount(a_vault, current_time)?;
        let vault_b_unlocked = read_vault_unlocked_amount(b_vault, current_time)?;
        let pool_a_lp_amount = read_token_amount(a_vault_lp)?;
        let pool_b_lp_amount = read_token_amount(b_vault_lp)?;
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

        let price = if base_vault_amount > 0 && quote_vault_amount > 0 {
            quote_vault_amount as f64 / base_vault_amount as f64
        } else {
            0.0
        };

        // Defer max amounts and transfer fees to prepare_for_execution()
        let instance = MeteoraDammV1 {
            pool_id: *pool_account.key,
            base_token_pk: token_a_mint,
            quote_token_pk: token_b_mint,
            base_vault_amount,
            quote_vault_amount,
            price,
            trade_fee_numerator,
            trade_fee_denominator,
            protocol_fee_numerator,
            protocol_fee_denominator,
            static_base,
            dyn_start,
            buy_max_in: 0,
            buy_max_out: 0,
            sell_max_in: 0,
            sell_max_out: 0,
            prepared: false,
        };
        // instance.log_accounts(accounts)?;
        Ok(instance)
    }

    /// Symmetric CP max amounts: computed once during init (integer math).
    fn compute_cached_max(base_vault: u64, quote_vault: u64, fee_num: u64, fee_den: u64) -> (u64, u64, u64, u64) {
        fn cp_max(x: u64, y: u64, fee_num: u64, fee_den: u64) -> (u64, u64) {
            let ff = (fee_den as u128).saturating_sub(fee_num as u128);
            if ff == 0 || y == 0 {
                return (0, y);
            }
            let dx = (x as u128).saturating_mul(99).saturating_mul(fee_den as u128) / ff;
            (dx.min(u64::MAX as u128) as u64, y)
        }
        let (buy_in, buy_out) = cp_max(base_vault, quote_vault, fee_num, fee_den);
        let (sell_in, sell_out) = cp_max(quote_vault, base_vault, fee_num, fee_den);
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

    /// Compute deferred fields: max amounts, transfer fee rates.
    /// Called only for instances that participate in a profitable arb path.
    pub fn prepare_for_execution<'a>(
        &mut self,
        _accounts: &[AccountInfo<'a>],
    ) {
        if self.prepared {
            return;
        }
        self.prepared = true;

        let (buy_in, buy_out, sell_in, sell_out) =
            Self::compute_cached_max(self.base_vault_amount, self.quote_vault_amount, self.trade_fee_numerator, self.trade_fee_denominator);
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

    async fn fetch_account_info_from_rpc(
        rpc_client: &RpcClient,
        key: Pubkey,
    ) -> AccountInfo<'static> {
        use solana_sdk::pubkey::Pubkey as SdkPubkey;
        let sdk_pubkey = SdkPubkey::try_from(key.to_bytes().as_ref())
            .expect("Failed to convert Pubkey");
        let account = rpc_client.get_account(&sdk_pubkey).await
            .unwrap_or_else(|e| panic!("Failed to fetch account {}: {}", key, e));
        account_to_account_info(key, account)
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

    fn get_rpc_client() -> RpcClient {
        let api_key = "f230200b-f911-43c1-a242-4e7b066d0993";
        RpcClient::new(format!("https://mainnet.helius-rpc.com/?api-key={}", api_key))
    }

    /// Build a MeteoraDammV1 instance from a pool_id by fetching all needed accounts from RPC.
    async fn build_from_pool_id(
        pool_id: Pubkey,
    ) -> (MeteoraDammV1, Vec<AccountInfo<'static>>, Clock) {
        use solana_sdk::pubkey::Pubkey as SdkPubkey;

        let rpc_client = get_rpc_client();

        let sdk_pool_id = SdkPubkey::try_from(pool_id.to_bytes().as_ref()).unwrap();
        let pool_account = rpc_client.get_account(&sdk_pool_id).await
            .unwrap_or_else(|e| panic!("Failed to fetch pool {}: {}", pool_id, e));

        // Parse pool state (after 8-byte Anchor discriminator)
        let d = &pool_account.data[8..];
        let token_a_mint_key = Pubkey::new_from_array(d[32..64].try_into().unwrap());
        let token_b_mint_key = Pubkey::new_from_array(d[64..96].try_into().unwrap());
        let a_vault_key = Pubkey::new_from_array(d[96..128].try_into().unwrap());
        let b_vault_key = Pubkey::new_from_array(d[128..160].try_into().unwrap());
        let a_vault_lp_key = Pubkey::new_from_array(d[160..192].try_into().unwrap());
        let b_vault_lp_key = Pubkey::new_from_array(d[192..224].try_into().unwrap());
        // admin_token_a_fee at 226, admin_token_b_fee at 258
        let proto_fee_a_key = Pubkey::new_from_array(d[226..258].try_into().unwrap());
        let proto_fee_b_key = Pubkey::new_from_array(d[258..290].try_into().unwrap());
        // trade_fee from pool data
        let trade_fee_num = read_u64(d, TRADE_FEE_NUM_OFFSET);
        let trade_fee_den = read_u64(d, TRADE_FEE_DEN_OFFSET);

        eprintln!("MeteoraDammV1 pool: {}", pool_id);
        eprintln!("  token_a_mint: {}", token_a_mint_key);
        eprintln!("  token_b_mint: {}", token_b_mint_key);
        eprintln!("  a_vault: {}", a_vault_key);
        eprintln!("  b_vault: {}", b_vault_key);
        eprintln!("  trade_fee: {}/{}", trade_fee_num, trade_fee_den);

        // Fetch vault accounts to parse token_vault and lp_mint
        let a_vault_account = rpc_client
            .get_account(&SdkPubkey::try_from(a_vault_key.to_bytes().as_ref()).unwrap())
            .await.expect("Failed to fetch a_vault");
        let b_vault_account = rpc_client
            .get_account(&SdkPubkey::try_from(b_vault_key.to_bytes().as_ref()).unwrap())
            .await.expect("Failed to fetch b_vault");

        // Vault layout (after disc): offset 11 = token_vault, offset 107 = lp_mint
        let a_vault_data = &a_vault_account.data[8..];
        let b_vault_data = &b_vault_account.data[8..];
        let a_token_vault_key = Pubkey::new_from_array(a_vault_data[11..43].try_into().unwrap());
        let b_token_vault_key = Pubkey::new_from_array(b_vault_data[11..43].try_into().unwrap());
        let a_vault_lp_mint_key = Pubkey::new_from_array(a_vault_data[107..139].try_into().unwrap());
        let b_vault_lp_mint_key = Pubkey::new_from_array(b_vault_data[107..139].try_into().unwrap());

        eprintln!("  a_token_vault: {}", a_token_vault_key);
        eprintln!("  b_token_vault: {}", b_token_vault_key);
        eprintln!("  a_vault_lp_mint: {}", a_vault_lp_mint_key);
        eprintln!("  b_vault_lp_mint: {}", b_vault_lp_mint_key);

        // Fetch all sub-accounts from RPC
        let a_token_vault_info = fetch_account_info_from_rpc(&rpc_client, a_token_vault_key).await;
        let b_token_vault_info = fetch_account_info_from_rpc(&rpc_client, b_token_vault_key).await;
        let a_vault_lp_mint_info = fetch_account_info_from_rpc(&rpc_client, a_vault_lp_mint_key).await;
        let b_vault_lp_mint_info = fetch_account_info_from_rpc(&rpc_client, b_vault_lp_mint_key).await;
        let a_vault_lp_info = fetch_account_info_from_rpc(&rpc_client, a_vault_lp_key).await;
        let b_vault_lp_info = fetch_account_info_from_rpc(&rpc_client, b_vault_lp_key).await;

        let program_id_info = create_mock_account_info_with_data(
            MeteoraDammV1::PROGRAM_ID, anchor_lang::solana_program::system_program::id(), None,
        );
        let pool_id_info = account_to_account_info(pool_id, pool_account);
        let a_vault_info = account_to_account_info(a_vault_key, a_vault_account);
        let b_vault_info = account_to_account_info(b_vault_key, b_vault_account);
        let proto_fee_a_info = create_mock_account_info_with_data(
            proto_fee_a_key, anchor_lang::solana_program::system_program::id(), None,
        );
        let proto_fee_b_info = create_mock_account_info_with_data(
            proto_fee_b_key, anchor_lang::solana_program::system_program::id(), None,
        );
        let vault_program_info = create_mock_account_info_with_data(
            Pubkey::new_unique(), anchor_lang::solana_program::system_program::id(), None,
        );
        let token_program_info = create_mock_account_info_with_data(
            anchor_spl::token::ID, anchor_lang::solana_program::system_program::id(), None,
        );

        // Layout:
        // Static (static_base=0): [program_id]
        // Dynamic (dyn_start=1): [pool, a_vault, b_vault, a_token_vault, b_token_vault,
        //   a_vault_lp_mint, b_vault_lp_mint, a_vault_lp, b_vault_lp,
        //   proto_fee_a, proto_fee_b, vault_program, token_program]
        let accounts = vec![
            program_id_info,       // S0
            pool_id_info,          // D0 = D_POOL
            a_vault_info,          // D1 = D_A_VAULT
            b_vault_info,          // D2 = D_B_VAULT
            a_token_vault_info,    // D3 = D_A_TOKEN_VAULT
            b_token_vault_info,    // D4 = D_B_TOKEN_VAULT
            a_vault_lp_mint_info,  // D5 = D_A_VAULT_LP_MINT
            b_vault_lp_mint_info,  // D6 = D_B_VAULT_LP_MINT
            a_vault_lp_info,       // D7 = D_A_VAULT_LP
            b_vault_lp_info,       // D8 = D_B_VAULT_LP
            proto_fee_a_info,      // D9 = D_PROTO_FEE_A
            proto_fee_b_info,      // D10 = D_PROTO_FEE_B
            vault_program_info,    // D11 = D_VAULT_PROGRAM
            token_program_info,    // D12 = D_TOKEN_PROGRAM
        ];

        let static_base: usize = 0;
        let dyn_start: usize = 1;
        let dyn_end: usize = accounts.len();

        // Convert trade_fee to millionths for pool_fees
        let fee_millionths = if trade_fee_den > 0 {
            ((trade_fee_num as u128 * 1_000_000) / trade_fee_den as u128) as u32
        } else {
            0
        };
        let pool_fees = [fee_millionths];

        let clock = get_clock_from_rpc(&rpc_client).await;

        let damm = MeteoraDammV1::new(&accounts, static_base, dyn_start, dyn_end, &clock, &pool_fees)
            .expect("MeteoraDammV1::new failed");

        eprintln!("  price: {}", damm.price);
        eprintln!("  base_vault_amount: {}", damm.base_vault_amount);
        eprintln!("  quote_vault_amount: {}", damm.quote_vault_amount);

        (damm, accounts, clock)
    }

    // ---- Tests ----

    #[tokio::test]
    async fn test_damm_v1_round_trip() {
        // SOL-USDC Meteora DAMM V1 pool
        let pool_id = Pubkey::from_str_const("HcjZvfeSNJbNkfLD4eEcRBr96AD3w1GpmMppaeRZf7ur");
        let (mut damm, accounts, clock) = build_from_pool_id(pool_id).await;

        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let no_fee = MintFee::ZERO;

        // 1. Print all account pubkeys
        eprintln!("\n=== Account Pubkeys ===");
        eprintln!("base_mint        : {}", damm.base_token_pk);
        eprintln!("quote_mint       : {}", damm.quote_token_pk);
        eprintln!("pool_id          : {}", accounts[1 + MeteoraDammV1::D_POOL].key);
        eprintln!("a_vault          : {}", accounts[1 + MeteoraDammV1::D_A_VAULT].key);
        eprintln!("b_vault          : {}", accounts[1 + MeteoraDammV1::D_B_VAULT].key);
        eprintln!("a_token_vault    : {}", accounts[1 + MeteoraDammV1::D_A_TOKEN_VAULT].key);
        eprintln!("b_token_vault    : {}", accounts[1 + MeteoraDammV1::D_B_TOKEN_VAULT].key);
        eprintln!("a_vault_lp_mint  : {}", accounts[1 + MeteoraDammV1::D_A_VAULT_LP_MINT].key);
        eprintln!("b_vault_lp_mint  : {}", accounts[1 + MeteoraDammV1::D_B_VAULT_LP_MINT].key);
        eprintln!("a_vault_lp       : {}", accounts[1 + MeteoraDammV1::D_A_VAULT_LP].key);
        eprintln!("b_vault_lp       : {}", accounts[1 + MeteoraDammV1::D_B_VAULT_LP].key);
        eprintln!("program_id       : {}", accounts[MeteoraDammV1::S_PROGRAM_ID].key);

        // 2. Print price and inverse_price
        let (price, inverse_price) = damm.get_prices().unwrap();
        eprintln!("\n=== Prices ===");
        eprintln!("price            : {}", price);
        eprintln!("inverse_price    : {}", inverse_price);

        // 3. Print fees
        let (fee_factor, fee_factor_2) = damm.get_fee_factor().unwrap();
        eprintln!("\n=== Fees ===");
        eprintln!("trade_fee_num    : {}", damm.trade_fee_numerator);
        eprintln!("trade_fee_den    : {}", damm.trade_fee_denominator);
        eprintln!("fee_factor       : {}", fee_factor);
        eprintln!("fee_factor_2     : {}", fee_factor_2);

        // 4. prepare_for_execution
        damm.prepare_for_execution(&accounts);
        eprintln!("\n=== After prepare_for_execution ===");
        eprintln!("buy_max_in       : {}", damm.buy_max_in);
        eprintln!("buy_max_out      : {}", damm.buy_max_out);
        eprintln!("sell_max_in      : {}", damm.sell_max_in);
        eprintln!("sell_max_out     : {}", damm.sell_max_out);

        // 5. Round-trip with start_amount = 1 WSOL
        let start_amount: u64 = 1_000_000_000; // 1 SOL

        let other_mint = if damm.base_token_pk == sol_mint {
            damm.quote_token_pk
        } else {
            damm.base_token_pk
        };

        // Direction 1: SOL -> TOKEN -> SOL
        eprintln!("\n=== Direction 1: SOL -> TOKEN -> SOL ===");
        let token_out = damm.swap_base_in(
            &accounts, sol_mint, start_amount, no_fee, no_fee, &clock,
        ).unwrap();
        let max_sol_in = damm.swap_base_out(
            &accounts, other_mint, token_out, no_fee, no_fee, &clock,
        ).unwrap();
        eprintln!("AMOUNT_IN {} -> AMOUNT_OUT {} -> MAX_AMOUNT_IN {}", start_amount, token_out, max_sol_in);

        // Direction 2: TOKEN -> SOL -> TOKEN
        eprintln!("\n=== Direction 2: TOKEN -> SOL -> TOKEN ===");
        let sol_out = damm.swap_base_in(
            &accounts, other_mint, token_out, no_fee, no_fee, &clock,
        ).unwrap();
        let max_token_in = damm.swap_base_out(
            &accounts, sol_mint, sol_out, no_fee, no_fee, &clock,
        ).unwrap();
        eprintln!("AMOUNT_IN {} -> AMOUNT_OUT {} -> MAX_AMOUNT_IN {}", token_out, sol_out, max_token_in);
    }
}
