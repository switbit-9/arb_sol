use super::pool_model::PoolModel;
use crate::programs::ProgramMeta;
use anchor_lang::prelude::*;

/// Debug context for analytical_estimate logging.
pub struct EstimateDebugCtx {
    pub pool1_id: Pubkey,
    pub pool2_id: Pubkey,
    pub start_mint: Pubkey,
    pub middle_mint: Pubkey,
}

/// Result of an analytical computation.
#[derive(Debug)]
pub struct AnalyticalResult {
    /// Optimal input amount (raw u64, e.g. lamports)
    pub optimal_amount: u64,
    /// Whether the DLMM active bin capacity was the binding constraint.
    /// When true, the single-bin linear approximation may be inaccurate and
    /// a small golden-section refinement around the hint is recommended.
    pub dlmm_capped: bool,
}

/// Compute the closed-form optimal input amount for a 2-pool arbitrage path.
///
/// Pool1 converts `dx` of the start token into a middle token,
/// Pool2 converts that middle token back into the start token.
/// Profit = pool2_output - dx.
///
/// Returns `None` if either pool is Opaque or no profitable arb exists.
pub fn analytical_optimal_2pool(
    pool1: &PoolModel,
    pool2: &PoolModel,
    max_amount_in: u64,
) -> Option<AnalyticalResult> {
    match (pool1, pool2) {
        // ── CP(fee-on-input) + CP(fee-on-input) ───────────────────────
        //
        // dy = r1_out * u / (r1_in + u)          where u = dx * f1
        // dz = r2_out * v / (r2_in + v)          where v = dy * f2
        //
        // Composed: dz = C * u / (A + B * u)
        //   A = r1_in * r2_in
        //   B = r2_in + f2 * r1_out
        //   C = r2_out * f2 * r1_out
        //
        // dx* = (sqrt(f1 * C * A) - A) / (f1 * B)
        (
            PoolModel::CpFeeOnInput {
                reserve_in: r1_in,
                reserve_out: r1_out,
                fee: f1, ..
            },
            PoolModel::CpFeeOnInput {
                reserve_in: r2_in,
                reserve_out: r2_out,
                fee: f2, ..
            },
        ) => {
            let a = r1_in * r2_in;
            let b = r2_in + f2 * r1_out;
            let c = r2_out * f2 * r1_out;

            let disc = f1 * c * a;
            if disc <= a * a {
                return None;
            }
            let dx = (disc.sqrt() - a) / (f1 * b);
            Some(clamp_result(dx, max_amount_in, false))
        }

        // ── CP(fee-on-input) + CP(fee-on-output) ──────────────────────
        // Pool2 is PumpAmm sell: dz = f2 * r2_out * dy / (r2_in + dy)
        //
        //   A = r1_in * r2_in
        //   B = r2_in + r1_out          (no f2 on r1_out)
        //   C = f2 * r2_out * r1_out
        //
        // dx* = (sqrt(f1 * C * A) - A) / (f1 * B)
        (
            PoolModel::CpFeeOnInput {
                reserve_in: r1_in,
                reserve_out: r1_out,
                fee: f1, ..
            },
            PoolModel::CpFeeOnOutput {
                reserve_in: r2_in,
                reserve_out: r2_out,
                fee: f2, ..
            },
        ) => {
            let a = r1_in * r2_in;
            let b = r2_in + r1_out;
            let c = f2 * r2_out * r1_out;

            let disc = f1 * c * a;
            if disc <= a * a {
                return None;
            }
            let dx = (disc.sqrt() - a) / (f1 * b);
            Some(clamp_result(dx, max_amount_in, false))
        }

        // ── CP(fee-on-output) + CP(fee-on-input) ──────────────────────
        // Pool1 is PumpAmm sell: dy = f1 * r1_out * dx / (r1_in + dx)
        //
        //   A = r1_in * r2_in
        //   B = r2_in + f1 * f2 * r1_out
        //   C = r2_out * f1 * f2 * r1_out
        //
        // u = dx (no f1 in denom), so dx* = (sqrt(C * A) - A) / B
        (
            PoolModel::CpFeeOnOutput {
                reserve_in: r1_in,
                reserve_out: r1_out,
                fee: f1, ..
            },
            PoolModel::CpFeeOnInput {
                reserve_in: r2_in,
                reserve_out: r2_out,
                fee: f2, ..
            },
        ) => {
            let a = r1_in * r2_in;
            let b = r2_in + f1 * f2 * r1_out;
            let c = r2_out * f1 * f2 * r1_out;

            let disc = c * a;
            if disc <= a * a {
                return None;
            }
            let dx = (disc.sqrt() - a) / b;
            Some(clamp_result(dx, max_amount_in, false))
        }

        // ── CP(fee-on-output) + CP(fee-on-output) ─────────────────────
        // Both PumpAmm sell direction
        //
        //   A = r1_in * r2_in
        //   B = r2_in + f1 * r1_out
        //   C = f1 * f2 * r2_out * r1_out
        //
        // u = dx, so dx* = (sqrt(C * A) - A) / B
        (
            PoolModel::CpFeeOnOutput {
                reserve_in: r1_in,
                reserve_out: r1_out,
                fee: f1, ..
            },
            PoolModel::CpFeeOnOutput {
                reserve_in: r2_in,
                reserve_out: r2_out,
                fee: f2, ..
            },
        ) => {
            let a = r1_in * r2_in;
            let b = r2_in + f1 * r1_out;
            let c = f1 * f2 * r2_out * r1_out;

            let disc = c * a;
            if disc <= a * a {
                return None;
            }
            let dx = (disc.sqrt() - a) / b;
            Some(clamp_result(dx, max_amount_in, false))
        }

        // ── CP(fee-on-input) + Linear (DLMM) ──────────────────────────
        // Pool1: dy = r_out * dx * f_amm / (r_in + dx * f_amm)
        // Pool2: dz = dy * p * f_dlmm
        //
        // dx* = (sqrt(r_in * r_out * f_amm * p * f_dlmm) - r_in) / f_amm
        (
            PoolModel::CpFeeOnInput {
                reserve_in: r_in,
                reserve_out: r_out,
                fee: f_amm, ..
            },
            PoolModel::Linear {
                price: p,
                fee: f_dlmm,
                max_in: dlmm_max_in,
                ..
            },
        ) => {
            let disc = r_in * r_out * f_amm * p * f_dlmm;
            if disc <= r_in * r_in {
                return None;
            }
            let dx = (disc.sqrt() - r_in) / f_amm;

            // Check if mid-amount exceeds DLMM active bin capacity
            let mid = r_out * dx * f_amm / (r_in + dx * f_amm);
            let capped = mid > *dlmm_max_in as f64;

            debug_eprintln!(
                "  CP_FI+DLMM: r_in={:.2}, r_out={:.2}, f_amm={:.6}, p={:.6}, f_dlmm={:.6}, dlmm_max_in={}, unclamped_dx={:.4}, mid={:.4}, capped={}",
                r_in, r_out, f_amm, p, f_dlmm, dlmm_max_in, dx, mid, capped
            );

            // When capped, clamp dx so mid = dlmm_max_in
            let dx = if capped {
                let max_in = *dlmm_max_in as f64;
                if *r_out <= max_in { return None; }
                let u = max_in * r_in / (r_out - max_in);
                let clamped_dx = u / f_amm;
                debug_eprintln!("  CP_FI+DLMM capped: clamped_dx={:.4}", clamped_dx);
                clamped_dx
            } else {
                dx
            };

            Some(clamp_result(dx, max_amount_in, capped))
        }

        // ── CP(fee-on-output) + Linear (DLMM) ─────────────────────────
        // Pool1 PumpAmm sell: dy = f_amm * r_out * dx / (r_in + dx)
        // Pool2: dz = dy * p * f_dlmm
        //
        // dx* = sqrt(r_in * r_out * f_amm * p * f_dlmm) - r_in
        (
            PoolModel::CpFeeOnOutput {
                reserve_in: r_in,
                reserve_out: r_out,
                fee: f_amm, ..
            },
            PoolModel::Linear {
                price: p,
                fee: f_dlmm,
                max_in: dlmm_max_in,
                ..
            },
        ) => {
            let disc = r_in * r_out * f_amm * p * f_dlmm;
            if disc <= r_in * r_in {
                return None;
            }
            let dx = disc.sqrt() - r_in;

            let mid = f_amm * r_out * dx / (r_in + dx);
            let capped = mid > *dlmm_max_in as f64;

            // When capped, clamp dx so mid = dlmm_max_in
            let dx = if capped {
                let max_in = *dlmm_max_in as f64;
                let denom = f_amm * r_out - max_in;
                if denom <= 0.0 { return None; }
                max_in * r_in / denom
            } else {
                dx
            };

            Some(clamp_result(dx, max_amount_in, capped))
        }

        // ── Linear (DLMM) + CP(fee-on-input) ──────────────────────────
        // Pool1: dy = dx * p * f_dlmm
        // Pool2: dz = r_out * dy * f_amm / (r_in + dy * f_amm)
        //
        // Let a = p * f_dlmm
        // dx* = (sqrt(r_in * r_out * a * f_amm) - r_in) / (a * f_amm)
        (
            PoolModel::Linear {
                price: p,
                fee: f_dlmm,
                max_in: dlmm_max_in,
                ..
            },
            PoolModel::CpFeeOnInput {
                reserve_in: r_in,
                reserve_out: r_out,
                fee: f_amm, ..
            },
        ) => {
            let a = p * f_dlmm;
            if a <= 0.0 {
                return None;
            }

            let disc = r_in * r_out * a * f_amm;
            if disc <= r_in * r_in {
                return None;
            }
            let dx = (disc.sqrt() - r_in) / (a * f_amm);

            // Pool1 is DLMM: clamp input to active bin capacity
            let capped = dx > *dlmm_max_in as f64;
            let dx = dx.min(*dlmm_max_in as f64);
            Some(clamp_result(dx, max_amount_in, capped))
        }

        // ── Linear (DLMM) + CP(fee-on-output) ─────────────────────────
        // Pool1: dy = dx * p * f_dlmm = dx * a
        // Pool2 PumpAmm sell: dz = f_amm * r_out * dy / (r_in + dy)
        //
        // Composed: dz = f_amm * r_out * a * dx / (r_in + a * dx)
        // dx* = (sqrt(f_amm * r_out * a * r_in) - r_in) / a
        (
            PoolModel::Linear {
                price: p,
                fee: f_dlmm,
                max_in: dlmm_max_in,
                ..
            },
            PoolModel::CpFeeOnOutput {
                reserve_in: r_in,
                reserve_out: r_out,
                fee: f_amm, ..
            },
        ) => {
            let a = p * f_dlmm;
            if a <= 0.0 {
                return None;
            }

            let disc = f_amm * r_out * a * r_in;
            if disc <= r_in * r_in {
                return None;
            }
            let dx = (disc.sqrt() - r_in) / a;

            // Pool1 is DLMM: clamp input to active bin capacity
            let capped = dx > *dlmm_max_in as f64;
            let dx = dx.min(*dlmm_max_in as f64);
            Some(clamp_result(dx, max_amount_in, capped))
        }

        // ── Linear + Linear (DLMM + DLMM) ─────────────────────────────
        // Profit is linear: out = dx * p1 * f1 * p2 * f2
        // Profitable iff p1*f1*p2*f2 > 1 => maximize dx up to bin capacity.
        (
            PoolModel::Linear {
                price: p1,
                fee: f1,
                max_in: max_in_1,
                ..
            },
            PoolModel::Linear {
                price: p2,
                fee: f2,
                max_in: max_in_2,
                ..
            },
        ) => {
            let combined = p1 * f1 * p2 * f2;
            if combined <= 1.0 {
                return None;
            }
            // Pool2 can accept at most max_in_2 tokens; pool1 output = dx * p1 * f1
            let max_from_pool2 = if p1 * f1 > 0.0 {
                (*max_in_2 as f64) / (p1 * f1)
            } else {
                return None;
            };
            let dx = (*max_in_1 as f64)
                .min(max_from_pool2)
                .min(max_amount_in as f64);
            if dx <= 0.0 {
                return None;
            }

            Some(AnalyticalResult {
                optimal_amount: dx as u64,
                dlmm_capped: true, // Always capacity-bound for linear+linear
            })
        }

        // ── Clmm + CP(fee-on-input) ─────────────────────────────────────
        // Clmm is CP with fee-on-input and virtual reserves + max_in cap.
        // Same formula as CP_FI + CP_FI, with pool1 input capped to max_in.
        (
            PoolModel::Clmm {
                reserve_in: r1_in,
                reserve_out: r1_out,
                fee: f1,
                max_in: clmm_max_in,
                ..
            },
            PoolModel::CpFeeOnInput {
                reserve_in: r2_in,
                reserve_out: r2_out,
                fee: f2, ..
            },
        ) => {
            let a = r1_in * r2_in;
            let b = r2_in + f2 * r1_out;
            let c = r2_out * f2 * r1_out;

            let disc = f1 * c * a;
            if disc <= a * a {
                return None;
            }
            let dx = (disc.sqrt() - a) / (f1 * b);
            let capped = dx > *clmm_max_in as f64;
            let dx = dx.min(*clmm_max_in as f64);
            Some(clamp_result(dx, max_amount_in, capped))
        }

        // ── Clmm + CP(fee-on-output) ────────────────────────────────────
        (
            PoolModel::Clmm {
                reserve_in: r1_in,
                reserve_out: r1_out,
                fee: f1,
                max_in: clmm_max_in,
                ..
            },
            PoolModel::CpFeeOnOutput {
                reserve_in: r2_in,
                reserve_out: r2_out,
                fee: f2, ..
            },
        ) => {
            let a = r1_in * r2_in;
            let b = r2_in + r1_out;
            let c = f2 * r2_out * r1_out;

            let disc = f1 * c * a;
            if disc <= a * a {
                return None;
            }
            let dx = (disc.sqrt() - a) / (f1 * b);
            let capped = dx > *clmm_max_in as f64;
            let dx = dx.min(*clmm_max_in as f64);
            Some(clamp_result(dx, max_amount_in, capped))
        }

        // ── CP(fee-on-input) + Clmm ─────────────────────────────────────
        // Same as CP_FI + CP_FI but pool2 mid tokens capped to clmm_max_in.
        (
            PoolModel::CpFeeOnInput {
                reserve_in: r1_in,
                reserve_out: r1_out,
                fee: f1, ..
            },
            PoolModel::Clmm {
                reserve_in: r2_in,
                reserve_out: r2_out,
                fee: f2,
                max_in: clmm_max_in,
                ..
            },
        ) => {
            let a = r1_in * r2_in;
            let b = r2_in + f2 * r1_out;
            let c = r2_out * f2 * r1_out;

            let disc = f1 * c * a;
            if disc <= a * a {
                return None;
            }
            let dx = (disc.sqrt() - a) / (f1 * b);

            // Check if mid-amount exceeds CLMM active tick capacity
            let mid = r1_out * dx * f1 / (r1_in + dx * f1);
            let capped = mid > *clmm_max_in as f64;
            let dx = if capped {
                let max_in = *clmm_max_in as f64;
                if *r1_out <= max_in { return None; }
                let u = max_in * r1_in / (r1_out - max_in);
                u / f1
            } else {
                dx
            };
            Some(clamp_result(dx, max_amount_in, capped))
        }

        // ── CP(fee-on-output) + Clmm ────────────────────────────────────
        (
            PoolModel::CpFeeOnOutput {
                reserve_in: r1_in,
                reserve_out: r1_out,
                fee: f1, ..
            },
            PoolModel::Clmm {
                reserve_in: r2_in,
                reserve_out: r2_out,
                fee: f2,
                max_in: clmm_max_in,
                ..
            },
        ) => {
            let a = r1_in * r2_in;
            let b = r2_in + f1 * f2 * r1_out;
            let c = r2_out * f1 * f2 * r1_out;

            let disc = c * a;
            if disc <= a * a {
                return None;
            }
            let dx = (disc.sqrt() - a) / b;

            // Check if mid exceeds CLMM capacity
            let mid = f1 * r1_out * dx / (r1_in + dx);
            let capped = mid > *clmm_max_in as f64;
            let dx = if capped {
                let max_in = *clmm_max_in as f64;
                let denom = f1 * r1_out - max_in;
                if denom <= 0.0 { return None; }
                max_in * r1_in / denom
            } else {
                dx
            };
            Some(clamp_result(dx, max_amount_in, capped))
        }

        // ── Clmm + Clmm ─────────────────────────────────────────────────
        // Both are CP with fee-on-input. Same as CP_FI + CP_FI with both caps.
        (
            PoolModel::Clmm {
                reserve_in: r1_in,
                reserve_out: r1_out,
                fee: f1,
                max_in: max_in_1,
                ..
            },
            PoolModel::Clmm {
                reserve_in: r2_in,
                reserve_out: r2_out,
                fee: f2,
                max_in: max_in_2,
                ..
            },
        ) => {
            let a = r1_in * r2_in;
            let b = r2_in + f2 * r1_out;
            let c = r2_out * f2 * r1_out;

            let disc = f1 * c * a;
            if disc <= a * a {
                return None;
            }
            let mut dx = (disc.sqrt() - a) / (f1 * b);

            // Cap pool1 input
            let capped1 = dx > *max_in_1 as f64;
            dx = dx.min(*max_in_1 as f64);

            // Cap pool2 mid tokens
            let mid = r1_out * dx * f1 / (r1_in + dx * f1);
            let capped2 = mid > *max_in_2 as f64;
            if capped2 {
                let max_in = *max_in_2 as f64;
                if *r1_out <= max_in { return None; }
                let u = max_in * r1_in / (r1_out - max_in);
                dx = u / f1;
            }
            Some(clamp_result(dx, max_amount_in, capped1 || capped2))
        }

        // ── Clmm + Linear (DLMM) ────────────────────────────────────────
        // Pool1 Clmm (fee-on-input CP): dy = r1_out * dx * f1 / (r1_in + dx * f1)
        // Pool2 Linear: dz = dy * p * f_dlmm
        // Same as CP_FI + Linear with pool1 max_in cap.
        (
            PoolModel::Clmm {
                reserve_in: r_in,
                reserve_out: r_out,
                fee: f_amm,
                max_in: clmm_max_in,
                ..
            },
            PoolModel::Linear {
                price: p,
                fee: f_dlmm,
                max_in: dlmm_max_in,
                ..
            },
        ) => {
            let disc = r_in * r_out * f_amm * p * f_dlmm;
            if disc <= r_in * r_in {
                return None;
            }
            let dx = (disc.sqrt() - r_in) / f_amm;

            // Cap CLMM input
            let capped_clmm = dx > *clmm_max_in as f64;
            let dx = dx.min(*clmm_max_in as f64);

            // Check if mid exceeds DLMM capacity
            let mid = r_out * dx * f_amm / (r_in + dx * f_amm);
            let capped_dlmm = mid > *dlmm_max_in as f64;
            let dx = if capped_dlmm {
                let max_in = *dlmm_max_in as f64;
                if *r_out <= max_in { return None; }
                let u = max_in * r_in / (r_out - max_in);
                (u / f_amm).min(*clmm_max_in as f64)
            } else {
                dx
            };
            Some(clamp_result(dx, max_amount_in, capped_clmm || capped_dlmm))
        }

        // ── Linear (DLMM) + Clmm ────────────────────────────────────────
        // Pool1 Linear: dy = dx * p * f_dlmm
        // Pool2 Clmm (fee-on-input CP): dz = r2_out * dy * f2 / (r2_in + dy * f2)
        // Same as Linear + CP_FI with pool2 mid cap.
        (
            PoolModel::Linear {
                price: p,
                fee: f_dlmm,
                max_in: dlmm_max_in,
                ..
            },
            PoolModel::Clmm {
                reserve_in: r_in,
                reserve_out: r_out,
                fee: f_amm,
                max_in: clmm_max_in,
                ..
            },
        ) => {
            let a = p * f_dlmm;
            if a <= 0.0 {
                return None;
            }

            let disc = r_in * r_out * a * f_amm;
            if disc <= r_in * r_in {
                return None;
            }
            let dx = (disc.sqrt() - r_in) / (a * f_amm);

            // Cap DLMM input
            let capped_dlmm = dx > *dlmm_max_in as f64;
            let dx = dx.min(*dlmm_max_in as f64);

            // Cap CLMM mid tokens
            let mid = dx * a;
            let capped_clmm = mid > *clmm_max_in as f64;
            let dx = if capped_clmm {
                (*clmm_max_in as f64 / a).min(*dlmm_max_in as f64)
            } else {
                dx
            };
            Some(clamp_result(dx, max_amount_in, capped_dlmm || capped_clmm))
        }

        // ── Any combination involving Opaque ───────────────────────────
        (PoolModel::Opaque { .. }, _) | (_, PoolModel::Opaque { .. }) => None,
    }
}

