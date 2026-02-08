use super::big_num::U256;

pub trait UnsafeMathTrait {
    fn div_rounding_up(a: Self, b: Self) -> Self;
}

impl UnsafeMathTrait for U256 {
    fn div_rounding_up(a: U256, b: U256) -> U256 {
        let (quotient, remainder) = a.div_mod(b);
        if remainder == U256::zero() {
            quotient
        } else {
            quotient + U256::one()
        }
    }
}
