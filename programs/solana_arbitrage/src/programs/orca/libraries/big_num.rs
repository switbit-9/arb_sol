use std::ops::{Add, BitAnd, Div, Mul, Shr, Sub};

/// U128 for intermediate calculations
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct U128(pub [u64; 2]);

impl U128 {
    pub const MAX: U128 = U128([u64::MAX, u64::MAX]);

    pub fn as_u128(&self) -> u128 {
        ((self.0[1] as u128) << 64) | (self.0[0] as u128)
    }
}

impl From<u128> for U128 {
    fn from(value: u128) -> Self {
        U128([value as u64, (value >> 64) as u64])
    }
}

impl Mul for U128 {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self::Output {
        let result = self.as_u128().saturating_mul(rhs.as_u128());
        U128::from(result)
    }
}

impl Div for U128 {
    type Output = Self;
    fn div(self, rhs: Self) -> Self::Output {
        let result = self.as_u128() / rhs.as_u128();
        U128::from(result)
    }
}

impl Shr<U128> for U128 {
    type Output = Self;
    fn shr(self, rhs: U128) -> Self::Output {
        let shift = rhs.0[0] as u32;
        let result = self.as_u128() >> shift;
        U128::from(result)
    }
}

const NUM_WORDS: usize = 4;
const U64_RESOLUTION: u32 = 64;
const U64_MAX: u128 = u64::MAX as u128;

pub trait LoHi {
    fn lo(self) -> u64;
    fn hi(self) -> u64;
    fn lo_u128(self) -> u128;
    fn hi_u128(self) -> u128;
}

impl LoHi for u128 {
    fn lo(self) -> u64 {
        (self & U64_MAX) as u64
    }
    fn lo_u128(self) -> u128 {
        self & U64_MAX
    }
    fn hi(self) -> u64 {
        (self >> U64_RESOLUTION) as u64
    }
    fn hi_u128(self) -> u128 {
        self >> U64_RESOLUTION
    }
}

pub fn hi_lo(hi: u64, lo: u64) -> u128 {
    ((hi as u128) << U64_RESOLUTION) | (lo as u128)
}

/// U256 for large number calculations - using 4 x u64 words like Orca
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct U256(pub [u64; NUM_WORDS]);

impl U256 {
    pub fn new(h: u128, l: u128) -> Self {
        U256([l.lo(), l.hi(), h.lo(), h.hi()])
    }

    pub fn zero() -> Self {
        U256([0, 0, 0, 0])
    }

    pub fn is_zero(&self) -> bool {
        self.0[0] == 0 && self.0[1] == 0 && self.0[2] == 0 && self.0[3] == 0
    }

    pub fn as_u64(&self) -> u64 {
        self.0[0]
    }

    pub fn as_u128(&self) -> u128 {
        hi_lo(self.0[1], self.0[0])
    }

    fn num_words(&self) -> usize {
        for i in (0..NUM_WORDS).rev() {
            if self.0[i] != 0 {
                return i + 1;
            }
        }
        0
    }

    fn get_word(&self, index: usize) -> u64 {
        self.0[index]
    }

    fn get_word_u128(&self, index: usize) -> u128 {
        self.0[index] as u128
    }

    fn update_word(&mut self, index: usize, value: u64) {
        self.0[index] = value;
    }

    pub fn shift_word_left(&self) -> Self {
        let mut result = U256::zero();
        for i in (0..NUM_WORDS - 1).rev() {
            result.0[i + 1] = self.0[i];
        }
        result
    }

    pub fn checked_shift_word_left(&self) -> Option<Self> {
        if self.0[NUM_WORDS - 1] > 0 {
            None
        } else {
            Some(self.shift_word_left())
        }
    }

    pub fn shift_word_right(&self) -> Self {
        let mut result = U256::zero();
        for i in 0..NUM_WORDS - 1 {
            result.0[i] = self.0[i + 1];
        }
        result
    }

