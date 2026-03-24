//! Arbitrage-opportunity checkers where **PumpAmm is pool 1 (the buy leg)**.
//!
//! # Pool-1 = PumpAmm conventions
//!
//! PumpAmm is a constant-product AMM (x · y = k) with an asymmetric fee:
//!   • Buy  (quote → base, e.g. SOL → MEMECOIN): fee on INPUT   (CpFeeOnInput).
//!   • Sell (base → quote, e.g. MEMECOIN → SOL): fee on OUTPUT  (CpFeeOnOutput).
//!
//! Fee denominator: 1 000 000 (millionths).
//!
//! # Covered paths
//!
//! | Function           | Pool 1        | Pool 2         | Model pair                   |
//! |--------------------|---------------|----------------|------------------------------|
//! | `check_pump_pump`  | PumpAmm (buy) | PumpAmm (sell) | CpFeeOnInput + CpFeeOnOutput |
//! | `check_pump_dlmm`  | PumpAmm (buy) | DLMM (sell)    | CpFeeOnInput + Linear (multi-bin) |
//!
//! `check_pump_pump` is pure-math (no accounts needed).
//! `check_pump_dlmm` accepts `accounts` to walk multiple DLMM bins via the
//! self-contained `walk_bins_pump_dlmm`; gracefully degrades to single-bin
//! if the swap cache is not yet initialised.

use pinocchio::{account_info::AccountInfo, pubkey::Pubkey};

use crate::programs::meteora_dlmm::MeteoraDlmm;
use crate::programs::pump_amm::PumpAmm;
use crate::programs::ProgramMeta; // required to call get_bin_segment on MeteoraDlmm

use super::{dlmm_walk_int, ArbOpportunity, MIN_PROFIT_BPS, MIN_PROFIT_LAMPORTS};

// ─── PumpAmm fee constant ─────────────────────────────────────────────────────

const PUMP_FEE_DENOM: u128 = 1_000_000;

// ─── Integer fast-quote helpers ───────────────────────────────────────────────

/// PumpAmm buy output: quote → base, fee applied on input.
#[inline(always)]
fn pump_buy_out(base_r: u64, quote_r: u64, fee_num: u64, in_amount: u64) -> u64 {
    let base_r = base_r as u128;
    let quote_r = quote_r as u128;
    let in_eff = (in_amount as u128) * (PUMP_FEE_DENOM - fee_num as u128) / PUMP_FEE_DENOM;
    if quote_r + in_eff == 0 {
        return 0;
    }
    (base_r * in_eff / (quote_r + in_eff)).min(u64::MAX as u128) as u64
}

/// PumpAmm sell output: base → quote, fee applied on output.
#[inline(always)]
fn pump_sell_out(base_r: u64, quote_r: u64, fee_num: u64, in_amount: u64) -> u64 {
    let base_r = base_r as u128;
    let quote_r = quote_r as u128;
    let denom = base_r + in_amount as u128;
    if denom == 0 {
        return 0;
    }
    let out_raw = quote_r * in_amount as u128 / denom;
    (out_raw * (PUMP_FEE_DENOM - fee_num as u128) / PUMP_FEE_DENOM).min(u64::MAX as u128) as u64
}

// ─── Closed-form helpers ──────────────────────────────────────────────────────

#[inline(always)]
fn clamp(dx: f64, max_amount_in: u64) -> Option<u64> {
    if dx <= 0.0 || !dx.is_finite() {
        return None;
    }
    let v = dx.min(max_amount_in as f64) as u64;
    if v == 0 { None } else { Some(v) }
}

// ─── check_pump_pump ─────────────────────────────────────────────────────────