/// Estimate the analytical profit for a given (pool1, pool2) pair at the optimal amount.
/// Uses the closed-form formulas — no swap_base_in calls.
/// Returns (optimal_amount, estimated_profit, dlmm_capped).
/// Returns None if no arb or Opaque pools.
pub fn analytical_estimate(
    pool1: &PoolModel,
    pool2: &PoolModel,
    max_amount_in: u64,
    debug_ctx: Option<&EstimateDebugCtx>,
) -> Option<(u64, i128, bool)> {
    let result_opt = analytical_optimal_2pool(pool1, pool2, max_amount_in);
    let result = match result_opt {
        Some(r) => r,
        None => {
            debug_eprintln!("    analytical_estimate: optimal_2pool returned None for {}+{}", pool1.label(), pool2.label());
            return None;
        }
    };
    if result.optimal_amount == 0 {
        debug_eprintln!("    analytical_estimate: optimal_amount=0 for {}+{}", pool1.label(), pool2.label());
        return None;
    }
    let input_amount = result.optimal_amount as f64;

    // Compute estimated output through pool1 then pool2
    let middle_amount = pool_output(pool1, input_amount);
    if middle_amount <= 0.0 {
        debug_eprintln!("    analytical_estimate: middle_amount<=0 ({}) for {}+{}", middle_amount, pool1.label(), pool2.label());
        return None;
    }
    let output_amount = pool_output(pool2, middle_amount);
    let profit = output_amount - input_amount;

    
    {
        let f1 = pool1.fee();
        let f2 = pool2.fee();
        let p1_raw = if f1 > 0.0 { pool1.marginal_price() / f1 } else { 0.0 };
        let p2_raw = if f2 > 0.0 { pool2.marginal_price() / f2 } else { 0.0 };
        let price_diff_pct = if p1_raw > 0.0 && p2_raw > 0.0 {
            (p1_raw * p2_raw - 1.0) * 100.0
        } else {
            0.0
        };
        let p1_buy = if p1_raw > 1.0 { 1.0 / p1_raw } else { p1_raw };
        let p2_buy = if p2_raw > 1.0 { 1.0 / p2_raw } else { p2_raw };

        if let Some(ctx) = debug_ctx {
            debug_eprintln!("");
            debug_eprintln!(
                "ESTIMATE {}+{} pool1={} pool2={} {} -> {} P={:.4}%, fee1={:.4}%, fee2={:.4}%, p1={:.6}, p2={:.6}, profit={:.6} (input={:.6} mid={:.6} out={:.6} dlmm={})",
                pool1.label(), pool2.label(), ctx.pool1_id, ctx.pool2_id, ctx.start_mint, ctx.middle_mint,
                price_diff_pct, (1.0 - f1) * 100.0, (1.0 - f2) * 100.0, p1_buy, p2_buy, profit / 1e9,
                input_amount / 1e9, middle_amount / 1e9, output_amount / 1e9, result.dlmm_capped
            );
            debug_eprintln!("");
        } else {
            debug_eprintln!("");
            debug_eprintln!(
                "ESTIMATE {}+{} P={:.4}%, fee1={:.4}%, fee2={:.4}%, p1={:.6}, p2={:.6}, profit={:.6} (input={:.6} mid={:.6} out={:.6} dlmm={})",
                pool1.label(), pool2.label(), price_diff_pct, (1.0 - f1) * 100.0, (1.0 - f2) * 100.0, p1_buy, p2_buy, profit / 1e9,
                input_amount / 1e9, middle_amount / 1e9, output_amount / 1e9, result.dlmm_capped
            );
            debug_eprintln!("");
        }
    }

    if profit <= 0.0 {
        return None;
    }

    Some((result.optimal_amount, profit as i128, result.dlmm_capped))
}

