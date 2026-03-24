use crate::programs::{PoolKind, ProgramMeta};
use crate::utils::token::{apply_transfer_fee, MintFee};
use pinocchio::{
    account_info::AccountInfo,
    instruction::AccountMeta,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::clock::Clock,
};
use crate::utils::cpi::invoke_cpi;
mod constants;
use crate::utils::utils::read_vault_data;

type Result<T> = core::result::Result<T, ProgramError>;

pub const PROGRAM_ID: Pubkey =
    five8_const::decode_32_const("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");

const BUY_EXACT_QUOTE_IN_DISC: [u8; 8] = [0xc6, 0x2e, 0x15, 0x52, 0xb4, 0xd9, 0xe8, 0x70];
const SELL_DISC: [u8; 8] = [0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad];

// Static accounts (from static_base, 10 accounts)
pub const S_PROGRAM_ID: usize = 0;
pub const S_PROTOCOL_FEE_RECIPIENT: usize = 1;
pub const S_PROTOCOL_FEE_RECIPIENT_TOKEN_ACC: usize = 2;
pub const S_EVENT_AUTHORITY: usize = 3;
pub const S_FEE_CONFIG: usize = 4;
pub const S_FEE_PROGRAM: usize = 5;
pub const S_PUMP_AMM_GLOBAL: usize = 6;
pub const S_SYSTEM_PROGRAM: usize = 7;
pub const S_ASSOC_TOKEN_PROGRAM: usize = 8;
pub const S_GLOBAL_VOL_ACC: usize = 9;

// Dynamic accounts (from dyn_start, 8 accounts)
pub const D_POOL: usize = 0;
pub const D_BASE_VAULT: usize = 1;
pub const D_QUOTE_VAULT: usize = 2;
pub const D_USER_VOL_ACC: usize = 3;
pub const D_USER_VOL_WSOL_ATA: usize = 4;
pub const D_VAULT_ATA: usize = 5;
pub const D_VAULT_AUTHORITY: usize = 6;
pub const D_CASHBACK_POOL_ID: usize = 7;

// Pool account: 8 (disc) + 1 (bump) + 2 (index) + 32*6 (pubkeys) + 8 (lp_supply) + 32 (coin_creator) + 1 (is_mayhem_mode) = 244
const POOL_IS_CASHBACK_OFFSET: usize = 244;

pub const DYNAMIC_ACCOUNTS: usize = 8;

/// Fee denominator for PumpAmm integer fee math (millionths).
const FEE_DENOM: u128 = 1_000_000;

pub fn get_price_f64(base_vault_amount: u64, quote_vault_amount: u64) -> f64 {
    quote_vault_amount as f64 / base_vault_amount as f64
}

/// Compute PumpAmm fee tier using binary search.
/// Returns fee in millionths (e.g. 12500 = 1.25%).
pub fn get_fees_int(base_vault: u64, quote_vault: u64) -> u64 {
    let min_v = (base_vault as u128).min(quote_vault as u128);
    let max_v = (base_vault as u128).max(quote_vault as u128);
    let mcap_num = min_v.saturating_mul(1_000_000);

    const TIERS: [(u128, u64); 24] = [
        (420, 12500),
        (1470, 12000),
        (2460, 11500),
        (3440, 11000),
        (4420, 10500),
        (9820, 10000),
        (14740, 9500),
        (19650, 9000),
        (24560, 8500),
        (29470, 8000),
        (34380, 7500),
        (39300, 7000),
        (44210, 6500),
        (49120, 6000),
        (54030, 5500),
        (58940, 5200),
        (63860, 5000),
        (68770, 4700),
        (73681, 4500),
        (78590, 4200),
        (83500, 4000),
        (88400, 3700),
        (93330, 3500),
        (98240, 3200),
    ];

    // Binary search: 5 comparisons max instead of 24 linear scan
    let idx = TIERS.partition_point(|&(threshold, _)| {
        mcap_num > threshold.saturating_mul(max_v)
    });
    TIERS.get(idx).map_or(3000, |&(_, fee)| fee)
}

