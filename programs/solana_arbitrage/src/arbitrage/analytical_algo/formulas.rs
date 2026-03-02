use super::pool_model::PoolModel;

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
                r_in: r1_in,
                r_out: r1_out,
                fee: f1,
            },
            PoolModel::CpFeeOnInput {
                r_in: r2_in,
                r_out: r2_out,
                fee: f2,
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
                r_in: r1_in,
                r_out: r1_out,
                fee: f1,
            },
            PoolModel::CpFeeOnOutput {
                r_in: r2_in,
                r_out: r2_out,
                fee: f2,
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
                r_in: r1_in,
                r_out: r1_out,
                fee: f1,
            },
            PoolModel::CpFeeOnInput {
                r_in: r2_in,
                r_out: r2_out,
                fee: f2,
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
                r_in: r1_in,
                r_out: r1_out,
                fee: f1,
            },
            PoolModel::CpFeeOnOutput {
                r_in: r2_in,
                r_out: r2_out,
                fee: f2,
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
                r_in,
                r_out,
                fee: f_amm,
            },
            PoolModel::Linear {
                price: p,
                fee: f_dlmm,
                max_in: dlmm_max_in,
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

            // When capped, clamp dx so mid = dlmm_max_in
            let dx = if capped {
                let max_in = *dlmm_max_in as f64;
                if *r_out <= max_in { return None; }
                let u = max_in * r_in / (r_out - max_in);
                u / f_amm
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
                r_in,
                r_out,
                fee: f_amm,
            },
            PoolModel::Linear {
                price: p,
                fee: f_dlmm,
                max_in: dlmm_max_in,
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
            },
            PoolModel::CpFeeOnInput {
                r_in,
                r_out,
                fee: f_amm,
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
            },
            PoolModel::CpFeeOnOutput {
                r_in,
                r_out,
                fee: f_amm,
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
            },
            PoolModel::Linear {
                price: p2,
                fee: f2,
                max_in: max_in_2,
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

        // ── Any combination involving Opaque ───────────────────────────
        (PoolModel::Opaque, _) | (_, PoolModel::Opaque) => None,
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
) -> Option<(u64, i128, bool)> {
    let result = analytical_optimal_2pool(pool1, pool2, max_amount_in)?;
    if result.optimal_amount == 0 {
        return None;
    }
    let dx = result.optimal_amount as f64;

    // Compute estimated output through pool1 then pool2
    let mid = pool_output(pool1, dx);
    if mid <= 0.0 {
        return None;
    }
    let out = pool_output(pool2, mid);
    let profit = out - dx;
    if profit <= 0.0 {
        return None;
    }

    Some((result.optimal_amount, profit as i128, result.dlmm_capped))
}

/// Compute the output of a single pool for a given input amount.
/// Uses the analytical model (no on-chain simulation).
fn pool_output(model: &PoolModel, dx: f64) -> f64 {
    match model {
        PoolModel::CpFeeOnInput { r_in, r_out, fee } => {
            let u = dx * fee;
            r_out * u / (r_in + u)
        }
        PoolModel::CpFeeOnOutput { r_in, r_out, fee } => {
            fee * r_out * dx / (r_in + dx)
        }
        PoolModel::Linear { price, fee, max_in } => {
            let clamped = dx.min(*max_in as f64);
            clamped * price * fee
        }
        PoolModel::Opaque => 0.0,
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
