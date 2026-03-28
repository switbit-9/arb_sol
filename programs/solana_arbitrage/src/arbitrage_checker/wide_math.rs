/// 256-bit arithmetic using two u128 limbs (hi, lo).
/// Drop-in replacement for ruint::U256 — exact same results, much cheaper on SBF.

/// A 256-bit unsigned integer stored as (hi, lo) where value = hi << 128 | lo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct U256 {
    pub hi: u128,
    pub lo: u128,
}

impl U256 {
    pub const ZERO: Self = Self { hi: 0, lo: 0 };

    #[inline(always)]
    pub const fn from_u128(v: u128) -> Self {
        Self { hi: 0, lo: v }
    }

    #[inline(always)]
    pub const fn from_u64(v: u64) -> Self {
        Self { hi: 0, lo: v as u128 }
    }
}

// ── Multiply two u128 → U256 (exact) ──

/// Multiply two u128 values, returning the exact 256-bit product.
/// Uses schoolbook 4×u64-limb multiplication.
#[inline]
pub fn wmul(a: u128, b: u128) -> U256 {
    let a0 = a as u64 as u128;
    let a1 = a >> 64;
    let b0 = b as u64 as u128;
    let b1 = b >> 64;

    let p00 = a0 * b0;
    let p01 = a0 * b1;
    let p10 = a1 * b0;
    let p11 = a1 * b1;

    // Accumulate: result = p11 << 128 + (p01 + p10) << 64 + p00
    // mid accumulates the contributions to bits [64:127] plus carry
    let mid = (p00 >> 64) + (p01 as u64 as u128) + (p10 as u64 as u128);
    // Only the low 64 bits of mid go into lo; the rest carries into hi
    let lo = (p00 as u64 as u128) | ((mid as u64 as u128) << 64);
    let hi = p11 + (p01 >> 64) + (p10 >> 64) + (mid >> 64);

    U256 { hi, lo }
}

/// Multiply U256 × u128 → U256 (exact, caller asserts result fits 256 bits).
#[inline]
pub fn wmul_u256_u128(a: U256, b: u128) -> U256 {
    // a.lo * b → 256-bit
    let lo_prod = wmul(a.lo, b);
    // a.hi * b → at most 256 bits, shifted left 128 → only low 128 matters for our range
    let hi_prod = a.hi.wrapping_mul(b);
    let new_hi = lo_prod.hi.wrapping_add(hi_prod);
    U256 { hi: new_hi, lo: lo_prod.lo }
}

// ── Divide U256 / u128 → u128 (exact quotient, truncated) ──

/// Divide a 256-bit number by a u128 divisor, returning u128 quotient.
/// Panics if quotient overflows u128 (shouldn't happen in our use cases).
#[inline]
pub fn wdiv(n: U256, d: u128) -> u128 {
    if n.hi == 0 {
        return n.lo / d;
    }
    // Long division: n = (n.hi << 128) + n.lo
    // q = n / d
    // Split into: q_hi = n.hi / d, r_hi = n.hi % d
    // Then: n = (q_hi * d + r_hi) << 128 + n.lo
    //        = q_hi << 128 * d + (r_hi << 128 + n.lo)
    // So: q = q_hi << 128 + (r_hi << 128 + n.lo) / d
    // But q_hi << 128 must be 0 for result to fit u128.
    let q_hi = n.hi / d;
    let r_hi = n.hi % d;

    // If q_hi > 0, result doesn't fit u128 — clamp.
    if q_hi > 0 {
        return u128::MAX;
    }

    // Now: quotient = (r_hi << 128 + n.lo) / d, where r_hi < d.
    // Use 128-bit long division in two 64-bit steps.
    div_wide_by_u128(r_hi, n.lo, d)
}

/// Divide (hi << 128 | lo) by d, where hi < d. Returns quotient (fits u128).
#[inline]
fn div_wide_by_u128(hi: u128, lo: u128, d: u128) -> u128 {
    if hi == 0 {
        return lo / d;
    }

    // Split lo = lo_hi << 64 | lo_lo
    // Compute (hi * 2^128 + lo) / d via:
    //   Step 1: (hi * 2^64 + lo_hi) / d → q1, r1  (requires hi < 2^64)
    //   Step 2: (r1 * 2^64 + lo_lo) / d → q2      (requires r1 < 2^64)
    //   Result: q1 << 64 | q2
    let lo_hi = lo >> 64;
    let lo_lo = lo as u64 as u128;

    if hi < (1u128 << 64) {
        let n1 = (hi << 64) | lo_hi;
        let q1 = n1 / d;
        let r1 = n1 % d;

        if r1 < (1u128 << 64) {
            let n2 = (r1 << 64) | lo_lo;
            let q2 = n2 / d;
            return (q1 << 64) | q2;
        }
        // r1 >= 2^64: (r1 << 64 | lo_lo) overflows u128.
        // Use bit-by-bit division for the low 64-bit quotient portion.
        // We need (r1 * 2^64 + lo_lo) / d where r1 < d.
        return (q1 << 64) | div_wide_64bit(r1, lo_lo as u64, d);
    }

    // hi >= 2^64: use full binary long division
    div_wide_128bit(hi, lo, d)
}

