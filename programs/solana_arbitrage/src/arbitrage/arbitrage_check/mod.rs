pub mod dammv2_checker;
pub mod dlmm_checker;
pub mod pump_checker;

use pinocchio::{account_info::AccountInfo, pubkey::Pubkey};
use crate::programs::meteora_dlmm::MeteoraDlmm;
use crate::programs::ProgramMeta;

// ─── Thresholds ───────────────────────────────────────────────────────────────

/// Minimum absolute profit in input-token raw units (e.g. lamports) to surface
/// an opportunity.  Set above typical base-fee + priority-fee cost.
pub const MIN_PROFIT_LAMPORTS: u64 = 10_000;

/// Minimum profit as basis points (profit / amount_in × 10 000).
/// Filters arbs too thin to be worth execution risk.
pub const MIN_PROFIT_BPS: u32 = 1; // 0.01 %

// ─── Shared output type ───────────────────────────────────────────────────────

/// A confirmed arbitrage opportunity between two pools.
///
/// All values are in raw units of the input/output token (e.g. lamports for SOL).
/// `expected_profit` is integer-verified: `integer_out − optimal_amount_in > 0`.
#[derive(Debug, Clone)]
pub struct ArbOpportunity {
    /// Pool that executes the first swap (buy side).
    pub buy_pool_id: Pubkey,
    /// Pool that executes the second swap (sell side).
    pub sell_pool_id: Pubkey,
    /// Token we start and end with (e.g. WSOL).
    pub input_mint: Pubkey,
    /// Intermediate token exchanged between the two pools.
    pub middle_mint: Pubkey,
    /// Analytically-optimal (multi-bin) amount to send into pool 1.
    pub optimal_amount_in: u64,
    /// Integer-verified profit = final_out − optimal_amount_in.
    pub expected_profit: u64,
    /// Profit in basis points: (profit × 10 000) / amount_in.
    pub profit_bps: u32,
}

// ─── Shared DLMM integer verifier ─────────────────────────────────────────────

const DLMM_FEE_PREC: u64 = 1_000_000_000;

/// Walk DLMM bins from the active bin outward, consuming `amount_in` and
/// accumulating integer output.
///
/// # Bin walk behaviour
///
/// Walks up to `floor(profit_bps / bin_step_bps)` bins (capped at 70).
/// Each bin supplies (slope, gross_capacity) via `get_bin_segment`; the
/// function fills each bin in order until `amount_in` is exhausted.
///
/// # Fallback
///
/// If `get_bin_segment` returns `None` on the very first call (swap cache not
/// yet initialised), the function falls back to the single-bin integer formula
/// using the cached `lb_price` and `fee_numerator`.  This is conservative but
/// never incorrect.
///
/// # Returns
///
/// Raw output token amount (lamports / raw units).  Always ≥ 0.
pub(crate) fn dlmm_walk_int(
    dlmm: &MeteoraDlmm,
    accounts: &[AccountInfo],
    input_mint: Pubkey,
    amount_in: u64,
    profit_pct: f64,
) -> u64 {
    let bin_step_bps = dlmm.lb_pair_slim.bin_step.max(1) as u64;
    let profit_bps = (profit_pct * 10_000.0).max(0.0) as u64;
    let max_bins = if profit_bps > bin_step_bps {
        (profit_bps / bin_step_bps).min(70) as i32
    } else {
        0i32
    };

    // Total capacity across both bin arrays (populated at init time).
    // buy_max_in = max base (MEMECOIN) input (X→Y direction).
    // sell_max_in = max quote (SOL) input (Y→X direction).
    let total_cap = if input_mint == dlmm.base_token_pk {
        dlmm.buy_max_in
    } else {
        dlmm.sell_max_in
    };
    let mut remaining = amount_in.min(total_cap.max(1)); // avoid 0-cap edge case
    let mut total_out: u64 = 0;
    let mut walked_any = false;

    for bin_offset in 0..=max_bins {
        if remaining == 0 {
            break;
        }
        match dlmm.get_bin_segment(accounts, input_mint, bin_offset) {
            Ok(Some((slope, cap, fee_f))) if slope > 0.0 && cap > 0 && fee_f > 0.0 => {
                // `cap` from get_bin_segment is NET capacity (input after fee extraction).
                // `remaining` is a GROSS amount (the raw token amount sent in, before fee).
                // Convert NET → GROSS so the comparison and subtraction are in the same units.
                // gross_cap = net_cap / fee_factor  (fee_factor = 1 - fee_rate)
                let gross_cap = if fee_f < 1.0 { ((cap as f64) / fee_f) as u64 } else { cap };
                let bin_in = remaining.min(gross_cap);
                // slope = price × fee_factor: output per GROSS input unit.
                total_out = total_out.saturating_add((bin_in as f64 * slope) as u64);
                remaining -= bin_in;
                walked_any = true;
            }
            Ok(Some(_)) => continue, // empty bin — skip to next
            Ok(None) | Err(_) => break, // cache not ready or no more bins
        }
    }

    // ── Fallback: cache not yet initialised ──────────────────────────────
    // If no bin was walked (get_bin_segment returned None on offset=0),
    // use the single-bin integer formula with the active lb_price.
    if !walked_any {
        let fee_denom = (DLMM_FEE_PREC - dlmm.fee_numerator) as u128;
        let fee = ((amount_in as u128 * dlmm.fee_numerator as u128) + fee_denom - 1) / fee_denom;
        let in_eff = (amount_in as u128).saturating_sub(fee);

        total_out = if input_mint == dlmm.base_token_pk {
            // Sell base → quote:  out = in_eff × lb_price >> 64
            (in_eff.saturating_mul(dlmm.lb_price) >> 64).min(u64::MAX as u128) as u64
        } else {
            // Buy base with quote: out = in_eff × 2^64 / lb_price
            if dlmm.lb_price == 0 {
                0
            } else {
                const Q64: u128 = 1u128 << 64;
                let q = Q64 / dlmm.lb_price;
                let r = Q64 % dlmm.lb_price;
                (in_eff * q + in_eff * r / dlmm.lb_price).min(u64::MAX as u128) as u64
            }
        };
    }

    total_out
}
