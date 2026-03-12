use crate::programs::ProgramMeta;
use crate::utils::token::{apply_transfer_fee, apply_transfer_inverse_fee, lookup_fee_rate};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use bytemuck;
pub mod damm_v2;

/// Precomputed Q64 scale factor (2^64) for sqrt_price calculations
/// Avoids recomputing `Q64_SCALE` on every call
const Q64_SCALE: f64 = 18446744073709551616.0; // Q64_SCALE
pub use damm_v2::curve::{get_spot_price_a_to_b, get_spot_price_b_to_a};
pub use crate::utils::utils::parse_token_account;
pub use damm_v2::{ActivationType, FeeMode, Pool, TradeDirection};
use damm_v2::constants::fee::get_max_fee_numerator;
use damm_v2::curve::{get_delta_amount_a_unsigned, get_delta_amount_a_unsigned_unchecked, get_delta_amount_b_unsigned, get_delta_amount_b_unsigned_unchecked};
use ruint::aliases::U256;
use damm_v2::u128x128_math::Rounding;
use std::marker::PhantomData;

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

pub fn get_prices(pool: Pool) -> Result<(f64, f64)> {
    // price : token_A -> token_B (A -> B)
    // inverse_price : token_B -> token_A (B -> A)
    let actual_sqrt_price = pool.sqrt_price as f64 / Q64_SCALE;
    let price_b_to_a_base = actual_sqrt_price * actual_sqrt_price; // token_b / token_a in base units
    let price = 1.0 / price_b_to_a_base; // token_a / token_b in base units
    Ok((price_b_to_a_base as f64, price as f64))
}

#[derive(Clone)]
pub struct MeteoraDammV2<'info> {
    // pub program_id: AccountInfo<'info>,
    // pub pool_id: AccountInfo<'info>,
    // pub base_vault: AccountInfo<'info>,
    // pub quote_vault: AccountInfo<'info>,
    pub pool_id: Pubkey,
    pub base_token_pk: Pubkey,
    pub quote_token_pk: Pubkey,
    // pub pool_authority: AccountInfo<'info>,
    // pub event_authority: AccountInfo<'info>,
    // pub referral_token_account: AccountInfo<'info>,
    pub pool: Pool,
    pub base_vault_amount: u64,
    pub quote_vault_amount: u64,
    pub static_base: usize,
    pub dyn_start: usize,
    pub price: f64,
    pub inverse_price: f64,
    pub fee_rate_a_to_b: f64,
    pub fee_rate_b_to_a: f64,
    pub buy_max_in: u128,
    pub buy_max_out: u64,
    pub sell_max_in: u128,
    pub sell_max_out: u64,
    pub base_fee_rate: f64,
    pub quote_fee_rate: f64,
    pub prepared: bool,
    pub phantom: PhantomData<&'info ()>,
}

impl<'info> ProgramMeta for MeteoraDammV2<'info> {
    fn get_id(&self) -> &Pubkey {
        &Self::PROGRAM_ID
    }

    fn get_pool_id(&self) -> &Pubkey {
        &self.pool_id
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        // price : token_A -> token_B (A -> B)
        // inverse_price : token_B -> token_A (B -> A)
        let actual_sqrt_price = self.pool.sqrt_price as f64 / Q64_SCALE;
        let price_b_to_a_base = actual_sqrt_price * actual_sqrt_price; // token_b / token_a in base units
        let price = 1.0 / price_b_to_a_base; // token_a / token_b in base units
        Ok((price_b_to_a_base as f64, price as f64))
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (&self.base_token_pk, &self.quote_token_pk)
    }

    fn name(&self) -> &'static str { "MeteoraDammV2" }

    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        // Virtual reserves from concentrated liquidity parameters.
        // Within a single range, CLAMM behaves as constant-product with:
        //   virtual_base  = L / sqrt_price_q64
        //   virtual_quote = L * sqrt_price_q64 / 2^128
        let sq = self.pool.sqrt_price as f64;
        let l = self.pool.liquidity as f64;
        let q128: f64 = Q64_SCALE * Q64_SCALE; // 2^128

        let virtual_base = l / sq;
        let virtual_quote = l * sq / q128;

        if !virtual_base.is_finite() || !virtual_quote.is_finite()
            || virtual_base <= 0.0 || virtual_quote <= 0.0
        {
            return Err(error!(crate::programs::SolarBError::InvalidAccountData));
        }

