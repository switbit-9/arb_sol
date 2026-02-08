use super::big_num::U256;
use super::fixed_point_64::Q64;
use super::full_math::MulDiv;
use super::unsafe_math::UnsafeMathTrait;

/// Add a signed delta to liquidity
pub fn add_delta(x: u128, y: i128) -> Option<u128> {
    if y < 0 {
        let abs_y = (-y) as u128;
        x.checked_sub(abs_y)
    } else {
        x.checked_add(y as u128)
    }
}

/// Get delta amount_0 for a given liquidity and price range
/// Formula: delta_x = L * (sqrt_price_upper - sqrt_price_lower) / (sqrt_price_upper * sqrt_price_lower)
/// Matches the real Raydium CLMM implementation for precision
pub fn get_delta_amount_0_unsigned(
    mut sqrt_ratio_a_x64: u128,
    mut sqrt_ratio_b_x64: u128,
    liquidity: u128,
    round_up: bool,
) -> Option<u64> {
    // sqrt_ratio_a_x64 should hold the smaller value (same as real Raydium)
    if sqrt_ratio_a_x64 > sqrt_ratio_b_x64 {
        std::mem::swap(&mut sqrt_ratio_a_x64, &mut sqrt_ratio_b_x64);
    }

    if sqrt_ratio_a_x64 == 0 {
        return None;
    }

    // Match real Raydium's formula: ((L << 64) * (b-a) / b) / a
    let numerator_1 = U256::from(liquidity) << 64;
    let numerator_2 = U256::from(sqrt_ratio_b_x64 - sqrt_ratio_a_x64);

    let result = if round_up {
        U256::div_rounding_up(
            numerator_1
                .mul_div_ceil(numerator_2, U256::from(sqrt_ratio_b_x64))?,
            U256::from(sqrt_ratio_a_x64),
        )
    } else {
        numerator_1
            .mul_div_floor(numerator_2, U256::from(sqrt_ratio_b_x64))?
            / U256::from(sqrt_ratio_a_x64)
    };

    if result > U256::from(u64::MAX) {
        return None;
    }

    Some(result.as_u64())
}

/// Get delta amount_1 for a given liquidity and price range
/// Formula: delta_y = L * (sqrt_price_upper - sqrt_price_lower) / Q64
/// Matches the real Raydium CLMM implementation
pub fn get_delta_amount_1_unsigned(
    mut sqrt_ratio_a_x64: u128,
    mut sqrt_ratio_b_x64: u128,
    liquidity: u128,
    round_up: bool,
) -> Option<u64> {
    // sqrt_ratio_a_x64 should hold the smaller value (same as real Raydium)
    if sqrt_ratio_a_x64 > sqrt_ratio_b_x64 {
        std::mem::swap(&mut sqrt_ratio_a_x64, &mut sqrt_ratio_b_x64);
    }

    let result = if round_up {
        U256::from(liquidity).mul_div_ceil(
            U256::from(sqrt_ratio_b_x64 - sqrt_ratio_a_x64),
            U256::from(Q64),
        )
    } else {
        U256::from(liquidity).mul_div_floor(
            U256::from(sqrt_ratio_b_x64 - sqrt_ratio_a_x64),
            U256::from(Q64),
        )
    }?;

    if result > U256::from(u64::MAX) {
        return None;
    }

    Some(result.as_u64())
}

/// Get the maximum amount that can be swapped given liquidity and price range
/// For zero_for_one: returns max token_1 output (amount_1)
/// For one_for_zero: returns max token_0 output (amount_0)
pub fn get_amount_out_for_liquidity(
    sqrt_price_current_x64: u128,
    sqrt_price_target_x64: u128,
    liquidity: u128,
    zero_for_one: bool,
) -> Option<u64> {
    // The underlying functions auto-swap to ensure correct ordering
    if zero_for_one {
        // Output is token_1
        get_delta_amount_1_unsigned(sqrt_price_current_x64, sqrt_price_target_x64, liquidity, false)
    } else {
        // Output is token_0
        get_delta_amount_0_unsigned(sqrt_price_current_x64, sqrt_price_target_x64, liquidity, false)
    }
}

/// Get the input amount needed given liquidity and price range
/// For zero_for_one: returns token_0 input (amount_0)
/// For one_for_zero: returns token_1 input (amount_1)
pub fn get_amount_in_for_liquidity(
    sqrt_price_current_x64: u128,
    sqrt_price_target_x64: u128,
    liquidity: u128,
    zero_for_one: bool,
) -> Option<u64> {
    // The underlying functions auto-swap to ensure correct ordering
    if zero_for_one {
        // Input is token_0
        get_delta_amount_0_unsigned(sqrt_price_current_x64, sqrt_price_target_x64, liquidity, true)
    } else {
        // Input is token_1
        get_delta_amount_1_unsigned(sqrt_price_current_x64, sqrt_price_target_x64, liquidity, true)
    }
}
