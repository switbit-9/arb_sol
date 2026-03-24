use crate::programs::{PoolKind, ProgramMeta, Result};
use crate::utils::token::{apply_transfer_fee, MintFee};
use crate::utils::utils::read_token_amount;
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};
use pinocchio::instruction::AccountMeta;
use pinocchio::sysvars::clock::Clock;
use crate::utils::cpi::invoke_cpi;

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
    data[offset..offset + 32].try_into().unwrap()
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

fn read_mint_supply(account: &AccountInfo) -> Result<u64> {
    let data = unsafe { account.borrow_data_unchecked() };
    if data.len() < MINT_SUPPLY_OFFSET + 8 {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(read_u64(data, MINT_SUPPLY_OFFSET))
}

/// Read the vault's unlocked amount from the Vault state account.
/// unlocked_amount = total_amount - locked_profit(current_time)
/// This matches Vault::get_unlocked_amount from the DAMM v1 SDK.
fn read_vault_unlocked_amount(vault_account: &AccountInfo, current_time: u64) -> Result<u64> {
    let data = unsafe { vault_account.borrow_data_unchecked() };
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

#[derive(Clone)]
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
    /// Pre-computed fee factor: 1 - trade_fee_numerator/trade_fee_denominator
    pub fee_factor: (f64, f64),
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
    fn pool_kind(&self) -> PoolKind { PoolKind::MeteoraDammV1 }

    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        Ok((self.base_vault_amount, self.quote_vault_amount))
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        let inverse = if self.price > 0.0 { 1.0 / self.price } else { 0.0 };
        Ok((self.price, inverse))
    }

    fn get_fee_factor(&self) -> Result<(f64, f64)> { Ok(self.fee_factor) }

    fn get_max_amount_in(&self, _accounts: &[AccountInfo], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_in) } else { Ok(self.sell_max_in) }
    }

    fn get_max_amount_out(&self, _accounts: &[AccountInfo], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_out) } else { Ok(self.sell_max_out) }
    }

    fn get_cached_max_amounts(&self, input_mint: Pubkey) -> (u64, u64) {
        if input_mint == self.base_token_pk { (self.buy_max_in, self.buy_max_out) } else { (self.sell_max_in, self.sell_max_out) }
    }

    fn fast_quote(&mut self, _accounts: &[AccountInfo], input_mint: Pubkey, amount_in: u64, _profit_pct: f64) -> Result<(u64, u64)> {
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

    fn swap_base_in(
        &mut self,
        accounts: &[AccountInfo],
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

    fn swap_base_out(
        &mut self,
        accounts: &[AccountInfo],
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

    fn invoke_swap_base_in(
        &mut self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        max_amount_in: u64,
        amount_out: Option<u64>,
        payer: &AccountInfo,
        user_mint_1_token_account: &AccountInfo,
        user_mint_2_token_account: &AccountInfo,
        mint_1_account: &AccountInfo,
        mint_2_account: &AccountInfo,
        mint_1_token_program: &AccountInfo,
        mint_2_token_program: &AccountInfo,
    ) -> Result<()> {
        let (user_source_token, user_destination_token) =
            if input_mint == *mint_1_account.key() {
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
        let metas = [
            AccountMeta::new(pool.key(), true, false),                  // pool
            AccountMeta::new(user_source_token.key(), true, false),     // user_source_token
            AccountMeta::new(user_destination_token.key(), true, false),// user_destination_token
            AccountMeta::new(a_vault.key(), true, false),               // a_vault
            AccountMeta::new(b_vault.key(), true, false),               // b_vault
            AccountMeta::new(a_token_vault.key(), true, false),         // a_token_vault
            AccountMeta::new(b_token_vault.key(), true, false),         // b_token_vault
            AccountMeta::new(a_vault_lp_mint.key(), true, false),       // a_vault_lp_mint
            AccountMeta::new(b_vault_lp_mint.key(), true, false),       // b_vault_lp_mint
            AccountMeta::new(a_vault_lp.key(), true, false),            // a_vault_lp
            AccountMeta::new(b_vault_lp.key(), true, false),            // b_vault_lp
            AccountMeta::new(protocol_token_fee.key(), true, false),    // protocol_token_fee
            AccountMeta::new(payer.key(), true, true),                  // user (signer)
            AccountMeta::new(vault_program.key(), false, false),        // vault_program
            AccountMeta::new(token_program.key(), false, false),        // token_program
        ];

        // Anchor swap discriminator: sha256("global:swap")[..8]
        let mut data = [0u8; 24];
        data[..8].copy_from_slice(&Self::SWAP_DISC);
        data[8..16].copy_from_slice(&max_amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&minimum_out.to_le_bytes());

        let accs: [&AccountInfo; 15] = [
            pool,
            user_source_token,
            user_destination_token,
            a_vault,
            b_vault,
            a_token_vault,
            b_token_vault,
            a_vault_lp_mint,
            b_vault_lp_mint,
            a_vault_lp,
            b_vault_lp,
            protocol_token_fee,
            payer,
            vault_program,
            token_program,
        ];
        invoke_cpi(&Self::PROGRAM_ID, &metas, &data, &accs)?;
        Ok(())
    }

    fn invoke_swap_base_out(
        &mut self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        amount_in: u64,
        min_amount_out: Option<u64>,
        payer: &AccountInfo,
        user_mint_1_token_account: &AccountInfo,
        user_mint_2_token_account: &AccountInfo,
        mint_1_account: &AccountInfo,
        mint_2_account: &AccountInfo,
        mint_1_token_program: &AccountInfo,
        mint_2_token_program: &AccountInfo,
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
    fn log_accounts(&self, accounts: &[AccountInfo]) -> Result<()> {
        pinocchio::log::sol_log("=== Meteora DAMM V1 ===");
        Ok(())
    }
}

impl MeteoraDammV1 {
    pub const PROGRAM_ID: Pubkey =
        five8_const::decode_32_const("Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB");
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

    pub const DYNAMIC_ACCOUNTS: usize = 13;

    pub fn new(
        accounts: &[AccountInfo],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
        clock: &Clock,
    ) -> Result<Self> {
        if dyn_end - dyn_start < Self::DYNAMIC_ACCOUNTS || dyn_end > accounts.len() {
            return Err(crate::programs::SolarBError::InsufficientAccounts.into());
        }

        let pool_account = &accounts[dyn_start + Self::D_POOL];
        let pool_data = unsafe { pool_account.borrow_data_unchecked() };

        // Parse Pool state from Borsh-serialized data (after 8-byte Anchor discriminator)
        let d = &pool_data[8..];

        let token_a_mint = read_pubkey(d, TOKEN_A_MINT_OFFSET);
        let token_b_mint = read_pubkey(d, TOKEN_B_MINT_OFFSET);
        // Placeholder: fee rates will be read from on-chain account data
        let trade_fee_numerator = 0u64;
        let trade_fee_denominator = 1_000_000u64;
        let protocol_fee_numerator = 0u64;
        let protocol_fee_denominator = 1u64;

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

        debug_eprintln!("MeteoraDammV1: pool_id {:?}, price {}, inverse_price {}, fee {}/{}", pool_account.key(), price, 1.0 / price, trade_fee_numerator, trade_fee_denominator);

        // Defer max amounts and transfer fees to prepare_for_execution()
        let instance = MeteoraDammV1 {
            pool_id: *pool_account.key(),
            base_token_pk: token_a_mint,
            quote_token_pk: token_b_mint,
            base_vault_amount,
            quote_vault_amount,
            price,
            trade_fee_numerator,
            trade_fee_denominator,
            protocol_fee_numerator,
            protocol_fee_denominator,
            fee_factor: { let f = 1.0 - trade_fee_numerator as f64 / trade_fee_denominator as f64; (f, f) },
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
    pub fn prepare_for_execution(
        &mut self,
        _accounts: &[AccountInfo],
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

// TODO: tests need rewriting with LiteSVM/Mollusk — pinocchio AccountInfo is not
// constructable from anchor/solana_sdk types. The old RPC-based tests have been removed.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::token::MintFee;


    // TODO: rewrite tests using LiteSVM/Mollusk
}