    pub fn shift_right(&self, mut shift_amount: u32) -> Self {
        if shift_amount >= U64_RESOLUTION * (NUM_WORDS as u32) {
            return U256::zero();
        }

        let mut result = *self;

        while shift_amount >= U64_RESOLUTION {
            result = result.shift_word_right();
            shift_amount -= U64_RESOLUTION;
        }

        if shift_amount == 0 {
            return result;
        }

        for i in 0..NUM_WORDS - 1 {
            result.0[i] = (result.0[i] >> shift_amount)
                | (result.0[i + 1] << (U64_RESOLUTION - shift_amount));
        }

        result.0[3] >>= shift_amount;
        result
    }

    pub fn shift_left(&self, mut shift_amount: u32) -> Self {
        if shift_amount >= U64_RESOLUTION * (NUM_WORDS as u32) {
            return U256::zero();
        }

        let mut result = *self;

        while shift_amount >= U64_RESOLUTION {
            result = result.shift_word_left();
            shift_amount -= U64_RESOLUTION;
        }

        if shift_amount == 0 {
            return result;
        }

        for i in (1..NUM_WORDS).rev() {
            result.0[i] = (result.0[i] << shift_amount)
                | (result.0[i - 1] >> (U64_RESOLUTION - shift_amount));
        }

        result.0[0] <<= shift_amount;
        result
    }

    pub fn lt(&self, other: U256) -> bool {
        for i in (0..NUM_WORDS).rev() {
            if self.0[i] < other.0[i] {
                return true;
            }
            if self.0[i] > other.0[i] {
                return false;
            }
        }
        false
    }

    pub fn lte(&self, other: U256) -> bool {
        for i in (0..NUM_WORDS).rev() {
            if self.0[i] < other.0[i] {
                return true;
            }
            if self.0[i] > other.0[i] {
                return false;
            }
        }
        true
    }

    pub fn try_into_u128(&self) -> Option<u128> {
        if self.num_words() > 2 {
            return None;
        }
        Some(hi_lo(self.0[1], self.0[0]))
    }

    pub fn add(&self, other: U256) -> Self {
        let mut result = U256::zero();
        let mut carry: u128 = 0;

        for i in 0..NUM_WORDS {
            let x = self.get_word_u128(i);
            let y = other.get_word_u128(i);
            let t = x + y + carry;
            result.update_word(i, t.lo());
            carry = t.hi_u128();
        }

        result
    }

    pub fn sub(&self, other: U256) -> Self {
        let mut result = U256::zero();
        let mut carry: u64 = 0;

        for i in 0..NUM_WORDS {
            let x = self.get_word(i);
            let y = other.get_word(i);
            let (t0, overflowing0) = x.overflowing_sub(y);
            let (t1, overflowing1) = t0.overflowing_sub(carry);
            result.update_word(i, t1);
            carry = if overflowing0 || overflowing1 { 1 } else { 0 };
        }

        result
    }

    pub fn mul(&self, other: U256) -> Self {
        let mut result = U256::zero();

        let m = self.num_words();
        let n = other.num_words();

        for j in 0..n {
            let mut k: u128 = 0;
            for i in 0..m {
                let x = self.get_word_u128(i);
                let y = other.get_word_u128(j);
                if i + j < NUM_WORDS {
                    let z = result.get_word_u128(i + j);
                    let t = x.wrapping_mul(y).wrapping_add(z).wrapping_add(k);
                    result.update_word(i + j, t.lo());
                    k = t.hi_u128();
                }
            }

            if j + m < NUM_WORDS {
                result.update_word(j + m, k as u64);
            }
        }

        result
    }

