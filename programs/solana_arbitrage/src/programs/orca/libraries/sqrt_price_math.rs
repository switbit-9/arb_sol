use super::big_num::{mul_u256, U256};
use super::fixed_point_64::Q64;
use super::full_math::MulDiv;

/// Get the next sqrt price from input amount of token A
/// Formula: sqrt_price_next = sqrt_price * liquidity / (liquidity + amount * sqrt_price)
/// Rounds up to ensure we don't overshoot the target price
pub fn get_next_sqrt_price_from_amount_a_rounding_up(
    sqrt_price_x64: u128,
    liquidity: u128,
    amount: u64,
    add: bool,
) -> Option<u128> {
    if amount == 0 {
        return Some(sqrt_price_x64);
    }

    // Use mul_u256 to multiply u128 * u128 without overflow
    let product = mul_u256(sqrt_price_x64, amount as u128);

    // numerator = sqrt_price * liquidity << 64
    let numerator = mul_u256(liquidity, sqrt_price_x64).checked_shift_word_left()?;

    // liquidity_shift_left = liquidity << 64
    let liquidity_shift_left = U256::new(0, liquidity).shift_word_left();

    if add {
        // Adding amount: sqrt_price_next = sqrt_price * liquidity / (liquidity + amount * sqrt_price)
        let denominator = liquidity_shift_left.add(product);

        if denominator.is_zero() {
            return None;
        }

        let (quotient, remainder) = numerator.div(denominator, true);
        let result = if !remainder.is_zero() {
            quotient.add(U256::from(1u128))
        } else {
            quotient
        };

        result.try_into_u128()
    } else {
        // Removing amount: sqrt_price_next = sqrt_price * liquidity / (liquidity - amount * sqrt_price)
        // Check for underflow
        if liquidity_shift_left.lte(product) {
            return None;
        }
        let denominator = liquidity_shift_left.sub(product);

        let (quotient, remainder) = numerator.div(denominator, true);
        let result = if !remainder.is_zero() {
            quotient.add(U256::from(1u128))
        } else {
            quotient
        };

        result.try_into_u128()
    }
}

/// Get the next sqrt price from input amount of token B
/// Formula: sqrt_price_next = sqrt_price + amount * Q64 / liquidity
/// Rounds down to minimize price impact
pub fn get_next_sqrt_price_from_amount_b_rounding_down(
    sqrt_price_x64: u128,
    liquidity: u128,
    amount: u64,
    add: bool,
) -> Option<u128> {
    if amount == 0 {
        return Some(sqrt_price_x64);
    }

    // amount_x64 = amount << 64
    let amount_x64 = (amount as u128) << 64;

    // delta = amount_x64 / liquidity (rounded up if not adding, rounded down if adding)
    let delta = if add {
        // Round down when adding
        amount_x64 / liquidity
    } else {
        // Round up when removing (to guarantee output)
        (amount_x64 + liquidity - 1) / liquidity
    };

    if add {
        sqrt_price_x64.checked_add(delta)
    } else {
        if delta > sqrt_price_x64 {
            return None;
        }
        Some(sqrt_price_x64 - delta)
    }
}

/// Get the next sqrt price given an input amount
/// Routes to the correct calculation based on direction
pub fn get_next_sqrt_price_from_input(
    sqrt_price_x64: u128,
    liquidity: u128,
    amount_in: u64,
    a_to_b: bool,
) -> Option<u128> {
    if a_to_b {
        // Selling token A, price decreases
        get_next_sqrt_price_from_amount_a_rounding_up(sqrt_price_x64, liquidity, amount_in, true)
    } else {
        // Selling token B, price increases
        get_next_sqrt_price_from_amount_b_rounding_down(sqrt_price_x64, liquidity, amount_in, true)
    }
}

/// Get the next sqrt price given an output amount
/// Routes to the correct calculation based on direction
pub fn get_next_sqrt_price_from_output(
    sqrt_price_x64: u128,
    liquidity: u128,
    amount_out: u64,
    a_to_b: bool,
) -> Option<u128> {
    if a_to_b {
        // Buying token B, price decreases
        get_next_sqrt_price_from_amount_b_rounding_down(sqrt_price_x64, liquidity, amount_out, false)
    } else {
        // Buying token A, price increases
        get_next_sqrt_price_from_amount_a_rounding_up(sqrt_price_x64, liquidity, amount_out, false)
    }
}
