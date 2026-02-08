use super::big_num::U256;
use super::fixed_point_64::Q64;
use super::full_math::MulDiv;
use super::unsafe_math::UnsafeMathTrait;

/// Get the next sqrt price from input amount of token_0 (x)
/// Formula: sqrt_price_next = sqrt_price * liquidity / (liquidity + amount * sqrt_price)
/// Rounds up to ensure we don't overshoot the target price
/// Matches real Raydium CLMM implementation
pub fn get_next_sqrt_price_from_amount_0_rounding_up(
    sqrt_price_x64: u128,
    liquidity: u128,
    amount: u64,
    add: bool,
) -> Option<u128> {
    if amount == 0 {
        return Some(sqrt_price_x64);
    }

    let numerator_1 = U256::from(liquidity) << 64;

    if add {
        // Adding amount: sqrt_price_next = (L << 64) * sqrt_price / ((L << 64) + amount * sqrt_price)
        if let Some(product) = U256::from(amount).checked_mul(U256::from(sqrt_price_x64)) {
            let denominator = numerator_1 + product;
            if denominator >= numerator_1 {
                return numerator_1
                    .mul_div_ceil(U256::from(sqrt_price_x64), denominator)
                    .map(|r| r.as_u128());
            }
        }
        // Fallback: use alternate form L / (L/sqrt_price + amount) to avoid overflow
        Some(
            U256::div_rounding_up(
                numerator_1,
                (numerator_1 / U256::from(sqrt_price_x64)) + U256::from(amount),
            )
            .as_u128(),
        )
    } else {
        // Removing amount: sqrt_price_next = (L << 64) * sqrt_price / ((L << 64) - amount * sqrt_price)
        let product = U256::from(amount) * U256::from(sqrt_price_x64);
        if product >= numerator_1 {
            return None;
        }
        let denominator = numerator_1 - product;
        numerator_1
            .mul_div_ceil(U256::from(sqrt_price_x64), denominator)
            .map(|r| r.as_u128())
    }
}

/// Get the next sqrt price from input amount of token_1 (y)
/// Formula: sqrt_price_next = sqrt_price + amount * Q64 / liquidity
/// Rounds down to minimize price impact
pub fn get_next_sqrt_price_from_amount_1_rounding_down(
    sqrt_price_x64: u128,
    liquidity: u128,
    amount: u64,
    add: bool,
) -> Option<u128> {
    if amount == 0 {
        return Some(sqrt_price_x64);
    }

    if add {
        // Round DOWN for add case (to minimize price increase)
        let quotient =
            U256::from(u128::from(amount) << 64) / U256::from(liquidity);
        let result = sqrt_price_x64.checked_add(quotient.as_u128())?;
        Some(result)
    } else {
        // Round UP for subtract case (to maximize price decrease) - matches real Raydium
        let quotient = U256::div_rounding_up(
            U256::from(u128::from(amount) << 64),
            U256::from(liquidity),
        );
        let result = sqrt_price_x64.checked_sub(quotient.as_u128())?;
        Some(result)
    }
}

/// Get the next sqrt price given an input amount
/// Routes to the correct calculation based on direction
pub fn get_next_sqrt_price_from_input(
    sqrt_price_x64: u128,
    liquidity: u128,
    amount_in: u64,
    zero_for_one: bool,
) -> Option<u128> {
    if zero_for_one {
        // Selling token_0, price decreases
        get_next_sqrt_price_from_amount_0_rounding_up(sqrt_price_x64, liquidity, amount_in, true)
    } else {
        // Selling token_1, price increases
        get_next_sqrt_price_from_amount_1_rounding_down(sqrt_price_x64, liquidity, amount_in, true)
    }
}

/// Get the next sqrt price given an output amount
/// Routes to the correct calculation based on direction
pub fn get_next_sqrt_price_from_output(
    sqrt_price_x64: u128,
    liquidity: u128,
    amount_out: u64,
    zero_for_one: bool,
) -> Option<u128> {
    if zero_for_one {
        // Buying token_1, price decreases
        get_next_sqrt_price_from_amount_1_rounding_down(sqrt_price_x64, liquidity, amount_out, false)
    } else {
        // Buying token_0, price increases
        get_next_sqrt_price_from_amount_0_rounding_up(sqrt_price_x64, liquidity, amount_out, false)
    }
}