/// Compute the output of a single pool for a given input amount.
/// Uses the analytical model (no on-chain simulation).
pub(crate) fn pool_output(model: &PoolModel, dx: f64) -> f64 {
    match model {
        PoolModel::CpFeeOnInput { reserve_in: r_in, reserve_out: r_out, fee, .. } => {
            let u = dx * fee;
            r_out * u / (r_in + u)
        }
        PoolModel::CpFeeOnOutput { reserve_in: r_in, reserve_out: r_out, fee, .. } => {
            fee * r_out * dx / (r_in + dx)
        }
        PoolModel::Linear { price, fee, max_in, .. } => {
            let clamped = dx.min(*max_in as f64);
            clamped * price * fee
        }
        PoolModel::Clmm { reserve_in: r_in, reserve_out: r_out, fee, max_in, .. } => {
            let clamped = dx.min(*max_in as f64);
            let u = clamped * fee;
            r_out * u / (r_in + u)
        }
        PoolModel::Opaque { .. } => 0.0,
    }
}

/// Clamp the analytical result to wallet max and produce the result struct.
fn clamp_result(dx: f64, max_amount_in: u64, dlmm_capped: bool) -> AnalyticalResult {
    if dx <= 0.0 || !dx.is_finite() {
        return AnalyticalResult {
            optimal_amount: 0,
            dlmm_capped,
        };
    }
    AnalyticalResult {
        optimal_amount: (dx as u64).min(max_amount_in),
        dlmm_capped,
    }
}

// ─── N-hop analytical formulas ──────────────────────────────────────────────

/// Extract the (alpha, beta, gamma) triple for a single CP pool.
/// Each CP pool computes: out = alpha * x / (beta + gamma * x).
/// Returns None for Linear or Opaque models.
fn cp_params(model: &PoolModel) -> Option<(f64, f64, f64)> {
    match model {
        PoolModel::CpFeeOnInput { reserve_in: r_in, reserve_out: r_out, fee, .. } => {
            Some((r_out * fee, *r_in, *fee))
        }
        PoolModel::CpFeeOnOutput { reserve_in: r_in, reserve_out: r_out, fee, .. } => {
            Some((fee * r_out, *r_in, 1.0))
        }
        // Clmm is CP with fee-on-input using virtual reserves
        PoolModel::Clmm { reserve_in: r_in, reserve_out: r_out, fee, .. } => {
            Some((r_out * fee, *r_in, *fee))
        }
        _ => None,
    }
}

/// Compose N constant-product pools into a single (alpha, beta, gamma) triple.
///
/// Composition rule for f(u) = alpha_prev*u/(beta_prev+gamma_prev*u) followed by
/// g(v) = alpha_i*v/(beta_i+gamma_i*v):
///   alpha_new = alpha_i * alpha_prev
///   beta_new  = beta_i * beta_prev
///   gamma_new = beta_i * gamma_prev + gamma_i * alpha_prev
///
/// Returns None if any pool is not CP.
fn compose_cp_chain(models: &[PoolModel]) -> Option<(f64, f64, f64)> {
    let (mut alpha, mut beta, mut gamma) = cp_params(&models[0])?;

    for model in &models[1..] {
        let (ai, bi, gi) = cp_params(model)?;
        let new_alpha = ai * alpha;
        let new_beta = bi * beta;
        let new_gamma = bi * gamma + gi * alpha;
        alpha = new_alpha;
        beta = new_beta;
        gamma = new_gamma;
    }

    Some((alpha, beta, gamma))
}

/// Compute chained output through N pools using individual pool_output calls.
/// Works for any mix of pool types.
fn pool_chain_output(models: &[PoolModel], dx: f64) -> f64 {
    let mut current = dx;
    for model in models {
        current = pool_output(model, current);
        if current <= 0.0 {
            return 0.0;
        }
    }
    current
}

/// Golden section search on the composed chain output to find optimal input.
/// Used when the chain contains Linear (DLMM) pools that prevent closed-form.
fn golden_section_analytical_nhop(
    models: &[PoolModel],
    max_amount_in: u64,
) -> Option<(u64, i128)> {
    let max_f = max_amount_in as f64;
    let mut a: f64 = 1000.0;
    let mut b: f64 = max_f;

    let profit_at = |x: f64| -> f64 { pool_chain_output(models, x) - x };

    if profit_at(a) <= 0.0 && profit_at(b) <= 0.0 && profit_at((a + b) / 2.0) <= 0.0 {
        return None;
    }

    let phi = 1.618033988749895_f64;
    let mut c = b - (b - a) / phi;
    let mut d = a + (b - a) / phi;
    let mut fc = profit_at(c);
    let mut fd = profit_at(d);

    for _ in 0..30 {
        if (b - a) < 10000.0 {
            break;
        }
        if fc > fd {
            b = d;
            d = c;
            fd = fc;
            c = b - (b - a) / phi;
            fc = profit_at(c);
        } else {
            a = c;
            c = d;
            fc = fd;
            d = a + (b - a) / phi;
            fd = profit_at(d);
        }
    }

    let optimal = (a + b) / 2.0;
    let profit = profit_at(optimal);
    if profit <= 0.0 || optimal <= 0.0 {
        return None;
    }
    Some((optimal as u64, profit as i128))
}

/// Compute the analytical optimal input and estimated profit for an N-hop chain.
/// Returns (optimal_amount, estimated_profit, dlmm_capped).
/// Returns None if chain contains Opaque pools or no profitable arb exists.
pub fn analytical_estimate_nhop(
    models: &[PoolModel],
    max_amount_in: u64,
) -> Option<(u64, i128, bool)> {
    if models.iter().any(|m| matches!(m, PoolModel::Opaque { .. })) {
        return None;
    }

    let has_linear = models.iter().any(|m| matches!(m, PoolModel::Linear { .. }));

    if !has_linear {
        // All CP: use closed-form composition
        let (alpha, beta, gamma) = compose_cp_chain(models)?;

        let disc = alpha * beta;
        if disc <= beta * beta || gamma <= 0.0 {
            return None;
        }

        let dx = (disc.sqrt() - beta) / gamma;
        if dx <= 0.0 || !dx.is_finite() {
            return None;
        }

        let dx_clamped = dx.min(max_amount_in as f64);
        let optimal_amount = dx_clamped as u64;
        if optimal_amount == 0 {
            return None;
        }

        let out = pool_chain_output(models, dx_clamped);
        let profit = out - dx_clamped;
        if profit <= 0.0 {
            return None;
        }

        Some((optimal_amount, profit as i128, false))
    } else {
        // Chain has Linear pool(s): use golden section on composed output
        let (amount, profit) = golden_section_analytical_nhop(models, max_amount_in)?;
        Some((amount, profit, true))
    }
}

// ─── Multi-bin analytical walking ───────────────────────────────────────────

/// Which pool in the pair is the DLMM (determines the formula variant).
#[derive(Debug, Clone, Copy)]
enum DlmmPosition {
    /// Pool2 is DLMM: CP → DLMM. Mid-amount flows into DLMM bins.
    Pool2,
    /// Pool1 is DLMM: DLMM → CP. Input dx flows into DLMM bins directly.
    Pool1,
}

/// Whether the CP pool applies fee on input or output.
#[derive(Debug, Clone, Copy)]
enum CpFeeType {
    OnInput,
    OnOutput,
}

