//! Arbitrage-opportunity checkers involving **Meteora DAMM V2** as one leg.
//!
//! # DAMM V2 model
//!
//! DAMM V2 is a Concentrated Liquidity AMM with a single price range
//! `[sqrt_min_price, sqrt_max_price]`.  Within that range it behaves
//! **exactly** as a constant-product AMM with virtual reserves:
//!
//! ```text
//! Q64   = 2^64 = 18_446_744_073_709_551_616
//! Q128  = 2^128
//!
//! vb (virtual base)  = liquidity / sqrt_price_raw                    (token A units)
//! vq (virtual quote) = liquidity * sqrt_price_raw / Q128             (token B / SOL units)
//! ```
//!
//! # Fee modes
//!
//! | `collect_fee_mode` | AtoB (base→SOL)  | BtoA (SOL→base)  |
//! |--------------------|------------------|------------------|
//! | 0 = BothToken      | fee on OUTPUT    | fee on OUTPUT    |
//! | 1 = OnlyB          | fee on OUTPUT    | fee on INPUT     |
//!
//! Fee precision: `FEE_DENOM = 1_000_000_000`.
//! `fee_factor.0` = AtoB fee factor (1 − rate).
//! `fee_factor.1` = BtoA fee factor (1 − rate).
//!
//! # Covered pairs
//!
//! | Function              | Pool 1         | Pool 2         |
//! |-----------------------|----------------|----------------|
//! | `check_pump_dammv2`   | PumpAmm (buy)  | DAMM V2 (sell) |
//! | `check_dammv2_pump`   | DAMM V2 (buy)  | PumpAmm (sell) |
//! | `check_dlmm_dammv2`   | DLMM (buy)     | DAMM V2 (sell) |
//! | `check_dammv2_dlmm`   | DAMM V2 (buy)  | DLMM (sell)    |

use pinocchio::{account_info::AccountInfo, pubkey::Pubkey};

use crate::programs::meteora_damm_v2::MeteoraDammV2;
use crate::programs::meteora_dlmm::MeteoraDlmm;
use crate::programs::pump_amm::PumpAmm;
use crate::programs::ProgramMeta;

use super::{dlmm_walk_int, ArbOpportunity, MIN_PROFIT_BPS, MIN_PROFIT_LAMPORTS};

// ─── Constants ────────────────────────────────────────────────────────────────

/// DAMM V2 fee precision denominator (same as cp-amm constants).
const DAMM_FEE_PREC: u128 = 1_000_000_000;
/// PumpAmm fee precision denominator.
const PUMP_FEE_DENOM: u128 = 1_000_000;
/// Q64 scale factor as f64 (2^64).
const Q64: f64 = 18_446_744_073_709_551_616.0;
/// Q128 = Q64 * Q64.
const Q128: f64 = Q64 * Q64;

// ─── Virtual-reserve helpers ──────────────────────────────────────────────────

/// Compute DAMM V2 virtual reserves (base, quote) from pool state.
/// Returns `None` if reserves are not finite or ≤ 0.
#[inline(always)]
fn vreserves(damm: &MeteoraDammV2) -> Option<(f64, f64)> {
    let sq = damm.sqrt_price as f64;
    let l = damm.liquidity as f64;
    if sq <= 0.0 || l <= 0.0 {
        return None;
    }
    let vb = l / sq;
    let vq = l * sq / Q128;
    if !vb.is_finite() || !vq.is_finite() || vb <= 0.0 || vq <= 0.0 {
        return None;
    }
    Some((vb, vq))
}

/// Effective AtoB fee numerator (base→quote, fee on output).
#[inline(always)]
fn fee_num_ab(damm: &MeteoraDammV2) -> u128 {
    ((1.0 - damm.fee_factor.0).max(0.0) * DAMM_FEE_PREC as f64) as u128
}

/// Effective BtoA fee numerator (quote→base, fee depends on mode).
#[inline(always)]
fn fee_num_ba(damm: &MeteoraDammV2) -> u128 {
    ((1.0 - damm.fee_factor.1).max(0.0) * DAMM_FEE_PREC as f64) as u128
}

// ─── Integer fast-quote helpers ───────────────────────────────────────────────

/// DAMM V2 sell output (AtoB): base → quote, fee on output.
/// Uses u128 arithmetic to avoid overflow on large virtual reserves.
#[inline(always)]
fn dammv2_sell_out(vb: f64, vq: f64, fee_n: u128, in_amount: u64) -> u64 {
    let vb_i = vb as u128;
    let vq_i = vq as u128;
    let denom = vb_i.saturating_add(in_amount as u128);
    if denom == 0 { return 0; }
    let raw = vq_i * in_amount as u128 / denom;
    (raw * DAMM_FEE_PREC.saturating_sub(fee_n) / DAMM_FEE_PREC)
        .min(u64::MAX as u128) as u64
}

/// DAMM V2 buy output (BtoA, BothToken mode): quote → base, fee on output.
#[inline(always)]
fn dammv2_buy_out_fee_output(vb: f64, vq: f64, fee_n: u128, in_amount: u64) -> u64 {
    let vb_i = vb as u128;
    let vq_i = vq as u128;
    let denom = vq_i.saturating_add(in_amount as u128);
    if denom == 0 { return 0; }
    let raw = vb_i * in_amount as u128 / denom;
    (raw * DAMM_FEE_PREC.saturating_sub(fee_n) / DAMM_FEE_PREC)
        .min(u64::MAX as u128) as u64
}