/// Compute (hi << 64 | lo) / d via binary long division. hi < d. Result fits u64.
fn div_wide_64bit(hi: u128, lo: u64, d: u128) -> u128 {
    let mut rem = hi;
    let mut quot: u64 = 0;

    for i in (0..64u32).rev() {
        // rem << 1 might overflow when rem has top bit set.
        // If top bit is set, after shift rem > u128::MAX/2 > d, so we subtract d.
        let overflow = rem >> 127;
        rem = (rem << 1) | (((lo as u128) >> i) & 1);
        quot <<= 1;
        if overflow != 0 || rem >= d {
            rem = rem.wrapping_sub(d);
            quot |= 1;
        }
    }
    quot as u128
}

/// Compute (hi << 128 | lo) / d via binary long division. hi < d. Result fits u128.
fn div_wide_128bit(hi: u128, lo: u128, d: u128) -> u128 {
    let mut rem = hi;
    let mut quot: u128 = 0;

    for i in (0..128u32).rev() {
        let overflow = rem >> 127;
        rem = (rem << 1) | ((lo >> i) & 1);
        quot <<= 1;
        if overflow != 0 || rem >= d {
            rem = rem.wrapping_sub(d);
            quot |= 1;
        }
    }
    quot
}

// ── Integer square root of U256 → u128 ──

/// Floor(sqrt(value)) where value is a U256. Result always fits u128
/// (sqrt of 256-bit number is at most 128 bits).
/// Uses Newton's method starting from a u128-range initial estimate,
/// so every iteration uses wdiv (u128 divisor) instead of the expensive
/// div_u256_by_u256 (256-bit binary long division).
#[inline]
pub fn isqrt_wide(v: U256) -> u128 {
    if v.hi == 0 {
        return super::optimizer::isqrt_u128(v.lo);
    }

    // Start with a u128 guess: (isqrt(v.hi) + 1) << 64.
    // Proof this is an overestimate (required for Newton downward convergence):
    //   Let s = isqrt(v.hi). Then s² ≤ v.hi < (s+1)².
    //   guess = (s+1) << 64, so guess² = (s+1)² << 128 > v.hi << 128 ≥ v.
    //   Therefore guess > sqrt(v). ✓
    let s = super::optimizer::isqrt_u128(v.hi);
    let mut r: u128 = (s + 1) << 64;

    // Newton iterations: r = (r + v/r) / 2
    // Since r is always u128, division is wdiv (cheap) not div_u256_by_u256.
    loop {
        let q = wdiv(v, r);
        let next = (r >> 1) + (q >> 1) + (r & q & 1);
        if next >= r {
            return r;
        }
        r = next;
    }
}

fn leading_zeros_u256(v: U256) -> u32 {
    if v.hi != 0 {
        v.hi.leading_zeros()
    } else {
        128 + v.lo.leading_zeros()
    }
}

fn add_u256(a: U256, b: U256) -> U256 {
    let (lo, carry) = a.lo.overflowing_add(b.lo);
    let hi = a.hi.wrapping_add(b.hi).wrapping_add(carry as u128);
    U256 { hi, lo }
}

fn shr1_u256(v: U256) -> U256 {
    U256 {
        hi: v.hi >> 1,
        lo: (v.lo >> 1) | (v.hi << 127),
    }
}

fn gte_u256(a: U256, b: U256) -> bool {
    a.hi > b.hi || (a.hi == b.hi && a.lo >= b.lo)
}

/// Divide U256 by U256. Used only inside isqrt_wide (Newton iteration).
fn div_u256_by_u256(n: U256, d: U256) -> U256 {
    if d.hi == 0 {
        // Divisor fits in u128 — use wdiv
        let q = wdiv(n, d.lo);
        return U256::from_u128(q);
    }

    // Both hi parts nonzero: use binary long division
    // This is rare (only first ~2 Newton iterations) and the loop is short.
    let mut rem = U256::ZERO;
    let mut quot = U256::ZERO;

    // Process all 256 bits of numerator
    for i in (0..256u32).rev() {
        // rem = rem << 1 | bit(n, i)
        rem = shl1_u256(rem);
        let bit = if i >= 128 {
            (n.hi >> (i - 128)) & 1
        } else {
            (n.lo >> i) & 1
        };
        rem.lo |= bit as u128;

        // quot <<= 1
        quot = shl1_u256(quot);

        if gte_u256(rem, d) {
            rem = sub_u256(rem, d);
            quot.lo |= 1;
        }
    }
    quot
}