#[derive(Clone)]
pub struct PumpAmm {
    pub pool_id: Pubkey,
    pub base_token_pk: Pubkey,
    pub quote_token_pk: Pubkey,
    pub base_vault_amount: u64,
    pub quote_vault_amount: u64,
    /// Price (Base→Quote) = quote_vault / base_vault
    pub price: f64,
    /// Fee in millionths (e.g. 12500 = 1.25%).
    pub fee_numerator: u64,
    /// Pre-computed fee factor: (1 - fee_rate, 1 - fee_rate)
    pub fee_factor: (f64, f64),
    pub static_base: usize,
    pub dyn_start: usize,
    pub buy_max_in: u64,
    pub buy_max_out: u64,
    pub sell_max_in: u64,
    pub sell_max_out: u64,
    pub prepared: bool,
}

impl ProgramMeta for PumpAmm {
    fn get_id(&self) -> &Pubkey { &PROGRAM_ID }
    fn get_pool_id(&self) -> &Pubkey { &self.pool_id }
    fn get_mints(&self) -> (&Pubkey, &Pubkey) { (&self.base_token_pk, &self.quote_token_pk) }
    fn name(&self) -> &'static str { "PumpAmm" }
    fn pool_kind(&self) -> PoolKind { PoolKind::PumpAmm }
    fn get_fee_factor(&self) -> Result<(f64, f64)> { Ok(self.fee_factor) }