        Ok((virtual_base as u64, virtual_quote as u64))
    }

    fn is_fee_on_input(&self, input_mint: Pubkey) -> bool {
        match self.pool.collect_fee_mode {
            0 => false, // BothToken: fee always on output
            1 => input_mint != self.base_token_pk, // OnlyB: fee on input only for B→A
            _ => false,
        }
    }

    fn get_fee_factor(&self) -> Result<(f64, f64)> { Ok((1.0 - self.fee_rate_a_to_b, 1.0 - self.fee_rate_b_to_a)) }

    fn get_max_amount_in<'a>(&self, _accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_in.min(u64::MAX as u128) as u64) } else { Ok(self.sell_max_in.min(u64::MAX as u128) as u64) }
    }

    fn get_max_amount_out<'a>(&self, _accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_out) } else { Ok(self.sell_max_out) }
    }

    fn get_cached_max_amounts(&self, input_mint: Pubkey) -> (u64, u64) {
        if input_mint == self.base_token_pk {
            ((self.buy_max_in.min(u64::MAX as u128)) as u64, self.buy_max_out)
        } else {
            ((self.sell_max_in.min(u64::MAX as u128)) as u64, self.sell_max_out)
        }
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
        eprintln!("max_in: {}, max_out: {}", max_in, max_out);
        let amount_in = amount_in.min(max_in);
        debug_eprintln!("[DAMM V2] Fast quote: {:.9} SOL ({}) -> {:.6} tokens ({})", amount_in as f64 / 1_000_000_000.0, amount_in, max_out as f64 / 1_000_000.0, max_out);

        let trade_direction = if input_mint == self.base_token_pk {
            TradeDirection::AtoB
        } else {
            TradeDirection::BtoA
        };

        // No referral for fast_quote, use u64::MAX as current_point (always past activation)
        let fee_mode = FeeMode::get_fee_mode(self.pool.collect_fee_mode, trade_direction, false)?;
        let results = self.pool.get_swap_result_from_exact_input(
            amount_in,
            &fee_mode,
            trade_direction,
            u64::MAX,
        )?;

        Ok((amount_in, results.output_amount.min(max_out)))
    }

    fn swap_base_in<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        clock: &Clock,
    ) -> Result<u64> {
        // Determine trade direction based on input_mint
        let trade_direction = if input_mint == self.base_token_pk {
            TradeDirection::AtoB
        } else {
            TradeDirection::BtoA
        };

        let current_timestamp = clock.unix_timestamp as u64;
        let current_slot = clock.slot as u64;

        let current_point =
            get_current_point(self.pool.activation_type, current_slot, current_timestamp)?;

        let referral_token_account = &accounts[self.static_base + Self::S_REFERRAL_TOKEN_ACCOUNT];
        let has_referral = !referral_token_account.key.eq(&Pubkey::default()); // TODO: check if this is correct
        let fee_mode: FeeMode =
            FeeMode::get_fee_mode(self.pool.collect_fee_mode, trade_direction, has_referral)?;

        let (fee_in, fee_out) = if input_mint == self.base_token_pk {
            (self.base_fee_rate, self.quote_fee_rate)
        } else {
            (self.quote_fee_rate, self.base_fee_rate)
        };

        let transfer_fee = apply_transfer_fee(amount_in, fee_in);
        let amount_in_after_fee = amount_in.checked_sub(transfer_fee).unwrap();

        let results = self.pool.get_swap_result_from_exact_input(
            amount_in_after_fee,
            &fee_mode,
            trade_direction,
            current_point,
        )?;

        let transfer_fee_out = apply_transfer_fee(results.output_amount, fee_out);
        let amount_out_after_fee = results.output_amount.checked_sub(transfer_fee_out).unwrap();

        Ok(amount_out_after_fee)
    }

    fn swap_base_out<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        clock: &Clock,
    ) -> Result<u64> {
        // Determine trade direction based on output_mint
        // If output is quote (B), direction is A→B (base to quote)
        // If output is base (A), direction is B→A (quote to base)
        let trade_direction = if output_mint == self.base_token_pk {
            TradeDirection::BtoA // Output is base, input is quote
        } else {
            TradeDirection::AtoB // Output is quote, input is base
        };
        let current_timestamp = clock.unix_timestamp as u64;
        let current_slot = clock.slot as u64;

        let current_point =
            get_current_point(self.pool.activation_type, current_slot, current_timestamp)?;

        let (fee_in, fee_out) = if output_mint == self.base_token_pk {
            (self.quote_fee_rate, self.base_fee_rate)
        } else {
            (self.base_fee_rate, self.quote_fee_rate)
        };

        let transfer_fee = apply_transfer_inverse_fee(amount_out, fee_out);
        let amount_out_with_fees = amount_out.checked_add(transfer_fee).unwrap();

        let referral_token_account = &accounts[self.static_base + Self::S_REFERRAL_TOKEN_ACCOUNT];
        let has_referral = !referral_token_account.key.eq(&Pubkey::default()); // TODO: check if this is correct
        let fee_mode =
            FeeMode::get_fee_mode(self.pool.collect_fee_mode, trade_direction, has_referral)?;
        let results = self.pool.get_swap_result_from_exact_output(
            amount_out_with_fees,
            &fee_mode,
            trade_direction,
            current_point,
        )?;

        let transfer_fee_in = apply_transfer_inverse_fee(results.included_fee_input_amount, fee_in);
        let amount_in_with_fees = results
            .included_fee_input_amount
            .checked_add(transfer_fee_in)
            .unwrap();

        // Return the input amount needed to get the desired output
        Ok(amount_in_with_fees)
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
        let (
            base_token_program,
            quote_token_program,
            user_base_token_account,
            user_quote_token_account,
        ) = if self.base_token_pk == *mint_1_account.key {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
            )
        } else {
            (
                mint_2_token_program,
                mint_1_token_program,
                user_mint_2_token_account,
                user_mint_1_token_account,
            )
        };

        let (input_token_account, output_token_account) = if self.base_token_pk == input_mint {
            (user_base_token_account, user_quote_token_account)
        } else {
            (user_quote_token_account, user_base_token_account)
        };

        let program_id = &accounts[self.static_base + Self::S_PROGRAM_ID];
        let pool_authority = &accounts[self.static_base + Self::S_POOL_AUTHORITY];
        let event_authority = &accounts[self.static_base + Self::S_EVENT_AUTHORITY];
        let referral_token_account = &accounts[self.static_base + Self::S_REFERRAL_TOKEN_ACCOUNT];
        let pool_id = &accounts[self.dyn_start + Self::D_POOL];
        let base_vault = &accounts[self.dyn_start + Self::D_BASE_VAULT];
        let quote_vault = &accounts[self.dyn_start + Self::D_QUOTE_VAULT];

        let amount_out_value = amount_out.unwrap_or(0);
        let metas = vec![
            AccountMeta::new_readonly(*pool_authority.key, false),
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new(*input_token_account.key, false),
            AccountMeta::new(*output_token_account.key, false),
            AccountMeta::new(*base_vault.key, false),
            AccountMeta::new(*quote_vault.key, false),
            AccountMeta::new_readonly(self.base_token_pk, false),
            AccountMeta::new_readonly(self.quote_token_pk, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*base_token_program.key, false),
            AccountMeta::new_readonly(*quote_token_program.key, false),
            if referral_token_account.key == program_id.key {
                AccountMeta::new_readonly(*referral_token_account.key, false)
            } else {
                AccountMeta::new(*referral_token_account.key, false)
            },
            AccountMeta::new_readonly(*event_authority.key, false),
            AccountMeta::new_readonly(*program_id.key, false),
        ];

        let mut data = [0u8; 24];
        data[..8].copy_from_slice(&Self::SWAP_DISC);
        data[8..16].copy_from_slice(&max_amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&amount_out_value.to_le_bytes());

        let swap_ix = Instruction {
            program_id: *program_id.key,
            accounts: metas,
            data: data.to_vec(),
        };

        // Stack-allocated accounts - avoids heap allocation
        let accounts_arr = [
            pool_authority.clone(),
            pool_id.clone(),
            base_vault.clone(),
            quote_vault.clone(),
            unsafe { std::mem::transmute(referral_token_account.clone()) },
            event_authority.clone(),
            program_id.clone(),
            unsafe { std::mem::transmute(input_token_account.to_account_info()) },
            unsafe { std::mem::transmute(output_token_account.to_account_info()) },
            unsafe { std::mem::transmute(payer.to_account_info()) },
            unsafe { std::mem::transmute(base_token_program.to_account_info()) },
            unsafe { std::mem::transmute(quote_token_program.to_account_info()) },
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
        let (
            base_token_program,
            quote_token_program,
            user_base_token_account,
            user_quote_token_account,
        ) = if mint_1_account.key == &self.base_token_pk {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
            )
        } else if mint_2_account.key == &self.base_token_pk {
            (
                mint_2_token_program,
                mint_1_token_program,
                user_mint_2_token_account,
                user_mint_1_token_account,
            )
        } else {
            return Err(ProgramError::InvalidAccountData.into());
        };

        let program_id = &accounts[self.static_base + Self::S_PROGRAM_ID];
        let pool_authority = &accounts[self.static_base + Self::S_POOL_AUTHORITY];
        let event_authority = &accounts[self.static_base + Self::S_EVENT_AUTHORITY];
        let referral_token_account = &accounts[self.static_base + Self::S_REFERRAL_TOKEN_ACCOUNT];
        let pool_id = &accounts[self.dyn_start + Self::D_POOL];
        let base_vault = &accounts[self.dyn_start + Self::D_BASE_VAULT];
        let quote_vault = &accounts[self.dyn_start + Self::D_QUOTE_VAULT];

        let (input_token_account, output_token_account) = if self.base_token_pk == input_mint {
            (user_base_token_account, user_quote_token_account)
        } else {
            (user_quote_token_account, user_base_token_account)
        };

        let min_amount_out_value = min_amount_out.unwrap_or(0);
        let metas = vec![
            AccountMeta::new_readonly(*pool_authority.key, false), // pool_authority
            AccountMeta::new(*pool_id.key, false),                 // pool_id
            AccountMeta::new(*input_token_account.key, false),     // input_token_account
            AccountMeta::new(*output_token_account.key, false),    // output_token_account
            AccountMeta::new(*base_vault.key, false), // base_vault
            AccountMeta::new(*quote_vault.key, false), // quote_vault
            AccountMeta::new_readonly(self.base_token_pk, false),
            AccountMeta::new_readonly(self.quote_token_pk, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*base_token_program.key, false),
            AccountMeta::new_readonly(*quote_token_program.key, false),
            if referral_token_account.key == program_id.key {
                AccountMeta::new_readonly(*referral_token_account.key, false)
            } else {
                AccountMeta::new(*referral_token_account.key, false)
            },
            AccountMeta::new_readonly(*event_authority.key, false), // event_authority
            AccountMeta::new_readonly(*program_id.key, false),      // program_id
        ];
        let mut data = [0u8; 24];
        data[..8].copy_from_slice(&Self::SWAP_DISC);
        data[8..16].copy_from_slice(&amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&min_amount_out_value.to_le_bytes());

        let swap_ix = Instruction {
            program_id: *program_id.key,
            accounts: metas,
            data: data.to_vec(),
        };

        // Stack-allocated accounts - avoids heap allocation
        let accounts_arr = [
            pool_authority.clone(),
            pool_id.clone(),
            base_vault.clone(),
            quote_vault.clone(),
            unsafe { std::mem::transmute(referral_token_account.to_account_info()) },
            event_authority.clone(),
            program_id.clone(),
            unsafe { std::mem::transmute(input_token_account.to_account_info()) },
            unsafe { std::mem::transmute(output_token_account.to_account_info()) },
            unsafe { std::mem::transmute(payer.to_account_info()) },
            unsafe { std::mem::transmute(base_token_program.to_account_info()) },
            unsafe { std::mem::transmute(quote_token_program.to_account_info()) },
        ];

        unsafe {
            let accounts_slice: &[AccountInfo<'a>] = std::mem::transmute(accounts_arr.as_slice());
            invoke(&swap_ix, accounts_slice)?;
        }
        Ok(())
    }

        #[cfg(any(test, feature = "debug"))]
        fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        msg!("=== Meteora DAMM V2 ===");
        msg!("S0 program_id: {}", accounts[self.static_base + Self::S_PROGRAM_ID].key);
        msg!("S1 pool_authority: {}", accounts[self.static_base + Self::S_POOL_AUTHORITY].key);
        msg!("S2 event_authority: {}", accounts[self.static_base + Self::S_EVENT_AUTHORITY].key);
        msg!("S3 referral_token_account: {}", accounts[self.static_base + Self::S_REFERRAL_TOKEN_ACCOUNT].key);
        msg!("D0 pool: {}", accounts[self.dyn_start + Self::D_POOL].key);
        msg!("D1 base_vault: {}", accounts[self.dyn_start + Self::D_BASE_VAULT].key);
        msg!("D2 quote_vault: {}", accounts[self.dyn_start + Self::D_QUOTE_VAULT].key);
        msg!("base_token (from pool): {}", self.base_token_pk);
        msg!("quote_token (from pool): {}", self.quote_token_pk);
        msg!("base_fee_mode: {} cliff_fee: {} actual_fee_rate: {}",
            self.pool.pool_fees.base_fee.base_fee_mode,
            self.pool.pool_fees.base_fee.cliff_fee_numerator,
            self.fee_rate_a_to_b);
        Ok(())
    }

}

