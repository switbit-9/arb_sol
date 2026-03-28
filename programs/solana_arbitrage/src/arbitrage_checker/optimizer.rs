use super::wide_math;
use super::FD;

// ── Integer square root (Newton's method, ~8 iterations for u128) ──

/// Integer square root of a u128. Returns floor(sqrt(x)).
#[inline]
pub fn isqrt_u128(x: u128) -> u128 {
    if x <= 1 {
        return x;
    }
    // Initial guess: 2^(ceil(bits/2))
    let bits = 128 - x.leading_zeros();
    let mut r = 1u128 << ((bits + 1) / 2);
    // Newton iterations (converges in ~7 steps for u128)
    loop {
        let next = (r + x / r) >> 1;
        if next >= r {
            return r;
        }
        r = next;
    }
}

/// Integer square root of a U256 (ruint). Returns floor(sqrt(x)).
/// Kept only for test cross-validation. Not used in production paths.
#[cfg(test)]
pub fn isqrt_u256(x: ruint::aliases::U256) -> ruint::aliases::U256 {
    if x <= ruint::aliases::U256::from(1u64) {
        return x;
    }
    let bits = 256 - x.leading_zeros();
    let mut r = ruint::aliases::U256::from(1u64) << ((bits + 1) / 2);
    loop {
        let next = (r + x / r) >> 1;
        if next >= r {
            return r;
        }
        r = next;
    }
}

// ── Analytical optimal for AMM-AMM (two CP pools) ──

/// Analytical optimal amount_in for 2-hop arb between two CP AMM pools.
///
/// Path: quote_in -> Pool A (buy base) -> base -> Pool B (sell base) -> quote_out
///
/// Pool A: buy side. reserves (quote_a, base_a), fees (buy_input_fee_a, buy_output_fee_a)
/// Pool B: sell side. reserves (base_b, quote_b), fees (sell_input_fee_b, sell_output_fee_b)
///
/// Returns (optimal_amount_in, profit) or (0, 0) if no arb.
///
/// Derivation (generalized with 4 fee factors):
///   fi_a = FD - buy_input_fee_a,  fo_a = FD - buy_output_fee_a
///   fi_b = FD - sell_input_fee_b, fo_b = FD - sell_output_fee_b
///
///   u = amount_in * fi_a / FD
///   mid_raw = base_a * u / (quote_a + u)
///   mid = mid_raw * fo_a / FD
///   m = mid * fi_b / FD
///   raw_out = quote_b * m / (base_b + m)
///   out = raw_out * fo_b / FD
///
///   Combined fee product: combined = fi_a * fo_a * fi_b * fo_b (≤ 10^24, fits u128)
///   K = quote_a * base_a * base_b * quote_b * combined / FD^4
///   A = base_a * fo_a * fi_b / FD^2
///   u_opt = (sqrt(K) - base_b * quote_a) / (base_b + A)
///   amount_in = u_opt * FD / fi_a
pub fn optimal_amm_amm(
    quote_a: u64,
    base_a: u64,
    buy_input_fee_a: u64,
    buy_output_fee_a: u64,
    base_b: u64,
    quote_b: u64,
    sell_input_fee_b: u64,
    sell_output_fee_b: u64,
) -> (u64, i128) {
    let qa = quote_a as u128;
    let ba = base_a as u128;
    let bb = base_b as u128;
    let qb = quote_b as u128;
    let fi_a = FD - buy_input_fee_a as u128;
    let fo_a = FD - buy_output_fee_a as u128;
    let fi_b = FD - sell_input_fee_b as u128;
    let fo_b = FD - sell_output_fee_b as u128;

    // combined fee product (all ≤ FD, so product ≤ FD^4 = 10^24, fits u128)
    let combined = fi_a * fo_a / FD * (fi_b * fo_b / FD);
    // K = qb * combined * bb * ba * qa / FD^2
    let fd_sq = FD * FD;
    let sqrt_k = wide_math::sqrt_mul5_div(qb, combined, bb, ba, qa, fd_sq);
    let bb_qa = bb * qa;

    if sqrt_k <= bb_qa {
        return (0, 0);
    }

    // A = ba * fo_a * fi_b / FD^2
    let a_val = ba * (fo_a * fi_b / fd_sq).max(1).max(
        // More precise: compute ba * fo_a * fi_b / FD^2 avoiding early truncation
        // by doing (ba * fo_a / FD) * fi_b / FD when possible
        0
    );
    // Precise computation of A
    let a_val = if let Some(v) = ba.checked_mul(fo_a).and_then(|v| v.checked_mul(fi_b)) {
        v / fd_sq
    } else {
        // Wide fallback
        wide_math::mul4_div(ba, fo_a, fi_b, 1, fd_sq)
    };

    let u_denom = bb + a_val;
    if u_denom == 0 {
        return (0, 0);
    }

    let u_opt = (sqrt_k - bb_qa) / u_denom;

    let amount_in: u64 = {
        let v = u_opt * FD / fi_a;
        if v > u64::MAX as u128 { return (0, 0); }
        v as u64
    };

    if amount_in == 0 {
        return (0, 0);
    }

    let profit = amm_amm_profit(qa, ba, fi_a, fo_a, bb, qb, fi_b, fo_b, amount_in);

    if profit <= 0 {
        (0, 0)
    } else {
        (amount_in, profit)
    }
}