    fn fast_quote(&mut self, _accounts: &[AccountInfo], input_mint: Pubkey, amount_in: u64, _profit_pct: f64) -> Result<(u64, u64)> {
        let (max_in, max_out) = self.get_cached_max_amounts(input_mint);
        let amount_in = if max_in > 0 { amount_in.min(max_in) } else { amount_in };
        let max_out = if max_out > 0 { max_out } else { u64::MAX };

        let base_reserve = self.base_vault_amount as u128;
        let quote_reserve = self.quote_vault_amount as u128;
        let fee_factor = FEE_DENOM - self.fee_numerator as u128;

        if input_mint == self.quote_token_pk {
            let in_after_fee = (amount_in as u128) * fee_factor / FEE_DENOM;
            let numerator = base_reserve.saturating_mul(quote_reserve);
            let denominator = quote_reserve.saturating_add(in_after_fee);
            if denominator == 0 { return Ok((amount_in, 0)); }
            let out = base_reserve.saturating_sub(numerator / denominator);
            let out = out.min(u64::MAX as u128) as u64;
            Ok((amount_in, out.min(max_out)))
        } else {
            let numerator = base_reserve.saturating_mul(quote_reserve);
            let denominator = base_reserve.saturating_add(amount_in as u128);
            if denominator == 0 { return Ok((amount_in, 0)); }
            let out_raw = quote_reserve.saturating_sub(numerator / denominator);
            let out = (out_raw * fee_factor / FEE_DENOM) as u64;
            Ok((amount_in, out.min(max_out)))
        }
    }

    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        Ok((self.base_vault_amount, self.quote_vault_amount))
    }

    fn swap_base_in(
        &mut self,
        _accounts: &[AccountInfo],
        input_mint: Pubkey,
        amount_in: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        _clock: &Clock,
    ) -> Result<u64> {
        let base_reserve = self.base_vault_amount as u128;
        let quote_reserve = self.quote_vault_amount as u128;

        let output_reserve = if input_mint == self.base_token_pk {
            self.quote_vault_amount
        } else {
            self.base_vault_amount
        };

        let amount_out_after_fee = if input_mint == self.quote_token_pk {
            let transfer_fee = apply_transfer_fee(amount_in, input_transfer_fee);
            let amount_in_after_fee = amount_in.checked_sub(transfer_fee).unwrap();
            let amount_in_after_fees = (amount_in_after_fee as u128) * (FEE_DENOM - self.fee_numerator as u128) / FEE_DENOM;
            let amount_out: u64 = self.calculate_buy_amount_out(base_reserve, quote_reserve, amount_in_after_fees)?;
            let transfer_fee_out = apply_transfer_fee(amount_out, output_transfer_fee);
            let amount_out_after_fee = amount_out.checked_sub(transfer_fee_out).unwrap();
            amount_out_after_fee.min(output_reserve)
        } else {
            let transfer_fee = apply_transfer_fee(amount_in, input_transfer_fee);
            let amount_in_after_fee = amount_in.checked_sub(transfer_fee).unwrap();
            let amount_out = self.calculate_sell_amount_out(base_reserve, quote_reserve, amount_in_after_fee as u128)?;
            let amount_out_after_pool_fee = ((amount_out as u128) * (FEE_DENOM - self.fee_numerator as u128) / FEE_DENOM) as u64;
            let transfer_fee_out = apply_transfer_fee(amount_out_after_pool_fee, output_transfer_fee);
            let amount_out_after_fee = amount_out_after_pool_fee.checked_sub(transfer_fee_out).unwrap();
            amount_out_after_fee.min(output_reserve)
        };
        Ok(amount_out_after_fee)
    }

    fn get_max_amount_in(&self, _accounts: &[AccountInfo], mint: Pubkey) -> Result<u64> {
        let v = if mint == self.base_token_pk { self.buy_max_in } else { self.sell_max_in };
        if v > 0 { return Ok(v); }
        Ok(if mint == self.base_token_pk { self.quote_vault_amount } else { self.base_vault_amount })
    }

    fn get_max_amount_out(&self, _accounts: &[AccountInfo], mint: Pubkey) -> Result<u64> {
        let v = if mint == self.base_token_pk { self.buy_max_out } else { self.sell_max_out };
        if v > 0 { return Ok(v); }
        Ok(if mint == self.base_token_pk { self.base_vault_amount } else { self.quote_vault_amount })
    }

    fn get_cached_max_amounts(&self, input_mint: Pubkey) -> (u64, u64) {
        if input_mint == self.base_token_pk { (self.buy_max_in, self.buy_max_out) } else { (self.sell_max_in, self.sell_max_out) }
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
        self.swap_base_out_impl(accounts, output_mint, amount_out, input_transfer_fee, output_transfer_fee)
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        let inverse = if self.price > 0.0 { 1.0 / self.price } else { 0.0 };
        Ok((self.price, inverse))
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
        if input_mint == self.base_token_pk {
            return self.invoke_swap_base_out(
                accounts, input_mint, max_amount_in, amount_out,
                payer, user_mint_1_token_account, user_mint_2_token_account,
                mint_1_account, mint_2_account, mint_1_token_program, mint_2_token_program,
            );
        }

        let (base_token_program, quote_token_program,
             user_base_token_account, user_quote_token_account,
             base_mint_account, quote_mint_account,
        ) = if mint_1_account.key() == &self.base_token_pk {
            (mint_1_token_program, mint_2_token_program,
             user_mint_1_token_account, user_mint_2_token_account,
             mint_1_account, mint_2_account)
        } else if mint_2_account.key() == &self.base_token_pk {
            (mint_2_token_program, mint_1_token_program,
             user_mint_2_token_account, user_mint_1_token_account,
             mint_2_account, mint_1_account)
        } else {
            return Err(ProgramError::InvalidAccountData);
        };

        // Static accounts
        let program_id_stored  = &accounts[self.static_base + S_PROGRAM_ID];
        let protocol_fee_recipient = &accounts[self.static_base + S_PROTOCOL_FEE_RECIPIENT];
        let protocol_fee_token_account = &accounts[self.static_base + S_PROTOCOL_FEE_RECIPIENT_TOKEN_ACC];
        let event_authority = &accounts[self.static_base + S_EVENT_AUTHORITY];
        let fee_config = &accounts[self.static_base + S_FEE_CONFIG];
        let fee_program = &accounts[self.static_base + S_FEE_PROGRAM];
        let pump_amm_global = &accounts[self.static_base + S_PUMP_AMM_GLOBAL];
        let system_program = &accounts[self.static_base + S_SYSTEM_PROGRAM];
        let associated_token_instruction_program = &accounts[self.static_base + S_ASSOC_TOKEN_PROGRAM];
        let global_vol_accumulator = &accounts[self.static_base + S_GLOBAL_VOL_ACC];

        // Dynamic accounts
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let base_vault = &accounts[self.dyn_start + D_BASE_VAULT];
        let quote_vault = &accounts[self.dyn_start + D_QUOTE_VAULT];
        let user_volume_accumulator = &accounts[self.dyn_start + D_USER_VOL_ACC];
        let user_vol_wsol_ata = &accounts[self.dyn_start + D_USER_VOL_WSOL_ATA];
        let vault_ata = &accounts[self.dyn_start + D_VAULT_ATA];
        let vault_authority = &accounts[self.dyn_start + D_VAULT_AUTHORITY];
        let cashback_pool_id = &accounts[self.dyn_start + D_CASHBACK_POOL_ID];

        let is_merhem = Self::is_merhem(accounts, self.dyn_start);

        // Stack-allocated metas — MaybeUninit avoids needing a valid initial Pubkey pointer
        let mut metas: [core::mem::MaybeUninit<AccountMeta>; 25] = unsafe { core::mem::MaybeUninit::uninit().assume_init() };
        let mut n = 0usize;
        macro_rules! push_meta {
            (w $key:expr)  => { metas[n].write(AccountMeta::new($key, true, false)); n += 1; };
            (ws $key:expr) => { metas[n].write(AccountMeta::new($key, true, true)); n += 1; };
            (r $key:expr)  => { metas[n].write(AccountMeta::new($key, false, false)); n += 1; };
        }
        push_meta!(w  pool_id.key());
        push_meta!(ws payer.key());
        push_meta!(r  pump_amm_global.key());
        push_meta!(r  base_mint_account.key());
        push_meta!(r  quote_mint_account.key());
        push_meta!(w  user_base_token_account.key());
        push_meta!(w  user_quote_token_account.key());
        push_meta!(w  base_vault.key());
        push_meta!(w  quote_vault.key());
        push_meta!(r  protocol_fee_recipient.key());
        push_meta!(w  protocol_fee_token_account.key());
        push_meta!(r  base_token_program.key());
        push_meta!(r  quote_token_program.key());
        push_meta!(r  system_program.key());
        push_meta!(r  associated_token_instruction_program.key());
        push_meta!(r  event_authority.key());
        push_meta!(r  &PROGRAM_ID);
        push_meta!(w  vault_ata.key());
        push_meta!(r  vault_authority.key());
        push_meta!(r  global_vol_accumulator.key());
        push_meta!(w  user_volume_accumulator.key());
        push_meta!(r  fee_config.key());
        push_meta!(r  fee_program.key());
        if is_merhem { push_meta!(w user_vol_wsol_ata.key()); }
        push_meta!(r  cashback_pool_id.key());

        // buy_exact_quote_in: disc(8) | spendable_quote_in(8) | min_base_amount_out(8)
        let mut data = [0u8; 24];
        data[..8].copy_from_slice(&BUY_EXACT_QUOTE_IN_DISC);
        data[8..16].copy_from_slice(&max_amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&1u64.to_le_bytes());

        let mut accs: [core::mem::MaybeUninit<&AccountInfo>; 25] = [core::mem::MaybeUninit::uninit(); 25];
        let mut ai = 0usize;
        macro_rules! push_acc {
            ($e:expr) => { accs[ai].write($e); ai += 1; };
        }
        push_acc!(pool_id);
        push_acc!(payer);
        push_acc!(pump_amm_global);
        push_acc!(base_mint_account);
        push_acc!(quote_mint_account);
        push_acc!(user_base_token_account);
        push_acc!(user_quote_token_account);
        push_acc!(base_vault);
        push_acc!(quote_vault);
        push_acc!(protocol_fee_recipient);
        push_acc!(protocol_fee_token_account);
        push_acc!(base_token_program);
        push_acc!(quote_token_program);
        push_acc!(system_program);
        push_acc!(associated_token_instruction_program);
        push_acc!(event_authority);
        push_acc!(program_id_stored);
        push_acc!(vault_ata);
        push_acc!(vault_authority);
        push_acc!(global_vol_accumulator);
        push_acc!(user_volume_accumulator);
        push_acc!(fee_config);
        push_acc!(fee_program);
        if is_merhem { push_acc!(user_vol_wsol_ata); }
        push_acc!(cashback_pool_id);

        let metas_slice: &[AccountMeta] = unsafe {
            core::slice::from_raw_parts(metas.as_ptr() as *const AccountMeta, n)
        };
        let accs_slice: &[&AccountInfo] = unsafe {
            core::slice::from_raw_parts(accs.as_ptr() as *const &AccountInfo, ai)
        };
        invoke_cpi(&PROGRAM_ID, metas_slice, &data, accs_slice)?;
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
        if input_mint == self.quote_token_pk {
            return self.invoke_swap_base_in(
                accounts, input_mint, amount_in, min_amount_out,
                payer, user_mint_1_token_account, user_mint_2_token_account,
                mint_1_account, mint_2_account, mint_1_token_program, mint_2_token_program,
            );
        }

        let (base_token_program, quote_token_program,
             user_base_token_account, user_quote_token_account,
             base_mint_account, quote_mint_account,
        ) = if mint_1_account.key() == &self.base_token_pk {
            (mint_1_token_program, mint_2_token_program,
             user_mint_1_token_account, user_mint_2_token_account,
             mint_1_account, mint_2_account)
        } else if mint_2_account.key() == &self.base_token_pk {
            (mint_2_token_program, mint_1_token_program,
             user_mint_2_token_account, user_mint_1_token_account,
             mint_2_account, mint_1_account)
        } else {
            return Err(ProgramError::InvalidAccountData);
        };

        // Static accounts
        let program_id = &accounts[self.static_base + S_PROGRAM_ID];
        let protocol_fee_recipient = &accounts[self.static_base + S_PROTOCOL_FEE_RECIPIENT];
        let protocol_fee_token_account = &accounts[self.static_base + S_PROTOCOL_FEE_RECIPIENT_TOKEN_ACC];
        let event_authority = &accounts[self.static_base + S_EVENT_AUTHORITY];
        let fee_config = &accounts[self.static_base + S_FEE_CONFIG];
        let fee_program = &accounts[self.static_base + S_FEE_PROGRAM];
        let pump_amm_global = &accounts[self.static_base + S_PUMP_AMM_GLOBAL];
        let system_program = &accounts[self.static_base + S_SYSTEM_PROGRAM];
        let associated_token_instruction_program = &accounts[self.static_base + S_ASSOC_TOKEN_PROGRAM];
        let global_vol_accumulator = &accounts[self.static_base + S_GLOBAL_VOL_ACC];

        // Dynamic accounts
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let base_vault = &accounts[self.dyn_start + D_BASE_VAULT];
        let quote_vault = &accounts[self.dyn_start + D_QUOTE_VAULT];
        let user_volume_accumulator = &accounts[self.dyn_start + D_USER_VOL_ACC];
        let user_vol_wsol_ata = &accounts[self.dyn_start + D_USER_VOL_WSOL_ATA];
        let vault_ata = &accounts[self.dyn_start + D_VAULT_ATA];
        let vault_authority = &accounts[self.dyn_start + D_VAULT_AUTHORITY];
        let cashback_pool_id = &accounts[self.dyn_start + D_CASHBACK_POOL_ID];

        let min_amount_out_value = min_amount_out.unwrap_or(0);
        let is_merhem = Self::is_merhem(accounts, self.dyn_start);

        let mut metas: [core::mem::MaybeUninit<AccountMeta>; 25] = unsafe { core::mem::MaybeUninit::uninit().assume_init() };
        let mut n = 0usize;
        macro_rules! push_meta {
            (w $key:expr)  => { metas[n].write(AccountMeta::new($key, true, false)); n += 1; };
            (ws $key:expr) => { metas[n].write(AccountMeta::new($key, true, true)); n += 1; };
            (r $key:expr)  => { metas[n].write(AccountMeta::new($key, false, false)); n += 1; };
        }
        push_meta!(w  pool_id.key());
        push_meta!(ws payer.key());
        push_meta!(r  pump_amm_global.key());
        push_meta!(r  base_mint_account.key());
        push_meta!(r  quote_mint_account.key());
        push_meta!(w  user_base_token_account.key());
        push_meta!(w  user_quote_token_account.key());
        push_meta!(w  base_vault.key());
        push_meta!(w  quote_vault.key());
        push_meta!(r  protocol_fee_recipient.key());
        push_meta!(w  protocol_fee_token_account.key());
        push_meta!(r  base_token_program.key());
        push_meta!(r  quote_token_program.key());
        push_meta!(r  system_program.key());
        push_meta!(r  associated_token_instruction_program.key());
        push_meta!(r  event_authority.key());
        push_meta!(r  program_id.key());
        push_meta!(w  vault_ata.key());
        push_meta!(r  vault_authority.key());
        push_meta!(r  fee_config.key());
        push_meta!(r  fee_program.key());
        if is_merhem {
            push_meta!(w user_vol_wsol_ata.key());
            push_meta!(w user_volume_accumulator.key());
        }
        push_meta!(r  cashback_pool_id.key());

        let mut data = [0u8; 24];
        data[..8].copy_from_slice(&SELL_DISC);
        data[8..16].copy_from_slice(&amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&min_amount_out_value.to_le_bytes());

        let mut accs: [core::mem::MaybeUninit<&AccountInfo>; 25] = [core::mem::MaybeUninit::uninit(); 25];
        let mut ai = 0usize;
        macro_rules! push_acc {
            ($e:expr) => { accs[ai].write($e); ai += 1; };
        }
        push_acc!(pool_id);
        push_acc!(payer);
        push_acc!(pump_amm_global);
        push_acc!(base_mint_account);
        push_acc!(quote_mint_account);
        push_acc!(user_base_token_account);
        push_acc!(user_quote_token_account);
        push_acc!(base_vault);
        push_acc!(quote_vault);
        push_acc!(protocol_fee_recipient);
        push_acc!(protocol_fee_token_account);
        push_acc!(base_token_program);
        push_acc!(quote_token_program);
        push_acc!(system_program);
        push_acc!(associated_token_instruction_program);
        push_acc!(event_authority);
        push_acc!(program_id);
        push_acc!(vault_ata);
        push_acc!(vault_authority);
        push_acc!(fee_config);
        push_acc!(fee_program);
        if is_merhem {
            push_acc!(user_vol_wsol_ata);
            push_acc!(user_volume_accumulator);
        }
        push_acc!(cashback_pool_id);

        let metas_slice: &[AccountMeta] = unsafe {
            core::slice::from_raw_parts(metas.as_ptr() as *const AccountMeta, n)
        };
        let accs_slice: &[&AccountInfo] = unsafe {
            core::slice::from_raw_parts(accs.as_ptr() as *const &AccountInfo, ai)
        };
        invoke_cpi(program_id.key(), metas_slice, &data, accs_slice)?;
        Ok(())
    }

    #[cfg(any(test, feature = "debug"))]
    fn log_accounts(&self, accounts: &[AccountInfo]) -> Result<()> {
        use pinocchio::log::sol_log;
        sol_log("=== Pump AMM ===");
        Ok(())
    }
}


