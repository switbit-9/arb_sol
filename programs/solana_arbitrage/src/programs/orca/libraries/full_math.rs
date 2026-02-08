use super::big_num::U256;

/// Trait for multiply-divide operations
pub trait MulDiv: Sized {
    fn mul_div_floor(self, mul: Self, div: Self) -> Option<Self>;
    fn mul_div_ceil(self, mul: Self, div: Self) -> Option<Self>;
}

impl MulDiv for U256 {
    fn mul_div_floor(self, mul: Self, div: Self) -> Option<Self> {
        if div.is_zero() {
            return None;
        }
        let product = self * mul;
        Some(product / div)
    }

    fn mul_div_ceil(self, mul: Self, div: Self) -> Option<Self> {
        if div.is_zero() {
            return None;
        }
        let product = self * mul;
        let quotient = product / div;
        let remainder = product - quotient * div;
        if remainder.is_zero() {
            Some(quotient)
        } else {
            Some(quotient + U256::from(1u128))
        }
    }
}

/// Trait for unsafe math operations (wrapping)
pub trait UnsafeMathTrait: Sized {
    fn div_rounding_up(self, denominator: Self) -> Self;
}

impl UnsafeMathTrait for U256 {
    fn div_rounding_up(self, denominator: Self) -> Self {
        U256::div_rounding_up(self, denominator)
    }
}