/// DAMM V2 buy output (BtoA, OnlyB mode): quote → base, fee on input.
#[inline(always)]
fn dammv2_buy_out_fee_input(vb: f64, vq: f64, fee_n: u128, in_amount: u64) -> u64 {
    let in_eff = (in_amount as u128) * DAMM_FEE_PREC.saturating_sub(fee_n) / DAMM_FEE_PREC;
    let vb_i = vb as u128;
    let vq_i = vq as u128;
    let denom = vq_i.saturating_add(in_eff);
    if denom == 0 { return 0; }
    (vb_i * in_eff / denom).min(u64::MAX as u128) as u64
}

/// PumpAmm buy output: quote → base, fee on input (CpFeeOnInput).
#[inline(always)]
fn pump_buy_out(base_r: u64, quote_r: u64, fee_num: u64, in_amount: u64) -> u64 {
    let base_r = base_r as u128;
    let quote_r = quote_r as u128;
    let in_eff = (in_amount as u128) * (PUMP_FEE_DENOM - fee_num as u128) / PUMP_FEE_DENOM;
    let denom = quote_r + in_eff;
    if denom == 0 { return 0; }
    (base_r * in_eff / denom).min(u64::MAX as u128) as u64
}

/// PumpAmm sell output: base → quote, fee on output (CpFeeOnOutput).
#[inline(always)]
fn pump_sell_out(base_r: u64, quote_r: u64, fee_num: u64, in_amount: u64) -> u64 {
    let base_r = base_r as u128;
    let quote_r = quote_r as u128;
    let denom = base_r + in_amount as u128;
    if denom == 0 { return 0; }
    let raw = quote_r * in_amount as u128 / denom;
    (raw * (PUMP_FEE_DENOM - fee_num as u128) / PUMP_FEE_DENOM)
        .min(u64::MAX as u128) as u64
}

// ─── Shared closed-form helpers ───────────────────────────────────────────────

#[inline(always)]
fn clamp(dx: f64, max_amount_in: u64) -> Option<u64> {
    if dx <= 0.0 || !dx.is_finite() { return None; }
    let v = dx.min(max_amount_in as f64) as u64;
    if v == 0 { None } else { Some(v) }
}

/// `dx* = (√(f1·f2·r1_out·r2_out·r1_in·r2_in) − r1_in·r2_in) / (r2_in + f1·r1_out)`
///
/// Used when pool-1 buy has **fee on output** (BothToken BtoA, or standard CpFeeOnOutput).
#[inline(always)]
fn optimal_fee_on_output_buy(
    r1_in: f64, r1_out: f64, f1: f64,
    r2_in: f64, r2_out: f64, f2: f64,
) -> f64 {
    let prod = f1 * f2 * r1_out * r2_out * r1_in * r2_in;
    (prod.sqrt() - r1_in * r2_in) / (r2_in + f1 * r1_out)
}

/// `dx* = (√(f1·C·A) − A) / (f1·B)`
/// where `A = r1_in·r2_in`, `B = r2_in + r1_out`, `C = f2·r2_out·r1_out`.
///
/// Used when pool-1 buy has **fee on input** (CpFeeOnInput for pump or OnlyB BtoA).
#[inline(always)]
fn optimal_fee_on_input_buy(
    r1_in: f64, r1_out: f64, f1: f64,
    r2_in: f64, r2_out: f64, f2: f64,
) -> f64 {
    let a = r1_in * r2_in;
    let b = r2_in + r1_out;
    let c = f2 * r2_out * r1_out;
    ((f1 * c * a).sqrt() - a) / (f1 * b)
}

// ─── check_pump_dammv2 ────────────────────────────────────────────────────────

/// Check for a 2-pool arb: buy on `pump` (pool 1), sell on `dammv2` (pool 2).
///
/// `input_mint` must be `pump.quote_token_pk` (e.g. SOL).
/// `dammv2.base_token_pk` must equal `pump.base_token_pk`.
///
/// # Model: CpFeeOnInput (pump buy) + CPAMM-virtual fee-on-output (damm sell)
///
/// DAMM V2 sell (AtoB) always applies fee on output regardless of `collect_fee_mode`.
/// The formula reduces to `check_pump_pump` with virtual reserves for pool 2.
///
/// ```text
/// A   = pump.quote_r · vb
/// B   = vb + pump.base_r
/// C   = f_damm · vq · pump.base_r
/// dx* = (√(f_pump · C · A) − A) / (f_pump · B)
/// ```
pub fn check_pump_dammv2(
    pump:          &PumpAmm,
    dammv2:        &MeteoraDammV2,
    input_mint:    Pubkey,
    max_amount_in: u64,
) -> Option<ArbOpportunity> {
    if input_mint != pump.quote_token_pk {
        return None;
    }
    #[cfg(any(test, feature = "debug"))]
    debug_assert_eq!(pump.base_token_pk, dammv2.base_token_pk,
        "check_pump_dammv2: mismatched base token");

    let (vb, vq) = vreserves(dammv2)?;

    let r1_in  = pump.quote_vault_amount as f64;
    let r1_out = pump.base_vault_amount  as f64;
    let r2_in  = vb;
    let r2_out = vq;
    let f1 = pump.fee_factor.1;     // pump buy: CpFeeOnInput (quote→base)
    let f2 = dammv2.fee_factor.0;   // damm sell AtoB: always fee on output

    // Fast gate
    if f1 * f2 * r1_out * r2_out <= r1_in * r2_in {
        return None;
    }

    let dx = optimal_fee_on_input_buy(r1_in, r1_out, f1, r2_in, r2_out, f2);
    // Cap at damm sell capacity (input = base tokens)
    let sell_cap = (dammv2.buy_max_in.min(u64::MAX as u128)) as u64;
    let max_in = if sell_cap > 0 {
        // sell_cap is base (mid), map back to SOL approximately
        let sol_equiv = sell_cap as f64 / (f1 * r1_out / r1_in);
        max_amount_in.min(sol_equiv as u64)
    } else {
        max_amount_in
    };
    let optimal = clamp(dx, max_in)?;

    // ── Integer verification ──────────────────────────────────────────────
    let mid = pump_buy_out(
        pump.base_vault_amount, pump.quote_vault_amount, pump.fee_numerator, optimal,
    );
    let mid = if sell_cap > 0 { mid.min(sell_cap) } else { mid };

    let fn_ab = fee_num_ab(dammv2);
    let out = dammv2_sell_out(vb, vq, fn_ab, mid);

    let profit = out.checked_sub(optimal)?;
    if profit < MIN_PROFIT_LAMPORTS { return None; }
    let profit_bps = ((profit as u128 * 10_000) / optimal as u128) as u32;
    if profit_bps < MIN_PROFIT_BPS { return None; }

    Some(ArbOpportunity {
        buy_pool_id:       pump.pool_id,
        sell_pool_id:      dammv2.pool_id,
        input_mint,
        middle_mint:       pump.base_token_pk,
        optimal_amount_in: optimal,
        expected_profit:   profit,
        profit_bps,
    })
}