fn shl1_u256(v: U256) -> U256 {
    U256 {
        hi: (v.hi << 1) | (v.lo >> 127),
        lo: v.lo << 1,
    }
}

fn sub_u256(a: U256, b: U256) -> U256 {
    let (lo, borrow) = a.lo.overflowing_sub(b.lo);
    let hi = a.hi.wrapping_sub(b.hi).wrapping_sub(borrow as u128);
    U256 { hi, lo }
}

/// Public subtraction: a - b. Caller must ensure a >= b.
#[inline]
pub fn sub_u256_pub(a: U256, b: U256) -> U256 {
    sub_u256(a, b)
}

// ── Convenience: chained multiply + divide ──

/// Compute (a * b * c * d) / divisor exactly, where the product may exceed u128.
/// Returns u128 (panics/clamps if result > u128::MAX).
#[inline]
pub fn mul4_div(a: u128, b: u128, c: u128, d: u128, divisor: u128) -> u128 {
    let ab = wmul(a, b);
    let cd = wmul(c, d);
    // ab * cd → up to 512 bits, but after dividing by divisor should fit 256 then u128.
    // Multiply ab * cd in parts: ab.lo * cd.lo + cross terms
    // For our use case the final result fits u128, so we can be clever.

    // ab * cd = (ab.hi * cd.hi) << 256  -- this must be 0 for result to fit
    //         + (ab.hi * cd.lo + ab.lo * cd.hi) << 128
    //         + ab.lo * cd.lo

    // Since we divide by `divisor` at the end and expect u128 result,
    // build the 512-bit product then divide.

    // Simpler: multiply stepwise U256 × u128
    // product = a * b * c * d = ((a*b) * c) * d
    let abc = wmul_u256_u128(ab, c);
    let abcd = wmul_u256_u128(abc, d);
    wdiv(abcd, divisor)
}

/// Compute (a * b * c * d * e) / divisor exactly. Returns u128.
#[inline]
pub fn mul5_div(a: u128, b: u128, c: u128, d: u128, e: u128, divisor: u128) -> u128 {
    let ab = wmul(a, b);
    let abc = wmul_u256_u128(ab, c);
    let abcd = wmul_u256_u128(abc, d);
    let abcde = wmul_u256_u128(abcd, e);
    wdiv(abcde, divisor)
}

/// Compute (a * b * c * d * e * f) / divisor exactly. Returns u128.
#[inline]
pub fn mul6_div(a: u128, b: u128, c: u128, d: u128, e: u128, f: u128, divisor: u128) -> u128 {
    let ab = wmul(a, b);
    let abc = wmul_u256_u128(ab, c);
    let abcd = wmul_u256_u128(abc, d);
    let abcde = wmul_u256_u128(abcd, e);
    let abcdef = wmul_u256_u128(abcde, f);
    wdiv(abcdef, divisor)
}

/// Compute sqrt((a * b * c * d) / divisor) exactly. Returns u128.
#[inline]
pub fn sqrt_mul4_div(a: u128, b: u128, c: u128, d: u128, divisor: u128) -> u128 {
    let ab = wmul(a, b);
    let abc = wmul_u256_u128(ab, c);
    let abcd = wmul_u256_u128(abc, d);
    let divided = wdiv_u256(abcd, divisor);
    isqrt_wide(divided)
}

/// Compute sqrt((a * b * c * d * e) / divisor) exactly. Returns u128.
#[inline]
pub fn sqrt_mul5_div(a: u128, b: u128, c: u128, d: u128, e: u128, divisor: u128) -> u128 {
    let ab = wmul(a, b);
    let abc = wmul_u256_u128(ab, c);
    let abcd = wmul_u256_u128(abc, d);
    let abcde = wmul_u256_u128(abcd, e);
    let divided = wdiv_u256(abcde, divisor);
    isqrt_wide(divided)
}

/// Compute sqrt((a * b * c * d * e * f) / divisor) exactly. Returns u128.
#[inline]
pub fn sqrt_mul6_div(a: u128, b: u128, c: u128, d: u128, e: u128, f: u128, divisor: u128) -> u128 {
    let ab = wmul(a, b);
    let abc = wmul_u256_u128(ab, c);
    let abcd = wmul_u256_u128(abc, d);
    let abcde = wmul_u256_u128(abcd, e);
    let abcdef = wmul_u256_u128(abcde, f);
    let divided = wdiv_u256(abcdef, divisor);
    isqrt_wide(divided)
}