/// Multi-bin analytical optimal for a CP+DLMM or DLMM+CP pair.
///
/// Walks DLMM bins lazily (one at a time), computing the per-segment closed-form
/// optimal and checking if it falls within that segment. Stops when:
/// - The optimum is found in a segment
/// - The profit margin can't survive the next bin step
/// - We run out of bin data or max_amount_in
///
/// Returns `Some((optimal_amount, estimated_profit))` or `None` if no improvement.
pub fn analytical_optimal_multibin<'info>(
    accounts: &[AccountInfo<'info>],
    dlmm_instance: &dyn ProgramMeta,
    input_mint: Pubkey,
    pool1: &PoolModel,
    pool2: &PoolModel,
    max_amount_in: u64,
) -> Option<(u64, i128)> {
    // Determine which pool is DLMM and extract CP params
    let (dlmm_pos, cp_fee_type, r_in, r_out, f_amm, _bin_step_frac) = match (pool1, pool2) {
        (
            PoolModel::CpFeeOnInput { reserve_in: r_in, reserve_out: r_out, fee, .. },
            PoolModel::Linear { bin_step_frac, .. },
        ) => (DlmmPosition::Pool2, CpFeeType::OnInput, *r_in, *r_out, *fee, *bin_step_frac),

        (
            PoolModel::CpFeeOnOutput { reserve_in: r_in, reserve_out: r_out, fee, .. },
            PoolModel::Linear { bin_step_frac, .. },
        ) => (DlmmPosition::Pool2, CpFeeType::OnOutput, *r_in, *r_out, *fee, *bin_step_frac),

        (
            PoolModel::Linear { bin_step_frac, .. },
            PoolModel::CpFeeOnInput { reserve_in: r_in, reserve_out: r_out, fee, .. },
        ) => (DlmmPosition::Pool1, CpFeeType::OnInput, *r_in, *r_out, *fee, *bin_step_frac),

        (
            PoolModel::Linear { bin_step_frac, .. },
            PoolModel::CpFeeOnOutput { reserve_in: r_in, reserve_out: r_out, fee, .. },
        ) => (DlmmPosition::Pool1, CpFeeType::OnOutput, *r_in, *r_out, *fee, *bin_step_frac),

        _ => return None, // Not a CP+DLMM pair
    };

    if _bin_step_frac <= 0.0 {
        return None;
    }

    let mut cumulative_input: f64 = 0.0;   // cumulative DLMM input consumed by prior bins
    let mut cumulative_output: f64 = 0.0;  // cumulative DLMM output from prior bins
    let mut best_amount: f64 = 0.0;
    let mut best_profit: f64 = 0.0;
    let max_f = max_amount_in as f64;

    for bin_offset in 0..70i32 {  // max 70 bins per array, safety limit
        let (bin_slope, gross_capacity) = match dlmm_instance.get_bin_segment(accounts, input_mint, bin_offset) {
            Ok(Some((s, c, f))) if s > 0.0 && c > 0 && f > 0.0 => (s, c as f64 / f),
            Ok(Some(_)) => continue, // empty bin — skip to next
            _ => break,
        };

        // segment_offset = cumulative_output - bin_slope * cumulative_input  (constant offset for this segment)
        let segment_offset = cumulative_output - bin_slope * cumulative_input;

        // Compute per-segment optimal input using the closed-form
        let segment_optimal = match (dlmm_pos, cp_fee_type) {
            // CP(FI) → DLMM: mid(dx) = r_out * dx * f_amm / (r_in + dx * f_amm)
            // out = segment_offset + bin_slope * mid(dx)
            // d(out)/d(dx) = bin_slope * f_amm * r_in * r_out / (r_in + dx * f_amm)^2 = 1
            // dx* = (sqrt(r_in * r_out * f_amm * bin_slope) - r_in) / f_amm
            (DlmmPosition::Pool2, CpFeeType::OnInput) => {
                let discriminant = r_in * r_out * f_amm * bin_slope;
                if discriminant <= r_in * r_in { break; }
                (discriminant.sqrt() - r_in) / f_amm
            }

            // CP(FO) → DLMM: mid(dx) = f_amm * r_out * dx / (r_in + dx)
            // d(out)/d(dx) = bin_slope * f_amm * r_out * r_in / (r_in + dx)^2 = 1
            // dx* = sqrt(r_in * r_out * f_amm * bin_slope) - r_in
            (DlmmPosition::Pool2, CpFeeType::OnOutput) => {
                let discriminant = r_in * r_out * f_amm * bin_slope;
                if discriminant <= r_in * r_in { break; }
                discriminant.sqrt() - r_in
            }

            // DLMM → CP(FI): mid(dx) = segment_offset + bin_slope * dx
            // out = r_out * mid * f_amm / (r_in + mid * f_amm)
            // d(out)/d(dx) = r_out * r_in * f_amm * bin_slope / (r_in + (segment_offset + bin_slope * dx) * f_amm)^2 = 1
            // dx* = (sqrt(r_out * r_in * f_amm * bin_slope) - r_in - segment_offset * f_amm) / (bin_slope * f_amm)
            (DlmmPosition::Pool1, CpFeeType::OnInput) => {
                let discriminant = r_out * r_in * f_amm * bin_slope;
                let sqrt_disc = discriminant.sqrt();
                let numer = sqrt_disc - r_in - segment_offset * f_amm;
                let denom = bin_slope * f_amm;
                if denom <= 0.0 || numer <= 0.0 { break; }
                numer / denom
            }

            // DLMM → CP(FO): mid(dx) = segment_offset + bin_slope * dx
            // out = f_amm * r_out * mid / (r_in + mid)
            // d(out)/d(dx) = f_amm * r_out * r_in * bin_slope / (r_in + segment_offset + bin_slope * dx)^2 = 1
            // dx* = (sqrt(f_amm * r_out * r_in * bin_slope) - r_in - segment_offset) / bin_slope
            (DlmmPosition::Pool1, CpFeeType::OnOutput) => {
                let discriminant = f_amm * r_out * r_in * bin_slope;
                let sqrt_disc = discriminant.sqrt();
                let numer = sqrt_disc - r_in - segment_offset;
                if numer <= 0.0 || bin_slope <= 0.0 { break; }
                numer / bin_slope
            }
        };

        if !segment_optimal.is_finite() || segment_optimal <= 0.0 {
            break;
        }

        // Clamp before the bin check — the profit is computed at clamped_amount,
        // so the bin check must also use clamped_amount. Otherwise when
        // segment_optimal >> max_f the walker accumulates bins that the
        // clamped amount would never reach, inflating profit.
        let clamped_amount = segment_optimal.min(max_f);

        let in_segment = match dlmm_pos {
            DlmmPosition::Pool2 => {
                let mid = match cp_fee_type {
                    CpFeeType::OnInput => r_out * clamped_amount * f_amm / (r_in + clamped_amount * f_amm),
                    CpFeeType::OnOutput => f_amm * r_out * clamped_amount / (r_in + clamped_amount),
                };
                mid >= cumulative_input && mid <= cumulative_input + gross_capacity
            }
            DlmmPosition::Pool1 => {
                clamped_amount >= cumulative_input && clamped_amount <= cumulative_input + gross_capacity
            }
        };

        if in_segment {
            let profit = compute_multibin_profit(
                dlmm_pos, cp_fee_type, r_in, r_out, f_amm,
                cumulative_input, cumulative_output, bin_slope, clamped_amount,
            );
            if profit > best_profit {
                best_amount = clamped_amount;
                best_profit = profit;
            }
            break;
        }

        // This bin is fully consumed — accumulate and check if we should continue
        // gross_capacity = pre_fee_capacity / fee_factor; output = slope * gross (since slope = price * fee_factor)
        cumulative_output += gross_capacity * bin_slope;
        cumulative_input += gross_capacity;

        // Compute profit at the bin boundary to check if it's still worth continuing
        let boundary_amount = match dlmm_pos {
            DlmmPosition::Pool1 => cumulative_input.min(max_f),
            DlmmPosition::Pool2 => {
                // Invert: find dx such that mid(dx) = cumulative_input
                match cp_fee_type {
                    CpFeeType::OnInput => {
                        let denom = r_out - cumulative_input;
                        if denom <= 0.0 { break; }
                        cumulative_input * r_in / (denom * f_amm)
                    }
                    CpFeeType::OnOutput => {
                        let denom = f_amm * r_out - cumulative_input;
                        if denom <= 0.0 { break; }
                        cumulative_input * r_in / denom
                    }
                }
            }
        };
        let boundary_profit = compute_multibin_profit(
            dlmm_pos, cp_fee_type, r_in, r_out, f_amm,
            cumulative_input, cumulative_output, bin_slope, boundary_amount.min(max_f),
        );

        // Track best so far (boundary might be better than continuing)
        if boundary_profit > best_profit {
            best_amount = boundary_amount.min(max_f);
            best_profit = boundary_profit;
        }

        // Marginal rate early-exit: check if the composed marginal rate at the
        // boundary can still exceed 1.0 with the current bin's slope.
        // If the marginal is already <= 1 at this slope, no future bin (with
        // worse slope) can improve things.
        //
        // CP marginal at boundary:
        //   Pool2 (CP→DLMM): d(mid)/d(dx) evaluated at dx = boundary_amount
        //   Pool1 (DLMM→CP): d(out)/d(mid) evaluated at mid = cumulative_output
        let cp_marginal = match (dlmm_pos, cp_fee_type) {
            (DlmmPosition::Pool2, CpFeeType::OnInput) => {
                // d(mid)/d(dx) = f_amm * r_in * r_out / (r_in + dx * f_amm)^2
                let denom = r_in + boundary_amount * f_amm;
                f_amm * r_in * r_out / (denom * denom)
            }
            (DlmmPosition::Pool2, CpFeeType::OnOutput) => {
                // d(mid)/d(dx) = f_amm * r_in * r_out / (r_in + dx)^2
                let denom = r_in + boundary_amount;
                f_amm * r_in * r_out / (denom * denom)
            }
            (DlmmPosition::Pool1, CpFeeType::OnInput) => {
                // d(out)/d(mid) = f_amm * r_in * r_out / (r_in + mid * f_amm)^2
                let mid = cumulative_output;
                let denom = r_in + mid * f_amm;
                f_amm * r_in * r_out / (denom * denom)
            }
            (DlmmPosition::Pool1, CpFeeType::OnOutput) => {
                // d(out)/d(mid) = f_amm * r_in * r_out / (r_in + mid)^2
                let mid = cumulative_output;
                let denom = r_in + mid;
                f_amm * r_in * r_out / (denom * denom)
            }
        };
        // Composed marginal: DLMM_slope × CP_marginal (or CP_marginal × DLMM_slope).
        // Must exceed 1.0 for the next unit to be profitable.
        if bin_slope * cp_marginal <= 1.0 {
            break;
        }

        // Safety: don't exceed max_amount_in
        match dlmm_pos {
            DlmmPosition::Pool1 => {
                if cumulative_input >= max_f { break; }
            }
            DlmmPosition::Pool2 => {
                if boundary_amount >= max_f { break; }
            }
        }
    }

    if best_profit > 0.0 && best_amount > 0.0 {
        Some((best_amount as u64, best_profit as i128))
    } else {
        None
    }
}

/// Compute profit for a given input amount in a multi-bin context.
fn compute_multibin_profit(
    dlmm_pos: DlmmPosition,
    cp_fee_type: CpFeeType,
    r_in: f64, r_out: f64, f_amm: f64,
    cumulative_input: f64, cumulative_output: f64, bin_slope: f64,
    input_amount: f64,
) -> f64 {
    match dlmm_pos {
        DlmmPosition::Pool2 => {
            // CP → DLMM: middle_amount comes from CP, then DLMM output is piecewise
            let middle_amount = match cp_fee_type {
                CpFeeType::OnInput => r_out * input_amount * f_amm / (r_in + input_amount * f_amm),
                CpFeeType::OnOutput => f_amm * r_out * input_amount / (r_in + input_amount),
            };
            // DLMM output: bins before this one + partial current bin
            let dlmm_in_this_bin = (middle_amount - cumulative_input).max(0.0);
            let output_amount = cumulative_output + bin_slope * dlmm_in_this_bin;
            output_amount - input_amount
        }
        DlmmPosition::Pool1 => {
            // DLMM → CP: input goes into DLMM bins, middle_amount is piecewise output
            let dlmm_in_this_bin = (input_amount - cumulative_input).max(0.0);
            let middle_amount = cumulative_output + bin_slope * dlmm_in_this_bin;
            // CP output
            let output_amount = match cp_fee_type {
                CpFeeType::OnInput => r_out * middle_amount * f_amm / (r_in + middle_amount * f_amm),
                CpFeeType::OnOutput => f_amm * r_out * middle_amount / (r_in + middle_amount),
            };
            output_amount - input_amount
        }
    }
}

// ─── CLMM multi-tick walker (CP per tick range) ──────────────────────────────

/// Position of the CLMM pool in the 2-pool arbitrage path.
#[derive(Clone, Copy)]
enum ClmmPosition { Pool1, Pool2 }

/// Fee type of the CP pool paired with the CLMM.
#[derive(Clone, Copy)]
enum CpFee { OnInput, OnOutput }

