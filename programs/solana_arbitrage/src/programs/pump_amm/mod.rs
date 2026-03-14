use crate::programs::ProgramMeta;
use crate::utils::token::{apply_transfer_fee, MintFee};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke_unchecked,
    program_error::ProgramError,
    pubkey::Pubkey,
};
mod constants;
use crate::utils::utils::read_vault_data;

pub const PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
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
pub const D_POOL_V2: usize = 4;
pub const D_USER_VOL_WSOL_ATA: usize = 5;
pub const D_VAULT_ATA: usize = 6;
pub const D_VAULT_AUTHORITY: usize = 7;
pub const D_CASHBACK_POOL_ID: usize = 8;

// Pool account: 8 (disc) + 1 (bump) + 2 (index) + 32*6 (pubkeys) + 8 (lp_supply) + 32 (coin_creator) + 1 (is_mayhem_mode) = 244
const POOL_IS_CASHBACK_OFFSET: usize = 244;

pub const MIN_ACCOUNTS: usize = 9;

/// Fee denominator for PumpAmm integer fee math (millionths).
const FEE_DENOM: u128 = 1_000_000;

/// Price as an integer ratio: (numerator, denominator).
/// price (Base→Quote) = quote_vault / base_vault  → (quote_vault, base_vault)
/// inverse (Quote→Base) = base_vault / quote_vault → (base_vault, quote_vault)
/// Both share denominator base_vault * quote_vault when needed on a common basis.
pub fn get_price_f64(base_vault_amount: u64, quote_vault_amount: u64) -> f64 {
    quote_vault_amount as f64 / base_vault_amount as f64
}

/// Compute PumpAmm fee tier from vault amounts. Returns fee in millionths (e.g. 12500 = 1.25%).
/// Uses integer-only comparison: min_vault * 1_000_000 vs threshold * max_vault,
/// avoiding all f64 operations.
pub fn get_fees_int(base_vault: u64, quote_vault: u64) -> u64 {
    let min_v = (base_vault as u128).min(quote_vault as u128);
    let max_v = (base_vault as u128).max(quote_vault as u128);
    // market_cap_num = min_vault * 1_000_000 (compare against threshold * max_vault)
    let mcap_num = min_v.saturating_mul(1_000_000);

    // (threshold, fee_millionths) pairs — threshold in SOL units for market cap
    const TIERS: [(u128, u64); 24] = [
        (420, 12500),   // 1.25%
        (1470, 12000),  // 1.20%
        (2460, 11500),  // 1.15%
        (3440, 11000),  // 1.10%
        (4420, 10500),  // 1.05%
        (9820, 10000),  // 1.00%
        (14740, 9500),  // 0.95%
        (19650, 9000),  // 0.90%
        (24560, 8500),  // 0.85%
        (29470, 8000),  // 0.80%
        (34380, 7500),  // 0.75%
        (39300, 7000),  // 0.70%
        (44210, 6500),  // 0.65%
        (49120, 6000),  // 0.60%
        (54030, 5500),  // 0.55%
        (58940, 5200),  // 0.52%
        (63860, 5000),  // 0.50%
        (68770, 4700),  // 0.47%
        (73681, 4500),  // 0.45%
        (78590, 4200),  // 0.42%
        (83500, 4000),  // 0.40%
        (88400, 3700),  // 0.37%
        (93330, 3500),  // 0.35%
        (98240, 3200),  // 0.32%
    ];

    for &(threshold, fee) in &TIERS {
        if mcap_num <= threshold.saturating_mul(max_v) {
            return fee;
        }
    }
    3000 // 0.30% default (> 98240 SOL market cap)
}

pub struct PumpAmm {
    // pub program_id: AccountInfo<'info>,
    // pub pool_id: AccountInfo<'info>,
    // pub base_vault: AccountInfo<'info>,
    // pub quote_vault: AccountInfo<'info>,
    // pub base_token: AccountInfo<'info>,
    // pub quote_token: AccountInfo<'info>,
    // pub base_vault_account: TokenAccount,
    // pub quote_vault_account: TokenAccount,
    pub pool_id: Pubkey,
    pub base_token_pk: Pubkey,
    pub quote_token_pk: Pubkey,
    pub base_vault_amount: u64,
    pub quote_vault_amount: u64,
    /// Price (Base→Quote) = quote_vault / base_vault
    pub price: f64,
    /// Fee in millionths (e.g. 12500 = 1.25%). Integer replacement for fee_rate f64.
    pub fee_numerator: u64,
    pub static_base: usize,
    pub dyn_start: usize,
    /// Cached max amounts from init
    pub buy_max_in: u64,
    pub buy_max_out: u64,
    pub sell_max_in: u64,
    pub sell_max_out: u64,
    pub is_merhem: bool,
    /// Whether deferred fields (max amounts, transfer fees, is_merhem) have been computed
    pub prepared: bool,

}

impl ProgramMeta for PumpAmm {
    fn get_id(&self) -> &Pubkey {
        &PROGRAM_ID
    }

    fn get_pool_id(&self) -> &Pubkey {
        &self.pool_id
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (&self.base_token_pk, &self.quote_token_pk)
    }

    fn name(&self) -> &'static str { "PumpAmm" }

    fn get_fee_factor(&self) -> Result<(f64, f64)> { let f = 1.0 - (self.fee_numerator as f64 / FEE_DENOM as f64); Ok((f, f)) }

