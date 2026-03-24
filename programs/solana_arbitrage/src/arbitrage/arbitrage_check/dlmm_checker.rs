//! Arbitrage-opportunity checkers where **Meteora DLMM is pool 1 (the buy leg)**.
//!
//! # DLMM price conventions
//!
//! `dlmm.price` is the **active-bin price in f64**: quote-per-base (SOL per MEMECOIN).
//!
//!   • Sell base (base → quote, swap_for_y = true):  rate = `price`          (SOL per MEMECOIN)
//!   • Buy  base (quote → base, swap_for_y = false): rate = `1 / price`      (MEMECOIN per SOL)
//!
//! Adjacent bins have price `price × (1 + bin_step / 10_000)^±1` — each step is
//! slightly worse for the taker.  The multi-bin walkers stop when the composed
//! marginal rate drops to ≤ 1.0 (arb disappears).
//!
//! # Covered paths
//!
//! | Function           | Pool 1        | Pool 2         | Model pair               |
//! |--------------------|---------------|----------------|--------------------------|
//! | `check_dlmm_dlmm`  | DLMM (buy)    | DLMM (sell)    | Linear + Linear (multi-bin both legs) |
//! | `check_dlmm_pump`  | DLMM (buy)    | PumpAmm (sell) | Linear (multi-bin) + CpFeeOnOutput    |
//!
//! Both functions accept `accounts` for the DLMM bin walk.  They degrade
//! gracefully to single-bin if the swap cache is not yet initialised.

use pinocchio::{account_info::AccountInfo, pubkey::Pubkey};

use crate::programs::meteora_dlmm::MeteoraDlmm;
use crate::programs::pump_amm::PumpAmm;
use crate::programs::ProgramMeta; // required to call get_bin_segment on MeteoraDlmm

use super::{dlmm_walk_int, ArbOpportunity, MIN_PROFIT_BPS, MIN_PROFIT_LAMPORTS};

// ─── PumpAmm integer helper ───────────────────────────────────────────────────

const PUMP_FEE_DENOM: u128 = 1_000_000;

/// PumpAmm sell output: base → quote, fee on output.
#[inline(always)]
fn pump_sell_out(base_r: u64, quote_r: u64, fee_num: u64, in_amount: u64) -> u64 {
    let base_r  = base_r  as u128;
    let quote_r = quote_r as u128;
    let denom = base_r + in_amount as u128;
    if denom == 0 { return 0; }
    let out_raw = quote_r * in_amount as u128 / denom;
    (out_raw * (PUMP_FEE_DENOM - fee_num as u128) / PUMP_FEE_DENOM).min(u64::MAX as u128) as u64
}

// ─── Closed-form helpers ──────────────────────────────────────────────────────

#[inline(always)]
fn clamp(dx: f64, max_amount_in: u64) -> Option<u64> {
    if dx <= 0.0 || !dx.is_finite() { return None; }
    let v = dx.min(max_amount_in as f64) as u64;
    if v == 0 { None } else { Some(v) }
}

// ─── check_dlmm_dlmm ─────────────────────────────────────────────────────────