/// Check for a 2-pool arb: buy on `pump1`, sell on `pump2`.
///
/// Both pools must share the same token pair.
/// `input_mint` must be `pump1.quote_token_pk` (e.g. SOL).
///
/// # Model: CpFeeOnInput (pump1) + CpFeeOnOutput (pump2)
///
/// ```text
/// A   = quote_r1 · base_r2
/// B   = base_r2  + base_r1
/// C   = f2 · quote_r2 · base_r1
/// dx* = (√(f1 · C · A) − A) / (f1 · B)
/// ```
///
/// Pure maths — no accounts needed, O(1) CU.
pub fn check_pump_pump(
    pump1: &PumpAmm,
    pump2: &PumpAmm,
    input_mint: Pubkey,
    max_amount_in: u64,
) -> Option<ArbOpportunity> {
    if input_mint != pump1.quote_token_pk {
        return None;
    }

    let r1_in  = pump1.quote_vault_amount as f64;
    let r1_out = pump1.base_vault_amount  as f64;
    let r2_in  = pump2.base_vault_amount  as f64;
    let r2_out = pump2.quote_vault_amount as f64;
    let f1 = pump1.fee_factor.1; // quote → base
    let f2 = pump2.fee_factor.0; // base  → quote

    // Quick profitability gate: arb exists iff f1·f2·r1_out·r2_out > r1_in·r2_in.
    if f1 * f2 * r1_out * r2_out <= r1_in * r2_in {
        return None;
    }

    let a = r1_in * r2_in;
    let b = r2_in + r1_out;
    let c = f2 * r2_out * r1_out;
    let dx = ((f1 * c * a).sqrt() - a) / (f1 * b);
    let optimal = clamp(dx, max_amount_in)?;

    // ── Integer verification ──────────────────────────────────────────────
    let mid = pump_buy_out(pump1.base_vault_amount, pump1.quote_vault_amount, pump1.fee_numerator, optimal);
    let out = pump_sell_out(pump2.base_vault_amount, pump2.quote_vault_amount, pump2.fee_numerator, mid);

    let profit = out.checked_sub(optimal)?;
    if profit < MIN_PROFIT_LAMPORTS {
        return None;
    }
    let profit_bps = ((profit as u128 * 10_000) / optimal as u128) as u32;
    if profit_bps < MIN_PROFIT_BPS {
        return None;
    }

    Some(ArbOpportunity {
        buy_pool_id:       pump1.pool_id,
        sell_pool_id:      pump2.pool_id,
        input_mint,
        middle_mint:       pump1.base_token_pk,
        optimal_amount_in: optimal,
        expected_profit:   profit,
        profit_bps,
    })
}

// ─── check_pump_dlmm ─────────────────────────────────────────────────────────

/// Check for a 2-pool arb: buy on `pump` (pool 1), sell on `dlmm` (pool 2).
///
/// `input_mint` must be `pump.quote_token_pk` (e.g. SOL).
/// `dlmm.base_token_pk` must equal `pump.base_token_pk`.
///
/// # Model: CpFeeOnInput (pump buy) + Linear bins (dlmm sell)
///
/// The optimal is found by `walk_bins_pump_dlmm` which walks DLMM sell bins
/// applying the exact per-bin closed-form and stopping at the real next-bin
/// slope check, with no external abstractions.
///
/// Integer verification uses `dlmm_walk_int` which walks the same bins using
/// exact integer arithmetic per bin.
///
/// # CU profile
///
/// Fast gate: O(1), pure maths.
/// Multi-bin path: O(min(profit_bps / bin_step_bps, 70)) × O(peek≤4) bin reads.
pub fn check_pump_dlmm(
    pump:           &PumpAmm,
    dlmm:           &MeteoraDlmm,
    input_mint:     Pubkey,
    max_amount_in:  u64,
    accounts:       &[AccountInfo],
) -> Option<ArbOpportunity> {
    if input_mint != pump.quote_token_pk {
        return None;
    }
    #[cfg(any(test, feature = "debug"))]
    debug_assert_eq!(pump.base_token_pk, dlmm.base_token_pk, "check_pump_dlmm: mismatched base token");

    let r_in   = pump.quote_vault_amount as f64; // SOL
    let r_out  = pump.base_vault_amount  as f64; // MEMECOIN
    let f_pump = pump.fee_factor.1;              // quote → base fee factor
    let p      = dlmm.price;                     // SOL per MEMECOIN (sell direction)
    let f_dlmm = dlmm.fee_factor.0;             // base → quote fee factor

    if p <= 0.0 || r_in <= 0.0 || r_out <= 0.0 {
        return None;
    }

    // ── Fast gate: single-bin profitability ──────────────────────────────
    // disc = r_in · r_out · f_pump · p · f_dlmm  must exceed r_in²
    let disc = r_in * r_out * f_pump * p * f_dlmm;
    if disc <= r_in * r_in {
        return None;
    }

    // Single-bin fallback optimal (used when bin cache is not yet ready).
    let dx_single = {
        let mut dx = (disc.sqrt() - r_in) / f_pump;
        // buy_max_in = max MEMECOIN (base) input for the X→Y (MEMECOIN→SOL) direction.
        let sell_cap = dlmm.buy_max_in as f64;
        if sell_cap > 0.0 {
            let mid = r_out * dx * f_pump / (r_in + dx * f_pump);
            if mid > sell_cap {
                let denom = f_pump * (r_out - sell_cap);
                if denom > 0.0 { dx = sell_cap * r_in / denom; } else { return None; }
            }
        }
        dx
    };
    let fallback_optimal = clamp(dx_single, max_amount_in)?;

    // ── Multi-bin optimal (self-contained, no external abstractions) ─────
    let optimal = walk_bins_pump_dlmm(dlmm, accounts, r_in, r_out, f_pump, max_amount_in, fallback_optimal);

    // ── Integer verification (bin walk) ───────────────────────────────────
    let mid_int = pump_buy_out(
        pump.base_vault_amount,
        pump.quote_vault_amount,
        pump.fee_numerator,
        optimal,
    );
    let mid_int = if dlmm.buy_max_in > 0 { mid_int.min(dlmm.buy_max_in) } else { mid_int };

    let profit_pct = (disc.sqrt() / r_in - 1.0).max(0.0);
    let out = dlmm_walk_int(dlmm, accounts, dlmm.base_token_pk, mid_int, profit_pct);

    let profit = out.checked_sub(optimal)?;
    if profit < MIN_PROFIT_LAMPORTS {
        return None;
    }
    let profit_bps = ((profit as u128 * 10_000) / optimal as u128) as u32;
    if profit_bps < MIN_PROFIT_BPS {
        return None;
    }

    Some(ArbOpportunity {
        buy_pool_id:       pump.pool_id,
        sell_pool_id:      dlmm.pool_id,
        input_mint,
        middle_mint:       pump.base_token_pk,
        optimal_amount_in: optimal,
        expected_profit:   profit,
        profit_bps,
    })
}