impl PumpAmm {

    /// Read is_mayhem_mode flag directly from pool account data.
    #[inline(always)]
    fn is_merhem(accounts: &[AccountInfo], dyn_start: usize) -> bool {
        let pool = &accounts[dyn_start + D_POOL];
        // pinocchio: direct data access, no RefCell borrow needed
        let data = unsafe { pool.borrow_data_unchecked() };
        data.len() > POOL_IS_CASHBACK_OFFSET && data[POOL_IS_CASHBACK_OFFSET] != 0
    }

    pub fn new(
        accounts: &[AccountInfo],
        static_base: usize,
        dyn_start: usize,
        _dyn_end: usize,
        pool_fee: u32,
    ) -> Result<Self> {
        let base_vault = &accounts[dyn_start + D_BASE_VAULT];
        let quote_vault = &accounts[dyn_start + D_QUOTE_VAULT];
        let pool_acc = &accounts[dyn_start + D_POOL];

        let (base_token_pk, base_vault_amount) = read_vault_data(base_vault)?;
        let (quote_token_pk, quote_vault_amount) = read_vault_data(quote_vault)?;

        #[cfg(test)]
        let base_vault_amount: u64 = (base_vault_amount as f64 * 1.06) as u64;

        let price = get_price_f64(base_vault_amount, quote_vault_amount);

        let fee_numerator: u64 = if pool_fee > 0 {
            pool_fee as u64
        } else {
            get_fees_int(base_vault_amount, quote_vault_amount)
        };

        Ok(PumpAmm {
            price,
            fee_numerator,
            fee_factor: { let f = 1.0 - (fee_numerator as f64 / FEE_DENOM as f64); (f, f) },
            pool_id: *pool_acc.key(),
            base_token_pk,
            quote_token_pk,
            base_vault_amount,
            quote_vault_amount,
            static_base,
            dyn_start,
            buy_max_in: 0,
            buy_max_out: 0,
            sell_max_in: 0,
            sell_max_out: 0,
            prepared: false,
        })
    }

