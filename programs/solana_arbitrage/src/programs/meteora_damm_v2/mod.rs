use crate::programs::{PoolKind, ProgramMeta, Result};
use crate::utils::token::{apply_transfer_fee, apply_transfer_inverse_fee, MintFee};
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};
use pinocchio::instruction::AccountMeta;
use pinocchio::sysvars::clock::Clock;
use crate::utils::cpi::invoke_cpi;
use bytemuck;
pub mod damm_v2;

/// Precomputed Q64 scale factor (2^64) for sqrt_price calculations
/// Avoids recomputing `Q64_SCALE` on every call
const Q64_SCALE: f64 = 18446744073709551616.0; // Q64_SCALE
pub use damm_v2::curve::{get_spot_price_a_to_b, get_spot_price_b_to_a};
use crate::utils::utils::{read_vault_data};
pub use damm_v2::{ActivationType, FeeMode, Pool, TradeDirection};
use damm_v2::curve::{get_delta_amount_a_unsigned, get_delta_amount_a_unsigned_unchecked, get_delta_amount_b_unsigned, get_delta_amount_b_unsigned_unchecked};
use ruint::aliases::U256;
use damm_v2::u128x128_math::Rounding;

const SWAP_DISC: [u8; 8] = [0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];
pub const PROGRAM_ID: Pubkey =
    five8_const::decode_32_const("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");
// Static accounts (from static_base, shared across pools)
pub const S_PROGRAM_ID: usize = 0;
pub const S_POOL_AUTHORITY: usize = 1;
pub const S_EVENT_AUTHORITY: usize = 2;
pub const S_REFERRAL_TOKEN_ACCOUNT: usize = 3;

// Dynamic accounts (from dyn_start, per-pool)
pub const D_POOL: usize = 0;
pub const D_BASE_VAULT: usize = 1;
pub const D_QUOTE_VAULT: usize = 2;

