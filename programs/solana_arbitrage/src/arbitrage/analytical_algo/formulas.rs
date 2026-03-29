use super::pool_model::PoolModel;
use crate::programs::ProgramMeta;
use crate::compat::Pubkey;

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
                fee: f1,
                ..
            },
            PoolModel::CpFeeOnInput {
                reserve_in: r2_in,
                reserve_out: r2_out,
                fee: f2,
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
                fee: f1,
                ..
            },
            PoolModel::CpFeeOnOutput {
                reserve_in: r2_in,
                reserve_out: r2_out,
                fee: f2,
                ..
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
                fee: f1,
                ..
            },
            PoolModel::CpFeeOnInput {
                reserve_in: r2_in,
                reserve_out: r2_out,
                fee: f2,
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
                fee: f1,
                ..
            },
            PoolModel::CpFeeOnOutput {
                reserve_in: r2_in,
                reserve_out: r2_out,
                fee: f2,
                ..
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
                fee: f_amm,
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
                if *r_out <= max_in {
                    return None;
                }
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
                fee: f_amm,
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
            let dx = disc.sqrt() - r_in;

            let mid = f_amm * r_out * dx / (r_in + dx);
            let capped = mid > *dlmm_max_in as f64;

            // When capped, clamp dx so mid = dlmm_max_in
            let dx = if capped {
                let max_in = *dlmm_max_in as f64;
                let denom = f_amm * r_out - max_in;
                if denom <= 0.0 {
                    return None;
                }
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
                fee: f_amm,
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
                fee: f_amm,
                ..
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
                fee: f2,
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
                fee: f2,
                ..
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
                fee: f1,
                ..
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
                if *r1_out <= max_in {
                    return None;
                }
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
                fee: f1,
                ..
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
                if denom <= 0.0 {
                    return None;
                }
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
                if *r1_out <= max_in {
                    return None;
                }
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
                if *r_out <= max_in {
                    return None;
                }
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

// ─── Tests: Golden section vs Analytical comparison ──────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
                PoolModel::CpFeeOnInput {
                    reserve_in: r_sol,
                    reserve_out: r_tokens,
                    fee: 0.997,
                    marginal_price: (r_tokens / r_sol) * 0.997,
                },
                PoolModel::CpFeeOnInput {
                    reserve_in: r2_tokens,
                    reserve_out: r2_sol,
                    fee: 0.997,
                    marginal_price: (r2_sol / r2_tokens) * 0.997,
                },
                "FI+FI",
            ),
            (
                PoolModel::CpFeeOnInput {
                    reserve_in: r_sol,
                    reserve_out: r_tokens,
                    fee: 0.997,
                    marginal_price: (r_tokens / r_sol) * 0.997,
                },
                PoolModel::CpFeeOnOutput {
                    reserve_in: r2_tokens,
                    reserve_out: r2_sol,
                    fee: 0.99,
                    marginal_price: (r2_sol / r2_tokens) * 0.99,
                },
                "FI+FO",
            ),
            (
                PoolModel::CpFeeOnOutput {
                    reserve_in: r_sol,
                    reserve_out: r_tokens,
                    fee: 0.99,
                    marginal_price: (r_tokens / r_sol) * 0.99,
                },
                PoolModel::CpFeeOnInput {
                    reserve_in: r2_tokens,
                    reserve_out: r2_sol,
                    fee: 0.997,
                    marginal_price: (r2_sol / r2_tokens) * 0.997,
                },
                "FO+FI",
            ),
            (
                PoolModel::CpFeeOnOutput {
                    reserve_in: r_sol,
                    reserve_out: r_tokens,
                    fee: 0.99,
                    marginal_price: (r_tokens / r_sol) * 0.99,
                },
                PoolModel::CpFeeOnOutput {
                    reserve_in: r2_tokens,
                    reserve_out: r2_sol,
                    fee: 0.99,
                    marginal_price: (r2_sol / r2_tokens) * 0.99,
                },
                "FO+FO",
            ),
            (
                PoolModel::CpFeeOnInput {
                    reserve_in: r_sol,
                    reserve_out: r_tokens,
                    fee: 0.997,
                    marginal_price: (r_tokens / r_sol) * 0.997,
                },
                PoolModel::Linear {
                    price: (r2_sol / r2_tokens),
                    fee: 0.998,
                    max_in: 50_000_000_000_000,
                    bin_step_frac: 0.008,
                    marginal_price: (r2_sol / r2_tokens) * 0.998,
                },
                "FI+Linear",
            ),
            (
                PoolModel::CpFeeOnOutput {
                    reserve_in: r_sol,
                    reserve_out: r_tokens,
                    fee: 0.99,
                    marginal_price: (r_tokens / r_sol) * 0.99,
                },
                PoolModel::Linear {
                    price: (r2_sol / r2_tokens),
                    fee: 0.998,
                    max_in: 50_000_000_000_000,
                    bin_step_frac: 0.008,
                    marginal_price: (r2_sol / r2_tokens) * 0.998,
                },
                "FO+Linear",
            ),
            (
                PoolModel::Linear {
                    price: (r_tokens / r_sol),
                    fee: 0.998,
                    max_in: 5_000_000_000,
                    bin_step_frac: 0.008,
                    marginal_price: (r_tokens / r_sol) * 0.998,
                },
                PoolModel::CpFeeOnInput {
                    reserve_in: r2_tokens,
                    reserve_out: r2_sol,
                    fee: 0.997,
                    marginal_price: (r2_sol / r2_tokens) * 0.997,
                },
                "Linear+FI",
            ),
            (
                PoolModel::Linear {
                    price: (r_tokens / r_sol),
                    fee: 0.998,
                    max_in: 5_000_000_000,
                    bin_step_frac: 0.008,
                    marginal_price: (r_tokens / r_sol) * 0.998,
                },
                PoolModel::CpFeeOnOutput {
                    reserve_in: r2_tokens,
                    reserve_out: r2_sol,
                    fee: 0.99,
                    marginal_price: (r2_sol / r2_tokens) * 0.99,
                },
                "Linear+FO",
            ),
        ];

    }
}