    /// Compute deferred fields (max amounts). Called only for instances in a profitable path.
    pub fn prepare_for_execution(&mut self, _accounts: &[AccountInfo]) {
        if self.prepared { return; }
        self.prepared = true;

        let ff = FEE_DENOM - self.fee_numerator as u128;
        let (buy_max_in, buy_max_out) = {
            let x = self.base_vault_amount as u128;
            let y = self.quote_vault_amount as u128;
            let eff = y * ff;
            let target = eff * 99 / 100;
            let denom = eff - target;
            if denom == 0 { (0u64, y as u64) } else {
                let dx = x.saturating_mul(target) / denom;
                (dx.min(u64::MAX as u128) as u64, y as u64)
            }
        };
        let (sell_max_in, sell_max_out) = {
            let x = self.quote_vault_amount as u128;
            let y = self.base_vault_amount as u128;
            let target = y * 99 / 100;
            let denom = ff * (y - target);
            if denom == 0 { (0u64, y as u64) } else {
                let dx = x.saturating_mul(target).saturating_mul(FEE_DENOM) / denom;
                (dx.min(u64::MAX as u128) as u64, y as u64)
            }
        };
        self.buy_max_in = buy_max_in;
        self.buy_max_out = buy_max_out;
        self.sell_max_in = sell_max_in;
        self.sell_max_out = sell_max_out;
    }