    fn fast_quote(&mut self, input_mint: Pubkey, amount_in: u64, _profit_pct: f64) -> Result<(u64, u64)> {
        let (max_in, max_out) = self.get_cached_max_amounts(input_mint);
        let amount_in = if max_in > 0 { amount_in.min(max_in) } else { amount_in };
        let max_out = if max_out > 0 { max_out } else { u64::MAX };
        debug_eprintln!("[PUMP AMM] Fast quote: {:.9} SOL ({}) -> {:.6} tokens ({})", amount_in as f64 / 1_000_000_000.0, amount_in, max_out as f64 / 1_000_000.0, max_out);

        let base_reserve = self.base_vault_amount as u128;
        let quote_reserve = self.quote_vault_amount as u128;
        let fee_factor = FEE_DENOM - self.fee_numerator as u128;

        if input_mint == self.quote_token_pk {
            // Buying base: fee applied BEFORE swap on quote input (integer)
            let in_after_fee = (amount_in as u128) * fee_factor / FEE_DENOM;
            // CP: out = base - (base * quote) / (quote + in_after_fee)
            let numerator = base_reserve.saturating_mul(quote_reserve);
            let denominator = quote_reserve.saturating_add(in_after_fee);
            if denominator == 0 { return Ok((amount_in, 0)); }
            let out = base_reserve.saturating_sub(numerator / denominator);
            let out = out.min(u64::MAX as u128) as u64;
            Ok((amount_in, out.min(max_out)))
        } else {
            // Selling base: fee applied AFTER swap on quote output (integer)
            // CP: out_raw = quote - (base * quote) / (base + amount_in)
            let numerator = base_reserve.saturating_mul(quote_reserve);
            let denominator = base_reserve.saturating_add(amount_in as u128);
            if denominator == 0 { return Ok((amount_in, 0)); }
            let out_raw = quote_reserve.saturating_sub(numerator / denominator);
            let out = (out_raw * fee_factor / FEE_DENOM) as u64;
            Ok((amount_in, out.min(max_out)))
        }
    }

    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        Ok((
            self.base_vault_amount as u64,
            self.quote_vault_amount as u64,
        ))
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

            let amount_out: u64 =
                self.calculate_buy_amount_out(base_reserve, quote_reserve, amount_in_after_fees)?;

