use crate::programs::{PoolKind, ProgramMeta};
use crate::utils::token::{apply_transfer_fee, apply_transfer_inverse_fee, MintFee};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke_unchecked,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use bytemuck;
pub mod damm_v2;

/// Precomputed Q64 scale factor (2^64) for sqrt_price calculations
/// Avoids recomputing `Q64_SCALE` on every call
const Q64_SCALE: f64 = 18446744073709551616.0; // Q64_SCALE
pub use damm_v2::curve::{get_spot_price_a_to_b, get_spot_price_b_to_a};
use crate::utils::utils::{read_token_amount, read_vault_data};
pub use damm_v2::{ActivationType, FeeMode, Pool, TradeDirection};
use damm_v2::curve::{get_delta_amount_a_unsigned, get_delta_amount_a_unsigned_unchecked, get_delta_amount_b_unsigned, get_delta_amount_b_unsigned_unchecked};
use ruint::aliases::U256;
use damm_v2::u128x128_math::Rounding;

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

pub fn get_current_point(
    activation_type: u8,
    current_slot: u64,
    current_timestamp: u64,
) -> Result<u64> {
    let activation_type =
        ActivationType::try_from(activation_type).map_err(|_| ProgramError::InvalidAccountData)?;

    let current_point = match activation_type {
        ActivationType::Slot => current_slot,
        ActivationType::Timestamp => current_timestamp,
    };

    Ok(current_point)
}

pub fn get_prices(sqrt_price: u128) -> Result<(f64, f64)> {
    // price : token_A -> token_B (A -> B)
    // inverse_price : token_B -> token_A (B -> A)
    let actual_sqrt_price = sqrt_price as f64 / Q64_SCALE;
    let price_b_to_a_base = actual_sqrt_price * actual_sqrt_price; // token_b / token_a in base units
    let price = 1.0 / price_b_to_a_base; // token_a / token_b in base units
    Ok((price_b_to_a_base as f64, price as f64))
}

#[derive(Clone)]
pub struct MeteoraDammV2 {
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
    pub pool: Option<Pool>,
    pub sqrt_price: u128,
    pub liquidity: u128,
    pub collect_fee_mode: u8,
    pub activation_type: u8,
    pub base_vault_amount: u64,
    pub quote_vault_amount: u64,
    pub static_base: usize,
    pub dyn_start: usize,
    pub price: f64,
    pub inverse_price: f64,
    pub fee_rate_a_to_b: f64,
    pub fee_rate_b_to_a: f64,
    /// Pre-computed fee factor: (1 - fee_rate_a_to_b, 1 - fee_rate_b_to_a)
    pub fee_factor: (f64, f64),
    pub buy_max_in: u128,
    pub buy_max_out: u64,
    pub sell_max_in: u128,
    pub sell_max_out: u64,
    pub prepared: bool,
}

impl ProgramMeta for MeteoraDammV2 {
    fn get_id(&self) -> &Pubkey {
        &PROGRAM_ID
    }

    fn get_pool_id(&self) -> &Pubkey {
        &self.pool_id
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        // price : token_A -> token_B (A -> B)
        // inverse_price : token_B -> token_A (B -> A)
        let actual_sqrt_price = self.sqrt_price as f64 / Q64_SCALE;
        let price_b_to_a_base = actual_sqrt_price * actual_sqrt_price; // token_b / token_a in base units
        let price = 1.0 / price_b_to_a_base; // token_a / token_b in base units
        Ok((price_b_to_a_base as f64, price as f64))
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (&self.base_token_pk, &self.quote_token_pk)
    }