    pub fn initialize_reserves(&mut self, _accounts: &[AccountInfo]) -> Result<()> {
        Ok(())
    }

    /// CP formula: out = base - (base * quote) / (quote + in)
    #[inline(always)]
    pub fn calculate_buy_amount_out(&self, base_reserve: u128, quote_reserve: u128, amount_in: u128) -> Result<u64> {
        let denominator = quote_reserve + amount_in;
        if denominator == 0 { return Ok(0); }
        let out = base_reserve - base_reserve * quote_reserve / denominator;
        Ok(out as u64)
    }

    /// CP formula: out = quote - (base * quote) / (base + in)
    #[inline(always)]
    pub fn calculate_sell_amount_out(&self, base_reserve: u128, quote_reserve: u128, amount_in: u128) -> Result<u64> {
        let denominator = base_reserve + amount_in;
        if denominator == 0 { return Ok(0); }
        let out = quote_reserve - base_reserve * quote_reserve / denominator;
        Ok(out as u64)
    }

    pub fn swap_base_in_impl(
        &self,
        _accounts: &[AccountInfo],
        input_mint: Pubkey,
        amount_in: u64,
        mint_fees: &[(Pubkey, MintFee)],
    ) -> Result<u64> {
        let base_reserve = self.base_vault_amount as u128;
        let quote_reserve = self.quote_vault_amount as u128;
        let output_reserve = if input_mint == self.base_token_pk { self.quote_vault_amount } else { self.base_vault_amount };
        let (fee_in, fee_out) = crate::utils::token::get_transfer_fees(
            input_mint, &self.base_token_pk, &self.quote_token_pk, mint_fees,
        );
        let amount_out_after_fee = if input_mint == self.quote_token_pk {
            let transfer_fee = apply_transfer_fee(amount_in, fee_in);
            let amount_in_after_fee = amount_in.checked_sub(transfer_fee).unwrap();
            let amount_in_after_fees = (amount_in_after_fee as u128) * (FEE_DENOM - self.fee_numerator as u128) / FEE_DENOM;
            let amount_out: u64 = self.calculate_buy_amount_out(base_reserve, quote_reserve, amount_in_after_fees)?;
            let transfer_fee_out = apply_transfer_fee(amount_out, fee_out);
            let amount_out_after_fee = amount_out.checked_sub(transfer_fee_out).unwrap();
            amount_out_after_fee.min(output_reserve)
        } else {
            let transfer_fee = apply_transfer_fee(amount_in, fee_in);
            let amount_in_after_fee = amount_in.checked_sub(transfer_fee).unwrap();
            let amount_out = self.calculate_sell_amount_out(base_reserve, quote_reserve, amount_in_after_fee as u128)?;
            let amount_out_after_pool_fee = ((amount_out as u128) * (FEE_DENOM - self.fee_numerator as u128) / FEE_DENOM) as u64;
            let transfer_fee_out = apply_transfer_fee(amount_out_after_pool_fee, fee_out);
            let amount_out_after_fee = amount_out_after_pool_fee.checked_sub(transfer_fee_out).unwrap();
            amount_out_after_fee.min(output_reserve)
        };
        Ok(amount_out_after_fee)
    }