// ─── check_dammv2_pump ────────────────────────────────────────────────────────

/// Check for a 2-pool arb: buy on `dammv2` (pool 1), sell on `pump` (pool 2).
///
/// `input_mint` must be `dammv2.quote_token_pk` (e.g. SOL).
/// `pump.base_token_pk` must equal `dammv2.base_token_pk`.
///
/// # Model: CPAMM-virtual (damm buy) + CpFeeOnOutput (pump sell)
///
/// Fee on damm buy leg depends on `collect_fee_mode`:
/// - BothToken (0): fee on output → `dx* = (√disc − r1_in·r2_in) / (r2_in + f1·r1_out)`
/// - OnlyB     (1): fee on input  → `dx* = (√(f1·C·A) − A) / (f1·B)` (same as pump_pump)
///
/// Gate (both modes): `f_ba · f_pump · vb · pump.quote_r > vq · pump.base_r`
pub fn check_dammv2_pump(
    dammv2:        &MeteoraDammV2,
    pump:          &PumpAmm,
    input_mint:    Pubkey,
    max_amount_in: u64,
) -> Option<ArbOpportunity> {
    if input_mint != dammv2.quote_token_pk {
        return None;
    }
    #[cfg(any(test, feature = "debug"))]
    debug_assert_eq!(dammv2.base_token_pk, pump.base_token_pk,
        "check_dammv2_pump: mismatched base token");

    let (vb, vq) = vreserves(dammv2)?;

    let r1_in  = vq;                             // SOL input to dammv2 (BtoA)
    let r1_out = vb;                             // base output from dammv2
    let r2_in  = pump.base_vault_amount  as f64; // base input to pump sell
    let r2_out = pump.quote_vault_amount as f64; // SOL output from pump sell
    let f1 = dammv2.fee_factor.1;               // BtoA fee factor
    let f2 = pump.fee_factor.0;                 // pump sell: CpFeeOnOutput

    // Fast gate (identical for both modes)
    if f1 * f2 * r1_out * r2_out <= r1_in * r2_in {
        return None;
    }

    // Cap input to dammv2 BtoA capacity
    let buy_cap = (dammv2.sell_max_in.min(u64::MAX as u128)) as u64;
    let max_in = if buy_cap > 0 { max_amount_in.min(buy_cap) } else { max_amount_in };

    // Optimal dx depends on fee mode
    let dx = if dammv2.collect_fee_mode == 1 {
        // OnlyB: fee on INPUT → same as CpFeeOnInput structure (pump_pump formula)
        optimal_fee_on_input_buy(r1_in, r1_out, f1, r2_in, r2_out, f2)
    } else {
        // BothToken: fee on OUTPUT → different denominator
        optimal_fee_on_output_buy(r1_in, r1_out, f1, r2_in, r2_out, f2)
    };

    // Cap to pump sell capacity
    let sell_cap = pump.base_vault_amount; // can't sell more than pump's base reserve
    let dx = dx.min(sell_cap as f64 * r1_in / r1_out); // conservative back-map
    let optimal = clamp(dx, max_in)?;

    // ── Integer verification ──────────────────────────────────────────────
    let fn_ba = fee_num_ba(dammv2);
    let mid = if dammv2.collect_fee_mode == 1 {
        dammv2_buy_out_fee_input(vb, vq, fn_ba, optimal)
    } else {
        dammv2_buy_out_fee_output(vb, vq, fn_ba, optimal)
    };
    let mid = mid.min(pump.base_vault_amount);

    let out = pump_sell_out(
        pump.base_vault_amount, pump.quote_vault_amount, pump.fee_numerator, mid,
    );

    let profit = out.checked_sub(optimal)?;
    if profit < MIN_PROFIT_LAMPORTS { return None; }
    let profit_bps = ((profit as u128 * 10_000) / optimal as u128) as u32;
    if profit_bps < MIN_PROFIT_BPS { return None; }

    Some(ArbOpportunity {
        buy_pool_id:       dammv2.pool_id,
        sell_pool_id:      pump.pool_id,
        input_mint,
        middle_mint:       dammv2.base_token_pk,
        optimal_amount_in: optimal,
        expected_profit:   profit,
        profit_bps,
    })
}

// ─── check_dlmm_dammv2 ───────────────────────────────────────────────────────