pub const DYNAMIC_ACCOUNTS: usize = 3;

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
    pub pool: Option<Box<Pool>>,
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
        Ok((self.price, self.inverse_price))
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
            return Err(crate::programs::SolarBError::InvalidAccountData.into());
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

    fn get_max_amount_in(&self, _accounts: &[AccountInfo], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_in.min(u64::MAX as u128) as u64) } else { Ok(self.sell_max_in.min(u64::MAX as u128) as u64) }
    }

    fn get_max_amount_out(&self, _accounts: &[AccountInfo], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_out) } else { Ok(self.sell_max_out) }
    }

    fn get_cached_max_amounts(&self, input_mint: Pubkey) -> (u64, u64) {
        if input_mint == self.base_token_pk {
            ((self.buy_max_in.min(u64::MAX as u128)) as u64, self.buy_max_out)
        } else {
            ((self.sell_max_in.min(u64::MAX as u128)) as u64, self.sell_max_out)
        }
    }



    fn fast_quote(&mut self, accounts: &[AccountInfo], input_mint: Pubkey, amount_in: u64, _profit_pct: f64) -> Result<(u64, u64)> {
        let (max_in, max_out) = self.get_cached_max_amounts(input_mint);
        debug_eprintln!("max_in: {}, max_out: {}", max_in, max_out);
        let amount_in = amount_in.min(max_in);
        debug_eprintln!("[DAMM V2] Fast quote: {:.9} SOL ({}) -> {:.6} tokens ({})", amount_in as f64 / 1_000_000_000.0, amount_in, max_out as f64 / 1_000_000.0, max_out);

        if amount_in == 0 {
            return Ok((0, 0));
        }

        // Lazy-load the full Pool on first fast_quote call
        if self.pool.is_none() {
            let pool_id = &accounts[self.dyn_start + D_POOL];
            let pool_data = unsafe { pool_id.borrow_data_unchecked() };
            self.pool = Some(Box::new(bytemuck::pod_read_unaligned(&pool_data[8..])));
        }
        let pool = self.pool.as_ref().unwrap();

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

    fn swap_base_in(
        &mut self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        amount_in: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        clock: &Clock,
    ) -> Result<u64> {
        if self.pool.is_none() {
            let pool_id = &accounts[self.dyn_start + D_POOL];
            let pool_data = unsafe { pool_id.borrow_data_unchecked() };
            self.pool = Some(Box::new(bytemuck::pod_read_unaligned(&pool_data[8..])));
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
        let has_referral = !referral_token_account.key().eq(&Pubkey::default());
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

    fn swap_base_out(
        &mut self,
        accounts: &[AccountInfo],
        output_mint: Pubkey,
        amount_out: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        clock: &Clock,
    ) -> Result<u64> {
        if self.pool.is_none() {
            let pool_id = &accounts[self.dyn_start + D_POOL];
            let pool_data = unsafe { pool_id.borrow_data_unchecked() };
            self.pool = Some(Box::new(bytemuck::pod_read_unaligned(&pool_data[8..])));
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
        let has_referral = !referral_token_account.key().eq(&Pubkey::default());
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
        let (
            base_token_program,
            quote_token_program,
            user_base_token_account,
            user_quote_token_account,
            base_mint_info,
            quote_mint_info,
        ) = if self.base_token_pk == *mint_1_account.key() {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
                mint_1_account,
                mint_2_account,
            )
        } else {
            (
                mint_2_token_program,
                mint_1_token_program,
                user_mint_2_token_account,
                user_mint_1_token_account,
                mint_2_account,
                mint_1_account,
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
        let referral_meta = if referral_token_account.key() == program_id.key() {
            AccountMeta::new(referral_token_account.key(), false, false)
        } else {
            AccountMeta::new(referral_token_account.key(), true, false)
        };
        let metas_arr: [AccountMeta; 14] = [
            AccountMeta::new(pool_authority.key(), false, false),
            AccountMeta::new(pool_id.key(), true, false),
            AccountMeta::new(input_token_account.key(), true, false),
            AccountMeta::new(output_token_account.key(), true, false),
            AccountMeta::new(base_vault.key(), true, false),
            AccountMeta::new(quote_vault.key(), true, false),
            AccountMeta::new(&self.base_token_pk, false, false),
            AccountMeta::new(&self.quote_token_pk, false, false),
            AccountMeta::new(payer.key(), true, true),
            AccountMeta::new(base_token_program.key(), false, false),
            AccountMeta::new(quote_token_program.key(), false, false),
            referral_meta,
            AccountMeta::new(event_authority.key(), false, false),
            AccountMeta::new(program_id.key(), false, false),
        ];

        let mut data = [0u8; 24];
        data[..8].copy_from_slice(&SWAP_DISC);
        data[8..16].copy_from_slice(&max_amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&amount_out_value.to_le_bytes());

        let accs: [&AccountInfo; 14] = [
            pool_authority,
            pool_id,
            base_vault,
            quote_vault,
            referral_token_account,
            event_authority,
            program_id,
            input_token_account,
            output_token_account,
            payer,
            base_token_program,
            quote_token_program,
            base_mint_info,
            quote_mint_info,
        ];
        invoke_cpi(program_id.key(), &metas_arr, &data, &accs)?;

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
        let (
            base_token_program,
            quote_token_program,
            user_base_token_account,
            user_quote_token_account,
            base_mint_info,
            quote_mint_info,
        ) = if mint_1_account.key() == &self.base_token_pk {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
                mint_1_account,
                mint_2_account,
            )
        } else if mint_2_account.key() == &self.base_token_pk {
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
        let referral_meta = if referral_token_account.key() == program_id.key() {
            AccountMeta::new(referral_token_account.key(), false, false)
        } else {
            AccountMeta::new(referral_token_account.key(), true, false)
        };
        let metas_arr: [AccountMeta; 14] = [
            AccountMeta::new(pool_authority.key(), false, false),
            AccountMeta::new(pool_id.key(), true, false),
            AccountMeta::new(input_token_account.key(), true, false),
            AccountMeta::new(output_token_account.key(), true, false),
            AccountMeta::new(base_vault.key(), true, false),
            AccountMeta::new(quote_vault.key(), true, false),
            AccountMeta::new(&self.base_token_pk, false, false),
            AccountMeta::new(&self.quote_token_pk, false, false),
            AccountMeta::new(payer.key(), true, true),
            AccountMeta::new(base_token_program.key(), false, false),
            AccountMeta::new(quote_token_program.key(), false, false),
            referral_meta,
            AccountMeta::new(event_authority.key(), false, false),
            AccountMeta::new(program_id.key(), false, false),
        ];
        let mut data = [0u8; 24];
        data[..8].copy_from_slice(&SWAP_DISC);
        data[8..16].copy_from_slice(&amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&min_amount_out_value.to_le_bytes());

        let accs: [&AccountInfo; 14] = [
            pool_authority,
            pool_id,
            base_vault,
            quote_vault,
            referral_token_account,
            event_authority,
            program_id,
            input_token_account,
            output_token_account,
            payer,
            base_token_program,
            quote_token_program,
            base_mint_info,
            quote_mint_info,
        ];
        invoke_cpi(program_id.key(), &metas_arr, &data, &accs)?;
        Ok(())
    }

    #[cfg(any(test, feature = "debug"))]
    fn log_accounts(&self, _accounts: &[AccountInfo]) -> Result<()> {
        pinocchio::log::sol_log("=== Meteora DAMM V2 ===");
        Ok(())
    }

}

impl MeteoraDammV2 {

    pub fn new(
        accounts: &[AccountInfo],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
        clock: &Clock,
    ) -> Result<Self> {
        // Access accounts by indices
        let pool_id = &accounts[dyn_start + D_POOL];
        let pool_data = unsafe { pool_id.borrow_data_unchecked() };
        // Read fields directly at byte offsets (after 8-byte discriminator) to avoid deserializing entire Pool
        // Pool-relative offsets: liquidity=352, sqrt_price=448, activation_type=472, collect_fee_mode=476
        // Raw offsets (with 8-byte disc): 360, 456, 480, 484
        let sqrt_price = u128::from_le_bytes(pool_data[456..472].try_into().unwrap());
        let liquidity = u128::from_le_bytes(pool_data[360..376].try_into().unwrap());
        let activation_type = pool_data[480];
        let collect_fee_mode = pool_data[484];
        // Read base/quote token pubkeys from pool state (no longer passed as accounts)

        #[cfg(test)]
        let sqrt_price: u128 = (sqrt_price as f64 * 1.2) as u128;

        let (price, inverse_price) = get_prices(sqrt_price)?;
        let base_vault = &accounts[dyn_start + D_BASE_VAULT];
        let quote_vault = &accounts[dyn_start + D_QUOTE_VAULT];
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

        debug_eprintln!("MeteoraDammV2: pool_id {:?}, price {}, inverse_price {}, fee_rate {}", pool_id.key(), price, inverse_price, fee_rate);

        // Defer max amounts and transfer fees to prepare_for_execution()
        let instance = MeteoraDammV2 {
            base_token_pk,
            quote_token_pk,
            pool: None,
            sqrt_price,
            liquidity,
            collect_fee_mode,
            activation_type,
            pool_id: *pool_id.key(),
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
    /// Reads only the 4 fields needed (64 bytes) instead of the full 1104-byte Pool struct.
    /// Full Pool deser is deferred to swap_base_in/swap_base_out/fast_quote when actually needed.
    pub fn prepare_for_execution(
        &mut self,
        accounts: &[AccountInfo],
    ) {
        if self.prepared {
            return;
        }
        self.prepared = true;

        // Read only sqrt_min/max_price; liquidity and sqrt_price are cached from new()
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let pool_data = unsafe { pool_id.borrow_data_unchecked() };
        let sqrt_min_price = u128::from_le_bytes(pool_data[424..440].try_into().unwrap());
        let sqrt_max_price = u128::from_le_bytes(pool_data[440..456].try_into().unwrap());
        drop(pool_data);
        let liquidity = self.liquidity;
        let sqrt_price = self.sqrt_price;

        // Cache max amounts from curve math (sqrt_min_price / sqrt_max_price boundaries)
        // A→B (buy): price moves from sqrt_price down toward sqrt_min_price
        self.buy_max_in = get_delta_amount_a_unsigned_unchecked(
            sqrt_min_price, sqrt_price, liquidity, Rounding::Up,
        ).map(|v| v.min(U256::from(u128::MAX)).try_into().unwrap_or(u128::MAX)).unwrap_or(0);
        self.buy_max_out = get_delta_amount_b_unsigned(
            sqrt_min_price, sqrt_price, liquidity, Rounding::Down,
        ).unwrap_or(0).min(self.quote_vault_amount);
        // B→A (sell): price moves from sqrt_price up toward sqrt_max_price
        self.sell_max_in = get_delta_amount_b_unsigned_unchecked(
            sqrt_price, sqrt_max_price, liquidity, Rounding::Up,
        ).map(|v| v.min(U256::from(u128::MAX)).try_into().unwrap_or(u128::MAX)).unwrap_or(0);
        self.sell_max_out = get_delta_amount_a_unsigned(
            sqrt_price, sqrt_max_price, liquidity, Rounding::Down,
        ).unwrap_or(0).min(self.base_vault_amount);
    }
}

// TODO: tests need rewriting with LiteSVM/Mollusk
#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::token::MintFee;

    // TODO: rewrite tests using LiteSVM/Mollusk
}