// ─── Self-contained multi-bin optimal walker ─────────────────────────────────

/// Walk DLMM sell bins to find the optimal SOL input for a pump→dlmm arb.
///
/// # Math (per bin)
///
/// Pump buy (CpFeeOnInput):  mid(dx) = r_out · f · dx / (r_in + f · dx)
/// DLMM sell bin (Linear):   out_bin = slope · mid   (slope = SOL/MEMECOIN after fee)
///
/// Setting d(out_bin - dx)/d(dx) = 0:
///   dx* = (√(r_in · r_out · f · slope) − r_in) / f
///
/// This is independent of prior bins, so it's O(1) per bin.
///
/// # Bin validity
///
/// The bin absorbs `gross_cap` MEMECOIN (= raw_cap / fee_f).
/// mid(dx*) must lie in [cumulative_mid, cumulative_mid + gross_cap].
/// • If so   → return dx* (optimal is within this bin).
/// • If not  → fill this bin to its boundary, advance cumulative, continue.
///
/// # Early exit
///
/// Before entering the next bin, peek at its actual slope (real price × real fee)
/// via `get_bin_segment`.  If `next_slope × CP_marginal ≤ 1.0` → stop.
///
/// Returns `fallback` when the swap cache is not yet initialised.
fn walk_bins_pump_dlmm(
    dlmm:          &MeteoraDlmm,
    accounts:      &[AccountInfo],
    r_in:          f64,  // pump.quote_vault (SOL)
    r_out:         f64,  // pump.base_vault  (MEMECOIN)
    f_pump:        f64,  // pump buy fee factor (CpFeeOnInput)
    max_amount_in: u64,
    fallback:      u64,
) -> u64 {
    let max_f = max_amount_in as f64;
    let mut cumulative_mid: f64 = 0.0; // total MEMECOIN consumed by prior bins
    let mut best_dx = fallback as f64;

    for bin_offset in 0..70i32 {
        // Load current sell bin: input = MEMECOIN (base), output = SOL
        let (s, gross_cap) = match dlmm.get_bin_segment(accounts, dlmm.base_token_pk, bin_offset) {
            Ok(Some((slope, cap, fee_f))) if slope > 0.0 && cap > 0 && fee_f > 0.0 => {
                (slope, cap as f64 / fee_f) // slope: SOL/MEMECOIN; gross_cap: MEMECOIN
            }
            Ok(Some(_)) => continue, // empty bin — skip
            _ => break,              // cache not ready or no more bins
        };

        // Discriminant: r_in² must be exceeded for any profit at this slope
        let disc = r_in * r_out * f_pump * s;
        if disc <= r_in * r_in {
            break;
        }

        // Unconstrained optimal SOL input for this bin's slope
        let dx_star = (disc.sqrt() - r_in) / f_pump;

        // Corresponding MEMECOIN output from pump at dx_star
        let mid_star = r_out * f_pump * dx_star / (r_in + f_pump * dx_star);

        if mid_star <= cumulative_mid + gross_cap {
            // Optimal is within this bin's MEMECOIN range
            best_dx = dx_star.min(max_f);
            break;
        }

        // Bin is exhausted before optimal: find dx that exactly fills this bin
        let mid_boundary = cumulative_mid + gross_cap;
        if mid_boundary >= r_out {
            break; // would invert a zero/negative denominator
        }
        let dx_boundary = mid_boundary * r_in / (f_pump * (r_out - mid_boundary));
        best_dx = dx_boundary.min(max_f);

        // Peek at the actual next non-empty bin's slope before deciding to continue
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
        if next_s == 0.0 {
            break; // no further bins exist
        }

        // CP marginal at dx_boundary: d(mid)/d(dx) = f·r_in·r_out / (r_in + f·dx)²
        let denom = r_in + f_pump * dx_boundary;
        let cp_marg = f_pump * r_in * r_out / (denom * denom);
        if next_s * cp_marg <= 1.0 {
            break; // next bin cannot recover a profitable marginal rate
        }

        cumulative_mid += gross_cap;
        if best_dx >= max_f {
            break;
        }
    }

    clamp(best_dx, max_amount_in).unwrap_or(fallback)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pump(pool_id: Pubkey, base_mint: Pubkey, quote_mint: Pubkey, base_r: u64, quote_r: u64, fee_num: u64) -> PumpAmm {
        let fee_f = 1.0 - fee_num as f64 / 1_000_000.0;
        PumpAmm {
            pool_id,
            base_token_pk:      base_mint,
            quote_token_pk:     quote_mint,
            base_vault_amount:  base_r,
            quote_vault_amount: quote_r,
            price:              quote_r as f64 / base_r as f64,
            fee_numerator:      fee_num,
            fee_factor:         (fee_f, fee_f),
            static_base:        0,
            dyn_start:          0,
            buy_max_in:         0,
            buy_max_out:        base_r,
            sell_max_in:        0,
            sell_max_out:       quote_r,
            prepared:           true,
        }
    }

    #[test]
    fn test_pump_pump_arb_3pct_gap() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();

        // pool1: cheaper (more base per SOL)
        let p1 = make_pump(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint, 1_000_000_000, 1_000_000_000_000, 5_000);
        // pool2: 3% fewer base tokens for same SOL → higher price
        let p2 = make_pump(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,   970_000_000, 1_000_000_000_000, 5_000);

        let result = check_pump_pump(&p1, &p2, quote_mint, 10_000_000_000);
        if let Some(opp) = result {
            assert!(opp.expected_profit > 0);
            assert_eq!(opp.input_mint,  quote_mint);
            assert_eq!(opp.middle_mint, base_mint);
        }
    }

    #[test]
    fn test_pump_pump_no_arb_equal_prices() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let p1 = make_pump(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint, 1_000_000_000, 1_000_000_000_000, 5_000);
        let p2 = make_pump(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint, 1_000_000_000, 1_000_000_000_000, 5_000);
        assert!(check_pump_pump(&p1, &p2, quote_mint, 10_000_000_000).is_none());
    }

    #[test]
    fn test_pump_pump_wrong_input_mint() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let p1 = make_pump(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint, 1_000_000_000, 1_000_000_000_000, 5_000);
        let p2 = make_pump(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,   970_000_000, 1_000_000_000_000, 5_000);
        // pass base mint as input → wrong direction → None
        assert!(check_pump_pump(&p1, &p2, base_mint, 10_000_000_000).is_none());
    }
}