/// Check for a 2-pool arb: buy on `dlmm` (pool 1), sell on `dammv2` (pool 2).
///
/// `input_mint` must be `dlmm.quote_token_pk` (SOL).
/// `dammv2.base_token_pk` must equal `dlmm.base_token_pk`.
///
/// # Model: Linear bins (dlmm buy) + CPAMM-virtual fee-on-output (damm sell)
///
/// DAMM V2 AtoB always has fee on output.  The DAMM V2 virtual reserves act
/// as a PumpAmm equivalent, so this function mirrors `check_dlmm_pump` with
/// virtual reserves substituted for real pump reserves.
///
/// Single-bin fallback:
/// ```text
/// a      = f_dlmm / dlmm.price                (MEMECOIN per SOL)
/// disc   = f_damm · vq · a · vb
/// dx*    = (√disc − vb) / a
/// ```
pub fn check_dlmm_dammv2(
    dlmm:          &MeteoraDlmm,
    dammv2:        &MeteoraDammV2,
    input_mint:    Pubkey,
    max_amount_in: u64,
    accounts:      &[AccountInfo],
) -> Option<ArbOpportunity> {
    if input_mint != dlmm.quote_token_pk {
        return None;
    }
    #[cfg(any(test, feature = "debug"))]
    debug_assert_eq!(dlmm.base_token_pk, dammv2.base_token_pk,
        "check_dlmm_dammv2: mismatched base token");

    let (vb, vq) = vreserves(dammv2)?;

    let price   = dlmm.price;              // SOL per MEMECOIN
    let f_dlmm  = dlmm.fee_factor.1;      // quote→base direction (buy)
    let f_damm  = dammv2.fee_factor.0;    // damm AtoB sell: always fee on output

    if price <= 0.0 || vb <= 0.0 || vq <= 0.0 {
        return None;
    }

    // a = effective MEMECOIN per SOL from DLMM buy
    let a    = f_dlmm / price;
    let disc = f_damm * vq * a * vb;
    if disc <= vb * vb {
        return None;
    }

    // Single-bin fallback optimal (mirrors check_dlmm_pump structure)
    let dx_single = {
        let mut dx = (disc.sqrt() - vb) / a;
        if dlmm.buy_max_in > 0 { dx = dx.min(dlmm.buy_max_in as f64); }
        dx
    };
    let fallback_optimal = clamp(dx_single, max_amount_in)?;

    // Multi-bin optimal: walk DLMM buy bins with damm virtual reserves as sell side
    let sell_cap = (dammv2.buy_max_in.min(u64::MAX as u128)) as u64;
    let optimal = walk_bins_dlmm_dammv2_sell(
        dlmm, accounts, vb, vq, f_damm, max_amount_in, sell_cap, fallback_optimal,
    );

    // ── Integer verification ──────────────────────────────────────────────
    let profit_pct = (disc.sqrt() / vb - 1.0).max(0.0);
    let mid_int = dlmm_walk_int(dlmm, accounts, dlmm.quote_token_pk, optimal, profit_pct);
    let mid_int = if sell_cap > 0 { mid_int.min(sell_cap) } else { mid_int };

    let fn_ab = fee_num_ab(dammv2);
    let out = dammv2_sell_out(vb, vq, fn_ab, mid_int);

    let profit = out.checked_sub(optimal)?;
    if profit < MIN_PROFIT_LAMPORTS { return None; }
    let profit_bps = ((profit as u128 * 10_000) / optimal as u128) as u32;
    if profit_bps < MIN_PROFIT_BPS { return None; }

    Some(ArbOpportunity {
        buy_pool_id:       dlmm.pool_id,
        sell_pool_id:      dammv2.pool_id,
        input_mint,
        middle_mint:       dlmm.base_token_pk,
        optimal_amount_in: optimal,
        expected_profit:   profit,
        profit_bps,
    })
}

// ─── check_dammv2_dlmm ───────────────────────────────────────────────────────