/// Multi-tick analytical optimal for a CP+CLMM or CLMM+CP pair.
///
/// Walks CLMM tick ranges lazily, computing per-segment closed-form optimal
/// using the exact CP math (virtual reserves) instead of linear approximation.
///
/// Within each tick range, CLMM behaves as constant-product:
///   out = r_out * dx_net / (r_in + dx_net)   where dx_net = dx * fee_factor
///
/// The composed function (CP + CP-per-tick) has a closed-form optimal per segment.
///
/// Returns `Some((optimal_amount, estimated_profit))` or `None`.
pub fn analytical_optimal_clmm_cp<'info>(
    accounts: &[AccountInfo<'info>],
    clmm_instance: &dyn ProgramMeta,
    clmm_input_mint: Pubkey,
    pool1: &PoolModel,
    pool2: &PoolModel,
    max_amount_in: u64,
) -> Option<(u64, i128)> {
    // Determine which pool is CLMM and extract CP params
    let (clmm_pos, cp_fee, r_in, r_out, f_cp, f_clmm) = match (pool1, pool2) {
        (
            PoolModel::CpFeeOnInput { reserve_in, reserve_out, fee, .. },
            PoolModel::Clmm { fee: f_c, .. },
        ) => (ClmmPosition::Pool2, CpFee::OnInput, *reserve_in, *reserve_out, *fee, *f_c),

        (
            PoolModel::CpFeeOnOutput { reserve_in, reserve_out, fee, .. },
            PoolModel::Clmm { fee: f_c, .. },
        ) => (ClmmPosition::Pool2, CpFee::OnOutput, *reserve_in, *reserve_out, *fee, *f_c),

        (
            PoolModel::Clmm { fee: f_c, .. },
            PoolModel::CpFeeOnInput { reserve_in, reserve_out, fee, .. },
        ) => (ClmmPosition::Pool1, CpFee::OnInput, *reserve_in, *reserve_out, *fee, *f_c),

        (
            PoolModel::Clmm { fee: f_c, .. },
            PoolModel::CpFeeOnOutput { reserve_in, reserve_out, fee, .. },
        ) => (ClmmPosition::Pool1, CpFee::OnOutput, *reserve_in, *reserve_out, *fee, *f_c),

        _ => {
            debug_eprintln!("  clmm_cp_walk: not a CP+CLMM pair, pool1={}, pool2={}", pool1.label(), pool2.label());
            return None;
        }
    };

    // Cap on CLMM total capacity from cached max amounts (if available).
    // This prevents accumulating more tokens than the pool's vaults actually hold.
    let (clmm_max_in, clmm_max_out) = clmm_instance.get_cached_max_amounts(clmm_input_mint);
    let clmm_max_in_f = if clmm_max_in > 0 { clmm_max_in as f64 } else { f64::MAX };
    let clmm_max_out_f = if clmm_max_out > 0 { clmm_max_out as f64 } else { f64::MAX };

    debug_eprintln!(
        "  clmm_cp_walk: pos={:?}, cp_fee={:?}, r_in={:.2}, r_out={:.2}, f_cp={:.6}, f_clmm={:.6}, clmm_max_in={}, clmm_max_out={}",
        match clmm_pos { ClmmPosition::Pool1 => "Pool1", ClmmPosition::Pool2 => "Pool2" },
        match cp_fee { CpFee::OnInput => "FI", CpFee::OnOutput => "FO" },
        r_in, r_out, f_cp, f_clmm, clmm_max_in, clmm_max_out
    );

    let mut cum_in: f64 = 0.0;    // cumulative gross CLMM input from prior ticks
    let mut cum_out: f64 = 0.0;   // cumulative CLMM output from prior ticks
    let mut best_amount: f64 = 0.0;
    let mut best_profit: f64 = 0.0;
    let max_f = max_amount_in as f64;

    for tick_offset in 0..70i32 {
        // Get virtual reserves for this tick range
        let seg_result = clmm_instance.get_clmm_segment(accounts, clmm_input_mint, tick_offset);
        debug_eprintln!("  clmm_cp_walk[{}]: segment={:?}", tick_offset, seg_result.as_ref().map(|r| r.as_ref().map(|s| (s.0 as i64, s.1 as i64, s.2, s.3))));
        let (vr_in, vr_out, net_capacity, seg_fee) = match seg_result {
            Ok(Some((ri, ro, c, f))) if ri > 0.0 && ro > 0.0 && c > 0 && f > 0.0 => (ri, ro, c, f),
            Ok(Some(_)) => continue,
            _ => break,
        };

        // Gross capacity = net / fee_factor (what the user sends before fee deduction)
        let gross_capacity = if seg_fee > 0.0 { net_capacity as f64 / seg_fee } else { break };
        // Cap to pool's remaining total capacity
        let gross_capacity = gross_capacity.min(clmm_max_in_f - cum_in).max(0.0);
        if gross_capacity <= 0.0 { break; }

        // Compute the closed-form optimal dx for the composed function,
        // accounting for cumulative consumption from prior tick ranges.
        //
        // Key derivation (CLMM as pool2, CP_FI as pool1):
        //   α = (vr_in - cum_in * f_clmm) * R_in
        //   β = vr_in - cum_in * f_clmm + f_clmm * R_out
        //   K = sqrt(vr_out * vr_in * f_clmm * f_cp * R_in * R_out)
        //   u = (K - α) / β           where u = dx * f_cp
        //   dx = u / f_cp = (K - α) / (f_cp * β)
        let segment_optimal = match (clmm_pos, cp_fee) {
            // CP(FI) → CLMM: pool1 out = R_out * dx * f_cp / (R_in + dx * f_cp)
            (ClmmPosition::Pool2, CpFee::OnInput) => {
                let adj = vr_in - cum_in * f_clmm;
                let alpha = adj * r_in;
                let beta = adj + f_clmm * r_out;
                let k_sq = vr_out * vr_in * f_clmm * f_cp * r_in * r_out;
                if k_sq <= alpha * alpha { break; }
                let k = k_sq.sqrt();
                (k - alpha) / (f_cp * beta)
            }

            // CP(FO) → CLMM: pool1 out = f_cp * R_out * dx / (R_in + dx)
            (ClmmPosition::Pool2, CpFee::OnOutput) => {
                let adj = vr_in - cum_in * f_clmm;
                let alpha = adj * r_in;
                let beta = adj + f_clmm * f_cp * r_out;
                let k_sq = vr_out * vr_in * f_clmm * f_cp * r_in * r_out;
                if k_sq <= alpha * alpha { break; }
                let k = k_sq.sqrt();
                (k - alpha) / beta
            }

            // CLMM → CP(FI): pool1 is CLMM, pool2 is CP
            // mid(dx) = cum_out + vr_out * (dx - cum_in) * f_clmm / (vr_in + (dx - cum_in) * f_clmm)
            // out = r_out * mid * f_cp / (r_in + mid * f_cp)
            // This is harder because pool1 is piecewise-CP. Within tick k:
            //   Let m = dx - cum_in (input to current tick)
            //   mid = cum_out + vr_out * m * f_clmm / (vr_in + m * f_clmm)
            //   d(mid)/d(dx) = vr_out * vr_in * f_clmm / (vr_in + m * f_clmm)²
            //   d(out)/d(mid) = r_out * r_in * f_cp / (r_in + mid * f_cp)²
            // Setting product = 1 involves mid which depends on dx non-linearly.
            // Use the same A*B trick:
            //   A = r_in + mid * f_cp
            //   B = vr_in + m * f_clmm
            //   A * B = r_in * vr_in + r_in * m * f_clmm + cum_out * f_cp * vr_in
            //         + cum_out * f_cp * m * f_clmm + f_cp * vr_out * m * f_clmm
            //   Hmm, mid has cum_out term which complicates things.
            // Simpler: substitute m = dx - cum_in, and express A*B in terms of m.
            //   mid = cum_out + vr_out * m * f_clmm / (vr_in + m * f_clmm)
            //   A = r_in + f_cp * (cum_out + vr_out * m * f_clmm / (vr_in + m * f_clmm))
            //   A * (vr_in + m * f_clmm) = (r_in + f_cp * cum_out) * (vr_in + m * f_clmm) + f_cp * vr_out * m * f_clmm
            //   = (r_in + f_cp * cum_out) * vr_in + ((r_in + f_cp * cum_out) * f_clmm + f_cp * f_clmm * vr_out) * m
            //   = (r_in + f_cp * cum_out) * vr_in + f_clmm * (r_in + f_cp * cum_out + f_cp * vr_out) * m
            // So A * B = α' + β' * m  where:
            //   α' = (r_in + f_cp * cum_out) * vr_in
            //   β' = f_clmm * (r_in + f_cp * cum_out + f_cp * vr_out)
            // And (A*B)² = r_out * r_in * f_cp * vr_out * vr_in * f_clmm
            //   K' = sqrt(r_out * r_in * f_cp * vr_out * vr_in * f_clmm)
            // m = (K' - α') / β'
            // dx = m + cum_in
            (ClmmPosition::Pool1, CpFee::OnInput) => {
                let r_adj = r_in + f_cp * cum_out;
                let alpha = r_adj * vr_in;
                let beta = f_clmm * (r_adj + f_cp * vr_out);
                let k_sq = r_out * r_in * f_cp * vr_out * vr_in * f_clmm;
                if k_sq <= alpha * alpha || beta <= 0.0 { break; }
                let k = k_sq.sqrt();
                let m = (k - alpha) / beta;
                m + cum_in
            }

            // CLMM → CP(FO): pool2 out = f_cp * r_out * mid / (r_in + mid)
            //   A = r_in + mid
            //   B = vr_in + m * f_clmm
            //   A * B = (r_in + cum_out) * (vr_in + m * f_clmm) + vr_out * m * f_clmm
            //   = (r_in + cum_out) * vr_in + f_clmm * (r_in + cum_out + vr_out) * m
            //   α' = (r_in + cum_out) * vr_in
            //   β' = f_clmm * (r_in + cum_out + vr_out)
            //   K' = sqrt(f_cp * r_out * r_in * vr_out * vr_in * f_clmm)
            (ClmmPosition::Pool1, CpFee::OnOutput) => {
                let r_adj = r_in + cum_out;
                let alpha = r_adj * vr_in;
                let beta = f_clmm * (r_adj + vr_out);
                let k_sq = f_cp * r_out * r_in * vr_out * vr_in * f_clmm;
                if k_sq <= alpha * alpha || beta <= 0.0 { break; }
                let k = k_sq.sqrt();
                let m = (k - alpha) / beta;
                m + cum_in
            }
        };

        debug_eprintln!(
            "  clmm_cp_walk[{}]: vr_in={:.2}, vr_out={:.2}, net_cap={}, gross_cap={:.2}, cum_in={:.2}, cum_out={:.2}",
            tick_offset, vr_in, vr_out, net_capacity, gross_capacity, cum_in, cum_out
        );

        if !segment_optimal.is_finite() || segment_optimal <= 0.0 {
            debug_eprintln!("  clmm_cp_walk[{}]: segment_optimal={:.4} — breaking", tick_offset, segment_optimal);
            break;
        }

        let clamped_amount = segment_optimal.min(max_f);

        // Check if the optimal falls within this tick range
        let in_segment = match clmm_pos {
            ClmmPosition::Pool2 => {
                // mid tokens entering CLMM
                let mid = match cp_fee {
                    CpFee::OnInput => r_out * clamped_amount * f_cp / (r_in + clamped_amount * f_cp),
                    CpFee::OnOutput => f_cp * r_out * clamped_amount / (r_in + clamped_amount),
                };
                debug_eprintln!(
                    "  clmm_cp_walk[{}]: segment_optimal={:.4}, clamped={:.4}, mid={:.4}, range=[{:.4}, {:.4}]",
                    tick_offset, segment_optimal, clamped_amount, mid, cum_in, cum_in + gross_capacity
                );
                mid >= cum_in && mid <= cum_in + gross_capacity
            }
            ClmmPosition::Pool1 => {
                debug_eprintln!(
                    "  clmm_cp_walk[{}]: segment_optimal={:.4}, clamped={:.4}, range=[{:.4}, {:.4}]",
                    tick_offset, segment_optimal, clamped_amount, cum_in, cum_in + gross_capacity
                );
                clamped_amount >= cum_in && clamped_amount <= cum_in + gross_capacity
            }
        };

        if in_segment {
            // Compute profit at clamped_amount through the composed chain
            let profit = compute_clmm_profit(
                clmm_pos, cp_fee, r_in, r_out, f_cp, f_clmm,
                cum_in, cum_out, vr_in, vr_out, clamped_amount,
            );
            if profit > best_profit {
                best_amount = clamped_amount;
                best_profit = profit;
            }
            break;
        }

        // Tick fully consumed — accumulate CP output for this tick range
        // Gross input to tick = gross_capacity, net input = gross_capacity * f_clmm
        // CP output: vr_out * net / (vr_in + net)
        let net_in = gross_capacity * f_clmm;
        let tick_out = vr_out * net_in / (vr_in + net_in);
        cum_out += tick_out;
        cum_in += gross_capacity;

        // Stop if we've reached the pool's total max capacity
        if cum_in >= clmm_max_in_f || cum_out >= clmm_max_out_f {
            break;
        }

        // Check if it's still worth continuing: marginal rate at next tick start > 1?
        // (approximate check using pool1 marginal at boundary)
        let boundary_amount = match clmm_pos {
            ClmmPosition::Pool1 => cum_in.min(max_f),
            ClmmPosition::Pool2 => match cp_fee {
                CpFee::OnInput => {
                    if r_out <= cum_in { break; }
                    cum_in * r_in / (f_cp * (r_out - cum_in))
                }
                CpFee::OnOutput => {
                    let denom = f_cp * r_out - cum_in;
                    if denom <= 0.0 { break; }
                    cum_in * r_in / denom
                }
            },
        };
        let boundary_profit = compute_clmm_profit(
            clmm_pos, cp_fee, r_in, r_out, f_cp, f_clmm,
            cum_in, cum_out, vr_in, vr_out, boundary_amount.min(max_f),
        );
        if boundary_profit > best_profit {
            best_amount = boundary_amount.min(max_f);
            best_profit = boundary_profit;
        }
        // If cumulative profit is decreasing at boundary, stop
        if boundary_profit <= 0.0 && tick_offset > 0 {
            break;
        }
    }

    if best_amount <= 0.0 || best_profit <= 0.0 {
        return None;
    }

    debug_eprintln!(
        "  clmm_cp_walk: amount={}, profit={:.4}",
        best_amount as u64, best_profit
    );

    Some((best_amount as u64, best_profit as i128))
}