            let transfer_fee_out = apply_transfer_fee(amount_out, output_transfer_fee);
            let amount_out_after_fee = amount_out.checked_sub(transfer_fee_out).unwrap();
            amount_out_after_fee.min(output_reserve)
        } else {
            // Selling base for quote: fee is applied on quote OUTPUT (not base input)
            let transfer_fee = apply_transfer_fee(amount_in, input_transfer_fee);
            let amount_in_after_fee = amount_in.checked_sub(transfer_fee).unwrap();

            let amount_out = self.calculate_sell_amount_out(base_reserve, quote_reserve, amount_in_after_fee as u128)?;

            // Apply pool fee on quote output
            let amount_out_after_pool_fee = ((amount_out as u128) * (FEE_DENOM - self.fee_numerator as u128) / FEE_DENOM) as u64;

            let transfer_fee_out = apply_transfer_fee(amount_out_after_pool_fee, output_transfer_fee);
            let amount_out_after_fee = amount_out_after_pool_fee.checked_sub(transfer_fee_out).unwrap();
            amount_out_after_fee.min(output_reserve)
        };
        Ok(amount_out_after_fee)
    }

    fn get_max_amount_in<'a>(&self, _accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        let v = if mint == self.base_token_pk { self.buy_max_in } else { self.sell_max_in };
        if v > 0 { return Ok(v); }
        // Not prepared yet — estimate from vault amounts
        Ok(if mint == self.base_token_pk {
            self.quote_vault_amount as u64
        } else {
            self.base_vault_amount as u64
        })
    }

    fn get_max_amount_out<'a>(&self, _accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        let v = if mint == self.base_token_pk { self.buy_max_out } else { self.sell_max_out };
        if v > 0 { return Ok(v); }
        // Not prepared yet — estimate from vault amounts
        Ok(if mint == self.base_token_pk {
            self.base_vault_amount as u64
        } else {
            self.quote_vault_amount as u64
        })
    }

    fn get_cached_max_amounts(&self, input_mint: Pubkey) -> (u64, u64) {
        if input_mint == self.base_token_pk { (self.buy_max_in, self.buy_max_out) } else { (self.sell_max_in, self.sell_max_out) }
    }

    fn has_output_liquidity(&self, input_mint: Pubkey) -> bool {
        // Use vault amounts directly — no need for deferred max amounts
        if input_mint == self.base_token_pk {
            self.quote_vault_amount > 0
        } else {
            self.base_vault_amount > 0
        }
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
        return self.swap_base_out_impl(accounts, output_mint, amount_out, input_transfer_fee, output_transfer_fee);
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        let inverse = if self.price > 0.0 { 1.0 / self.price } else { 0.0 };
        Ok((self.price, inverse))
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
        if input_mint == self.base_token_pk {
            return self.invoke_swap_base_out(
                accounts,
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
            );
        }

        let (
            base_token_program,
            quote_token_program,
            user_base_token_account,
            user_quote_token_account,
            base_mint_account,
            quote_mint_account,
        ) = if mint_1_account.key == &self.base_token_pk {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
                mint_1_account,
                mint_2_account,
            )
        } else if mint_2_account.key == &self.base_token_pk {
            (
                mint_2_token_program,
                mint_1_token_program,
                user_mint_2_token_account,
                user_mint_1_token_account,
                mint_2_account,
                mint_1_account,
            )
        } else {
            return Err(ProgramError::InvalidAccountData.into());
        };

        // Static accounts
        let program_id_stored = &accounts[self.static_base + S_PROGRAM_ID];
        let protocol_fee_recipient = &accounts[self.static_base + S_PROTOCOL_FEE_RECIPIENT];
        let protocol_fee_token_account =
            &accounts[self.static_base + S_PROTOCOL_FEE_RECIPIENT_TOKEN_ACC];
        let event_authority = &accounts[self.static_base + S_EVENT_AUTHORITY];
        let fee_config = &accounts[self.static_base + S_FEE_CONFIG];
        let fee_program = &accounts[self.static_base + S_FEE_PROGRAM];
        let pump_amm_global = &accounts[self.static_base + S_PUMP_AMM_GLOBAL];
        let system_program = &accounts[self.static_base + S_SYSTEM_PROGRAM];
        let associated_token_instruction_program =
            &accounts[self.static_base + S_ASSOC_TOKEN_PROGRAM];
        let global_vol_accumulator = &accounts[self.static_base + S_GLOBAL_VOL_ACC];

        // Dynamic accounts
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let base_vault = &accounts[self.dyn_start + D_BASE_VAULT];
        let quote_vault = &accounts[self.dyn_start + D_QUOTE_VAULT];
        let user_volume_accumulator =
            &accounts[self.dyn_start + D_USER_VOL_ACC];
        let _pool_v2 = &accounts[self.dyn_start + D_POOL_V2];
        let user_vol_wsol_ata = &accounts[self.dyn_start + D_USER_VOL_WSOL_ATA];
        let vault_ata = &accounts[self.dyn_start + D_VAULT_ATA];
        let vault_authority = &accounts[self.dyn_start + D_VAULT_AUTHORITY];
        let cashback_pool_id = &accounts[self.dyn_start + D_CASHBACK_POOL_ID];

        // Stack-allocated arrays — avoids heap Vec allocation (~2-4k CU savings)
        let mut metas: [AccountMeta; 25] = core::array::from_fn(|_| AccountMeta::new_readonly(Pubkey::default(), false));
        let mut n = 0usize;
        macro_rules! push_meta {
            (w $key:expr) => { metas[n] = AccountMeta::new($key, false); n += 1; };
            (ws $key:expr) => { metas[n] = AccountMeta::new($key, true); n += 1; };
            (r $key:expr) => { metas[n] = AccountMeta::new_readonly($key, false); n += 1; };
        }
        push_meta!(w *pool_id.key);
        push_meta!(ws *payer.key);
        push_meta!(r *pump_amm_global.key);
        push_meta!(r *base_mint_account.key);
        push_meta!(r *quote_mint_account.key);
        push_meta!(w *user_base_token_account.key);
        push_meta!(w *user_quote_token_account.key);
        push_meta!(w *base_vault.key);
        push_meta!(w *quote_vault.key);
        push_meta!(r *protocol_fee_recipient.key);
        push_meta!(w *protocol_fee_token_account.key);
        push_meta!(r *base_token_program.key);
        push_meta!(r *quote_token_program.key);
        push_meta!(r *system_program.key);
        push_meta!(r *associated_token_instruction_program.key);
        push_meta!(r *event_authority.key);
        push_meta!(r PROGRAM_ID);
        push_meta!(w *vault_ata.key);
        push_meta!(r *vault_authority.key);
        push_meta!(r *global_vol_accumulator.key);
        push_meta!(w *user_volume_accumulator.key);
        push_meta!(r *fee_config.key);
        push_meta!(r *fee_program.key);
        if self.is_merhem {
            push_meta!(w *user_vol_wsol_ata.key);
        }
        push_meta!(r *cashback_pool_id.key);

        // buy_exact_quote_in: disc(8) | spendable_quote_in(8) | min_base_amount_out(8)
        let mut data = [0u8; 24];
        data[..8].copy_from_slice(&BUY_EXACT_QUOTE_IN_DISC);
        data[8..16].copy_from_slice(&max_amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&1u64.to_le_bytes());

        let swap_ix = Instruction {
            program_id: PROGRAM_ID,
            accounts: metas[..n].to_vec(),
            data: data.to_vec(),
        };

        // Stack-allocated account infos array — no heap Vec
        let mut accs: [AccountInfo<'a>; 25] = unsafe { core::mem::zeroed() };
        let mut ai = 0usize;
        macro_rules! push_acc {
            (ref $e:expr) => { accs[ai] = unsafe { std::mem::transmute($e.clone()) }; ai += 1; };
            (own $e:expr) => { accs[ai] = unsafe { std::mem::transmute($e.to_account_info()) }; ai += 1; };
        }
        push_acc!(ref pool_id);
        push_acc!(own payer);
        push_acc!(ref pump_amm_global);
        push_acc!(own base_mint_account);
        push_acc!(own quote_mint_account);
        push_acc!(own user_base_token_account);
        push_acc!(own user_quote_token_account);
        push_acc!(ref base_vault);
        push_acc!(ref quote_vault);
        push_acc!(ref protocol_fee_recipient);
        push_acc!(ref protocol_fee_token_account);
        push_acc!(own base_token_program);
        push_acc!(own quote_token_program);
        push_acc!(ref system_program);
        push_acc!(ref associated_token_instruction_program);
        push_acc!(ref event_authority);
        push_acc!(ref program_id_stored);
        push_acc!(ref vault_ata);
        push_acc!(ref vault_authority);
        push_acc!(ref global_vol_accumulator);
        push_acc!(ref user_volume_accumulator);
        push_acc!(ref fee_config);
        push_acc!(ref fee_program);
        if self.is_merhem {
            push_acc!(ref user_vol_wsol_ata);
        }
        push_acc!(ref cashback_pool_id);

        invoke_unchecked(&swap_ix, &accs[..ai])?;
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
        if input_mint == self.quote_token_pk {
            return self.invoke_swap_base_in(
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
            );
        }
        let (
            base_token_program,
            quote_token_program,
            user_base_token_account,
            user_quote_token_account,
            base_mint_account,
            quote_mint_account,
        ) = if mint_1_account.key == &self.base_token_pk {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
                mint_1_account,
                mint_2_account,
            )
        } else if mint_2_account.key == &self.base_token_pk {
            (
                mint_2_token_program,
                mint_1_token_program,
                user_mint_2_token_account,
                user_mint_1_token_account,
                mint_2_account,
                mint_1_account,
            )
        } else {
            return Err(ProgramError::InvalidAccountData.into());
        };

        // Static accounts
        let program_id = &accounts[self.static_base + S_PROGRAM_ID];
        let protocol_fee_recipient = &accounts[self.static_base + S_PROTOCOL_FEE_RECIPIENT];
        let protocol_fee_token_account =
            &accounts[self.static_base + S_PROTOCOL_FEE_RECIPIENT_TOKEN_ACC];
        let event_authority = &accounts[self.static_base + S_EVENT_AUTHORITY];
        let fee_config = &accounts[self.static_base + S_FEE_CONFIG];
        let fee_program = &accounts[self.static_base + S_FEE_PROGRAM];
        let pump_amm_global = &accounts[self.static_base + S_PUMP_AMM_GLOBAL];
        let system_program = &accounts[self.static_base + S_SYSTEM_PROGRAM];
        let associated_token_instruction_program =
            &accounts[self.static_base + S_ASSOC_TOKEN_PROGRAM];
        let global_vol_accumulator = &accounts[self.static_base + S_GLOBAL_VOL_ACC];

        // Dynamic accounts
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let base_vault = &accounts[self.dyn_start + D_BASE_VAULT];
        let quote_vault = &accounts[self.dyn_start + D_QUOTE_VAULT];
        let user_volume_accumulator =
            &accounts[self.dyn_start + D_USER_VOL_ACC];
        let _pool_v2 = &accounts[self.dyn_start + D_POOL_V2];
        let user_vol_wsol_ata = &accounts[self.dyn_start + D_USER_VOL_WSOL_ATA];
        let vault_ata = &accounts[self.dyn_start + D_VAULT_ATA];
        let vault_authority = &accounts[self.dyn_start + D_VAULT_AUTHORITY];
        let cashback_pool_id = &accounts[self.dyn_start + D_CASHBACK_POOL_ID];

        let min_amount_out_value = min_amount_out.unwrap_or(0);

        // Stack-allocated metas array
        let mut metas: [AccountMeta; 24] = core::array::from_fn(|_| AccountMeta::new_readonly(Pubkey::default(), false));
        let mut n = 0usize;
        macro_rules! push_meta {
            (w $key:expr) => { metas[n] = AccountMeta::new($key, false); n += 1; };
            (ws $key:expr) => { metas[n] = AccountMeta::new($key, true); n += 1; };
            (r $key:expr) => { metas[n] = AccountMeta::new_readonly($key, false); n += 1; };
        }
        push_meta!(w *pool_id.key);
        push_meta!(ws *payer.key);
        push_meta!(r *pump_amm_global.key);
        push_meta!(r *base_mint_account.key);
        push_meta!(r *quote_mint_account.key);
        push_meta!(w *user_base_token_account.key);
        push_meta!(w *user_quote_token_account.key);
        push_meta!(w *base_vault.key);
        push_meta!(w *quote_vault.key);
        push_meta!(r *protocol_fee_recipient.key);
        push_meta!(w *protocol_fee_token_account.key);
        push_meta!(r *base_token_program.key);
        push_meta!(r *quote_token_program.key);
        push_meta!(r *system_program.key);
        push_meta!(r *associated_token_instruction_program.key);
        push_meta!(r *event_authority.key);
        push_meta!(r *program_id.key);
        push_meta!(w *vault_ata.key);
        push_meta!(r *vault_authority.key);
        push_meta!(r *fee_config.key);
        push_meta!(r *fee_program.key);
        if self.is_merhem {
            push_meta!(w *user_vol_wsol_ata.key);
            push_meta!(w *user_volume_accumulator.key);
        }
        push_meta!(r *cashback_pool_id.key);

        let mut data = [0u8; 24];
        data[..8].copy_from_slice(&SELL_DISC);
        data[8..16].copy_from_slice(&amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&min_amount_out_value.to_le_bytes());

        let swap_ix = Instruction {
            program_id: *program_id.key,
            accounts: metas[..n].to_vec(),
            data: data.to_vec(),
        };

        // Stack-allocated account infos array — no heap Vec
        let mut accs: [AccountInfo<'a>; 24] = unsafe { core::mem::zeroed() };
        let mut ai = 0usize;
        macro_rules! push_acc {
            (ref $e:expr) => { accs[ai] = unsafe { std::mem::transmute($e.clone()) }; ai += 1; };
            (own $e:expr) => { accs[ai] = unsafe { std::mem::transmute($e.to_account_info()) }; ai += 1; };
        }
        push_acc!(ref pool_id);
        push_acc!(own payer);
        push_acc!(ref pump_amm_global);
        push_acc!(own base_mint_account);
        push_acc!(own quote_mint_account);
        push_acc!(own user_base_token_account);
        push_acc!(own user_quote_token_account);
        push_acc!(ref base_vault);
        push_acc!(ref quote_vault);
        push_acc!(ref protocol_fee_recipient);
        push_acc!(ref protocol_fee_token_account);
        push_acc!(own base_token_program);
        push_acc!(own quote_token_program);
        push_acc!(ref system_program);
        push_acc!(ref associated_token_instruction_program);
        push_acc!(ref event_authority);
        push_acc!(ref program_id);
        push_acc!(ref vault_ata);
        push_acc!(ref vault_authority);
        push_acc!(ref fee_config);
        push_acc!(ref fee_program);
        if self.is_merhem {
            push_acc!(ref user_vol_wsol_ata);
            push_acc!(ref user_volume_accumulator);
        }
        push_acc!(ref cashback_pool_id);

        invoke_unchecked(&swap_ix, &accs[..ai])?;
        Ok(())
    }
    

    #[cfg(any(test, feature = "debug"))]
    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        msg!("=== Pump AMM (static_base={}, dyn_start={}) ===", self.static_base, self.dyn_start);
        // Static accounts
        msg!("S0 program_id: {}", accounts[self.static_base + S_PROGRAM_ID].key);
        msg!("S1 protocol_fee_recipient: {}", accounts[self.static_base + S_PROTOCOL_FEE_RECIPIENT].key);
        msg!("S2 protocol_fee_recipient_token_acc: {}", accounts[self.static_base + S_PROTOCOL_FEE_RECIPIENT_TOKEN_ACC].key);
        msg!("S3 event_authority: {}", accounts[self.static_base + S_EVENT_AUTHORITY].key);
        msg!("S4 fee_config: {}", accounts[self.static_base + S_FEE_CONFIG].key);
        msg!("S5 fee_program: {}", accounts[self.static_base + S_FEE_PROGRAM].key);
        msg!("S6 pump_amm_global: {}", accounts[self.static_base + S_PUMP_AMM_GLOBAL].key);
        msg!("S7 system_program: {}", accounts[self.static_base + S_SYSTEM_PROGRAM].key);
        msg!("S8 assoc_token_program: {}", accounts[self.static_base + S_ASSOC_TOKEN_PROGRAM].key);
        msg!("S9 global_vol_acc: {}", accounts[self.static_base + S_GLOBAL_VOL_ACC].key);
        // Dynamic accounts
        msg!("D0 pool: {}", accounts[self.dyn_start + D_POOL].key);
        msg!("D1 base_vault: {}", accounts[self.dyn_start + D_BASE_VAULT].key);
        msg!("D2 quote_vault: {}", accounts[self.dyn_start + D_QUOTE_VAULT].key);
        msg!("D3 user_vol_acc: {}", accounts[self.dyn_start + D_USER_VOL_ACC].key);
        msg!("D4 pool_v2: {}", accounts[self.dyn_start + D_POOL_V2].key);
        msg!("D5 user_vol_wsol_ata: {}", accounts[self.dyn_start + D_USER_VOL_WSOL_ATA].key);
        msg!("D6 vault_ata: {}", accounts[self.dyn_start + D_VAULT_ATA].key);
        msg!("D7 vault_authority: {}", accounts[self.dyn_start + D_VAULT_AUTHORITY].key);
        msg!("D8 cashback_pool_id: {}", accounts[self.dyn_start + D_CASHBACK_POOL_ID].key);
        // Mints from pool state
        msg!("base_token_pk: {}", self.base_token_pk);
        msg!("quote_token_pk: {}", self.quote_token_pk);
        Ok(())
    }

}