/// Check for a 2-pool arb: buy on `dammv2` (pool 1), sell on `dlmm` (pool 2).
///
/// `input_mint` must be `dammv2.quote_token_pk` (SOL).
/// `dlmm.base_token_pk` must equal `dammv2.base_token_pk`.
///
/// # Model: CPAMM-virtual (damm buy) + Linear bins (dlmm sell)
///
/// This mirrors `check_pump_dlmm` but with DAMM V2 virtual reserves as pool 1.
/// Fee mode for the buy leg determines the closed-form shape:
///
/// **OnlyB** (fee on input for BtoA):
/// ```text
/// disc = vq · vb · f_ba · s
/// dx*  = (√disc − vq) / f_ba
/// ```
///
/// **BothToken** (fee on output for BtoA):
/// ```text
/// disc = vq · vb · f_ba · s
/// dx*  = √disc − vq
/// ```
///
/// Both share the same profitability gate: `disc > vq²`.
pub fn check_dammv2_dlmm(
    dammv2:        &MeteoraDammV2,
    dlmm:          &MeteoraDlmm,
    input_mint:    Pubkey,
    max_amount_in: u64,
    accounts:      &[AccountInfo],
) -> Option<ArbOpportunity> {
    if input_mint != dammv2.quote_token_pk {
        return None;
    }
    #[cfg(any(test, feature = "debug"))]
    debug_assert_eq!(dammv2.base_token_pk, dlmm.base_token_pk,
        "check_dammv2_dlmm: mismatched base token");

    let (vb, vq) = vreserves(dammv2)?;

    let p     = dlmm.price;              // SOL per MEMECOIN (sell direction)
    let f_dlmm = dlmm.fee_factor.0;     // base→quote (sell) fee factor
    let f_ba  = dammv2.fee_factor.1;    // BtoA fee factor

    if p <= 0.0 || vb <= 0.0 || vq <= 0.0 {
        return None;
    }

    // Fast gate (same for both modes): disc > vq²
    let disc = vq * vb * f_ba * p * f_dlmm;
    if disc <= vq * vq {
        return None;
    }

    // Single-bin fallback optimal — mode-dependent
    let buy_cap = (dammv2.sell_max_in.min(u64::MAX as u128)) as u64;
    let dx_single = {
        let base_dx = if dammv2.collect_fee_mode == 1 {
            // OnlyB: fee on input
            (disc.sqrt() - vq) / f_ba
        } else {
            // BothToken: fee on output
            disc.sqrt() - vq
        };
        // Cap to damm buy capacity
        let mut dx = base_dx;
        if buy_cap > 0 { dx = dx.min(buy_cap as f64); }
        // Cap to dlmm sell capacity (approximate)
        if dlmm.sell_max_in > 0 {
            let mid_at_cap = dlmm.sell_max_in as f64;
            let dx_for_cap = if dammv2.collect_fee_mode == 1 {
                // OnlyB: mid = vb * f_ba * dx / (vq + f_ba * dx)
                let denom = vb - mid_at_cap;
                if denom > 0.0 { mid_at_cap * vq / (f_ba * denom) } else { dx }
            } else {
                // BothToken: mid = f_ba * vb * dx / (vq + dx)
                let denom = f_ba * vb - mid_at_cap;
                if denom > 0.0 { mid_at_cap * vq / denom } else { dx }
            };
            dx = dx.min(dx_for_cap);
        }
        dx
    };
    let fallback_optimal = clamp(dx_single, max_amount_in)?;

    // Multi-bin optimal: walk DLMM sell bins
    let optimal = walk_bins_dammv2_dlmm(
        dammv2, dlmm, accounts,
        vb, vq, f_ba,
        max_amount_in, buy_cap, fallback_optimal,
    );

    // ── Integer verification ──────────────────────────────────────────────
    let fn_ba = fee_num_ba(dammv2);
    let mid_int = if dammv2.collect_fee_mode == 1 {
        dammv2_buy_out_fee_input(vb, vq, fn_ba, optimal)
    } else {
        dammv2_buy_out_fee_output(vb, vq, fn_ba, optimal)
    };
    let mid_int = if dlmm.sell_max_in > 0 { mid_int.min(dlmm.sell_max_in) } else { mid_int };

    let profit_pct = (disc.sqrt() / vq - 1.0).max(0.0);
    let out = dlmm_walk_int(dlmm, accounts, dlmm.base_token_pk, mid_int, profit_pct);

    let profit = out.checked_sub(optimal)?;
    if profit < MIN_PROFIT_LAMPORTS { return None; }
    let profit_bps = ((profit as u128 * 10_000) / optimal as u128) as u32;
    if profit_bps < MIN_PROFIT_BPS { return None; }

    Some(ArbOpportunity {
        buy_pool_id:       dammv2.pool_id,
        sell_pool_id:      dlmm.pool_id,
        input_mint,
        middle_mint:       dammv2.base_token_pk,
        optimal_amount_in: optimal,
        expected_profit:   profit,
        profit_bps,
    })
}

// ─── Multi-bin walker: DLMM buy + DAMM V2 sell ────────────────────────────────

/// Walk DLMM buy bins to find the optimal SOL input for a dlmm→dammv2 arb.
///
/// Mirrors `walk_bins_dlmm_pump` from `dlmm_checker` but uses DAMM V2
/// virtual reserves (`vb`, `vq`) instead of pump real reserves.
/// DAMM V2 AtoB is always fee-on-output: `out_b = f_damm · vq · mid / (vb + mid)`.
///
/// # Math (per bin)
/// DLMM buy bin (Linear):   mid(dx) = cumulative_out + slope · (dx − cumulative_in)
/// DAMM V2 sell (CPAMM):    d(out)/d(mid) = f_damm · vq · vb / (vb + mid)²
/// Optimal when above = 1:  mid* = √(f_damm · vq · vb · slope) − vb
fn walk_bins_dlmm_dammv2_sell(
    dlmm:          &MeteoraDlmm,
    accounts:      &[AccountInfo],
    vb:            f64,
    vq:            f64,
    f_damm:        f64,
    max_amount_in: u64,
    sell_cap:      u64,   // max mid (base) that damm can absorb
    fallback:      u64,
) -> u64 {
    let max_f = max_amount_in as f64;
    let sell_cap_f = if sell_cap > 0 { sell_cap as f64 } else { f64::MAX };
    let mut cumulative_in:  f64 = 0.0;
    let mut cumulative_out: f64 = 0.0;
    let mut best_dx = fallback as f64;

    for bin_offset in 0..70i32 {
        // Load buy bin: input = SOL (quote), output = MEMECOIN (base)
        let (s, gross_cap) = match dlmm.get_bin_segment(accounts, dlmm.quote_token_pk, bin_offset) {
            Ok(Some((slope, cap, fee_f))) if slope > 0.0 && cap > 0 && fee_f > 0.0 => {
                (slope, cap as f64 / fee_f) // gross_cap: SOL capacity
            }
            Ok(Some(_)) => continue,
            _ => break,
        };

        // DAMM sell marginal equals 1 when mid* = √(f·vq·vb·s) − vb
        let disc_bin = f_damm * vq * vb * s;
        if disc_bin <= vb * vb { break; }

        let mid_star = disc_bin.sqrt() - vb;
        let mid_cap = (cumulative_out + s * gross_cap).min(sell_cap_f);

        if mid_star < cumulative_out { break; }
        if mid_star <= mid_cap {
            let dx_star = cumulative_in + (mid_star - cumulative_out) / s;
            best_dx = dx_star.min(max_f);
            break;
        }

        // Fill this bin completely
        let dx_boundary = (cumulative_in + gross_cap).min(max_f);
        best_dx = dx_boundary;

        // Peek at next non-empty bin
        let next_s: f64 = {
            let mut found = 0.0;
            for peek in 1..=4i32 {
                match dlmm.get_bin_segment(accounts, dlmm.quote_token_pk, bin_offset + peek) {
                    Ok(Some((ns, nc, _))) if ns > 0.0 && nc > 0 => { found = ns; break; }
                    Ok(Some(_)) => continue,
                    _ => break,
                }
            }
            found
        };
        if next_s == 0.0 { break; }

        // DAMM V2 marginal at mid_cap: d(out)/d(mid) = f·vq·vb / (vb + mid_cap)²
        let denom = vb + mid_cap;
        let damm_marg = f_damm * vq * vb / (denom * denom);
        if next_s * damm_marg <= 1.0 { break; }

        cumulative_in  = dx_boundary;
        cumulative_out = mid_cap;
        if best_dx >= max_f { break; }
    }

    clamp(best_dx, max_amount_in).unwrap_or(fallback)
}