/// Compute profit for an AMM-AMM arb at given amount_in.
#[inline]
fn amm_amm_profit(
    qa: u128, ba: u128, fi_a: u128, fo_a: u128,
    bb: u128, qb: u128, fi_b: u128, fo_b: u128,
    amount_in: u64,
) -> i128 {
    let x = amount_in as u128;

    // Buy: in_eff = x * fi_a / FD, mid_raw = ba * in_eff / (qa + in_eff), mid = mid_raw * fo_a / FD
    let in_eff = x * fi_a / FD;
    let denom_a = qa + in_eff;
    if denom_a == 0 { return -(x as i128); }
    let mid_raw = ba * in_eff / denom_a;
    let mid = mid_raw * fo_a / FD;

    // Sell: m = mid * fi_b / FD, raw_out = qb * m / (bb + m), out = raw_out * fo_b / FD
    let m = mid * fi_b / FD;
    let denom_b = bb + m;
    if denom_b == 0 { return -(x as i128); }
    let raw_out = qb * m / denom_b;
    let out = raw_out * fo_b / FD;

    out as i128 - x as i128
}

// ── Analytical optimal for DLMM-bin + AMM CP (one bin, linear DLMM rate vs CP curve) ──

/// For a single DLMM bin feeding into an AMM CP swap:
///
/// DLMM gives: mid = amount_in * rate_num / rate_den
/// AMM CP:     in_eff = mid * cp_input_ff / FD
///             raw_out = reserve_out * in_eff / (reserve_in + in_eff)
///             out = raw_out * cp_output_ff / FD
///
/// The input fee is absorbed into the rate for the analytical solution:
///   rate_num' = rate_num * cp_input_ff,  rate_den' = rate_den * FD
///
/// Then the formula reduces to the single-fee-factor form with cp_output_ff as fee_factor.
pub fn optimal_linear_vs_cp(
    rate_num: u128,
    rate_den: u128,
    max_in: u64,
    reserve_in: u128,
    reserve_out: u128,
    cp_input_ff: u128,  // FD - input_fee_numerator
    cp_output_ff: u128, // FD - output_fee_numerator
) -> (u64, i128) {
    if rate_den == 0 || rate_num == 0 || reserve_in == 0 || cp_input_ff == 0 {
        return (0, 0);
    }

    // Absorb cp_input_ff into the rate
    let rn = rate_num * cp_input_ff;
    let rd = rate_den * FD;
    let ff = cp_output_ff;

    // S = reserve_in * rd
    // K = reserve_out * ff * rn * reserve_in * rd / FD
    //   = reserve_out * ff * rn * reserve_in * rate_den   (since rd = rate_den * FD)
    // x = (sqrt(K) - S) / rn
    //
    // NOTE: We use rate_den (not rd) in the product to avoid U256 overflow.
    // The full product reserve_out * ff * rn * reserve_in * rd can exceed 256 bits
    // when reserves are large (e.g. 100T+ tokens), corrupting the wide-math result.
    // Factoring out FD keeps the product within ~240 bits.

    let s_u128 = reserve_in.checked_mul(rd);

    // Fast path: try u128
    let (sqrt_k_u128, s_val) = {
        let k_u128 = reserve_out
            .checked_mul(ff)
            .and_then(|v| v.checked_mul(rn))
            .and_then(|v| v.checked_mul(reserve_in))
            .and_then(|v| v.checked_mul(rate_den));
        if let (Some(k_val), Some(sv)) = (k_u128, s_u128) {
            // Pre-check: if K <= S², then sqrt(K) <= S and there's no arb.
            // A wide multiply (~20 CU) to skip isqrt (~1400 CU) in the common no-arb case.
            let s_sq = wide_math::wmul(sv, sv);
            let k_wide = wide_math::U256::from_u128(k_val);
            if k_wide.hi < s_sq.hi || (k_wide.hi == s_sq.hi && k_wide.lo <= s_sq.lo) {
                debug_eprintln!(
                    "[optimal_linear_vs_cp] u128 path: K<=S² → no arb | K={} S={} rn={} rd={} ff={} res_in={} res_out={}",
                    k_val, sv, rn, rd, ff, reserve_in, reserve_out
                );
                return (0, 0);
            }
            (Some(isqrt_u128(k_val)), sv)
        } else {
            debug_eprintln!(
                "[optimal_linear_vs_cp] u128 overflow → wide path | rn={} rd={} ff={} res_in={} res_out={} rate_den={}",
                rn, rd, ff, reserve_in, reserve_out, rate_den
            );
            (None, s_u128.unwrap_or(u128::MAX))
        }
    };

    let (_sqrt_k, x_opt): (u128, u128) = if let Some(sq) = sqrt_k_u128 {
        if sq <= s_val {
            debug_eprintln!(
                "[optimal_linear_vs_cp] u128 path: sqrt_k<=S → no arb | sqrt_k={} S={} diff={}",
                sq, s_val, s_val - sq
            );
            return (0, 0);
        }
        let x = (sq - s_val) / rn;
        debug_eprintln!(
            "[optimal_linear_vs_cp] u128 path: sqrt_k={} S={} diff={} x_opt={}",
            sq, s_val, sq - s_val, x
        );
        (sq, x)
    } else {
        // Wide math fallback — use rate_den (not rd) to stay within 256 bits
        let sq = wide_math::sqrt_mul5_div(reserve_out, ff, rn, reserve_in, rate_den, 1);
        let s_wide = wide_math::wmul(reserve_in, rd);
        let sq_wide = wide_math::U256::from_u128(sq);
        if sq_wide.hi < s_wide.hi || (sq_wide.hi == s_wide.hi && sq_wide.lo <= s_wide.lo) {
            debug_eprintln!(
                "[optimal_linear_vs_cp] wide path: sqrt_k<=S → no arb | sqrt_k={} S_hi={} S_lo={}",
                sq, s_wide.hi, s_wide.lo
            );
            return (0, 0);
        }
        let diff = wide_math::sub_u256_pub(sq_wide, s_wide);
        let x = wide_math::wdiv(diff, rn);
        debug_eprintln!(
            "[optimal_linear_vs_cp] wide path: sqrt_k={} S_lo={} diff_lo={} x_opt={}",
            sq, s_wide.lo, diff.lo, x
        );
        (sq, x)
    };

    let amount_in: u64 = if x_opt > max_in as u128 {
        max_in
    } else {
        x_opt as u64
    };

    if amount_in == 0 {
        debug_eprintln!(
            "[optimal_linear_vs_cp] amount_in=0 after clamp (x_opt={} max_in={})",
            x_opt, max_in
        );
        return (0, 0);
    }

    // Compute actual profit using the adjusted rate
    let mid = (amount_in as u128) * rn / rd;
    let denom = reserve_in + mid;
    if denom == 0 {
        return (0, 0);
    }
    let raw_out = reserve_out * mid / denom;
    let out = raw_out * ff / FD;

    let profit = out as i128 - amount_in as i128;

    if profit <= 0 {
        (0, 0)
    } else {
        (amount_in, profit)
    }
}