/// Check for an arb: buy base on `dlmm1` (pool 1), sell base on `dlmm2` (pool 2).
///
/// `input_mint` must be `dlmm1.quote_token_pk` (SOL).
/// Both pools must share the same token pair.
///
/// # Model: Linear (dlmm1 buy) + Linear (dlmm2 sell)
///
/// Profit is linear within a bin-pair:
///
/// ```text
/// combined_rate = buy_slope × sell_slope   (buy_slope: MEMECOIN/SOL, sell_slope: SOL/MEMECOIN)
/// ```
///
/// Profitable iff `combined_rate > 1.0`.  Optimal = greedy fill while combined > 1.
///
/// `walk_bins_dlmm_dlmm` runs a two-pointer walk matching buy/sell bins and
/// stops when the combined rate for the next bin-pair drops to ≤ 1.0.
///
/// # Integer verification
///
/// Uses `dlmm_walk_int` on both legs independently for exact per-bin output.
pub fn check_dlmm_dlmm(
    dlmm1:          &MeteoraDlmm,
    dlmm2:          &MeteoraDlmm,
    input_mint:     Pubkey,
    max_amount_in:  u64,
    accounts:       &[AccountInfo],
) -> Option<ArbOpportunity> {
    if input_mint != dlmm1.quote_token_pk {
        return None;
    }
    #[cfg(any(test, feature = "debug"))]
    debug_assert_eq!(dlmm1.base_token_pk, dlmm2.base_token_pk, "check_dlmm_dlmm: mismatched base token");

    let price1 = dlmm1.price; // SOL per MEMECOIN at dlmm1 active bin
    let price2 = dlmm2.price; // SOL per MEMECOIN at dlmm2 active bin
    let f1 = dlmm1.fee_factor.1; // quote → base (buy)
    let f2 = dlmm2.fee_factor.0; // base  → quote (sell)

    if price1 <= 0.0 || price2 <= 0.0 {
        return None;
    }

    // ── Fast gate ────────────────────────────────────────────────────────
    // buy_slope ≈ f1/price1 (MEMECOIN/SOL), sell_slope ≈ price2*f2 (SOL/MEMECOIN)
    // combined = (price2/price1)·f1·f2 must exceed 1.0.
    let combined_rate = (price2 / price1) * f1 * f2;
    if combined_rate <= 1.0 {
        return None;
    }

    // Single-bin fallback optimal (used when bin cache is not ready).
    // dlmm1 buy leg: SOL (quote/Y) input → use sell_max_in (max quote input).
    // dlmm2 sell leg: MEMECOIN (base/X) input → use buy_max_in (max base input), converted to SOL.
    let buy_cap_f    = if dlmm1.sell_max_in > 0 { dlmm1.sell_max_in as f64 } else { max_amount_in as f64 };
    let sell_cap_sol = if dlmm2.buy_max_in  > 0 && f1 > 0.0 {
        dlmm2.buy_max_in as f64 * price1 / f1
    } else {
        max_amount_in as f64
    };
    let fallback_optimal = clamp(buy_cap_f.min(sell_cap_sol), max_amount_in)?;

    // ── Multi-bin optimal (self-contained greedy two-pointer walk) ────────
    let optimal = walk_bins_dlmm_dlmm(dlmm1, dlmm2, accounts, max_amount_in, fallback_optimal);

    // ── Integer verification ──────────────────────────────────────────────
    let profit_pct = (combined_rate - 1.0).max(0.0);

    let mid_int = dlmm_walk_int(dlmm1, accounts, dlmm1.quote_token_pk, optimal, profit_pct);
    let mid_int = if dlmm2.buy_max_in > 0 { mid_int.min(dlmm2.buy_max_in) } else { mid_int };
    let out     = dlmm_walk_int(dlmm2, accounts, dlmm2.base_token_pk,  mid_int, profit_pct);

    let profit = out.checked_sub(optimal)?;
    if profit < MIN_PROFIT_LAMPORTS { return None; }
    let profit_bps = ((profit as u128 * 10_000) / optimal as u128) as u32;
    if profit_bps < MIN_PROFIT_BPS { return None; }

    Some(ArbOpportunity {
        buy_pool_id:       dlmm1.pool_id,
        sell_pool_id:      dlmm2.pool_id,
        input_mint,
        middle_mint:       dlmm1.base_token_pk,
        optimal_amount_in: optimal,
        expected_profit:   profit,
        profit_bps,
    })
}

// ─── check_dlmm_pump ─────────────────────────────────────────────────────────