impl<'info> MeteoraDammV2<'info> {
    const SWAP_DISC: [u8; 8] = [0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];
    pub const PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");
    // Static accounts (from static_base, shared across pools)
    pub const S_PROGRAM_ID: usize = 0;
    pub const S_POOL_AUTHORITY: usize = 1;
    pub const S_EVENT_AUTHORITY: usize = 2;
    pub const S_REFERRAL_TOKEN_ACCOUNT: usize = 3;

    // Dynamic accounts (from dyn_start, per-pool)
    pub const D_POOL: usize = 0;
    pub const D_BASE_VAULT: usize = 1;
    pub const D_QUOTE_VAULT: usize = 2;

    pub const MIN_ACCOUNTS: usize = 3; // dynamic account count
    pub fn new(
        accounts: &[AccountInfo<'info>],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
        clock: &Clock,
        mint_fees: &[(Pubkey, f64)],
    ) -> Result<Self> {
        if dyn_end.saturating_sub(dyn_start) < Self::MIN_ACCOUNTS {
            return Err(ProgramError::NotEnoughAccountKeys.into());
        }
        // Access accounts by indices
        let pool_id = accounts[dyn_start + Self::D_POOL].clone();
        let pool_data = pool_id.try_borrow_data()?;
        let pool: Pool = bytemuck::pod_read_unaligned(&pool_data[8..]);
        // Read base/quote token pubkeys from pool state (no longer passed as accounts)
        let base_token_pk = pool.token_a_mint;
        let quote_token_pk = pool.token_b_mint;
        let (price, inverse_price) = get_prices(pool)?;
        let base_vault = accounts[dyn_start + Self::D_BASE_VAULT].clone();
        let quote_vault = accounts[dyn_start + Self::D_QUOTE_VAULT].clone();
        let base_vault_amount = parse_token_account(&base_vault)?.amount;
        let quote_vault_amount = parse_token_account(&quote_vault)?.amount;

        // debug_eprintln!("base_vault_amount: {:?}", base_vault_amount);
        // debug_eprintln!("quote_vault_amount: {:?}", quote_vault_amount);

        // Compute actual fee using the full fee pipeline (base fee scheduler + dynamic fee)
        // cliff_fee_numerator is the initial/max fee, but the actual fee decays over time
        // via FeeSchedulerLinear/Exponential, and dynamic (volatility) fee is added on top
        let current_point = get_current_point(
            pool.activation_type,
            clock.slot,
            clock.unix_timestamp as u64,
        )
        .unwrap_or(0);
        let max_fee = get_max_fee_numerator(pool.version).unwrap_or(500_000_000);
        let fallback_fee = pool.pool_fees.base_fee.cliff_fee_numerator as f64 / 1_000_000_000.0;
        let fee_rate_a_to_b = pool.pool_fees
            .get_total_trading_fee_from_excluded_fee_amount(
                current_point,
                pool.activation_point,
                0,
                TradeDirection::AtoB,
                max_fee,
            )
            .map(|fee_num| fee_num as f64 / 1_000_000_000.0)
            .unwrap_or(fallback_fee);
        let fee_rate_b_to_a = pool.pool_fees
            .get_total_trading_fee_from_excluded_fee_amount(
                current_point,
                pool.activation_point,
                0,
                TradeDirection::BtoA,
                max_fee,
            )
            .map(|fee_num| fee_num as f64 / 1_000_000_000.0)
            .unwrap_or(fallback_fee);

        // Defer max amounts and transfer fees to prepare_for_execution()
        let instance = MeteoraDammV2 {
            base_token_pk,
            quote_token_pk,
            pool: pool.clone(),
            pool_id: *pool_id.key,
            price: price,
            inverse_price: inverse_price,
            base_vault_amount: base_vault_amount,
            quote_vault_amount: quote_vault_amount,
            static_base,
            dyn_start,
            fee_rate_a_to_b,
            fee_rate_b_to_a,
            buy_max_in: 0,
            buy_max_out: 0,
            sell_max_in: 0,
            sell_max_out: 0,
            base_fee_rate: 0.0,
            quote_fee_rate: 0.0,
            prepared: false,
            phantom: PhantomData,
        };
        // instance.log_accounts(accounts)?;
        Ok(instance)
    }

    /// Compute deferred fields: max amounts, transfer fee rates.
    /// Called only for instances that participate in a profitable arb path.
    pub fn prepare_for_execution(
        &mut self,
        _accounts: &[AccountInfo<'info>],
        mint_fees: &[(Pubkey, f64)],
    ) {
        if self.prepared {
            return;
        }
        self.prepared = true;

        self.base_fee_rate = lookup_fee_rate(mint_fees, &self.base_token_pk);
        self.quote_fee_rate = lookup_fee_rate(mint_fees, &self.quote_token_pk);

        // Cache max amounts from curve math (sqrt_min_price / sqrt_max_price boundaries)
        // A→B (buy): price moves from sqrt_price down toward sqrt_min_price
        self.buy_max_in = get_delta_amount_a_unsigned_unchecked(
            self.pool.sqrt_min_price, self.pool.sqrt_price, self.pool.liquidity, Rounding::Up,
        ).map(|v| v.min(U256::from(u128::MAX)).try_into().unwrap_or(u128::MAX)).unwrap_or(0);
        self.buy_max_out = get_delta_amount_b_unsigned(
            self.pool.sqrt_min_price, self.pool.sqrt_price, self.pool.liquidity, Rounding::Down,
        ).unwrap_or(0).min(self.quote_vault_amount);
        // B→A (sell): price moves from sqrt_price up toward sqrt_max_price
        self.sell_max_in = get_delta_amount_b_unsigned_unchecked(
            self.pool.sqrt_price, self.pool.sqrt_max_price, self.pool.liquidity, Rounding::Up,
        ).map(|v| v.min(U256::from(u128::MAX)).try_into().unwrap_or(u128::MAX)).unwrap_or(0);
        self.sell_max_out = get_delta_amount_a_unsigned(
            self.pool.sqrt_price, self.pool.sqrt_max_price, self.pool.liquidity, Rounding::Down,
        ).unwrap_or(0).min(self.base_vault_amount);
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use anchor_lang::solana_program::{
        account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, system_program,
    };
    use bytemuck;

    fn default_clock() -> Clock {
        Clock {
            slot: 350_000_000,
            epoch_start_timestamp: 0,
            epoch: 700,
            leader_schedule_epoch: 0,
            unix_timestamp: 1739800000, // ~Feb 2025
        }
    }
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
        let pool_id = Pubkey::new_unique();
        let pool = create_mock_account_info(pool_id, system_program::id(), None);
        let pool_data = pool.try_borrow_data().unwrap();
        let pool: Pool = bytemuck::pod_read_unaligned(&pool_data[8..]);
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
        // static_base=0, dyn_start=4, dyn_end=4 -> 0 dynamic accounts < MIN_ACCOUNTS
        let program_id = Pubkey::new_unique();
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        let referral_token_account = Pubkey::new_unique();
        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),
            create_mock_account_info(pool_authority, system_program::id(), None),
            create_mock_account_info(event_authority, system_program::id(), None),
            create_mock_account_info(referral_token_account, system_program::id(), None),
        ];
        let result = MeteoraDammV2::new(accounts.as_slice(), 0, 4, 4, &default_clock(), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_meteora_damm_v2_new_sufficient_accounts() {
        let program_id = Pubkey::new_unique();
        let pool_id = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        let referral_token_account = Pubkey::new_unique();

        // Static accounts: [0..4), Dynamic accounts: [4..7)
        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),       // S0
            create_mock_account_info(pool_authority, system_program::id(), None),    // S1
            create_mock_account_info(event_authority, system_program::id(), None),   // S2
            create_mock_account_info(referral_token_account, system_program::id(), None), // S3
            create_mock_account_info(pool_id, system_program::id(), None),           // D0
            create_mock_account_info(base_vault, system_program::id(), None),        // D1
            create_mock_account_info(quote_vault, system_program::id(), None),       // D2
        ];

        let result = MeteoraDammV2::new(accounts.as_slice(), 0, 4, 7, &default_clock(), &[]);
        assert!(result.is_ok());

        let meteora = result.unwrap();
        // base_token_pk and quote_token_pk now come from pool state (token_a_mint / token_b_mint)
        // With a zeroed pool, they will be Pubkey::default()
        assert_eq!(meteora.base_token_pk, Pubkey::default());
        assert_eq!(meteora.quote_token_pk, Pubkey::default());
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
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        let referral_token_account = Pubkey::new_unique();

        // Static [0..4), Dynamic [4..7)
        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),       // S0
            create_mock_account_info(pool_authority, system_program::id(), None),    // S1
            create_mock_account_info(event_authority, system_program::id(), None),   // S2
            create_mock_account_info(referral_token_account, system_program::id(), None), // S3
            pool_account.clone(),                                                    // D0
            create_mock_account_info(base_vault, system_program::id(), None),        // D1
            create_mock_account_info(quote_vault, system_program::id(), None),       // D2
        ];

        let mut meteora = MeteoraDammV2::new(accounts.as_slice(), 0, 4, 7, &default_clock(), &[]).unwrap();
        let data = accounts[4].try_borrow_data().unwrap();
        let pool: Pool = bytemuck::pod_read_unaligned(&data[8..]);

        debug_eprintln!("pool: {:?}", pool.token_a_mint);
        debug_eprintln!("pool: {:?}", pool.token_b_mint);
        debug_eprintln!("pool: {:?}", pool.token_a_vault);
        debug_eprintln!("pool: {:?}", pool.token_b_vault);
        debug_eprintln!("pool activation_point: {}", pool.activation_point);
        debug_eprintln!("pool activation_type: {}", pool.activation_type);
        debug_eprintln!("pool liquidity: {}", pool.liquidity);
        debug_eprintln!("pool pool_status: {}", pool.pool_status);
        debug_eprintln!("pool sqrt_price: {}", pool.sqrt_price);

        // Use actual addresses from pool data for important accounts
        let program_id = MeteoraDammV2::PROGRAM_ID;
        let base_vault = pool.token_a_vault;
        let quote_vault = pool.token_b_vault;
        let base_token = pool.token_a_mint;
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        let referral_token_account = Pubkey::default(); // Use default for no referral

        let correct_accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),       // S0
            create_mock_account_info(pool_authority, system_program::id(), None),    // S1
            create_mock_account_info(event_authority, system_program::id(), None),   // S2
            create_mock_account_info(referral_token_account, system_program::id(), None), // S3
            pool_account.clone(),                                                    // D0
            create_mock_account_info(base_vault, system_program::id(), None),        // D1
            create_mock_account_info(quote_vault, system_program::id(), None),       // D2
        ];

        let mut meteora_correct =
            MeteoraDammV2::new(correct_accounts.as_slice(), 0, 4, 7, &default_clock(), &[]).unwrap();

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
        let result =
            meteora_correct.swap_base_in(correct_accounts.as_slice(), input_mint, amount_in, &clock);
        debug_eprintln!("result: {:?}", result);
        if let Err(ref e) = result {
            debug_eprintln!("Error: {:?}", e);
        }
        // Should succeed and return some output amount
        assert!(result.is_ok());
        let output_amount = result.unwrap();
        assert!(output_amount > 0);
        debug_eprintln!("Result {:?}", output_amount);
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
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        let referral_token_account = Pubkey::new_unique();

        // Static [0..4), Dynamic [4..7)
        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),       // S0
            create_mock_account_info(pool_authority, system_program::id(), None),    // S1
            create_mock_account_info(event_authority, system_program::id(), None),   // S2
            create_mock_account_info(referral_token_account, system_program::id(), None), // S3
            pool_account.clone(),                                                    // D0
            create_mock_account_info(base_vault, system_program::id(), None),        // D1
            create_mock_account_info(quote_vault, system_program::id(), None),       // D2
        ];

        let mut meteora = MeteoraDammV2::new(accounts.as_slice(), 0, 4, 7, &default_clock(), &[]).unwrap();
        let data = accounts[4].try_borrow_data().unwrap();
        let pool: Pool = bytemuck::pod_read_unaligned(&data[8..]);

        debug_eprintln!("pool: {:?}", pool.token_a_mint);
        debug_eprintln!("pool: {:?}", pool.token_b_mint);

        let clock = Clock {
            slot: 1000,
            epoch_start_timestamp: 0,
            epoch: 0,
            leader_schedule_epoch: 0,
            unix_timestamp: 1234567890,
        };

        // Test with a small amount (desired output amount)
        let amount_out = 1_000_000_000; // Desired output amount
        let quote_token = pool.token_b_mint;
        let input_mint = quote_token; // For swap_base_out, input is quote_token to get base_token out
        let result = meteora.swap_base_out(accounts.as_slice(), input_mint, amount_out, &clock);

        // Should succeed and return some output amount
        assert!(result.is_ok());
        let output_amount = result.unwrap();
        assert!(output_amount > 0);
        debug_eprintln!("Result {:?}", output_amount);
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
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        // Use a non-default referral token account
        let referral_token_account = Pubkey::new_unique();

        // Static [0..4), Dynamic [4..7)
        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),       // S0
            create_mock_account_info(pool_authority, system_program::id(), None),    // S1
            create_mock_account_info(event_authority, system_program::id(), None),   // S2
            create_mock_account_info(referral_token_account, system_program::id(), None), // S3
            pool_account.clone(),                                                    // D0
            create_mock_account_info(base_vault, system_program::id(), None),        // D1
            create_mock_account_info(quote_vault, system_program::id(), None),       // D2
        ];

        let mut meteora = MeteoraDammV2::new(accounts.as_slice(), 0, 4, 7, &default_clock(), &[]).unwrap();

        let clock = Clock {
            slot: 1000,
            epoch_start_timestamp: 0,
            epoch: 0,
            leader_schedule_epoch: 0,
            unix_timestamp: 1234567890,
        };

        let amount_in = 1_000_000;
        let base_token = meteora.base_token_pk;
        let input_mint = base_token; // Swap base token in
        let result = meteora.swap_base_in(accounts.as_slice(), input_mint, amount_in, &clock);

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
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        // Use default (zero) referral token account
        let referral_token_account = Pubkey::default();

        // Static [0..4), Dynamic [4..7)
        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),       // S0
            create_mock_account_info(pool_authority, system_program::id(), None),    // S1
            create_mock_account_info(event_authority, system_program::id(), None),   // S2
            create_mock_account_info(referral_token_account, system_program::id(), None), // S3
            pool_account.clone(),                                                    // D0
            create_mock_account_info(base_vault, system_program::id(), None),        // D1
            create_mock_account_info(quote_vault, system_program::id(), None),       // D2
        ];

        let mut meteora = MeteoraDammV2::new(accounts.as_slice(), 0, 4, 7, &default_clock(), &[]).unwrap();

        let clock = Clock {
            slot: 1000,
            epoch_start_timestamp: 0,
            epoch: 0,
            leader_schedule_epoch: 0,
            unix_timestamp: 1234567890,
        };

        let amount_in = 1_000_000;
        let base_token = meteora.base_token_pk;
        let input_mint = base_token; // Swap base token in
        let result = meteora.swap_base_in(accounts.as_slice(), input_mint, amount_in, &clock);

        // Should succeed without referral
        assert!(result.is_ok());
    }

    #[test]
    fn test_program_meta_implementation() {
        let program_id = MeteoraDammV2::PROGRAM_ID;
        let pool_id = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        let referral_token_account = Pubkey::new_unique();

        // Static [0..4), Dynamic [4..7)
        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),       // S0
            create_mock_account_info(pool_authority, system_program::id(), None),    // S1
            create_mock_account_info(event_authority, system_program::id(), None),   // S2
            create_mock_account_info(referral_token_account, system_program::id(), None), // S3
            create_mock_account_info(pool_id, system_program::id(), None),           // D0
            create_mock_account_info(base_vault, system_program::id(), None),        // D1
            create_mock_account_info(quote_vault, system_program::id(), None),       // D2
        ];

        let meteora = MeteoraDammV2::new(accounts.as_slice(), 0, 4, 7, &default_clock(), &[]).unwrap();

        // Test ProgramMeta trait implementation
        let id = meteora.get_id();
        assert_eq!(*id, MeteoraDammV2::PROGRAM_ID);
    }

    #[tokio::test]
    async fn test_damm_v2_swap() {
        use anchor_client::Cluster;
        use solana_client::nonblocking::rpc_client::RpcClient;

        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        let pool_id = Pubkey::from_str_const("EHxQbaBa2Mc4MjgGnTuf9iv2yZnzugwq4RcMEMgujN9d");
        let pool_account_info = fetch_account_info_from_rpc(&rpc_client, pool_id).await;

        // Read pool data from AccountInfo in a separate scope to drop the borrow
        let (token_a_mint, token_b_mint, token_a_vault, token_b_vault) = {
            let pool_data: std::cell::Ref<'_, &mut [u8]> =
                pool_account_info.try_borrow_data().unwrap();
            let pool: Pool = bytemuck::pod_read_unaligned(&pool_data[8..]);

            debug_eprintln!("Mint A: {:?}", pool.token_a_mint);
            debug_eprintln!("Mint B: {:?}", pool.token_b_mint);
            debug_eprintln!("Pool A Vault: {:?}", pool.token_a_vault);
            debug_eprintln!("Pool B Vault: {:?}", pool.token_b_vault);
            debug_eprintln!("pool activation_point: {}", pool.activation_point);
            debug_eprintln!("pool activation_type: {}", pool.activation_type);
            debug_eprintln!("pool liquidity: {}", pool.liquidity);
            debug_eprintln!("pool pool_status: {}", pool.pool_status);
            debug_eprintln!("pool sqrt_price: {}", pool.sqrt_price);

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

        // Static [0..4), Dynamic [4..7)
        let accounts = vec![
            program_id_account,             // S0: program_id
            pool_authority.clone(),         // S1: pool_authority
            event_authority.clone(),        // S2: event_authority
            referral_token_account.clone(), // S3: referral_token_account
            pool_account_info.clone(),      // D0: pool
            base_vault_account.clone(),     // D1: base_vault
            quote_vault_account.clone(),    // D2: quote_vault
        ];

        let clock1 = get_clock(&rpc_client).await.unwrap();
        let mut meteora_damm_v2 = MeteoraDammV2::new(accounts.as_slice(), 0, 4, 7, &clock1, &[]).unwrap();

        let prices = meteora_damm_v2.get_prices().unwrap();
        let price = prices.0;
        let inverse_price = prices.1;
        debug_eprintln!("price: {:?}", price);
        debug_eprintln!("inverse_price: {:?}", inverse_price);
        debug_eprintln!("================================================");

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

        let (max_sol_in, max_token_out) = (meteora_damm_v2.buy_max_in, meteora_damm_v2.buy_max_out);
        debug_eprintln!("Max SOL IN: {:?} -> MAX TOKEN OUT: {:?}", max_sol_in as f64 / 1_000_000_000.0, max_token_out as f64 / 1_000_000.0);

        debug_eprintln!("Sol price: {:?}", sol_price);
        debug_eprintln!("Token price: {:?}", token_price);
        let token_out: u64 = meteora_damm_v2
            .swap_base_in(accounts.as_slice(), sol_mint, in_sol_amount, &clock1)
            .unwrap();

        // Expected using oracle price (for debug only)
        let expected_token_out = (in_sol_amount as f64 * sol_price) as u64;

        debug_eprintln!(
            "Step 1 (swap_base_in): {} SOL -> {} TOKEN / {}",
            in_sol_amount as f64 / 1_000_000_000.0,
            token_out as f64 / 1_000_000.0,
            expected_token_out as f64 / 1_000_000.0,
        );

        let max_sol_in = meteora_damm_v2
            .swap_base_out(accounts.as_slice(), token_mint, token_out, &clock1)
            .unwrap();
        debug_eprintln!(
            "Step 1 (swap_base_out): {} MAX SOL IN -> {} TOKEN OUT",
            max_sol_in as f64 / 1_000_000_000.0,
            token_out as f64 / 1_000_000.0
        );
        debug_eprintln!("================================================");

        let sol_out = meteora_damm_v2
            .swap_base_in(accounts.as_slice(), token_mint, token_out, &clock1)
            .unwrap();
        let expected_sol_out = (token_out as f64 * token_price) as u64;

        debug_eprintln!(
            "Step 2 (swap_base_in): {} TOKEN -> {} SOL / {}",
            token_out as f64 / 1_000_000.0,
            sol_out as f64 / 1_000_000_000.0,
            expected_sol_out as f64 / 1_000_000_000.0,
        );
        let max_token_in = meteora_damm_v2
            .swap_base_out(accounts.as_slice(), sol_mint, sol_out, &clock1)
            .unwrap();
        debug_eprintln!(
            "Step 2 (swap_base_out): {} MAX TOKEN IN -> {} SOL OUT",
            max_token_in as f64 / 1_000_000.0,
            sol_out as f64 / 1_000_000_000.0,
        );
        debug_eprintln!("================================================");
    }
}