    fn name(&self) -> &'static str { "MeteoraDammV2" }
    fn pool_kind(&self) -> PoolKind { PoolKind::MeteoraDammV2 }

    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        // Virtual reserves from concentrated liquidity parameters.
        // Within a single range, CLAMM behaves as constant-product with:
        //   virtual_base  = L / sqrt_price_q64
        //   virtual_quote = L * sqrt_price_q64 / 2^128
        let sq = self.sqrt_price as f64;
        let l = self.liquidity as f64;
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
        match self.collect_fee_mode {
            0 => false, // BothToken: fee always on output
            1 => input_mint != self.base_token_pk, // OnlyB: fee on input only for B→A
            _ => false,
        }
    }

    fn get_fee_factor(&self) -> Result<(f64, f64)> { Ok(self.fee_factor) }

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

    fn fast_quote<'a>(&mut self, _accounts: &[AccountInfo<'a>], input_mint: Pubkey, amount_in: u64, _profit_pct: f64) -> Result<(u64, u64)> {
        let (max_in, max_out) = self.get_cached_max_amounts(input_mint);
        debug_eprintln!("max_in: {}, max_out: {}", max_in, max_out);
        let amount_in = amount_in.min(max_in);
        debug_eprintln!("[DAMM V2] Fast quote: {:.9} SOL ({}) -> {:.6} tokens ({})", amount_in as f64 / 1_000_000_000.0, amount_in, max_out as f64 / 1_000_000.0, max_out);

        if amount_in == 0 {
            return Ok((0, 0));
        }

        let pool = match self.pool.as_ref() {
            Some(p) => p,
            None => return Ok((0, 0)),
        };

        let trade_direction = if input_mint == self.base_token_pk {
            TradeDirection::AtoB
        } else {
            TradeDirection::BtoA
        };

        // No referral for fast_quote, use u64::MAX as current_point (always past activation)
        let fee_mode = FeeMode::get_fee_mode(self.collect_fee_mode, trade_direction, false)?;
        let results = pool.get_swap_result_from_exact_input(
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
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        clock: &Clock,
    ) -> Result<u64> {
        if self.pool.is_none() {
            let pool_id = &accounts[self.dyn_start + D_POOL];
            let pool_data = pool_id.try_borrow_data()?;
            self.pool = Some(bytemuck::pod_read_unaligned(&pool_data[8..]));
        }
        let trade_direction = if input_mint == self.base_token_pk {
            TradeDirection::AtoB
        } else {
            TradeDirection::BtoA
        };

        let current_timestamp = clock.unix_timestamp as u64;
        let current_slot = clock.slot as u64;

        let current_point =
            get_current_point(self.activation_type, current_slot, current_timestamp)?;

        let referral_token_account = &accounts[self.static_base + S_REFERRAL_TOKEN_ACCOUNT];
        let has_referral = !referral_token_account.key.eq(&Pubkey::default());
        let fee_mode: FeeMode =
            FeeMode::get_fee_mode(self.collect_fee_mode, trade_direction, has_referral)?;

        let transfer_fee = apply_transfer_fee(amount_in, input_transfer_fee);
        let amount_in_after_fee = amount_in.checked_sub(transfer_fee).unwrap();

        let pool = self.pool.as_ref().unwrap();
        let results = pool.get_swap_result_from_exact_input(
            amount_in_after_fee,
            &fee_mode,
            trade_direction,
            current_point,
        )?;

        let transfer_fee_out = apply_transfer_fee(results.output_amount, output_transfer_fee);
        let amount_out_after_fee = results.output_amount.checked_sub(transfer_fee_out).unwrap();

        Ok(amount_out_after_fee)
    }

    fn swap_base_out<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        clock: &Clock,
    ) -> Result<u64> {
        if self.pool.is_none() {
            let pool_id = &accounts[self.dyn_start + D_POOL];
            let pool_data = pool_id.try_borrow_data()?;
            self.pool = Some(bytemuck::pod_read_unaligned(&pool_data[8..]));
        }
        let trade_direction = if output_mint == self.base_token_pk {
            TradeDirection::BtoA
        } else {
            TradeDirection::AtoB
        };
        let current_timestamp = clock.unix_timestamp as u64;
        let current_slot = clock.slot as u64;

        let current_point =
            get_current_point(self.activation_type, current_slot, current_timestamp)?;

        let transfer_fee = apply_transfer_inverse_fee(amount_out, output_transfer_fee);
        let amount_out_with_fees = amount_out.checked_add(transfer_fee).unwrap();

        let referral_token_account = &accounts[self.static_base + S_REFERRAL_TOKEN_ACCOUNT];
        let has_referral = !referral_token_account.key.eq(&Pubkey::default());
        let fee_mode =
            FeeMode::get_fee_mode(self.collect_fee_mode, trade_direction, has_referral)?;
        let pool = self.pool.as_ref().unwrap();
        let results = pool.get_swap_result_from_exact_output(
            amount_out_with_fees,
            &fee_mode,
            trade_direction,
            current_point,
        )?;

        let transfer_fee_in = apply_transfer_inverse_fee(results.included_fee_input_amount, input_transfer_fee);
        let amount_in_with_fees = results
            .included_fee_input_amount
            .checked_add(transfer_fee_in)
            .unwrap();

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

        let program_id = &accounts[self.static_base + S_PROGRAM_ID];
        let pool_authority = &accounts[self.static_base + S_POOL_AUTHORITY];
        let event_authority = &accounts[self.static_base + S_EVENT_AUTHORITY];
        let referral_token_account = &accounts[self.static_base + S_REFERRAL_TOKEN_ACCOUNT];
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let base_vault = &accounts[self.dyn_start + D_BASE_VAULT];
        let quote_vault = &accounts[self.dyn_start + D_QUOTE_VAULT];

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
        data[..8].copy_from_slice(&SWAP_DISC);
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
            invoke_unchecked(&swap_ix, accounts_slice)?;
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

        let program_id = &accounts[self.static_base + S_PROGRAM_ID];
        let pool_authority = &accounts[self.static_base + S_POOL_AUTHORITY];
        let event_authority = &accounts[self.static_base + S_EVENT_AUTHORITY];
        let referral_token_account = &accounts[self.static_base + S_REFERRAL_TOKEN_ACCOUNT];
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let base_vault = &accounts[self.dyn_start + D_BASE_VAULT];
        let quote_vault = &accounts[self.dyn_start + D_QUOTE_VAULT];

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
        data[..8].copy_from_slice(&SWAP_DISC);
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
            invoke_unchecked(&swap_ix, accounts_slice)?;
        }
        Ok(())
    }

        #[cfg(any(test, feature = "debug"))]
        fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        msg!("=== Meteora DAMM V2 ===");
        msg!("S0 program_id: {}", accounts[self.static_base + S_PROGRAM_ID].key);
        msg!("S1 pool_authority: {}", accounts[self.static_base + S_POOL_AUTHORITY].key);
        msg!("S2 event_authority: {}", accounts[self.static_base + S_EVENT_AUTHORITY].key);
        msg!("S3 referral_token_account: {}", accounts[self.static_base + S_REFERRAL_TOKEN_ACCOUNT].key);
        msg!("D0 pool: {}", accounts[self.dyn_start + D_POOL].key);
        msg!("D1 base_vault: {}", accounts[self.dyn_start + D_BASE_VAULT].key);
        msg!("D2 quote_vault: {}", accounts[self.dyn_start + D_QUOTE_VAULT].key);
        msg!("base_token (from pool): {}", self.base_token_pk);
        msg!("quote_token (from pool): {}", self.quote_token_pk);
        msg!("base_fee_mode: {} cliff_fee: {} actual_fee_rate: {}",
            self.pool.as_ref().map(|p| p.pool_fees.base_fee.base_fee_mode).unwrap_or(0),
            self.pool.as_ref().map(|p| p.pool_fees.base_fee.cliff_fee_numerator).unwrap_or(0),
            self.fee_rate_a_to_b);
        Ok(())
    }

}