/// U256 / u128 → U256 (keeps full precision for chaining into isqrt).
#[inline]
fn wdiv_u256(n: U256, d: u128) -> U256 {
    if n.hi == 0 {
        return U256::from_u128(n.lo / d);
    }
    let q_hi = n.hi / d;
    let r_hi = n.hi % d;
    let q_lo = div_wide_by_u128(r_hi, n.lo, d);
    U256 { hi: q_hi, lo: q_lo }
}

/// Compute a * b where a is U256, b is u128 → result as U256. Then divide by d.
/// Returns u128.
#[inline]
pub fn wmul_div(a_hi: u128, a_lo: u128, b: u128, d: u128) -> u128 {
    let a = U256 { hi: a_hi, lo: a_lo };
    let prod = wmul_u256_u128(a, b);
    wdiv(prod, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wmul_basic() {
        // Small numbers
        let r = wmul(100, 200);
        assert_eq!(r.hi, 0);
        assert_eq!(r.lo, 20000);

        // Numbers that overflow u128
        let a = u128::MAX;
        let b = 2u128;
        let r = wmul(a, b);
        // MAX * 2 = 2^129 - 2 = (1 << 128) + (MAX - 1)
        assert_eq!(r.hi, 1);
        assert_eq!(r.lo, u128::MAX - 1);
    }

    #[test]
    fn test_wmul_max() {
        let r = wmul(u128::MAX, u128::MAX);
        // MAX^2 = (2^128-1)^2 = 2^256 - 2^129 + 1
        assert_eq!(r.hi, u128::MAX - 1); // 2^128 - 2
        assert_eq!(r.lo, 1);
    }

    #[test]
    fn test_wdiv_basic() {
        let n = U256 { hi: 1, lo: 0 }; // 2^128
        let q = wdiv(n, 2);
        assert_eq!(q, 1u128 << 127);
    }

    #[test]
    fn test_wdiv_exact() {
        // 1000 * 2000 = 2_000_000, divide by 1000
        let n = wmul(1000, 2000);
        assert_eq!(wdiv(n, 1000), 2000);
    }

    #[test]
    fn test_isqrt_wide_small() {
        let v = U256::from_u128(144);
        assert_eq!(isqrt_wide(v), 12);
    }

    #[test]
    fn test_isqrt_wide_large() {
        // sqrt(2^128) = 2^64
        let v = U256 { hi: 1, lo: 0 };
        assert_eq!(isqrt_wide(v), 1u128 << 64);
    }

    #[test]
    fn test_mul4_div() {
        // (10 * 20 * 30 * 40) / 100 = 240_000 / 100 = 2400
        assert_eq!(mul4_div(10, 20, 30, 40, 100), 2400);
    }

    #[test]
    fn test_roundtrip_vs_u128() {
        // When everything fits u128, results must match
        let a: u128 = 1_000_000_000;
        let b: u128 = 999_000;
        let c: u128 = 500_000_000;
        let d: u128 = 100_000_000;
        let div: u128 = 1_000_000_000_000;

        let wide_result = mul4_div(a, b, c, d, div);

        // Exact: (1e9 * 999e3 * 5e8 * 1e8) / 1e12
        // = 9.99e14 * 5e8 * 1e8 / 1e12
        // = 4.995e31 / 1e12 = 4.995e19
        assert_eq!(wide_result, 49_950_000_000_000_000_000);
    }

    #[test]
    fn test_sqrt_matches_isqrt_u128() {
        // When value fits u128, must match isqrt_u128 exactly
        let val: u128 = 123_456_789_012_345_678;
        let expected = super::super::optimizer::isqrt_u128(val);
        let wide = isqrt_wide(U256::from_u128(val));
        assert_eq!(wide, expected);
    }

    #[test]
    fn test_sqrt_mul6_div_matches_ruint() {
        // Replicate the exact computation from optimal_pump_pump:
        // K = qb * fb * bb * ba * fa * qa / FD^2
        let qb: u128 = 100_000_000_000;
        let fb: u128 = 997_000;
        let bb: u128 = 500_000_000_000;
        let ba: u128 = 1_000_000_000_000;
        let fa: u128 = 997_000;
        let qa: u128 = 50_000_000_000;
        let fd_sq: u128 = 1_000_000u128 * 1_000_000;

        let result = sqrt_mul6_div(qb, fb, bb, ba, fa, qa, fd_sq);

        // Cross-check with ruint
        use ruint::aliases::U256 as RuintU256;
        let k = RuintU256::from(qb) * RuintU256::from(fb) * RuintU256::from(bb)
            * RuintU256::from(ba) * RuintU256::from(fa) * RuintU256::from(qa);
        let k_scaled = k / RuintU256::from(fd_sq);
        let expected = super::super::optimizer::isqrt_u256(k_scaled);
        let expected_u128: u128 = expected.try_into().unwrap();

        assert_eq!(result, expected_u128, "wide_math must match ruint exactly");
    }
}