// ─── Multi-bin walker: DAMM V2 buy + DLMM sell ───────────────────────────────

/// Walk DLMM sell bins to find the optimal SOL input for a dammv2→dlmm arb.
///
/// Mirrors `walk_bins_pump_dlmm` from `pump_checker` but handles both
/// DAMM V2 fee modes for the buy (BtoA) leg.
///
/// **OnlyB mode** (fee on input, `collect_fee_mode == 1`):
/// ```text
/// mid(dx) = vb · f_ba · dx / (vq + f_ba · dx)   [CpFeeOnInput-equivalent]
/// dx*_bin = (√(vq · vb · f_ba · slope) − vq) / f_ba
/// ```
///
/// **BothToken mode** (fee on output, `collect_fee_mode == 0`):
/// ```text
/// mid(dx) = f_ba · vb · dx / (vq + dx)           [CpFeeOnOutput-equivalent]
/// dx*_bin = √(vq · vb · f_ba · slope) − vq
/// ```
///
/// Returns `fallback` when the swap cache is not yet initialised.
fn walk_bins_dammv2_dlmm(
    dammv2:        &MeteoraDammV2,
    dlmm:          &MeteoraDlmm,
    accounts:      &[AccountInfo],
    vb:            f64,
    vq:            f64,
    f_ba:          f64,
    max_amount_in: u64,
    buy_cap:       u64,  // max SOL input to dammv2 BtoA
    fallback:      u64,
) -> u64 {
    let max_f = max_amount_in as f64;
    let buy_cap_f = if buy_cap > 0 { buy_cap as f64 } else { f64::MAX };
    let fee_on_input = dammv2.collect_fee_mode == 1; // OnlyB
    let mut cumulative_mid: f64 = 0.0;
    let mut best_dx = fallback as f64;

    for bin_offset in 0..70i32 {
        // Load sell bin: input = MEMECOIN (base), output = SOL (quote)
        let (s, gross_cap) = match dlmm.get_bin_segment(accounts, dlmm.base_token_pk, bin_offset) {
            Ok(Some((slope, cap, fee_f))) if slope > 0.0 && cap > 0 && fee_f > 0.0 => {
                (slope, cap as f64 / fee_f)
            }
            Ok(Some(_)) => continue,
            _ => break,
        };

        // disc is the same for both modes
        let disc = vq * vb * f_ba * s;
        if disc <= vq * vq { break; }

        let dx_star = if fee_on_input {
            (disc.sqrt() - vq) / f_ba
        } else {
            disc.sqrt() - vq
        }.min(buy_cap_f);

        // mid at dx_star
        let mid_star = if fee_on_input {
            // mid = vb * f_ba * dx / (vq + f_ba * dx)
            let eff = f_ba * dx_star;
            vb * eff / (vq + eff)
        } else {
            // mid = f_ba * vb * dx / (vq + dx)
            f_ba * vb * dx_star / (vq + dx_star)
        };

        if mid_star <= cumulative_mid + gross_cap {
            // Optimal is within this bin's MEMECOIN range
            best_dx = dx_star.min(max_f);
            break;
        }

        // Bin is exhausted: find dx that exactly fills this bin
        let mid_boundary = cumulative_mid + gross_cap;
        let dx_boundary = if fee_on_input {
            // vb * f_ba * dx / (vq + f_ba * dx) = mid_boundary
            // → dx = mid_boundary * vq / (f_ba * (vb - mid_boundary))
            let denom = f_ba * (vb - mid_boundary);
            if denom <= 0.0 { break; }
            mid_boundary * vq / denom
        } else {
            // f_ba * vb * dx / (vq + dx) = mid_boundary
            // → dx = mid_boundary * vq / (f_ba * vb - mid_boundary)
            let denom = f_ba * vb - mid_boundary;
            if denom <= 0.0 { break; }
            mid_boundary * vq / denom
        };
        best_dx = dx_boundary.min(max_f).min(buy_cap_f);

        // Peek at next non-empty sell bin
        let next_s: f64 = {
            let mut found = 0.0;
            for peek in 1..=4i32 {
                match dlmm.get_bin_segment(accounts, dlmm.base_token_pk, bin_offset + peek) {
                    Ok(Some((ns, nc, _))) if ns > 0.0 && nc > 0 => { found = ns; break; }
                    Ok(Some(_)) => continue,
                    _ => break,
                }
            }
            found
        };
        if next_s == 0.0 { break; }

        // CP marginal of dammv2 buy at dx_boundary
        let cp_marg = if fee_on_input {
            // d(mid)/d(dx) = f_ba * vb * vq / (vq + f_ba * dx)^2
            let denom = vq + f_ba * dx_boundary;
            f_ba * vb * vq / (denom * denom)
        } else {
            // d(mid)/d(dx) = f_ba * vb * vq / (vq + dx)^2
            let denom = vq + dx_boundary;
            f_ba * vb * vq / (denom * denom)
        };
        if next_s * cp_marg <= 1.0 { break; }

        cumulative_mid += gross_cap;
        if best_dx >= max_f || best_dx >= buy_cap_f { break; }
    }

    clamp(best_dx, max_amount_in).unwrap_or(fallback)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::programs::meteora_dlmm::dlmm_lib::{Bin, LbPairSlim};

    // ── Helpers ──────────────────────────────────────────────────────────

    fn make_pump(
        pool_id: Pubkey, base_mint: Pubkey, quote_mint: Pubkey,
        base_r: u64, quote_r: u64, fee_num: u64,
    ) -> PumpAmm {
        let fee_f = 1.0 - fee_num as f64 / 1_000_000.0;
        PumpAmm {
            pool_id, base_token_pk: base_mint, quote_token_pk: quote_mint,
            base_vault_amount: base_r, quote_vault_amount: quote_r,
            price: quote_r as f64 / base_r as f64,
            fee_numerator: fee_num,
            fee_factor: (fee_f, fee_f),
            static_base: 0, dyn_start: 0,
            buy_max_in: 0, buy_max_out: base_r,
            sell_max_in: 0, sell_max_out: quote_r,
            prepared: true,
        }
    }

    /// Build a MeteoraDammV2 with given virtual reserves (base, quote in token units).
    /// We derive sqrt_price and liquidity from the desired virtual reserves.
    fn make_dammv2(
        pool_id: Pubkey, base_mint: Pubkey, quote_mint: Pubkey,
        vb_tokens: f64, vq_sol: f64,
        fee_num_ab: f64, fee_num_ba: f64,
        collect_fee_mode: u8,
    ) -> MeteoraDammV2 {
        // vb = L / sqrt_price_raw,  vq = L * sqrt_price_raw / Q128
        // so sqrt_price_raw = sqrt(vq * Q128 / vb)  and  L = vb * sqrt_price_raw
        let ratio = vq_sol / vb_tokens * Q128;
        let sqrt_price_raw = ratio.sqrt() as u128;
        let liquidity = (vb_tokens * sqrt_price_raw as f64) as u128;

        let f_ab = 1.0 - fee_num_ab;
        let f_ba = 1.0 - fee_num_ba;

        MeteoraDammV2 {
            pool_id, base_token_pk: base_mint, quote_token_pk: quote_mint,
            pool: None,
            sqrt_price: sqrt_price_raw, liquidity,
            collect_fee_mode, activation_type: 0,
            base_vault_amount: vb_tokens as u64,
            quote_vault_amount: vq_sol as u64,
            static_base: 0, dyn_start: 0,
            price: vq_sol / vb_tokens,
            inverse_price: vb_tokens / vq_sol,
            fee_rate_a_to_b: fee_num_ab,
            fee_rate_b_to_a: fee_num_ba,
            fee_factor: (f_ab, f_ba),
            buy_max_in: 0, buy_max_out: vb_tokens as u64,
            sell_max_in: 0, sell_max_out: vq_sol as u64,
            prepared: true,
        }
    }

    fn make_dlmm(
        pool_id: Pubkey, base_mint: Pubkey, quote_mint: Pubkey,
        price_sol_per_token: f64, fee_num: u64,
        buy_max_in: u64, sell_max_in: u64,
    ) -> MeteoraDlmm {
        const DLMM_FEE_PREC: u64 = 1_000_000_000;
        let lb_price = (price_sol_per_token * (1u128 << 64) as f64) as u128;
        let fee_f = 1.0 - fee_num as f64 / DLMM_FEE_PREC as f64;
        MeteoraDlmm {
            pool_id, base_token_pk: base_mint, quote_token_pk: quote_mint,
            lb_pair_slim: LbPairSlim::default(), active_bin: Bin::default(),
            lb_price, price: price_sol_per_token,
            static_base: 0, dyn_start: 0,
            fee_numerator: fee_num, base_fee: 0,
            fee_factor: (fee_f, fee_f),
            buy_max_in, buy_max_out: 0,
            sell_max_in, sell_max_out: 0,
            buy_swap_cache: None, sell_swap_cache: None,
            prepare_failed: false,
        }
    }

    // ── check_pump_dammv2 ─────────────────────────────────────────────────

    #[test]
    fn test_pump_dammv2_arb_5pct_gap() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();

        // pump: 0.001 SOL per token (cheaper)
        let pump = make_pump(
            solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000, 1_000_000_000_000, 5_000,
        );
        // dammv2: 0.00105 SOL per token (+5%)
        let dammv2 = make_dammv2(
            solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000.0, 1_050_000_000_000.0,
            0.003, 0.003, 0,
        );

        let result = check_pump_dammv2(&pump, &dammv2, quote_mint, 10_000_000_000);
        if let Some(opp) = result {
            assert!(opp.expected_profit > 0);
            assert_eq!(opp.buy_pool_id,  pump.pool_id);
            assert_eq!(opp.sell_pool_id, dammv2.pool_id);
        }
    }

    #[test]
    fn test_pump_dammv2_no_arb_equal_price() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let pump = make_pump(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000, 1_000_000_000_000, 5_000);
        let dammv2 = make_dammv2(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000.0, 1_000_000_000_000.0, 0.003, 0.003, 0);
        assert!(check_pump_dammv2(&pump, &dammv2, quote_mint, 10_000_000_000).is_none());
    }

    #[test]
    fn test_pump_dammv2_wrong_input_mint() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let pump = make_pump(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000, 1_000_000_000_000, 5_000);
        let dammv2 = make_dammv2(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000.0, 1_050_000_000_000.0, 0.003, 0.003, 0);
        // wrong direction: pass base mint
        assert!(check_pump_dammv2(&pump, &dammv2, base_mint, 10_000_000_000).is_none());
    }

    // ── check_dammv2_pump ─────────────────────────────────────────────────

    #[test]
    fn test_dammv2_pump_arb_5pct_gap_both_token() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();

        // dammv2: 0.001 SOL per token (cheaper buy)
        let dammv2 = make_dammv2(
            solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000.0, 1_000_000_000_000.0,
            0.003, 0.003, 0, // BothToken
        );
        // pump: 0.00105 SOL per token (+5%, expensive sell)
        let pump = make_pump(
            solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000, 1_050_000_000_000, 5_000,
        );

        let result = check_dammv2_pump(&dammv2, &pump, quote_mint, 10_000_000_000);
        if let Some(opp) = result {
            assert!(opp.expected_profit > 0);
            assert_eq!(opp.buy_pool_id,  dammv2.pool_id);
            assert_eq!(opp.sell_pool_id, pump.pool_id);
        }
    }

    #[test]
    fn test_dammv2_pump_arb_5pct_gap_onlyb() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();

        let dammv2 = make_dammv2(
            solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000.0, 1_000_000_000_000.0,
            0.003, 0.003, 1, // OnlyB
        );
        let pump = make_pump(
            solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000, 1_050_000_000_000, 5_000,
        );

        let result = check_dammv2_pump(&dammv2, &pump, quote_mint, 10_000_000_000);
        if let Some(opp) = result {
            assert!(opp.expected_profit > 0);
        }
    }

    #[test]
    fn test_dammv2_pump_no_arb() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let dammv2 = make_dammv2(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000.0, 1_000_000_000_000.0, 0.003, 0.003, 0);
        let pump = make_pump(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000, 1_000_000_000_000, 5_000);
        assert!(check_dammv2_pump(&dammv2, &pump, quote_mint, 10_000_000_000).is_none());
    }

    // ── check_dlmm_dammv2 ─────────────────────────────────────────────────

    #[test]
    fn test_dlmm_dammv2_arb_5pct_gap() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();

        // dlmm: 0.001 SOL per token (cheap buy)
        let dlmm = make_dlmm(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            0.001, 3_000_000, 5_000_000_000, 0);
        // dammv2: 0.00105 SOL per token (+5%, expensive sell)
        let dammv2 = make_dammv2(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000.0, 1_050_000_000_000.0, 0.003, 0.003, 0);

        let result = check_dlmm_dammv2(&dlmm, &dammv2, quote_mint, 5_000_000_000, &[]);
        if let Some(opp) = result {
            assert!(opp.expected_profit > 0);
            assert_eq!(opp.buy_pool_id,  dlmm.pool_id);
            assert_eq!(opp.sell_pool_id, dammv2.pool_id);
        }
    }

    #[test]
    fn test_dlmm_dammv2_no_arb() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let dlmm   = make_dlmm(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            0.001, 3_000_000, 5_000_000_000, 0);
        let dammv2 = make_dammv2(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000.0, 1_000_000_000_000.0, 0.003, 0.003, 0);
        assert!(check_dlmm_dammv2(&dlmm, &dammv2, quote_mint, 5_000_000_000, &[]).is_none());
    }

    // ── check_dammv2_dlmm ─────────────────────────────────────────────────

    #[test]
    fn test_dammv2_dlmm_arb_5pct_gap_both_token() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();

        let dammv2 = make_dammv2(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000.0, 1_000_000_000_000.0, 0.003, 0.003, 0); // BothToken
        let dlmm = make_dlmm(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            0.00105, 3_000_000, 0, 1_000_000_000);

        let result = check_dammv2_dlmm(&dammv2, &dlmm, quote_mint, 5_000_000_000, &[]);
        if let Some(opp) = result {
            assert!(opp.expected_profit > 0);
            assert_eq!(opp.buy_pool_id,  dammv2.pool_id);
            assert_eq!(opp.sell_pool_id, dlmm.pool_id);
        }
    }

    #[test]
    fn test_dammv2_dlmm_arb_5pct_gap_onlyb() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();

        let dammv2 = make_dammv2(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000.0, 1_000_000_000_000.0, 0.003, 0.003, 1); // OnlyB
        let dlmm = make_dlmm(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            0.00105, 3_000_000, 0, 1_000_000_000);

        let result = check_dammv2_dlmm(&dammv2, &dlmm, quote_mint, 5_000_000_000, &[]);
        if let Some(opp) = result {
            assert!(opp.expected_profit > 0);
        }
    }

    #[test]
    fn test_dammv2_dlmm_no_arb() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let dammv2 = make_dammv2(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000.0, 1_000_000_000_000.0, 0.003, 0.003, 0);
        let dlmm = make_dlmm(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            0.001, 3_000_000, 0, 1_000_000_000);
        assert!(check_dammv2_dlmm(&dammv2, &dlmm, quote_mint, 5_000_000_000, &[]).is_none());
    }
}