impl MeteoraDammV2 {

    pub fn new<'info>(
        accounts: &[AccountInfo<'info>],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
        clock: &Clock,
    ) -> Result<Self> {
        // Access accounts by indices
        let pool_id = accounts[dyn_start + D_POOL].clone();
        let pool_data = pool_id.try_borrow_data()?;
        // Read fields directly at byte offsets (after 8-byte discriminator) to avoid deserializing entire Pool
        // Pool-relative offsets: liquidity=352, sqrt_price=448, activation_type=472, collect_fee_mode=476
        // Raw offsets (with 8-byte disc): 360, 456, 480, 484
        let sqrt_price = u128::from_le_bytes(pool_data[456..472].try_into().unwrap());
        let liquidity = u128::from_le_bytes(pool_data[360..376].try_into().unwrap());
        let activation_type = pool_data[480];
        let collect_fee_mode = pool_data[484];
        // Read base/quote token pubkeys from pool state (no longer passed as accounts)


        let (price, inverse_price) = get_prices(sqrt_price)?;
        let base_vault = accounts[dyn_start + D_BASE_VAULT].clone();
        let quote_vault = accounts[dyn_start + D_QUOTE_VAULT].clone();
        let (base_token_pk, base_vault_amount )= read_vault_data(&base_vault)?;
        let (quote_token_pk, quote_vault_amount) = read_vault_data(&quote_vault)?;

        // debug_eprintln!("base_vault_amount: {:?}", base_vault_amount);
        // debug_eprintln!("quote_vault_amount: {:?}", quote_vault_amount);

        // Read cliff_fee_numerator (Pool offset 0, raw offset 8) as conservative fee approximation.
        // This is the initial/max base fee before schedule decay; actual fee may be lower.
        // The full fee pipeline (schedule + dynamic) runs in fast_quote/swap_base_in via Pool methods.
        let cliff_fee_numerator = u64::from_le_bytes(pool_data[8..16].try_into().unwrap());
        let fee_rate = cliff_fee_numerator as f64 / 1_000_000_000.0;
        let fee_rate_a_to_b = fee_rate;
        let fee_rate_b_to_a = fee_rate;

        debug_eprintln!("MeteoraDammV2: pool_id {} , price {}, inverse_price {}, fee_rate {}", *pool_id.key, price, inverse_price, fee_rate);

        // Defer max amounts and transfer fees to prepare_for_execution()
        let instance = MeteoraDammV2 {
            base_token_pk,
            quote_token_pk,
            pool: None,
            sqrt_price,
            liquidity,
            collect_fee_mode,
            activation_type,
            pool_id: *pool_id.key,
            price: price,
            inverse_price: inverse_price,
            base_vault_amount: base_vault_amount,
            quote_vault_amount: quote_vault_amount,
            static_base,
            dyn_start,
            fee_rate_a_to_b,
            fee_rate_b_to_a,
            fee_factor: (1.0 - fee_rate_a_to_b, 1.0 - fee_rate_b_to_a),
            buy_max_in: 0,
            buy_max_out: 0,
            sell_max_in: 0,
            sell_max_out: 0,
            prepared: false,
        };
        // instance.log_accounts(accounts)?;
        Ok(instance)
    }