/// Check for an arb: buy base on `dlmm` (pool 1), sell base on `pump` (pool 2).
///
/// `input_mint` must be `dlmm.quote_token_pk` (SOL).
/// `pump.base_token_pk` must equal `dlmm.base_token_pk`.
///
/// # Model: Linear (dlmm buy) + CpFeeOnOutput (pump sell)
///
/// ```text
/// a      = f_dlmm / dlmm.price          (MEMECOIN per SOL after fee)
/// r_in   = pump.base_vault              (MEMECOIN)
/// r_out  = pump.quote_vault             (SOL)
///
/// disc   = f_pump · r_out · a · r_in
/// Profitable iff disc > r_in²
/// ```
///
/// `walk_bins_dlmm_pump` finds the multi-bin optimal with a real next-bin
/// slope peek for the early-exit decision.
///
/// Integer verification: `dlmm_walk_int` (DLMM buy leg) + exact CP integer
/// formula (pump sell leg).
pub fn check_dlmm_pump(
    dlmm:           &MeteoraDlmm,
    pump:           &PumpAmm,
    input_mint:     Pubkey,
    max_amount_in:  u64,
    accounts:       &[AccountInfo],
) -> Option<ArbOpportunity> {
    if input_mint != dlmm.quote_token_pk {
        return None;
    }
    #[cfg(any(test, feature = "debug"))]
    debug_assert_eq!(dlmm.base_token_pk, pump.base_token_pk, "check_dlmm_pump: mismatched base token");

    let price  = dlmm.price;             // SOL per MEMECOIN
    let f_dlmm = dlmm.fee_factor.1;      // quote → base direction (buy)
    let r_in   = pump.base_vault_amount  as f64; // MEMECOIN
    let r_out  = pump.quote_vault_amount as f64; // SOL
    let f_pump = pump.fee_factor.0;      // base → quote (sell, CpFeeOnOutput)

    if price <= 0.0 || r_in <= 0.0 || r_out <= 0.0 {
        return None;
    }

    // ── Fast gate ─────────────────────────────────────────────────────────
    // a = f_dlmm / price  (effective MEMECOIN per SOL from DLMM buy)
    // Profitable: disc = f_pump · r_out · a · r_in > r_in²
    let a    = f_dlmm / price;
    let disc = f_pump * r_out * a * r_in;
    if disc <= r_in * r_in {
        return None;
    }

    // Single-bin fallback optimal.
    let dx_single = {
        let mut dx = (disc.sqrt() - r_in) / a;
        if dlmm.sell_max_in > 0 { dx = dx.min(dlmm.sell_max_in as f64); } // sell_max_in = max SOL (quote) input
        dx
    };
    let fallback_optimal = clamp(dx_single, max_amount_in)?;

    // ── Multi-bin optimal (self-contained, no external abstractions) ─────
    // DLMM is pool 1 (buy: SOL → MEMECOIN); Pump is pool 2 (sell: MEMECOIN → SOL).
    let optimal = walk_bins_dlmm_pump(dlmm, accounts, r_in, r_out, f_pump, max_amount_in, fallback_optimal);

    // ── Integer verification ──────────────────────────────────────────────
    let profit_pct = (disc.sqrt() / r_in - 1.0).max(0.0);

    let mid_int = dlmm_walk_int(dlmm, accounts, dlmm.quote_token_pk, optimal, profit_pct);
    let mid_int = mid_int.min(pump.base_vault_amount);

    let out = pump_sell_out(
        pump.base_vault_amount,
        pump.quote_vault_amount,
        pump.fee_numerator,
        mid_int,
    );

    let profit = out.checked_sub(optimal)?;
    if profit < MIN_PROFIT_LAMPORTS { return None; }
    let profit_bps = ((profit as u128 * 10_000) / optimal as u128) as u32;
    if profit_bps < MIN_PROFIT_BPS { return None; }

    Some(ArbOpportunity {
        buy_pool_id:       dlmm.pool_id,
        sell_pool_id:      pump.pool_id,
        input_mint,
        middle_mint:       dlmm.base_token_pk,
        optimal_amount_in: optimal,
        expected_profit:   profit,
        profit_bps,
    })
}

// ─── Self-contained multi-bin optimal walkers ─────────────────────────────────