impl PumpAmm {

    pub fn new<'a>(
        accounts: &[AccountInfo<'a>],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
        pool_fees: &[u32],
    ) -> Result<Self> {
        let base_vault = &accounts[dyn_start + D_BASE_VAULT];
        let quote_vault = &accounts[dyn_start + D_QUOTE_VAULT];

        let pool_acc = &accounts[dyn_start + D_POOL];
        // Read mint pubkeys from pool state account data
        // Pool layout: 8 disc + 1 bump + 2 index + 32 creator + 32 base_mint + 32 quote_mint
        // let pool_data = pool_acc.try_borrow_data()
        //     .map_err(|_| ProgramError::InvalidAccountData)?;
        // let base_token_pk = Pubkey::try_from(&pool_data[43..75])
        //     .map_err(|_| ProgramError::InvalidAccountData)?;
        // let quote_token_pk = Pubkey::try_from(&pool_data[75..107])
        //     .map_err(|_| ProgramError::InvalidAccountData)?;
        // drop(pool_data);
        
        let (base_token_pk, base_vault_amount) = read_vault_data(base_vault)?;
        let (quote_token_pk, quote_vault_amount) = read_vault_data(quote_vault)?;
        // TODO: maket to run in test
        #[cfg(test)]
        let base_vault_amount: u64 = (base_vault_amount as f64 * 1.04) as u64;
        let price = get_price_f64(base_vault_amount, quote_vault_amount);
        // fee from client-side pool_fees[0] (millionths, e.g. 12500 = 1.25%)
        let fee_numerator: u64 = pool_fees[0] as u64;
        
        debug_eprintln!("pool_id {} , price {}, inverse_price {}",  *pool_acc.key, price, 1.0 / price);

        // Defer max amounts, transfer fees, and is_merhem to prepare_for_execution()
        let instance = PumpAmm {
            price,
            fee_numerator,
            pool_id: *pool_acc.key,
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
            is_merhem: false,
            prepared: false,

        };
        // instance.log_accounts(accounts)?;
        Ok(instance)
    }