    /// Compute deferred fields: max amounts, transfer fee rates.
    /// Called only for instances that participate in a profitable arb path.
    pub fn prepare_for_execution<'info>(
        &mut self,
        accounts: &[AccountInfo<'info>],
    ) {
        if self.prepared {
            return;
        }
        self.prepared = true;

        // Load the full pool now that we know this instance participates in a profitable path
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let pool_data = pool_id.try_borrow_data().unwrap();
        let pool: Pool = bytemuck::pod_read_unaligned(&pool_data[8..]);
        self.pool = Some(pool);

        let pool = self.pool.as_ref().unwrap();
        // Cache max amounts from curve math (sqrt_min_price / sqrt_max_price boundaries)
        // A→B (buy): price moves from sqrt_price down toward sqrt_min_price
        self.buy_max_in = get_delta_amount_a_unsigned_unchecked(
            pool.sqrt_min_price, pool.sqrt_price, pool.liquidity, Rounding::Up,
        ).map(|v| v.min(U256::from(u128::MAX)).try_into().unwrap_or(u128::MAX)).unwrap_or(0);
        self.buy_max_out = get_delta_amount_b_unsigned(
            pool.sqrt_min_price, pool.sqrt_price, pool.liquidity, Rounding::Down,
        ).unwrap_or(0).min(self.quote_vault_amount);
        // B→A (sell): price moves from sqrt_price up toward sqrt_max_price
        self.sell_max_in = get_delta_amount_b_unsigned_unchecked(
            pool.sqrt_price, pool.sqrt_max_price, pool.liquidity, Rounding::Up,
        ).map(|v| v.min(U256::from(u128::MAX)).try_into().unwrap_or(u128::MAX)).unwrap_or(0);
        self.sell_max_out = get_delta_amount_a_unsigned(
            pool.sqrt_price, pool.sqrt_max_price, pool.liquidity, Rounding::Down,
        ).unwrap_or(0).min(self.base_vault_amount);
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

    /// Build a MeteoraDammV2 instance from a pool_id by fetching all needed accounts from RPC.
    /// Returns (instance, accounts_vec, clock) ready for testing.
    async fn build_from_pool_id(
        pool_id: Pubkey,
    ) -> (MeteoraDammV2, Vec<AccountInfo<'static>>, Clock) {
        let rpc_client = get_rpc_client();

        // Fetch pool account
        let sdk_pool_id = solana_sdk::pubkey::Pubkey::try_from(pool_id.to_bytes().as_ref()).unwrap();
        let pool_account = rpc_client.get_account(&sdk_pool_id).await
            .unwrap_or_else(|e| panic!("Failed to fetch pool {}: {}", pool_id, e));
        let pool: Pool = bytemuck::pod_read_unaligned(&pool_account.data[8..]);

        eprintln!("Pool: {}", pool_id);
        eprintln!("  token_a (base): {}", pool.token_a_mint);
        eprintln!("  token_b (quote): {}", pool.token_b_mint);
        eprintln!("  sqrt_price: {}, liquidity: {}", pool.sqrt_price, pool.liquidity);

        // Fetch all needed accounts from RPC
        let pool_id_info = account_to_account_info(pool_id, pool_account);
        let base_vault_info = fetch_account_info_from_rpc(&rpc_client, pool.token_a_vault).await;
        let quote_vault_info = fetch_account_info_from_rpc(&rpc_client, pool.token_b_vault).await;

        let program_id_info = create_mock_account_info_with_data(
            PROGRAM_ID, anchor_lang::solana_program::system_program::id(), None,
        );
        let pool_authority_info = create_mock_account_info_with_data(
            PROGRAM_ID, anchor_lang::solana_program::system_program::id(), None,
        );
        let event_authority_info = create_mock_account_info_with_data(
            PROGRAM_ID, anchor_lang::solana_program::system_program::id(), None,
        );
        let referral_token_info = create_mock_account_info_with_data(
            PROGRAM_ID, anchor_lang::solana_program::system_program::id(), None,
        );

        // Layout:
        // Static (static_base=0): [program_id, pool_authority, event_authority, referral_token_account]
        // Dynamic (dyn_start=4): [pool, base_vault, quote_vault]
        let accounts = vec![
            program_id_info,          // S0
            pool_authority_info,      // S1
            event_authority_info,     // S2
            referral_token_info,      // S3
            pool_id_info,             // D0
            base_vault_info,          // D1
            quote_vault_info,         // D2
        ];

        let static_base: usize = 0;
        let dyn_start: usize = 4;
        let dyn_end: usize = accounts.len();

        let clock = get_clock_from_rpc(&rpc_client).await;

        let mut meteora = MeteoraDammV2::new(&accounts, static_base, dyn_start, dyn_end, &clock)
            .expect("MeteoraDammV2::new failed");

        meteora.prepare_for_execution(&accounts);

        eprintln!("  price: {}", meteora.price);
        eprintln!("  inverse_price: {}", meteora.inverse_price);

        (meteora, accounts, clock)
    }

    // ---- Tests ----

    #[tokio::test]
    async fn test_damm_v2_round_trip() {
        let pool_id = Pubkey::from_str_const("8CjMpwjfEePgyqKjwq62oSyTAR5JPiwFWCWNxCC9j7tH");
        let (mut meteora, accounts, clock) = build_from_pool_id(pool_id).await;

        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let no_fee = MintFee::ZERO;

        // 1. Print all account pubkeys
        eprintln!("\n=== Account Pubkeys ===");
        eprintln!("base_mint        : {}", meteora.base_token_pk);
        eprintln!("quote_mint       : {}", meteora.quote_token_pk);
        eprintln!("pool_id          : {}", accounts[4 + D_POOL].key);
        eprintln!("base_vault       : {}", accounts[4 + D_BASE_VAULT].key);
        eprintln!("quote_vault      : {}", accounts[4 + D_QUOTE_VAULT].key);
        eprintln!("program_id       : {}", accounts[S_PROGRAM_ID].key);
        eprintln!("pool_authority   : {}", accounts[S_POOL_AUTHORITY].key);
        eprintln!("event_authority  : {}", accounts[S_EVENT_AUTHORITY].key);
        eprintln!("referral_token   : {}", accounts[S_REFERRAL_TOKEN_ACCOUNT].key);

        // 2. Program.new() -> print price and inverse_price
        let (price, inverse_price) = meteora.get_prices().unwrap();
        eprintln!("\n=== Prices ===");
        eprintln!("price            : {}", price);
        eprintln!("inverse_price    : {}", inverse_price);

        // 3. Print fees
        let (fee_factor, fee_factor_2) = meteora.get_fee_factor().unwrap();
        eprintln!("\n=== Fees ===");
        eprintln!("fee_rate_a_to_b  : {}", meteora.fee_rate_a_to_b);
        eprintln!("fee_rate_b_to_a  : {}", meteora.fee_rate_b_to_a);
        eprintln!("fee_factor       : {}", fee_factor);
        eprintln!("fee_factor_2     : {}", fee_factor_2);

        // 4. prepare_for_execution
        eprintln!("\n=== After prepare_for_execution ===");
        eprintln!("buy_max_in       : {}", meteora.buy_max_in);
        eprintln!("buy_max_out      : {}", meteora.buy_max_out);
        eprintln!("sell_max_in      : {}", meteora.sell_max_in);
        eprintln!("sell_max_out     : {}", meteora.sell_max_out);

        // 5. Round-trip with start_amount = 1 WSOL
        let start_amount: u64 = 1_000_000_000; // 1 SOL

        let other_mint = if meteora.base_token_pk == sol_mint {
            meteora.quote_token_pk
        } else {
            meteora.base_token_pk
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
        let token_out = meteora.swap_base_in(
            &accounts, sol_mint, start_amount, no_fee, no_fee, &clock,
        ).unwrap();
        let max_sol_in = meteora.swap_base_out(
            &accounts, other_mint, token_out, no_fee, no_fee, &clock,
        ).unwrap();
        eprintln!("AMOUNT_IN {} -> AMOUNT_OUT {} -> MAX_AMOUNT_IN {}", start_amount as f64 / sol_div, token_out as f64 / tok_div, max_sol_in as f64 / sol_div);

        // Direction 2: TOKEN -> SOL -> TOKEN
        eprintln!("\n=== Direction 2: TOKEN -> SOL -> TOKEN ===");
        let sol_out = meteora.swap_base_in(
            &accounts, other_mint, token_out, no_fee, no_fee, &clock,
        ).unwrap();
        let max_token_in = meteora.swap_base_out(
            &accounts, sol_mint, sol_out, no_fee, no_fee, &clock,
        ).unwrap();
        eprintln!("AMOUNT_IN {} -> AMOUNT_OUT {} -> MAX_AMOUNT_IN {}", token_out as f64 / tok_div, sol_out as f64 / sol_div, max_token_in as f64 / tok_div);
    }
}