    pub fn div(&self, divisor: U256, return_remainder: bool) -> (Self, Self) {
        let mut dividend = *self;
        let mut quotient = U256::zero();

        let num_dividend_words = dividend.num_words();
        let num_divisor_words = divisor.num_words();

        if num_divisor_words == 0 {
            panic!("divide by zero");
        }

        if num_dividend_words == 0 {
            return (U256::zero(), U256::zero());
        }

        if num_dividend_words < num_divisor_words {
            if return_remainder {
                return (U256::zero(), dividend);
            } else {
                return (U256::zero(), U256::zero());
            }
        }

        // Both fit in u128
        if num_dividend_words < 3 {
            let dividend_128 = dividend.try_into_u128().unwrap();
            let divisor_128 = divisor.try_into_u128().unwrap();
            let q = dividend_128 / divisor_128;
            if return_remainder {
                let r = dividend_128 % divisor_128;
                return (U256::new(0, q), U256::new(0, r));
            } else {
                return (U256::new(0, q), U256::zero());
            }
        }

        // Single word divisor
        if num_divisor_words == 1 {
            let mut k: u128 = 0;
            for j in (0..num_dividend_words).rev() {
                let d1 = hi_lo(k.lo(), dividend.get_word(j));
                let d2 = divisor.get_word_u128(0);
                let q = d1 / d2;
                k = d1 - d2 * q;
                quotient.update_word(j, q.lo());
            }

            if return_remainder {
                return (quotient, U256::new(0, k));
            } else {
                return (quotient, U256::zero());
            }
        }

        // Full long division
        let s = divisor.get_word(num_divisor_words - 1).leading_zeros();
        let b = dividend.get_word(num_dividend_words - 1).leading_zeros();

        let divisor_normalized = divisor.shift_left(s);
        let mut dividend_carry_space: u64 = 0;
        if num_dividend_words == NUM_WORDS && b < s {
            dividend_carry_space = dividend.0[num_dividend_words - 1] >> (U64_RESOLUTION - s);
        }
        dividend = dividend.shift_left(s);

        for j in (0..num_dividend_words - num_divisor_words + 1).rev() {
            let (q, d) = div_loop(
                j,
                num_divisor_words,
                dividend,
                &mut dividend_carry_space,
                divisor_normalized,
                quotient,
            );
            quotient = q;
            dividend = d;
        }

        if return_remainder {
            dividend = dividend.shift_right(s);
            (quotient, dividend)
        } else {
            (quotient, U256::zero())
        }
    }

    pub fn div_rounding_up(numerator: U256, denominator: U256) -> U256 {
        let (quotient, remainder) = U256::div(&numerator, denominator, true);
        if remainder.is_zero() {
            quotient
        } else {
            quotient.add(U256::from(1u128))
        }
    }
}

fn div_loop(
    index: usize,
    num_divisor_words: usize,
    mut dividend: U256,
    dividend_carry_space: &mut u64,
    divisor: U256,
    mut quotient: U256,
) -> (U256, U256) {
    let use_carry = (index + num_divisor_words) == NUM_WORDS;
    let div_hi = if use_carry {
        *dividend_carry_space
    } else {
        dividend.get_word(index + num_divisor_words)
    };
    let d0 = hi_lo(div_hi, dividend.get_word(index + num_divisor_words - 1));
    let d1 = divisor.get_word_u128(num_divisor_words - 1);

    let mut qhat = d0 / d1;
    let mut rhat = d0 - d1 * qhat;

    let d0_2 = dividend.get_word(index + num_divisor_words - 2);
    let d1_2 = divisor.get_word_u128(num_divisor_words - 2);

    let mut cmp1 = hi_lo(rhat.lo(), d0_2);
    let mut cmp2 = qhat.wrapping_mul(d1_2);

    while qhat.hi() != 0 || cmp2 > cmp1 {
        qhat -= 1;
        rhat += d1;
        if rhat.hi() != 0 {
            break;
        }
        cmp1 = hi_lo(rhat.lo(), cmp1.lo());
        cmp2 -= d1_2;
    }

    let mut k: u128 = 0;
    let mut t: u128;
    for i in 0..num_divisor_words {
        let p = qhat * (divisor.get_word_u128(i));
        t = (dividend.get_word_u128(index + i))
            .wrapping_sub(k)
            .wrapping_sub(p.lo_u128());
        dividend.update_word(index + i, t.lo());
        k = ((p >> U64_RESOLUTION) as u64).wrapping_sub((t >> U64_RESOLUTION) as u64) as u128;
    }

    let d_head = if use_carry {
        *dividend_carry_space as u128
    } else {
        dividend.get_word_u128(index + num_divisor_words)
    };

    t = d_head.wrapping_sub(k);
    if use_carry {
        *dividend_carry_space = t.lo();
    } else {
        dividend.update_word(index + num_divisor_words, t.lo());
    }

    if k > d_head {
        qhat -= 1;
        k = 0;
        for i in 0..num_divisor_words {
            t = dividend
                .get_word_u128(index + i)
                .wrapping_add(divisor.get_word_u128(i))
                .wrapping_add(k);
            dividend.update_word(index + i, t.lo());
            k = t >> U64_RESOLUTION;
        }

        let new_carry = dividend
            .get_word_u128(index + num_divisor_words)
            .wrapping_add(k)
            .lo();
        if use_carry {
            *dividend_carry_space = new_carry;
        } else {
            dividend.update_word(index + num_divisor_words, new_carry);
        }
    }

    quotient.update_word(index, qhat.lo());

    (quotient, dividend)
}