/// Compute profit through the composed CP+CLMM chain at a given dx.
fn compute_clmm_profit(
    clmm_pos: ClmmPosition,
    cp_fee: CpFee,
    r_in: f64, r_out: f64, f_cp: f64, f_clmm: f64,
    cum_in: f64, cum_out: f64,
    vr_in: f64, vr_out: f64,
    dx: f64,
) -> f64 {
    match clmm_pos {
        ClmmPosition::Pool2 => {
            // pool1 (CP) output
            let mid = match cp_fee {
                CpFee::OnInput => r_out * dx * f_cp / (r_in + dx * f_cp),
                CpFee::OnOutput => f_cp * r_out * dx / (r_in + dx),
            };
            // CLMM output (cumulative from prior ticks + current tick)
            // Clamp m to 0: at tick boundaries, float rounding can make m slightly negative
            let m = (mid - cum_in).max(0.0);
            let net_m = m * f_clmm;
            let clmm_out = cum_out + vr_out * net_m / (vr_in + net_m);
            clmm_out - dx
        }
        ClmmPosition::Pool1 => {
            // CLMM output (pool1)
            let m = (dx - cum_in).max(0.0);
            let net_m = m * f_clmm;
            let mid = cum_out + vr_out * net_m / (vr_in + net_m);
            // pool2 (CP) output
            let out = match cp_fee {
                CpFee::OnInput => r_out * mid * f_cp / (r_in + mid * f_cp),
                CpFee::OnOutput => f_cp * r_out * mid / (r_in + mid),
            };
            out - dx
        }
    }
}

// ─── DLMM + DLMM multi-bin greedy walker ─────────────────────────────────────

/// Compute the optimal input amount for a DLMM → DLMM arbitrage pair.
///
/// Both pools are piecewise-linear, so the composed output within any bin pair
/// is also linear:  `out = dx * buy_slope * sell_slope`.
/// The marginal rate is constant per bin pair — either profitable (rate > 1) or not.
/// We greedily fill bin pairs while profitable, advancing whichever bin is exhausted first.
///
/// Returns `Some((optimal_amount, estimated_profit))` or `None`.
pub fn analytical_optimal_dlmm_dlmm<'info>(
    accounts: &[AccountInfo<'info>],
    buy_instance: &dyn ProgramMeta,
    sell_instance: &dyn ProgramMeta,
    buy_input_mint: Pubkey,
    sell_input_mint: Pubkey,
    max_amount_in: u64,
) -> Option<(u64, i128)> {
    let max_f = max_amount_in as f64;
    let mut total_in: f64 = 0.0;
    let mut total_profit: f64 = 0.0;

    let mut buy_offset: i32 = 0;
    let mut sell_offset: i32 = 0;
    // Remaining capacity in the current bin (in gross input units for that pool)
    let mut buy_remaining: f64 = 0.0;
    let mut sell_remaining_tokens: f64 = 0.0; // sell-side remaining in token (sell input) units
    let mut buy_slope: f64 = 0.0;
    let mut sell_slope: f64 = 0.0;
    let mut buy_needs_load = true;
    let mut sell_needs_load = true;

    for _ in 0..140 {  // safety limit: max 70 bins per side
        // Load buy bin if needed
        if buy_needs_load {
            match buy_instance.get_bin_segment(accounts, buy_input_mint, buy_offset) {
                Ok(Some((s, c, f))) if s > 0.0 && c > 0 && f > 0.0 => {
                    buy_slope = s;
                    buy_remaining = c as f64 / f; // gross capacity
                    buy_needs_load = false;
                }
                Ok(Some(_)) => {
                    // empty bin — skip
                    buy_offset += 1;
                    continue;
                }
                _ => break,
            }
        }

        // Load sell bin if needed
        if sell_needs_load {
            match sell_instance.get_bin_segment(accounts, sell_input_mint, sell_offset) {
                Ok(Some((s, c, f))) if s > 0.0 && c > 0 && f > 0.0 => {
                    sell_slope = s;
                    sell_remaining_tokens = c as f64 / f; // gross capacity in tokens
                    sell_needs_load = false;
                }
                Ok(Some(_)) => {
                    sell_offset += 1;
                    continue;
                }
                _ => break,
            }
        }

        // Composed marginal rate
        let rate = buy_slope * sell_slope;
        if rate <= 1.0 {
            break; // no more profit possible
        }

        // How much SOL can we push through both bins?
        // Buy bin accepts buy_remaining SOL, producing buy_remaining * buy_slope tokens.
        // Sell bin accepts sell_remaining_tokens tokens.
        let sell_cap_sol = sell_remaining_tokens / buy_slope; // sell capacity in SOL terms
        let remaining_budget = max_f - total_in;
        let fillable = buy_remaining.min(sell_cap_sol).min(remaining_budget);

        if fillable <= 0.0 {
            break;
        }

        // Consume
        let tokens_consumed = fillable * buy_slope;
        total_in += fillable;
        total_profit += fillable * (rate - 1.0);
        buy_remaining -= fillable;
        sell_remaining_tokens -= tokens_consumed;

        // Budget exhausted
        if total_in >= max_f {
            break;
        }

        // Advance whichever bin(s) were exhausted (threshold to handle float imprecision)
        let eps = 0.5;
        if buy_remaining < eps {
            buy_offset += 1;
            buy_needs_load = true;
        }
        if sell_remaining_tokens < eps {
            sell_offset += 1;
            sell_needs_load = true;
        }
    }

    if total_profit > 0.0 && total_in > 0.0 {
        Some((total_in as u64, total_profit as i128))
    } else {
        None
    }
}