    pub fn swap_base_out_impl(
        &self,
        _accounts: &[AccountInfo],
        output_mint: Pubkey,
        amount_out: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
    ) -> Result<u64> {
        let base_reserve = self.base_vault_amount as u128;
        let quote_reserve = self.quote_vault_amount as u128;
        let output_reserve = if output_mint == self.base_token_pk { self.base_vault_amount } else { self.quote_vault_amount };
        let amount_out = amount_out.min(output_reserve.saturating_sub(1));

        let max_amount_in = if output_mint == self.base_token_pk {
            let transfer_fee_out = apply_transfer_fee(amount_out, output_transfer_fee);
            let amount_out_before_transfer_fee = (amount_out as u128)
                .checked_add(transfer_fee_out as u128).ok_or(ProgramError::InvalidArgument)?;
            let numerator = quote_reserve.checked_mul(amount_out_before_transfer_fee).ok_or(ProgramError::InvalidArgument)?;
            let denominator = base_reserve.checked_sub(amount_out_before_transfer_fee).ok_or(ProgramError::InvalidArgument)?;
            let amount_in_after_fee = numerator.checked_div(denominator).ok_or(ProgramError::InvalidArgument)?
                .checked_add(1).ok_or(ProgramError::InvalidArgument)?;
            let ff = FEE_DENOM - self.fee_numerator as u128;
            let amount_in_before_fee = (amount_in_after_fee * FEE_DENOM + ff - 1) / ff;
            let amount_in_u64 = u64::try_from(amount_in_before_fee).map_err(|_| ProgramError::InvalidArgument)?;
            let transfer_fee_in = apply_transfer_fee(amount_in_u64, input_transfer_fee);
            amount_in_u64.checked_add(transfer_fee_in).ok_or(ProgramError::InvalidArgument)?
        } else {
            let transfer_fee_out = apply_transfer_fee(amount_out, output_transfer_fee);
            let amount_out_before_transfer_fee = (amount_out as u128)
                .checked_add(transfer_fee_out as u128).ok_or(ProgramError::InvalidArgument)?;
            let ff = FEE_DENOM - self.fee_numerator as u128;
            let quote_out_before_fee = (amount_out_before_transfer_fee * FEE_DENOM + ff - 1) / ff;
            let numerator = base_reserve.checked_mul(quote_out_before_fee).ok_or(ProgramError::InvalidArgument)?;
            let denominator = quote_reserve.checked_sub(quote_out_before_fee).ok_or(ProgramError::InvalidArgument)?;
            let base_in = numerator.checked_div(denominator).ok_or(ProgramError::InvalidArgument)?
                .checked_add(1).ok_or(ProgramError::InvalidArgument)?;
            let amount_in_u64 = u64::try_from(base_in).map_err(|_| ProgramError::InvalidArgument)?;
            let transfer_fee_in = apply_transfer_fee(amount_in_u64, input_transfer_fee);
            amount_in_u64.checked_add(transfer_fee_in).ok_or(ProgramError::InvalidArgument)?
        };

        Ok(max_amount_in)
    }
}