    /// Compute deferred fields: max amounts, transfer fee rates, is_merhem.
    /// Called only for instances that participate in a profitable arb path.
    pub fn prepare_for_execution<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
    ) {
        if self.prepared {
            return;
        }
        self.prepared = true;

        let pool_id = &accounts[self.dyn_start + D_POOL];
        if let Ok(pool_data) = pool_id.try_borrow_data() {
            self.is_merhem = pool_data.len() > POOL_IS_CASHBACK_OFFSET
                && pool_data[POOL_IS_CASHBACK_OFFSET] != 0;
        }

        let ff = FEE_DENOM - self.fee_numerator as u128; // fee factor scaled by 1M
        let (buy_max_in, buy_max_out) = {
            // Buy: fee on input. dx = x * target / (eff - target)
            // where eff = y * ff / 1M, target = eff * 99 / 100
            let x = self.base_vault_amount as u128;
            let y = self.quote_vault_amount as u128;
            let eff = y * ff; // scaled by 1M
            let target = eff * 99 / 100;
            let denom = eff - target; // = eff / 100
            if denom == 0 {
                (0u64, y as u64)
            } else {
                let dx = x.saturating_mul(target) / denom;
                (dx.min(u64::MAX as u128) as u64, y as u64)
            }
        };
        let (sell_max_in, sell_max_out) = {
            // Sell: fee on output. dx = x * target / (ff * (y - target) / 1M)
            let x = self.quote_vault_amount as u128;
            let y = self.base_vault_amount as u128;
            let target = y * 99 / 100;
            let denom = ff * (y - target); // scaled by 1M
            if denom == 0 {
                (0u64, y as u64)
            } else {
                // dx = x * target * 1M / denom (cancel the 1M scaling in denom)
                let dx = x.saturating_mul(target).saturating_mul(FEE_DENOM) / denom;
                (dx.min(u64::MAX as u128) as u64, y as u64)
            }
        };
        self.buy_max_in = buy_max_in;
        self.buy_max_out = buy_max_out;
        self.sell_max_in = sell_max_in;
        self.sell_max_out = sell_max_out;
    }

    pub fn initialize_reserves<'a>(&mut self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        Ok(())
    }

    /// CP formula: out = base - (base * quote) / (quote + in)
    /// Safe without checked ops: base_reserve & quote_reserve are u64, so product fits u128.
    /// Output is always <= base_reserve (u64), so `as u64` is safe.
    #[inline(always)]
    pub fn calculate_buy_amount_out(
        &self,
        base_reserve: u128,
        quote_reserve: u128,
        amount_in: u128,
    ) -> Result<u64> {
        let denominator = quote_reserve + amount_in;
        if denominator == 0 { return Ok(0); }
        let out = base_reserve - base_reserve * quote_reserve / denominator;
        Ok(out as u64)
    }

    /// CP formula: out = quote - (base * quote) / (base + in)
    #[inline(always)]
    pub fn calculate_sell_amount_out(
        &self,
        base_reserve: u128,
        quote_reserve: u128,
        amount_in: u128,
    ) -> Result<u64> {
        let denominator = base_reserve + amount_in;
        if denominator == 0 { return Ok(0); }
        let out = quote_reserve - base_reserve * quote_reserve / denominator;
        Ok(out as u64)
    }
    /// Calculate base output amount for a given quote input amount
    /// Formula: base_amount_out = base_reserve - (base_reserve * quote_reserve) / (quote_reserve + quote_amount_in)
    /// Then applies 0.02% fee (multiply by 0.9998)
    pub fn swap_base_in_impl<'a>(
        &self,
        _accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        mint_fees: &[(Pubkey, MintFee)],
    ) -> Result<u64> {
        // Use u128 math to avoid overflow on large vaults
        let base_reserve = self.base_vault_amount as u128;
        let quote_reserve = self.quote_vault_amount as u128;

        let output_reserve = if input_mint == self.base_token_pk {
            self.quote_vault_amount
        } else {
            self.base_vault_amount
        };

        let (fee_in, fee_out) = crate::utils::token::get_transfer_fees(
            input_mint, &self.base_token_pk, &self.quote_token_pk, mint_fees,
        );

        let amount_out_after_fee = if input_mint == self.quote_token_pk {
            let transfer_fee = apply_transfer_fee(amount_in, fee_in);
            let amount_in_after_fee = amount_in.checked_sub(transfer_fee).unwrap();

            let amount_in_after_fees = (amount_in_after_fee as u128) * (FEE_DENOM - self.fee_numerator as u128) / FEE_DENOM;

            let amount_out: u64 =
                self.calculate_buy_amount_out(base_reserve, quote_reserve, amount_in_after_fees)?;

            let transfer_fee_out = apply_transfer_fee(amount_out, fee_out);
            let amount_out_after_fee = amount_out.checked_sub(transfer_fee_out).unwrap();
            amount_out_after_fee.min(output_reserve)
        } else {
            // Selling base for quote: fee is applied on quote OUTPUT (not base input)
            let transfer_fee = apply_transfer_fee(amount_in, fee_in);
            let amount_in_after_fee = amount_in.checked_sub(transfer_fee).unwrap();

            // No pool fee on base input
            let amount_out = self.calculate_sell_amount_out(base_reserve, quote_reserve, amount_in_after_fee as u128)?;

            // Apply pool fee on quote output
            let amount_out_after_pool_fee = ((amount_out as u128) * (FEE_DENOM - self.fee_numerator as u128) / FEE_DENOM) as u64;

            let transfer_fee_out = apply_transfer_fee(amount_out_after_pool_fee, fee_out);
            let amount_out_after_fee = amount_out_after_pool_fee.checked_sub(transfer_fee_out).unwrap();
            amount_out_after_fee.min(output_reserve)
        };
        Ok(amount_out_after_fee)
    }

    /// Calculate the maximum input amount required to receive a specific output amount
    /// This is the inverse of swap_base_in_impl
    /// Given output_mint and amount_out, returns the required amount_in
    /// Note: Pool fee is only applied on the QUOTE side (not base)
    pub fn swap_base_out_impl<'a>(
        &self,
        _accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
    ) -> Result<u64> {
        let base_reserve = self.base_vault_amount as u128;
        let quote_reserve = self.quote_vault_amount as u128;

        let output_reserve = if output_mint == self.base_token_pk {
            self.base_vault_amount
        } else {
            self.quote_vault_amount
        };
        let amount_out = amount_out.min(output_reserve.saturating_sub(1));

        let max_amount_in = if output_mint == self.base_token_pk {
            // Buying base with quote

            let transfer_fee_out = apply_transfer_fee(amount_out, output_transfer_fee);
            let amount_out_before_transfer_fee = (amount_out as u128)
                .checked_add(transfer_fee_out as u128)
                .ok_or(ProgramError::InvalidArgument)?;

            let numerator = quote_reserve
                .checked_mul(amount_out_before_transfer_fee)
                .ok_or(ProgramError::InvalidArgument)?;
            let denominator = base_reserve
                .checked_sub(amount_out_before_transfer_fee)
                .ok_or(ProgramError::InvalidArgument)?;
            let amount_in_after_fee = numerator
                .checked_div(denominator)
                .ok_or(ProgramError::InvalidArgument)?
                .checked_add(1)
                .ok_or(ProgramError::InvalidArgument)?;

            let ff = FEE_DENOM - self.fee_numerator as u128;
            let amount_in_before_fee = (amount_in_after_fee * FEE_DENOM + ff - 1) / ff;

            let amount_in_u64 = u64::try_from(amount_in_before_fee)
                .map_err(|_| ProgramError::InvalidArgument)?;
            let transfer_fee_in = apply_transfer_fee(amount_in_u64, input_transfer_fee);
            amount_in_u64
                .checked_add(transfer_fee_in)
                .ok_or(ProgramError::InvalidArgument)?
        } else {
            // Selling base for quote

            let transfer_fee_out = apply_transfer_fee(amount_out, output_transfer_fee);
            let amount_out_before_transfer_fee = (amount_out as u128)
                .checked_add(transfer_fee_out as u128)
                .ok_or(ProgramError::InvalidArgument)?;

            let ff = FEE_DENOM - self.fee_numerator as u128;
            let quote_out_before_fee = (amount_out_before_transfer_fee * FEE_DENOM + ff - 1) / ff;

            let numerator = base_reserve
                .checked_mul(quote_out_before_fee)
                .ok_or(ProgramError::InvalidArgument)?;
            let denominator = quote_reserve
                .checked_sub(quote_out_before_fee)
                .ok_or(ProgramError::InvalidArgument)?;
            let base_in = numerator
                .checked_div(denominator)
                .ok_or(ProgramError::InvalidArgument)?
                .checked_add(1)
                .ok_or(ProgramError::InvalidArgument)?;

            let amount_in_u64 = u64::try_from(base_in)
                .map_err(|_| ProgramError::InvalidArgument)?;
            let transfer_fee_in = apply_transfer_fee(amount_in_u64, input_transfer_fee);
            amount_in_u64
                .checked_add(transfer_fee_in)
                .ok_or(ProgramError::InvalidArgument)?
        };

        Ok(max_amount_in)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::token::MintFee;
    use crate::utils::utils::read_token_amount;
    use anchor_lang::solana_program::system_program;
    use solana_client::nonblocking::rpc_client::RpcClient;

    const POOL_BASE_MINT_OFFSET: usize = 43;
    const POOL_QUOTE_MINT_OFFSET: usize = 75;
    const POOL_BASE_VAULT_OFFSET: usize = 139;
    const POOL_QUOTE_VAULT_OFFSET: usize = 171;

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
        let sdk_pubkey = SdkPubkey::try_from(key.to_bytes().as_ref()).unwrap();
        let account = rpc_client
            .get_account(&sdk_pubkey)
            .await
            .unwrap_or_else(|e| panic!("Failed to fetch account {}: {}", key, e));
        account_to_account_info(key, account)
    }

    fn create_mock_account_info(key: Pubkey) -> AccountInfo<'static> {
        let data = Box::leak(Box::new(Vec::new()));
        let lamports = Box::leak(Box::new(0u64));
        let owner_static = Box::leak(Box::new(system_program::id()));
        let key_static = Box::leak(Box::new(key));
        AccountInfo::new(key_static, false, false, lamports, data, owner_static, false, 0)
    }

    fn get_rpc_client() -> RpcClient {
        let api_key = "f230200b-f911-43c1-a242-4e7b066d0993";
        RpcClient::new(format!("https://mainnet.helius-rpc.com/?api-key={}", api_key))
    }

    async fn build_from_pool_id(pool_id_key: Pubkey) -> (PumpAmm, Vec<AccountInfo<'static>>) {
        let rpc_client = get_rpc_client();

        // Fetch pool account and decode all needed pubkeys
        let pool_account_info = fetch_account_info_from_rpc(&rpc_client, pool_id_key).await;
        let pool_data = pool_account_info.try_borrow_data().unwrap();

        let base_mint = Pubkey::try_from(&pool_data[POOL_BASE_MINT_OFFSET..POOL_BASE_MINT_OFFSET + 32]).unwrap();
        let quote_mint = Pubkey::try_from(&pool_data[POOL_QUOTE_MINT_OFFSET..POOL_QUOTE_MINT_OFFSET + 32]).unwrap();
        let base_vault_key = Pubkey::try_from(&pool_data[POOL_BASE_VAULT_OFFSET..POOL_BASE_VAULT_OFFSET + 32]).unwrap();
        let quote_vault_key = Pubkey::try_from(&pool_data[POOL_QUOTE_VAULT_OFFSET..POOL_QUOTE_VAULT_OFFSET + 32]).unwrap();
        drop(pool_data);

        eprintln!("Pool ID:     {}", pool_id_key);
        eprintln!("Base mint:   {}", base_mint);
        eprintln!("Quote mint:  {}", quote_mint);
        eprintln!("Base vault:  {}", base_vault_key);
        eprintln!("Quote vault: {}", quote_vault_key);

        // Fetch vault accounts from RPC
        let base_vault_account = fetch_account_info_from_rpc(&rpc_client, base_vault_key).await;
        let quote_vault_account = fetch_account_info_from_rpc(&rpc_client, quote_vault_key).await;

        // Compute fee from vault amounts
        let base_vault_amount = read_token_amount(&base_vault_account).unwrap();
        let quote_vault_amount = read_token_amount(&quote_vault_account).unwrap();
        let fee = get_fees_int(base_vault_amount, quote_vault_amount);
        eprintln!("Fee: {} ({}%)", fee, fee as f64 / 10_000.0);

        // Static accounts (10) — mock with real keys
        let static_base = 0;
        let dyn_start = 10;
        let accounts = vec![
            create_mock_account_info(PROGRAM_ID),                                                              // S0
            create_mock_account_info(Pubkey::from_str_const("62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV")), // S1: protocol_fee_recipient
            create_mock_account_info(Pubkey::from_str_const("94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb")), // S2: protocol_fee_token_acc
            create_mock_account_info(Pubkey::from_str_const("GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR")), // S3: event_authority
            create_mock_account_info(Pubkey::from_str_const("5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx")), // S4: fee_config
            create_mock_account_info(Pubkey::from_str_const("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ")),  // S5: fee_program
            create_mock_account_info(Pubkey::from_str_const("ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw")), // S6: pump_amm_global
            create_mock_account_info(system_program::id()),                                                     // S7: system_program
            create_mock_account_info(Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL")), // S8: assoc_token_prog
            create_mock_account_info(Pubkey::from_str_const("C2aFPdENg4A2HQsmrd5rTw5TaYBX5Ku887cWjbFKtZpw")), // S9: global_vol_acc
            // Dynamic accounts (indices 10-18)
            pool_account_info,                                     // D0: pool
            base_vault_account,                                    // D1: base_vault
            quote_vault_account,                                   // D2: quote_vault
            create_mock_account_info(Pubkey::new_unique()),        // D3: user_vol_acc
            create_mock_account_info(Pubkey::new_unique()),        // D4: pool_v2
            create_mock_account_info(Pubkey::new_unique()),        // D5: user_vol_wsol_ata
            create_mock_account_info(Pubkey::new_unique()),        // D6: vault_ata
            create_mock_account_info(Pubkey::new_unique()),        // D7: vault_authority
            create_mock_account_info(Pubkey::new_unique()),        // D8: cashback_pool_id
        ];

        let dyn_end = accounts.len();
        let pump_amm = PumpAmm::new(accounts.as_slice(), static_base, dyn_start, dyn_end, &[fee as u32]).unwrap();

        (pump_amm, accounts)
    }

    #[tokio::test]
    async fn test_pump_amm_round_trip() {
        let pool_id = Pubkey::from_str_const("BM7Qw7JbGtyLoZw3canKF6Q6EJDp1Q3PYuHQhTNwoq2D");
        let (mut pump_amm, accounts) = build_from_pool_id(pool_id).await;
        eprintln!("{}", pump_amm.base_vault_amount);
        eprintln!("{}", pump_amm.quote_vault_amount);

        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let no_fee = MintFee::ZERO;

        // 1. Print all account pubkeys
        eprintln!("\n=== Account Pubkeys ===");
        eprintln!("base_mint        : {}", pump_amm.base_token_pk);
        eprintln!("quote_mint       : {}", pump_amm.quote_token_pk);
        eprintln!("pool_id          : {}", accounts[10 + D_POOL].key);
        eprintln!("base_vault       : {}", accounts[10 + D_BASE_VAULT].key);
        eprintln!("quote_vault      : {}", accounts[10 + D_QUOTE_VAULT].key);
        eprintln!("program_id       : {}", accounts[S_PROGRAM_ID].key);

        // 2. Prices
        let (price, inverse_price) = pump_amm.get_prices().unwrap();
        eprintln!("\n=== Prices ===");
        eprintln!("price            : {}", price);
        eprintln!("inverse_price    : {}", inverse_price);

        // 3. Fees
        let (fee_factor, fee_factor_2) = pump_amm.get_fee_factor().unwrap();
        eprintln!("\n=== Fees ===");
        eprintln!("fee_numerator    : {}", pump_amm.fee_numerator);
        eprintln!("fee_factor       : {}", fee_factor);
        eprintln!("fee_factor_2     : {}", fee_factor_2);

        // 4. prepare_for_execution
        pump_amm.prepare_for_execution(&accounts);
        eprintln!("\n=== After prepare_for_execution ===");
        eprintln!("buy_max_in       : {}", pump_amm.buy_max_in);
        eprintln!("buy_max_out      : {}", pump_amm.buy_max_out);
        eprintln!("sell_max_in      : {}", pump_amm.sell_max_in);
        eprintln!("sell_max_out     : {}", pump_amm.sell_max_out);

        // 5. Round-trip with start_amount = 1 WSOL
        let start_amount: u64 = 1_000_000_000; // 1 SOL
        let clock = Clock::default();

        let other_mint = if pump_amm.base_token_pk == sol_mint {
            pump_amm.quote_token_pk
        } else {
            pump_amm.base_token_pk
        };

        let rpc = get_rpc_client();
        let other_mint_account = rpc.get_account(
            &solana_sdk::pubkey::Pubkey::try_from(other_mint.to_bytes().as_ref()).unwrap()
        ).await.unwrap();
        let token_decimals = other_mint_account.data[44] as i32;
        let sol_div = 10f64.powi(9);
        let tok_div = 10f64.powi(token_decimals);

        // Direction 1: SOL -> TOKEN -> SOL
        eprintln!("\n=== Direction 1: SOL -> TOKEN -> SOL ===");
        let token_out = pump_amm.swap_base_in(
            &accounts, sol_mint, start_amount, no_fee, no_fee, &clock,
        ).unwrap();
        let max_sol_in = pump_amm.swap_base_out(
            &accounts, other_mint, token_out, no_fee, no_fee, &clock,
        ).unwrap();
        eprintln!("AMOUNT_IN {} -> AMOUNT_OUT {} -> MAX_AMOUNT_IN {}", start_amount as f64 / sol_div, token_out as f64 / tok_div, max_sol_in as f64 / sol_div);

        // Direction 2: TOKEN -> SOL -> TOKEN
        eprintln!("\n=== Direction 2: TOKEN -> SOL -> TOKEN ===");
        let sol_out = pump_amm.swap_base_in(
            &accounts, other_mint, token_out, no_fee, no_fee, &clock,
        ).unwrap();
        let max_token_in = pump_amm.swap_base_out(
            &accounts, sol_mint, sol_out, no_fee, no_fee, &clock,
        ).unwrap();
        eprintln!("AMOUNT_IN {} -> AMOUNT_OUT {} -> MAX_AMOUNT_IN {}", token_out as f64 / tok_div, sol_out as f64 / sol_div, max_token_in as f64 / tok_div);
    }
}