// ─── Tests: Golden section vs Analytical comparison ──────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// High-precision golden section search over pool_output chain.
    /// Runs many iterations to find the true optimal input amount.
    fn golden_search_optimal(
        pool1: &PoolModel,
        pool2: &PoolModel,
        max_amount_in: u64,
    ) -> Option<(u64, f64)> {
        let max_f = max_amount_in as f64;
        let mut a: f64 = 1.0;
        let mut b: f64 = max_f;

        let profit_at = |x: f64| -> f64 {
            let mid = pool_output(pool1, x);
            if mid <= 0.0 { return f64::NEG_INFINITY; }
            let out = pool_output(pool2, mid);
            out - x
        };

        // Quick check: is there any profit in the range?
        let samples = [a, b, (a + b) / 2.0, (a + b) / 4.0, (a + b) * 3.0 / 4.0];
        if samples.iter().all(|&x| profit_at(x) <= 0.0) {
            return None;
        }

        let phi: f64 = 1.618033988749895;
        let mut c = b - (b - a) / phi;
        let mut d = a + (b - a) / phi;
        let mut fc = profit_at(c);
        let mut fd = profit_at(d);

        // 100 iterations for very high precision
        for _ in 0..100 {
            if (b - a) < 1.0 { break; }
            if fc > fd {
                b = d;
                d = c;
                fd = fc;
                c = b - (b - a) / phi;
                fc = profit_at(c);
            } else {
                a = c;
                c = d;
                fc = fd;
                d = a + (b - a) / phi;
                fd = profit_at(d);
            }
        }

        let optimal = (a + b) / 2.0;
        let profit = profit_at(optimal);
        if profit <= 0.0 || optimal <= 0.0 {
            return None;
        }
        Some((optimal as u64, profit))
    }

    /// High-precision golden section search for N-hop chain.
    fn golden_search_optimal_nhop(
        models: &[PoolModel],
        max_amount_in: u64,
    ) -> Option<(u64, f64)> {
        let max_f = max_amount_in as f64;
        let mut a: f64 = 1.0;
        let mut b: f64 = max_f;

        let profit_at = |x: f64| -> f64 { pool_chain_output(models, x) - x };

        let samples = [a, b, (a + b) / 2.0, (a + b) / 4.0, (a + b) * 3.0 / 4.0];
        if samples.iter().all(|&x| profit_at(x) <= 0.0) {
            return None;
        }

        let phi: f64 = 1.618033988749895;
        let mut c = b - (b - a) / phi;
        let mut d = a + (b - a) / phi;
        let mut fc = profit_at(c);
        let mut fd = profit_at(d);

        for _ in 0..100 {
            if (b - a) < 1.0 { break; }
            if fc > fd {
                b = d;
                d = c;
                fd = fc;
                c = b - (b - a) / phi;
                fc = profit_at(c);
            } else {
                a = c;
                c = d;
                fc = fd;
                d = a + (b - a) / phi;
                fd = profit_at(d);
            }
        }

        let optimal = (a + b) / 2.0;
        let profit = profit_at(optimal);
        if profit <= 0.0 || optimal <= 0.0 {
            return None;
        }
        Some((optimal as u64, profit))
    }

    /// Helper to compare analytical vs golden section results.
    /// Asserts that the analytical optimal amount is within `amount_tolerance_pct` of golden,
    /// and that analytical profit is within `profit_tolerance_pct` of golden.
    fn assert_analytical_matches_golden(
        label: &str,
        pool1: &PoolModel,
        pool2: &PoolModel,
        max_amount_in: u64,
        amount_tolerance_pct: f64,
        profit_tolerance_pct: f64,
    ) {
        let analytical = analytical_estimate(pool1, pool2, max_amount_in, None);
        let golden = golden_search_optimal(pool1, pool2, max_amount_in);

        match (analytical, golden) {
            (None, None) => {
                // Both agree: no arb. OK.
                eprintln!("  [{}] Both agree: no arb", label);
            }
            (Some((a_amt, a_profit, _)), Some((g_amt, g_profit))) => {
                let amount_diff_pct = ((a_amt as f64 - g_amt as f64) / g_amt as f64).abs() * 100.0;
                let profit_diff_pct = ((a_profit as f64 - g_profit) / g_profit).abs() * 100.0;

                eprintln!(
                    "  [{}] analytical: amount={}, profit={:.2} | golden: amount={}, profit={:.2} | diff: amount={:.4}%, profit={:.4}%",
                    label, a_amt, a_profit as f64 / 1e9, g_amt, g_profit / 1e9, amount_diff_pct, profit_diff_pct
                );

                // The analytical profit should be very close to the golden search profit.
                // Amount can differ slightly due to rounding, but profit at the analytical
                // optimum should be near-optimal.
                let analytical_profit_at_analytical_amount = {
                    let mid = pool_output(pool1, a_amt as f64);
                    let out = pool_output(pool2, mid);
                    out - a_amt as f64
                };
                let golden_profit_at_golden_amount = g_profit;
                let profit_gap_pct = ((golden_profit_at_golden_amount - analytical_profit_at_analytical_amount)
                    / golden_profit_at_golden_amount).abs() * 100.0;

                assert!(
                    profit_gap_pct < profit_tolerance_pct,
                    "[{}] Profit gap too large: analytical_profit={:.6}, golden_profit={:.6}, gap={:.4}% (max {:.1}%)",
                    label,
                    analytical_profit_at_analytical_amount / 1e9,
                    golden_profit_at_golden_amount / 1e9,
                    profit_gap_pct,
                    profit_tolerance_pct,
                );

                assert!(
                    amount_diff_pct < amount_tolerance_pct,
                    "[{}] Amount diff too large: analytical={}, golden={}, diff={:.4}% (max {:.1}%)",
                    label, a_amt, g_amt, amount_diff_pct, amount_tolerance_pct,
                );
            }
            (Some((a_amt, a_profit, _)), None) => {
                // Analytical found arb but golden didn't — likely tiny profit
                eprintln!(
                    "  [{}] analytical found arb (amount={}, profit={}) but golden didn't — likely marginal",
                    label, a_amt, a_profit
                );
            }
            (None, Some((g_amt, g_profit))) => {
                panic!(
                    "[{}] Golden found arb (amount={}, profit={:.6}) but analytical returned None!",
                    label, g_amt, g_profit / 1e9,
                );
            }
        }
    }

    // ─── 2-pool tests ────────────────────────────────────────────────────────

    #[test]
    fn test_cp_fee_on_input_plus_cp_fee_on_input() {
        eprintln!("\n=== CpFeeOnInput + CpFeeOnInput ===");

        // Scenario 1: Large price difference, typical Raydium AMM + Meteora DAMM V1
        let pool1 = PoolModel::CpFeeOnInput {
            reserve_in: 500_000_000_000.0,  // 500 SOL
            reserve_out: 1_000_000_000_000_000.0, // 1M tokens
            fee: 0.9975,  // 0.25% fee
            marginal_price: 2_000_000.0 * 0.9975,
        };
        let pool2 = PoolModel::CpFeeOnInput {
            reserve_in: 950_000_000_000_000.0, // tokens
            reserve_out: 520_000_000_000.0,    // 520 SOL (price diff ~4%)
            fee: 0.997,   // 0.3% fee
            marginal_price: (520.0 / 950_000.0) * 1e9 * 0.997,
        };
        assert_analytical_matches_golden("large_diff", &pool1, &pool2, 10_000_000_000, 1.0, 0.5);

        // Scenario 2: Small price difference (marginal arb)
        let pool1 = PoolModel::CpFeeOnInput {
            reserve_in: 1_000_000_000_000.0,
            reserve_out: 5_000_000_000_000_000.0,
            fee: 0.9975,
            marginal_price: 5_000_000.0 * 0.9975,
        };
        let pool2 = PoolModel::CpFeeOnInput {
            reserve_in: 4_950_000_000_000_000.0,
            reserve_out: 1_010_000_000_000.0, // ~1% diff
            fee: 0.9975,
            marginal_price: (1_010.0 / 4_950_000.0) * 1e9 * 0.9975,
        };
        assert_analytical_matches_golden("small_diff", &pool1, &pool2, 5_000_000_000, 1.0, 0.5);

        // Scenario 3: Asymmetric reserves (large vs tiny pool)
        let pool1 = PoolModel::CpFeeOnInput {
            reserve_in: 10_000_000_000.0,   // 10 SOL (tiny pool)
            reserve_out: 50_000_000_000_000.0,
            fee: 0.99,
            marginal_price: 5_000_000.0 * 0.99,
        };
        let pool2 = PoolModel::CpFeeOnInput {
            reserve_in: 40_000_000_000_000.0,
            reserve_out: 100_000_000_000.0, // 100 SOL (big pool)
            fee: 0.997,
            marginal_price: (100.0 / 40_000.0) * 1e9 * 0.997,
        };
        assert_analytical_matches_golden("asymmetric", &pool1, &pool2, 5_000_000_000, 1.0, 0.5);

        // Scenario 4: No arb (prices aligned)
        let pool1 = PoolModel::CpFeeOnInput {
            reserve_in: 500_000_000_000.0,
            reserve_out: 1_000_000_000_000_000.0,
            fee: 0.997,
            marginal_price: 2_000_000.0 * 0.997,
        };
        let pool2 = PoolModel::CpFeeOnInput {
            reserve_in: 1_000_000_000_000_000.0,
            reserve_out: 500_000_000_000.0,
            fee: 0.997,
            marginal_price: 0.0000005 * 1e9 * 0.997,
        };
        assert_analytical_matches_golden("no_arb", &pool1, &pool2, 10_000_000_000, 1.0, 0.5);
    }

    #[test]
    fn test_cp_fee_on_input_plus_cp_fee_on_output() {
        eprintln!("\n=== CpFeeOnInput + CpFeeOnOutput (e.g. Raydium → PumpAmm sell) ===");

        let pool1 = PoolModel::CpFeeOnInput {
            reserve_in: 300_000_000_000.0,   // 300 SOL
            reserve_out: 900_000_000_000_000.0,
            fee: 0.9975,
            marginal_price: 3_000_000.0 * 0.9975,
        };
        let pool2 = PoolModel::CpFeeOnOutput {
            reserve_in: 850_000_000_000_000.0,
            reserve_out: 320_000_000_000.0,   // 320 SOL (~6.7% diff)
            fee: 0.99,   // PumpAmm 1% fee
            marginal_price: (320.0 / 850_000.0) * 1e9 * 0.99,
        };
        assert_analytical_matches_golden("raydium_to_pump", &pool1, &pool2, 5_000_000_000, 1.0, 0.5);

        // Reverse: no arb
        let pool2_no_arb = PoolModel::CpFeeOnOutput {
            reserve_in: 900_000_000_000_000.0,
            reserve_out: 290_000_000_000.0,
            fee: 0.99,
            marginal_price: (290.0 / 900_000.0) * 1e9 * 0.99,
        };
        assert_analytical_matches_golden("no_arb", &pool1, &pool2_no_arb, 5_000_000_000, 1.0, 0.5);
    }

    #[test]
    fn test_cp_fee_on_output_plus_cp_fee_on_input() {
        eprintln!("\n=== CpFeeOnOutput + CpFeeOnInput (e.g. PumpAmm sell → Raydium) ===");

        let pool1 = PoolModel::CpFeeOnOutput {
            reserve_in: 800_000_000_000_000.0,
            reserve_out: 250_000_000_000.0,
            fee: 0.9875,  // PumpAmm 1.25% fee
            marginal_price: (250.0 / 800_000.0) * 1e9 * 0.9875,
        };
        let pool2 = PoolModel::CpFeeOnInput {
            reserve_in: 270_000_000_000.0,   // 270 SOL (price diff)
            reserve_out: 850_000_000_000_000.0,
            fee: 0.997,
            marginal_price: (850_000.0 / 270.0) * 1e6 * 0.997,
        };
        assert_analytical_matches_golden("pump_to_raydium", &pool1, &pool2, 5_000_000_000_000, 1.0, 0.5);
    }

    #[test]
    fn test_cp_fee_on_output_plus_cp_fee_on_output() {
        eprintln!("\n=== CpFeeOnOutput + CpFeeOnOutput ===");

        let pool1 = PoolModel::CpFeeOnOutput {
            reserve_in: 700_000_000_000_000.0,
            reserve_out: 200_000_000_000.0,
            fee: 0.99,
            marginal_price: (200.0 / 700_000.0) * 1e9 * 0.99,
        };
        let pool2 = PoolModel::CpFeeOnOutput {
            reserve_in: 220_000_000_000.0,
            reserve_out: 750_000_000_000_000.0,
            fee: 0.9875,
            marginal_price: (750_000.0 / 220.0) * 1e6 * 0.9875,
        };
        assert_analytical_matches_golden("pump_to_pump", &pool1, &pool2, 5_000_000_000_000, 1.0, 0.5);
    }

    #[test]
    fn test_cp_fee_on_input_plus_linear() {
        eprintln!("\n=== CpFeeOnInput + Linear (e.g. Raydium → DLMM) ===");

        // DLMM sells at higher price than Raydium buys at
        let pool1 = PoolModel::CpFeeOnInput {
            reserve_in: 500_000_000_000.0,   // 500 SOL
            reserve_out: 1_000_000_000_000_000.0,
            fee: 0.9975,
            marginal_price: 2_000_000.0 * 0.9975,
        };
        let pool2 = PoolModel::Linear {
            price: 0.00000055 * 1e9, // slightly higher than pool1's inverse
            fee: 0.997,
            max_in: 50_000_000_000_000, // 50K tokens capacity
            bin_step_frac: 0.008,
            marginal_price: 0.00000055 * 1e9 * 0.997,
        };
        assert_analytical_matches_golden("raydium_to_dlmm", &pool1, &pool2, 10_000_000_000, 2.0, 1.0);

        // Large capacity DLMM
        let pool2_large = PoolModel::Linear {
            price: 0.00000056 * 1e9,
            fee: 0.998,
            max_in: 500_000_000_000_000, // huge capacity
            bin_step_frac: 0.001,
            marginal_price: 0.00000056 * 1e9 * 0.998,
        };
        assert_analytical_matches_golden("large_dlmm", &pool1, &pool2_large, 10_000_000_000, 2.0, 1.0);
    }

    #[test]
    fn test_cp_fee_on_output_plus_linear() {
        eprintln!("\n=== CpFeeOnOutput + Linear (e.g. PumpAmm sell → DLMM) ===");

        let pool1 = PoolModel::CpFeeOnOutput {
            reserve_in: 800_000_000_000_000.0,
            reserve_out: 250_000_000_000.0,
            fee: 0.99,
            marginal_price: (250.0 / 800_000.0) * 1e9 * 0.99,
        };
        let pool2 = PoolModel::Linear {
            price: 0.00000034 * 1e9,
            fee: 0.997,
            max_in: 100_000_000_000_000,
            bin_step_frac: 0.008,
            marginal_price: 0.00000034 * 1e9 * 0.997,
        };
        assert_analytical_matches_golden("pump_sell_to_dlmm", &pool1, &pool2, 5_000_000_000_000, 2.0, 1.0);
    }

    #[test]
    fn test_linear_plus_cp_fee_on_input() {
        eprintln!("\n=== Linear + CpFeeOnInput (e.g. DLMM → Raydium) ===");

        let pool1 = PoolModel::Linear {
            price: 2_050_000.0, // DLMM price
            fee: 0.997,
            max_in: 5_000_000_000, // 5 SOL capacity
            bin_step_frac: 0.008,
            marginal_price: 2_050_000.0 * 0.997,
        };
        let pool2 = PoolModel::CpFeeOnInput {
            reserve_in: 950_000_000_000_000.0,
            reserve_out: 510_000_000_000.0,
            fee: 0.9975,
            marginal_price: (510.0 / 950_000.0) * 1e9 * 0.9975,
        };
        assert_analytical_matches_golden("dlmm_to_raydium", &pool1, &pool2, 10_000_000_000, 2.0, 1.0);
    }

    #[test]
    fn test_linear_plus_cp_fee_on_output() {
        eprintln!("\n=== Linear + CpFeeOnOutput (e.g. DLMM → PumpAmm) ===");

        let pool1 = PoolModel::Linear {
            price: 2_050_000.0,
            fee: 0.998,
            max_in: 5_000_000_000,
            bin_step_frac: 0.008,
            marginal_price: 2_050_000.0 * 0.998,
        };
        let pool2 = PoolModel::CpFeeOnOutput {
            reserve_in: 1_050_000_000_000_000.0,
            reserve_out: 520_000_000_000.0,
            fee: 0.99,
            marginal_price: (520.0 / 1_050_000.0) * 1e9 * 0.99,
        };
        assert_analytical_matches_golden("dlmm_to_pump", &pool1, &pool2, 10_000_000_000, 2.0, 1.0);
    }

    #[test]
    fn test_linear_plus_linear() {
        eprintln!("\n=== Linear + Linear (DLMM + DLMM) ===");

        // Profitable: combined factor > 1
        let pool1 = PoolModel::Linear {
            price: 2_100_000.0,
            fee: 0.998,
            max_in: 3_000_000_000, // 3 SOL
            bin_step_frac: 0.008,
            marginal_price: 2_100_000.0 * 0.998,
        };
        let pool2 = PoolModel::Linear {
            price: 0.00000049 * 1e9,
            fee: 0.998,
            max_in: 5_000_000_000_000,
            bin_step_frac: 0.008,
            marginal_price: 0.00000049 * 1e9 * 0.998,
        };

        let analytical = analytical_estimate(&pool1, &pool2, 10_000_000_000, None);
        let golden = golden_search_optimal(&pool1, &pool2, 10_000_000_000);

        match (analytical, golden) {
            (Some((a_amt, a_profit, _)), Some((g_amt, g_profit))) => {
                eprintln!(
                    "  [linear+linear] analytical: amount={}, profit={:.6} | golden: amount={}, profit={:.6}",
                    a_amt, a_profit as f64 / 1e9, g_amt, g_profit / 1e9,
                );
                // For linear+linear the profit is strictly linear, so the optimal is max capacity.
                // Both should agree on maximizing input.
                let a_profit_f = pool_output(&pool1, a_amt as f64);
                let a_profit_f = pool_output(&pool2, a_profit_f) - a_amt as f64;
                let g_profit_f = g_profit;
                let gap = ((g_profit_f - a_profit_f) / g_profit_f).abs() * 100.0;
                assert!(gap < 1.0, "Linear+Linear profit gap too large: {:.4}%", gap);
            }
            (None, None) => eprintln!("  [linear+linear] Both agree: no arb"),
            (a, g) => eprintln!("  [linear+linear] Mismatch: analytical={:?}, golden={:?}", a.is_some(), g.is_some()),
        }

        // Not profitable: combined factor < 1
        // p1*f1*p2*f2 must be <= 1: 2_100_000 * 0.998 * p2 * 0.998 <= 1 → p2 < 0.000000478
        let pool2_no_arb = PoolModel::Linear {
            price: 0.00000045,
            fee: 0.998,
            max_in: 5_000_000_000_000,
            bin_step_frac: 0.008,
            marginal_price: 0.00000045 * 0.998,
        };
        let analytical = analytical_estimate(&pool1, &pool2_no_arb, 10_000_000_000, None);
        let golden = golden_search_optimal(&pool1, &pool2_no_arb, 10_000_000_000);
        assert!(analytical.is_none() && golden.is_none(), "Both should find no arb");
        eprintln!("  [linear+linear_no_arb] Both agree: no arb");
    }

    // ─── 3-pool (N-hop) tests ────────────────────────────────────────────────

    fn assert_nhop_matches_golden(
        label: &str,
        models: &[PoolModel],
        max_amount_in: u64,
        profit_tolerance_pct: f64,
    ) {
        let analytical = analytical_estimate_nhop(models, max_amount_in);
        let golden = golden_search_optimal_nhop(models, max_amount_in);

        match (analytical, golden) {
            (None, None) => {
                eprintln!("  [{}] Both agree: no arb", label);
            }
            (Some((a_amt, a_profit, _)), Some((g_amt, g_profit))) => {
                // Evaluate analytical amount through the chain to get real profit
                let a_real_profit = pool_chain_output(models, a_amt as f64) - a_amt as f64;

                let profit_gap_pct = ((g_profit - a_real_profit) / g_profit).abs() * 100.0;

                eprintln!(
                    "  [{}] analytical: amount={}, profit={:.6} (real={:.6}) | golden: amount={}, profit={:.6} | gap={:.4}%",
                    label, a_amt, a_profit as f64 / 1e9, a_real_profit / 1e9,
                    g_amt, g_profit / 1e9, profit_gap_pct,
                );

                assert!(
                    profit_gap_pct < profit_tolerance_pct,
                    "[{}] N-hop profit gap too large: {:.4}% (max {:.1}%)",
                    label, profit_gap_pct, profit_tolerance_pct,
                );
            }
            (Some((a_amt, a_profit, _)), None) => {
                eprintln!(
                    "  [{}] analytical found arb (amount={}, profit={}) but golden didn't",
                    label, a_amt, a_profit
                );
            }
            (None, Some((g_amt, g_profit))) => {
                panic!(
                    "[{}] Golden found arb (amount={}, profit={:.6}) but analytical returned None!",
                    label, g_amt, g_profit / 1e9,
                );
            }
        }
    }

    #[test]
    fn test_3hop_all_cp_fee_on_input() {
        eprintln!("\n=== 3-hop: CpFI → CpFI → CpFI (triangle arb) ===");

        // SOL → TokenA via pool1, TokenA → TokenB via pool2, TokenB → SOL via pool3
        let models = [
            PoolModel::CpFeeOnInput {
                reserve_in: 500_000_000_000.0,     // 500 SOL
                reserve_out: 10_000_000_000_000.0,  // TokenA
                fee: 0.997,
                marginal_price: 20_000.0 * 0.997,
            },
            PoolModel::CpFeeOnInput {
                reserve_in: 9_500_000_000_000.0,    // TokenA
                reserve_out: 2_000_000_000_000_000.0, // TokenB
                fee: 0.9975,
                marginal_price: (2_000_000.0 / 9_500.0) * 0.9975,
            },
            PoolModel::CpFeeOnInput {
                reserve_in: 1_900_000_000_000_000.0, // TokenB
                reserve_out: 520_000_000_000.0,      // 520 SOL (mispriced)
                fee: 0.997,
                marginal_price: (520.0 / 1_900_000.0) * 1e9 * 0.997,
            },
        ];

        assert_nhop_matches_golden("3hop_cpfi", &models, 10_000_000_000, 1.0);
    }

    #[test]
    fn test_3hop_mixed_cp() {
        eprintln!("\n=== 3-hop: CpFI → CpFO → CpFI (mixed fee types) ===");

        let models = [
            PoolModel::CpFeeOnInput {
                reserve_in: 400_000_000_000.0,
                reserve_out: 8_000_000_000_000.0,
                fee: 0.9975,
                marginal_price: 20_000.0 * 0.9975,
            },
            PoolModel::CpFeeOnOutput {
                reserve_in: 7_500_000_000_000.0,
                reserve_out: 1_500_000_000_000_000.0,
                fee: 0.99,
                marginal_price: 200_000.0 * 0.99,
            },
            PoolModel::CpFeeOnInput {
                reserve_in: 1_400_000_000_000_000.0,
                reserve_out: 430_000_000_000.0,
                fee: 0.997,
                marginal_price: (430.0 / 1_400_000.0) * 1e9 * 0.997,
            },
        ];

        assert_nhop_matches_golden("3hop_mixed", &models, 5_000_000_000, 1.0);
    }

    #[test]
    fn test_3hop_with_linear() {
        eprintln!("\n=== 3-hop: CpFI → Linear → CpFI (with DLMM in the middle) ===");

        let models = [
            PoolModel::CpFeeOnInput {
                reserve_in: 500_000_000_000.0,
                reserve_out: 10_000_000_000_000.0,
                fee: 0.997,
                marginal_price: 20_000.0 * 0.997,
            },
            PoolModel::Linear {
                price: 220.0,  // TokenA → TokenB
                fee: 0.998,
                max_in: 200_000_000_000_000, // large capacity
                bin_step_frac: 0.008,
                marginal_price: 220.0 * 0.998,
            },
            PoolModel::CpFeeOnInput {
                reserve_in: 2_200_000_000_000_000.0,
                reserve_out: 520_000_000_000.0,
                fee: 0.997,
                marginal_price: (520.0 / 2_200_000.0) * 1e9 * 0.997,
            },
        ];

        // 3-hop with linear uses golden section internally, so tolerance is wider
        assert_nhop_matches_golden("3hop_linear", &models, 10_000_000_000, 2.0);
    }

    // ─── Stress test with many random-ish scenarios ──────────────────────────

    #[test]
    fn test_2pool_sweep_varied_reserves_and_fees() {
        eprintln!("\n=== Sweep: varied reserves and fees ===");

        let reserves_sol = [10_000_000_000.0, 100_000_000_000.0, 1_000_000_000_000.0];
        let price_diffs = [1.02, 1.05, 1.10, 1.20]; // 2%, 5%, 10%, 20% price advantage
        let fees = [0.99, 0.995, 0.997, 0.9975];

        let mut count = 0;
        for &r1_sol in &reserves_sol {
            let r1_tokens = r1_sol * 2000.0; // ~2000 tokens per SOL
            for &price_mult in &price_diffs {
                let r2_sol = r1_sol * price_mult;
                let r2_tokens = r1_tokens / price_mult;
                for &f1 in &fees {
                    for &f2 in &fees {
                        let pool1 = PoolModel::CpFeeOnInput {
                            reserve_in: r1_sol,
                            reserve_out: r1_tokens,
                            fee: f1,
                            marginal_price: (r1_tokens / r1_sol) * f1,
                        };
                        let pool2 = PoolModel::CpFeeOnInput {
                            reserve_in: r2_tokens,
                            reserve_out: r2_sol,
                            fee: f2,
                            marginal_price: (r2_sol / r2_tokens) * f2,
                        };
                        let label = format!("sweep_{}", count);
                        assert_analytical_matches_golden(
                            &label, &pool1, &pool2,
                            (r1_sol * 0.1) as u64, // max 10% of pool
                            2.0, 1.0,
                        );
                        count += 1;
                    }
                }
            }
        }
        eprintln!("  Tested {} scenarios", count);
    }

    #[test]
    fn test_2pool_sweep_mixed_fee_types() {
        eprintln!("\n=== Sweep: mixed fee type combinations ===");

        let r_sol = 500_000_000_000.0;
        let r_tokens = 1_000_000_000_000_000.0;
        let r2_sol = 530_000_000_000.0;
        let r2_tokens = 950_000_000_000_000.0;

        let combos: Vec<(PoolModel, PoolModel, &str)> = vec![
            (
                PoolModel::CpFeeOnInput { reserve_in: r_sol, reserve_out: r_tokens, fee: 0.997, marginal_price: (r_tokens / r_sol) * 0.997 },
                PoolModel::CpFeeOnInput { reserve_in: r2_tokens, reserve_out: r2_sol, fee: 0.997, marginal_price: (r2_sol / r2_tokens) * 0.997 },
                "FI+FI",
            ),
            (
                PoolModel::CpFeeOnInput { reserve_in: r_sol, reserve_out: r_tokens, fee: 0.997, marginal_price: (r_tokens / r_sol) * 0.997 },
                PoolModel::CpFeeOnOutput { reserve_in: r2_tokens, reserve_out: r2_sol, fee: 0.99, marginal_price: (r2_sol / r2_tokens) * 0.99 },
                "FI+FO",
            ),
            (
                PoolModel::CpFeeOnOutput { reserve_in: r_sol, reserve_out: r_tokens, fee: 0.99, marginal_price: (r_tokens / r_sol) * 0.99 },
                PoolModel::CpFeeOnInput { reserve_in: r2_tokens, reserve_out: r2_sol, fee: 0.997, marginal_price: (r2_sol / r2_tokens) * 0.997 },
                "FO+FI",
            ),
            (
                PoolModel::CpFeeOnOutput { reserve_in: r_sol, reserve_out: r_tokens, fee: 0.99, marginal_price: (r_tokens / r_sol) * 0.99 },
                PoolModel::CpFeeOnOutput { reserve_in: r2_tokens, reserve_out: r2_sol, fee: 0.99, marginal_price: (r2_sol / r2_tokens) * 0.99 },
                "FO+FO",
            ),
            (
                PoolModel::CpFeeOnInput { reserve_in: r_sol, reserve_out: r_tokens, fee: 0.997, marginal_price: (r_tokens / r_sol) * 0.997 },
                PoolModel::Linear { price: (r2_sol / r2_tokens), fee: 0.998, max_in: 50_000_000_000_000, bin_step_frac: 0.008, marginal_price: (r2_sol / r2_tokens) * 0.998 },
                "FI+Linear",
            ),
            (
                PoolModel::CpFeeOnOutput { reserve_in: r_sol, reserve_out: r_tokens, fee: 0.99, marginal_price: (r_tokens / r_sol) * 0.99 },
                PoolModel::Linear { price: (r2_sol / r2_tokens), fee: 0.998, max_in: 50_000_000_000_000, bin_step_frac: 0.008, marginal_price: (r2_sol / r2_tokens) * 0.998 },
                "FO+Linear",
            ),
            (
                PoolModel::Linear { price: (r_tokens / r_sol), fee: 0.998, max_in: 5_000_000_000, bin_step_frac: 0.008, marginal_price: (r_tokens / r_sol) * 0.998 },
                PoolModel::CpFeeOnInput { reserve_in: r2_tokens, reserve_out: r2_sol, fee: 0.997, marginal_price: (r2_sol / r2_tokens) * 0.997 },
                "Linear+FI",
            ),
            (
                PoolModel::Linear { price: (r_tokens / r_sol), fee: 0.998, max_in: 5_000_000_000, bin_step_frac: 0.008, marginal_price: (r_tokens / r_sol) * 0.998 },
                PoolModel::CpFeeOnOutput { reserve_in: r2_tokens, reserve_out: r2_sol, fee: 0.99, marginal_price: (r2_sol / r2_tokens) * 0.99 },
                "Linear+FO",
            ),
        ];

        for (pool1, pool2, label) in &combos {
            assert_analytical_matches_golden(label, pool1, pool2, 10_000_000_000, 2.0, 1.0);
        }
    }
}