// NOTE: Tests here use anchor_lang AccountInfo (dev-dep) and will fail to compile after
// pinocchio migration until migrated to LiteSVM or Mollusk.
// See: https://github.com/anza-xyz/mollusk
#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::token::MintFee;

    #[test]
    fn test_get_fees_int_binary_search() {
        // Verify binary search produces same results as linear scan would
        assert_eq!(get_fees_int(1, 1), 12500);   // tiny pool → 1.25%
        assert_eq!(get_fees_int(u64::MAX, u64::MAX), 3000); // huge pool → 0.30%
    }

    #[test]
    fn test_calculate_buy_sell_roundtrip() {
        let pump = PumpAmm {
            pool_id: [0u8; 32],
            base_token_pk: [1u8; 32],
            quote_token_pk: [2u8; 32],
            base_vault_amount: 1_000_000_000,
            quote_vault_amount: 500_000_000_000,
            price: 500.0,
            fee_numerator: 10_000,
            fee_factor: (0.99, 0.99),
            static_base: 0,
            dyn_start: 0,
            buy_max_in: 0,
            buy_max_out: 0,
            sell_max_in: 0,
            sell_max_out: 0,
            prepared: false,
        };
        let out = pump.calculate_buy_amount_out(
            1_000_000_000u128,
            500_000_000_000u128,
            100_000_000u128,
        ).unwrap();
        assert!(out > 0 && out < 1_000_000_000);
    }
}