/// Multiply two u128 values to get U256 without overflow
pub fn mul_u256(v: u128, n: u128) -> U256 {
    // do 128 bits multiply
    //                   nh   nl
    //                *  vh   vl
    //                ----------
    // a0 =              vl * nl
    // a1 =         vl * nh
    // b0 =         vh * nl
    // b1 =  + vh * nh
    //       -------------------
    //        c1h  c1l  c0h  c0l

    let mut c0 = v.lo_u128() * n.lo_u128();
    let a1 = v.lo_u128() * n.hi_u128();
    let b0 = v.hi_u128() * n.lo_u128();

    let mut c1 = c0.hi_u128() + a1.lo_u128() + b0.lo_u128();

    c0 = hi_lo(c1.lo(), c0.lo());

    c1 = v.hi_u128() * n.hi_u128() + c1.hi_u128() + a1.hi_u128() + b0.hi_u128();

    U256::new(c1, c0)
}

impl From<u64> for U256 {
    fn from(value: u64) -> Self {
        U256([value, 0, 0, 0])
    }
}

impl From<u128> for U256 {
    fn from(value: u128) -> Self {
        U256::new(0, value)
    }
}

impl Add for U256 {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        U256::add(&self, rhs)
    }
}

impl Sub for U256 {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self::Output {
        U256::sub(&self, rhs)
    }
}

impl Mul for U256 {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self::Output {
        U256::mul(&self, rhs)
    }
}

impl Div for U256 {
    type Output = Self;
    fn div(self, rhs: Self) -> Self::Output {
        U256::div(&self, rhs, false).0
    }
}

impl Shr<u32> for U256 {
    type Output = Self;
    fn shr(self, rhs: u32) -> Self::Output {
        self.shift_right(rhs)
    }
}

impl BitAnd<u128> for U256 {
    type Output = u128;
    fn bitand(self, rhs: u128) -> Self::Output {
        self.as_u128() & rhs
    }
}

impl PartialEq<u128> for U256 {
    fn eq(&self, other: &u128) -> bool {
        self.0[2] == 0 && self.0[3] == 0 && self.as_u128() == *other
    }
}

impl PartialOrd<u128> for U256 {
    fn partial_cmp(&self, other: &u128) -> Option<std::cmp::Ordering> {
        if self.0[2] > 0 || self.0[3] > 0 {
            Some(std::cmp::Ordering::Greater)
        } else {
            self.as_u128().partial_cmp(other)
        }
    }
}

impl PartialEq<U256> for u128 {
    fn eq(&self, other: &U256) -> bool {
        other.0[2] == 0 && other.0[3] == 0 && other.as_u128() == *self
    }
}
