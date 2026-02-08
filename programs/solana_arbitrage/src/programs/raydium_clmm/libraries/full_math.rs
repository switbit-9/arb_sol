use super::big_num::{U128, U256};

pub trait MulDiv<T> {
    fn mul_div_floor(&self, num: T, denom: T) -> Option<T>;
    fn mul_div_ceil(&self, num: T, denom: T) -> Option<T>;
}

impl MulDiv<U128> for U128 {
    fn mul_div_floor(&self, num: U128, denom: U128) -> Option<U128> {
        if denom == U128::zero() {
            return None;
        }
        let product = U256::from(self.as_u128()) * U256::from(num.as_u128());
        let result = product / U256::from(denom.as_u128());
        if result > U256::from(u128::MAX) {
            None
        } else {
            Some(U128::from(result.as_u128()))
        }
    }

    fn mul_div_ceil(&self, num: U128, denom: U128) -> Option<U128> {
        if denom == U128::zero() {
            return None;
        }
        let product = U256::from(self.as_u128()) * U256::from(num.as_u128());
        let (quotient, remainder) = product.div_mod(U256::from(denom.as_u128()));
        let result = if remainder == U256::zero() {
            quotient
        } else {
            quotient + U256::one()
        };
        if result > U256::from(u128::MAX) {
            None
        } else {
            Some(U128::from(result.as_u128()))
        }
    }
}

impl MulDiv<U256> for U256 {
    fn mul_div_floor(&self, num: U256, denom: U256) -> Option<U256> {
        if denom == U256::zero() {
            return None;
        }
        // For U256 * U256, we need to be careful about overflow
        // Use checked_mul if available, otherwise use a simple approach
        let result = ((*self) * num) / denom;
        Some(result)
    }

    fn mul_div_ceil(&self, num: U256, denom: U256) -> Option<U256> {
        if denom == U256::zero() {
            return None;
        }
        let product = (*self) * num;
        let (quotient, remainder) = product.div_mod(denom);
        if remainder == U256::zero() {
            Some(quotient)
        } else {
            Some(quotient + U256::one())
        }
    }
}

impl MulDiv<u64> for u64 {
    fn mul_div_floor(&self, num: u64, denom: u64) -> Option<u64> {
        if denom == 0 {
            return None;
        }
        let product = (*self as u128) * (num as u128);
        let result = product / (denom as u128);
        if result > u64::MAX as u128 {
            None
        } else {
            Some(result as u64)
        }
    }

    fn mul_div_ceil(&self, num: u64, denom: u64) -> Option<u64> {
        if denom == 0 {
            return None;
        }
        let product = (*self as u128) * (num as u128);
        let denom128 = denom as u128;
        let quotient = product / denom128;
        let remainder = product % denom128;
        let result = if remainder == 0 {
            quotient
        } else {
            quotient + 1
        };
        if result > u64::MAX as u128 {
            None
        } else {
            Some(result as u64)
        }
    }
}

impl MulDiv<u128> for u128 {
    fn mul_div_floor(&self, num: u128, denom: u128) -> Option<u128> {
        if denom == 0 {
            return None;
        }
        let product = U256::from(*self) * U256::from(num);
        let result = product / U256::from(denom);
        if result > U256::from(u128::MAX) {
            None
        } else {
            Some(result.as_u128())
        }
    }

    fn mul_div_ceil(&self, num: u128, denom: u128) -> Option<u128> {
        if denom == 0 {
            return None;
        }
        let product = U256::from(*self) * U256::from(num);
        let (quotient, remainder) = product.div_mod(U256::from(denom));
        let result = if remainder == U256::zero() {
            quotient
        } else {
            quotient + U256::one()
        };
        if result > U256::from(u128::MAX) {
            None
        } else {
            Some(result.as_u128())
        }
    }
}