// ── Lean ternary search (integer-only, for DLMM-DLMM or general fallback) ──

/// Integer-only ternary search for maximum of a concave function.
/// Uses at most `max_iter` iterations (22 is enough for 10^10 range to ±1).
/// No f64 operations.
pub fn ternary_search_maximize<F>(
    profit_fn: F,
    lo: u64,
    hi: u64,
    max_iter: u32,
) -> (u64, i128)
where
    F: Fn(u64) -> i128,
{
    if hi <= lo {
        let p = profit_fn(lo);
        return if p > 0 { (lo, p) } else { (0, 0) };
    }

    let mut a = lo;
    let mut b = hi;

    for _ in 0..max_iter {
        if b - a <= 2 {
            break;
        }
        let m1 = a + (b - a) / 3;
        let m2 = b - (b - a) / 3;
        if profit_fn(m1) < profit_fn(m2) {
            a = m1;
        } else {
            b = m2;
        }
    }

    // Check all remaining candidates
    let mut best = (0u64, 0i128);
    for x in a..=b {
        let p = profit_fn(x);
        if p > best.1 {
            best = (x, p);
        }
    }
    if best.1 <= 0 {
        (0, 0)
    } else {
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_isqrt() {
        assert_eq!(isqrt_u128(0), 0);
        assert_eq!(isqrt_u128(1), 1);
        assert_eq!(isqrt_u128(4), 2);
        assert_eq!(isqrt_u128(9), 3);
        assert_eq!(isqrt_u128(10), 3);
        assert_eq!(isqrt_u128(100), 10);
        assert_eq!(isqrt_u128(u128::MAX), 18446744073709551615); // 2^64 - 1
    }

    #[test]
    fn test_optimal_amm_amm_no_arb() {
        // Same pool state on both sides — no arb after fees
        // Pump-style: buy=(fee,0), sell=(0,fee)
        let (amt, profit) = optimal_amm_amm(
            50_000_000_000, 1_000_000_000_000, 3000, 0,
            1_000_000_000_000, 50_000_000_000, 0, 3000,
        );
        assert_eq!(profit, 0);
        assert_eq!(amt, 0);
    }

    #[test]
    fn test_optimal_amm_amm_with_arb() {
        // Pool A: cheap base. buy_input=3000, buy_output=0 (Pump-style)
        // Pool B: expensive base. sell_input=0, sell_output=3000
        let (amt, profit) = optimal_amm_amm(
            50_000_000_000,    // quote_a
            1_000_000_000_000, // base_a
            3000, 0,           // buy fees A
            500_000_000_000,   // base_b
            100_000_000_000,   // quote_b
            0, 3000,           // sell fees B
        );
        assert!(profit > 0, "should find arb, profit={}", profit);
        assert!(amt > 0, "should have positive amount_in={}", amt);
    }

    #[test]
    fn test_ternary_search() {
        let (amt, profit) = ternary_search_maximize(
            |x| {
                let diff = x as i128 - 1000;
                500_000 - diff * diff
            },
            1,
            10_000,
            30,
        );
        assert!((amt as i128 - 1000).abs() <= 1, "got {}", amt);
        assert!(profit >= 499_999);
    }

    #[test]
    fn test_linear_vs_cp() {
        // Single fee_factor case: cp_input_ff=FD (no input fee), cp_output_ff=970_000
        let (amt, profit) = optimal_linear_vs_cp(
            10, 1,
            1_000_000_000,
            100_000_000,
            1_000_000_000,
            1_000_000,  // cp_input_ff = FD (no input fee)
            970_000,    // cp_output_ff (3% output fee)
        );
        assert!(amt > 0, "should find optimal, amt={}", amt);
        assert!(profit > 0, "should be profitable, profit={}", profit);
    }

    #[test]
    fn test_linear_vs_cp_large_reserves() {
        // Regression: large reserves caused U256 overflow in sqrt_mul5_div,
        // making the checker miss profitable DLMM→PumpAmm arb opportunities.
        // Values from a real pool: Azda DLMM → PumpAmm EgpJVVi.
        let (amt, profit) = optimal_linear_vs_cp(
            4224794050143715328,   // rate_num (DLMM bin rate, swap_for_y=false)
            10230243000000000,     // rate_den
            735011,                // max_in (single bin capacity)
            114561604622484,       // reserve_in (PumpAmm base_vault, ~114T tokens)
            284125788583,          // reserve_out (PumpAmm quote_vault, ~284B lamports)
            1_000_000,             // cp_input_ff = FD (no input fee)
            989_000,               // cp_output_ff (1.1% output fee)
        );
        assert!(amt > 0, "should find arb with large reserves, amt={}", amt);
        assert!(profit > 0, "should be profitable, profit={}", profit);
        // Expected: amt=735011 (clamped to max_in), profit≈9515
        assert_eq!(amt, 735011);
        assert!(profit > 9000 && profit < 10000, "unexpected profit={}", profit);
    }
}
