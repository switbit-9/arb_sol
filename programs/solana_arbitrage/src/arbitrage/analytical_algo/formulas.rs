use super::pool_model::PoolModel;
use crate::programs::ProgramMeta;
use anchor_lang::prelude::*;

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

    
    #[cfg(test)]
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

        debug_eprintln!("");
        debug_eprintln!(
            "ESTIMATE {}+{} P={:.4}%, F1={:.4}, F2={:.4}, profit={:.6} (input={:.6} mid={:.6} out={:.6} dlmm={})",
            pool1.label(), pool2.label(), price_diff_pct, f1, f2, profit / 1e9,
            input_amount / 1e9, middle_amount / 1e9, output_amount / 1e9, result.dlmm_capped
        );
        debug_eprintln!("");
    }

    if profit <= 0.0 {
        return None;
    }

    Some((result.optimal_amount, profit as i128, result.dlmm_capped))
}

/// Compute the output of a single pool for a given input amount.
/// Uses the analytical model (no on-chain simulation).
fn pool_output(model: &PoolModel, dx: f64) -> f64 {
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
    let (dlmm_pos, cp_fee_type, r_in, r_out, f_amm, bin_step_frac) = match (pool1, pool2) {
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

    if bin_step_frac <= 0.0 {
        return None;
    }

    let mut cumulative_input: f64 = 0.0;   // cumulative DLMM input consumed by prior bins
    let mut cumulative_output: f64 = 0.0;  // cumulative DLMM output from prior bins
    let mut best_amount: f64 = 0.0;
    let mut best_profit: f64 = 0.0;
    let max_f = max_amount_in as f64;

    for bin_offset in 0..70i32 {  // max 70 bins per array, safety limit
        let (bin_slope, bin_capacity) = match dlmm_instance.get_bin_segment(accounts, input_mint, bin_offset) {
            Ok(Some((s, c))) if s > 0.0 && c > 0 => (s, c as f64),
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

        // Check segment validity: does segment_optimal fall within this bin's range?
        let in_segment = match dlmm_pos {
            DlmmPosition::Pool2 => {
                // mid-amount must fall in [cumulative_input, cumulative_input + bin_capacity]
                let mid = match cp_fee_type {
                    CpFeeType::OnInput => r_out * segment_optimal * f_amm / (r_in + segment_optimal * f_amm),
                    CpFeeType::OnOutput => f_amm * r_out * segment_optimal / (r_in + segment_optimal),
                };
                mid >= cumulative_input && mid <= cumulative_input + bin_capacity
            }
            DlmmPosition::Pool1 => {
                // dx must fall in [cumulative_input, cumulative_input + bin_capacity]
                segment_optimal >= cumulative_input && segment_optimal <= cumulative_input + bin_capacity
            }
        };

        if in_segment {
            // Found the optimal segment — compute profit
            let clamped_amount = segment_optimal.min(max_f);
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
        cumulative_output += bin_capacity * bin_slope;
        cumulative_input += bin_capacity;

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

        // Check: can the arb margin survive the next bin step?
        let profit_pct = if boundary_amount > 0.0 { boundary_profit / boundary_amount } else { 0.0 };
        if profit_pct <= bin_step_frac {
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