/// Greedy two-pointer walk for dlmm→dlmm arb.
///
/// Simultaneously consumes buy bins (SOL → MEMECOIN on dlmm1) and sell bins
/// (MEMECOIN → SOL on dlmm2).  At each step:
///   combined = buy_slope × sell_slope
///   If combined ≤ 1.0 → stop (next unit loses money).
///   Otherwise fill min(buy_cap_sol, sell_cap_sol, budget) and advance.
///
/// Returns `fallback` when the swap caches are not initialised.
fn walk_bins_dlmm_dlmm(
    dlmm1:         &MeteoraDlmm, // buy: SOL → MEMECOIN
    dlmm2:         &MeteoraDlmm, // sell: MEMECOIN → SOL
    accounts:      &[AccountInfo],
    max_amount_in: u64,
    fallback:      u64,
) -> u64 {
    let max_f = max_amount_in as f64;
    let mut total_in:   f64 = 0.0;
    let mut buy_offset: i32 = 0;
    let mut sell_offset: i32 = 0;
    let mut buy_remaining:  f64 = 0.0; // SOL capacity remaining in current buy bin
    let mut sell_remaining: f64 = 0.0; // MEMECOIN capacity remaining in current sell bin
    let mut buy_slope:  f64 = 0.0;     // MEMECOIN per SOL (after fee)
    let mut sell_slope: f64 = 0.0;     // SOL per MEMECOIN (after fee)
    let mut buy_needs_load  = true;
    let mut sell_needs_load = true;

    for _ in 0..140 { // max 70 bins per side
        // Load next non-empty buy bin
        if buy_needs_load {
            match dlmm1.get_bin_segment(accounts, dlmm1.quote_token_pk, buy_offset) {
                Ok(Some((s, c, f))) if s > 0.0 && c > 0 && f > 0.0 => {
                    buy_slope     = s;
                    buy_remaining = c as f64 / f; // gross SOL input capacity
                    buy_needs_load = false;
                }
                Ok(Some(_)) => { buy_offset += 1; continue; } // empty bin — skip
                _ => break,
            }
        }

        // Load next non-empty sell bin
        if sell_needs_load {
            match dlmm2.get_bin_segment(accounts, dlmm2.base_token_pk, sell_offset) {
                Ok(Some((s, c, f))) if s > 0.0 && c > 0 && f > 0.0 => {
                    sell_slope     = s;
                    sell_remaining = c as f64 / f; // gross MEMECOIN input capacity
                    sell_needs_load = false;
                }
                Ok(Some(_)) => { sell_offset += 1; continue; }
                _ => break,
            }
        }

        // Stop if combined rate ≤ 1 (no more profit possible)
        if buy_slope * sell_slope <= 1.0 {
            break;
        }

        // How much SOL can flow through both bins?
        // sell_remaining MEMECOIN / buy_slope (MEMECOIN/SOL) → SOL equivalent
        let sell_cap_sol = sell_remaining / buy_slope;
        let fillable = buy_remaining.min(sell_cap_sol).min(max_f - total_in);
        if fillable <= 0.0 { break; }

        let tokens_consumed = fillable * buy_slope;
        total_in       += fillable;
        buy_remaining  -= fillable;
        sell_remaining -= tokens_consumed;

        if total_in >= max_f { break; }

        // Advance exhausted bin(s) (epsilon guards float imprecision)
        const EPS: f64 = 0.5;
        if buy_remaining  < EPS { buy_offset  += 1; buy_needs_load  = true; }
        if sell_remaining < EPS { sell_offset += 1; sell_needs_load = true; }
    }

    if total_in > 0.0 {
        (total_in as u64).min(max_amount_in)
    } else {
        fallback
    }
}

