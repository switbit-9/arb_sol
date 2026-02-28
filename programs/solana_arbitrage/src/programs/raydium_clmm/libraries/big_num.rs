use uint::construct_uint;

construct_uint! {
    pub struct U128(2);
}

construct_uint! {
    pub struct U256(4);
}

impl U256 {
    pub fn div_rounding_up(a: U256, b: U256) -> U256 {
        let (quotient, remainder) = a.div_mod(b);
        if remainder == U256::zero() {
            quotient
        } else {
            quotient + U256::one()
        }
    }
}