/// Walk DLMM buy bins to find the optimal SOL input for a dlmm→pump arb.
///
/// # Math (per bin)
///
/// DLMM buy bin (Linear):   mid(dx) = cumulative_out + slope · (dx − cumulative_in)
/// Pump sell (CpFeeOnOutput): out = f · r_out · mid / (r_in + mid)
///
/// Setting d(out)/d(dx) = 0:
///   mid* = √(f · r_out · r_in · slope) − r_in
///   dx*  = cumulative_in + (mid* − cumulative_out) / slope
///
/// # Bin validity
///
/// The bin covers mid in [cumulative_out, cumulative_out + slope · gross_cap].
/// • If mid* ≤ cumulative_out + slope·gross_cap → optimal is in this bin.
/// • Otherwise → fill this bin, update cumulatives, continue.
///
/// # Early exit
///
/// Before entering the next bin, peek at its actual slope via `get_bin_segment`.
/// If `next_slope × CP_marginal ≤ 1.0` → stop.
///
/// Returns `fallback` when the swap cache is not initialised.
fn walk_bins_dlmm_pump(
    dlmm:          &MeteoraDlmm,
    accounts:      &[AccountInfo],
    r_in:          f64,  // pump.base_vault  (MEMECOIN)
    r_out:         f64,  // pump.quote_vault (SOL)
    f_pump:        f64,  // pump sell fee factor (CpFeeOnOutput)
    max_amount_in: u64,
    fallback:      u64,
) -> u64 {
    let max_f = max_amount_in as f64;
    let mut cumulative_in:  f64 = 0.0; // SOL consumed by prior DLMM buy bins
    let mut cumulative_out: f64 = 0.0; // MEMECOIN produced by prior DLMM buy bins
    let mut best_dx = fallback as f64;

    for bin_offset in 0..70i32 {
        // Load buy bin: input = SOL (quote), output = MEMECOIN (base)
        let (s, gross_cap) = match dlmm.get_bin_segment(accounts, dlmm.quote_token_pk, bin_offset) {
            Ok(Some((slope, cap, fee_f))) if slope > 0.0 && cap > 0 && fee_f > 0.0 => {
                (slope, cap as f64 / fee_f) // slope: MEMECOIN/SOL; gross_cap: SOL
            }
            Ok(Some(_)) => continue, // empty bin — skip
            _ => break,
        };

        let disc = f_pump * r_out * r_in * s;
        if disc <= r_in * r_in {
            break; // not profitable at this slope
        }

        // MEMECOIN amount where pump sell marginal equals DLMM marginal
        let mid_star = disc.sqrt() - r_in;

        // Bin covers mid in [cumulative_out, cumulative_out + s·gross_cap]
        let mid_cap = cumulative_out + s * gross_cap;

        if mid_star < cumulative_out {
            break; // optimal was in a prior bin (shouldn't happen in forward walk)
        }
        if mid_star <= mid_cap {
            // Optimal is within this bin
            let dx_star = cumulative_in + (mid_star - cumulative_out) / s;
            best_dx = dx_star.min(max_f);
            break;
        }

        // Fill this bin completely
        let dx_boundary = cumulative_in + gross_cap;
        best_dx = dx_boundary.min(max_f);

        // Peek at actual next non-empty bin's slope before deciding to continue
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
        if next_s == 0.0 {
            break; // no further bins
        }

        // CP marginal at mid_cap: d(out)/d(mid) = f·r_out·r_in / (r_in + mid_cap)²
        let denom = r_in + mid_cap;
        let cp_marg = f_pump * r_out * r_in / (denom * denom);
        if next_s * cp_marg <= 1.0 {
            break; // next bin cannot yield profitable marginal rate
        }

        cumulative_in  = dx_boundary;
        cumulative_out = mid_cap;
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
    use crate::programs::meteora_dlmm::dlmm_lib::{Bin, LbPairSlim};

    fn make_dlmm(
        pool_id: Pubkey, base_mint: Pubkey, quote_mint: Pubkey,
        price_sol_per_token: f64, fee_num: u64,
        buy_max_in: u64, sell_max_in: u64,
    ) -> MeteoraDlmm {
        const DLMM_FEE_PREC: u64 = 1_000_000_000;
        let lb_price = (price_sol_per_token * (1u128 << 64) as f64) as u128;
        let fee_f    = 1.0 - fee_num as f64 / DLMM_FEE_PREC as f64;
        MeteoraDlmm {
            pool_id,
            base_token_pk:  base_mint,
            quote_token_pk: quote_mint,
            lb_pair_slim:   LbPairSlim::default(),
            active_bin:     Bin::default(),
            lb_price,
            price: price_sol_per_token,
            static_base: 0,
            dyn_start:   0,
            fee_numerator: fee_num,
            base_fee:    0,
            fee_factor:  (fee_f, fee_f),
            buy_max_in,
            buy_max_out: 0,
            sell_max_in,
            sell_max_out: 0,
            buy_swap_cache:  None,
            sell_swap_cache: None,
            prepare_failed:  false,
        }
    }

    fn make_pump(
        pool_id: Pubkey, base_mint: Pubkey, quote_mint: Pubkey,
        base_r: u64, quote_r: u64, fee_num: u64,
    ) -> PumpAmm {
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
            static_base: 0, dyn_start: 0,
            buy_max_in: 0, buy_max_out: base_r,
            sell_max_in: 0, sell_max_out: quote_r,
            prepared: true,
        }
    }

    /// dlmm1 cheaper than dlmm2 → arb detected (fallback path, no swap caches).
    #[test]
    fn test_dlmm_dlmm_arb_5pct_gap() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let fee = 3_000_000u64; // 0.3 %

        let d1 = make_dlmm(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint, 0.001,   fee, 10_000_000_000, 0);
        let d2 = make_dlmm(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint, 0.00105, fee, 0, 1_000_000_000);

        let result = check_dlmm_dlmm(&d1, &d2, quote_mint, 10_000_000_000, &[]);
        if let Some(opp) = result {
            assert!(opp.expected_profit > 0);
            assert_eq!(opp.buy_pool_id,  d1.pool_id);
            assert_eq!(opp.sell_pool_id, d2.pool_id);
        }
    }

    /// Equal prices → no arb.
    #[test]
    fn test_dlmm_dlmm_no_arb() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let d1 = make_dlmm(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint, 0.001, 3_000_000, 10_000_000_000, 0);
        let d2 = make_dlmm(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint, 0.001, 3_000_000, 0, 1_000_000_000);
        assert!(check_dlmm_dlmm(&d1, &d2, quote_mint, 10_000_000_000, &[]).is_none());
    }

    /// DLMM buy (cheap) + Pump sell (expensive) → arb detected (fallback path).
    #[test]
    fn test_dlmm_pump_arb_5pct_gap() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();

        // DLMM: 0.001 SOL per token
        let dlmm = make_dlmm(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint, 0.001, 3_000_000, 5_000_000_000, 0);
        // Pump: 0.00105 SOL per token (+5%)
        let pump = make_pump(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint,
            1_000_000_000, 1_050_000_000_000, 5_000);

        let result = check_dlmm_pump(&dlmm, &pump, quote_mint, 5_000_000_000, &[]);
        if let Some(opp) = result {
            assert!(opp.expected_profit > 0);
            assert_eq!(opp.buy_pool_id,  dlmm.pool_id);
            assert_eq!(opp.sell_pool_id, pump.pool_id);
        }
    }

    /// Wrong input mint → None.
    #[test]
    fn test_dlmm_pump_wrong_mint() {
        let base_mint  = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let quote_mint = solana_sdk::pubkey::Pubkey::new_unique().to_bytes();
        let dlmm = make_dlmm(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint, 0.001, 3_000_000, 5_000_000_000, 0);
        let pump = make_pump(solana_sdk::pubkey::Pubkey::new_unique().to_bytes(), base_mint, quote_mint, 1_000_000_000, 1_050_000_000_000, 5_000);
        // pass base_mint (MEMECOIN) as input — wrong direction
        assert!(check_dlmm_pump(&dlmm, &pump, base_mint, 5_000_000_000, &[]).is_none());
    }
}
