use super::big_num::{mul_u256, U256};

/// Add a signed delta to liquidity
pub fn add_delta(x: u128, y: i128) -> Option<u128> {
    if y < 0 {
        let abs_y = (-y) as u128;
        x.checked_sub(abs_y)
    } else {
        x.checked_add(y as u128)
    }
}

/// Get delta amount_0 (token A) for a given liquidity and price range
/// Formula: delta_a = L * (sqrt_price_upper - sqrt_price_lower) / (sqrt_price_upper * sqrt_price_lower)
pub fn get_delta_amount_0_unsigned(
    sqrt_price_lower_x64: u128,
    sqrt_price_upper_x64: u128,
    liquidity: u128,
    round_up: bool,
) -> Option<u64> {
    if sqrt_price_lower_x64 >= sqrt_price_upper_x64 {
        return Some(0);
    }

    let sqrt_price_diff = sqrt_price_upper_x64.checked_sub(sqrt_price_lower_x64)?;

    // Use mul_u256 for u128 * u128 multiplications
    let numerator = mul_u256(liquidity, sqrt_price_diff).checked_shift_word_left()?;
    let denominator = mul_u256(sqrt_price_upper_x64, sqrt_price_lower_x64);

    if denominator.is_zero() {
        return None;
    }

    let (quotient, remainder) = numerator.div(denominator, true);

    let result = if round_up && !remainder.is_zero() {
        quotient.add(U256::from(1u128))
    } else {
        quotient
    };

    let result_u128 = result.try_into_u128()?;
    if result_u128 > u64::MAX as u128 {
        return None;
    }

    Some(result_u128 as u64)
}

/// Get delta amount_1 (token B) for a given liquidity and price range
/// Formula: delta_b = L * (sqrt_price_upper - sqrt_price_lower) / Q64
pub fn get_delta_amount_1_unsigned(
    sqrt_price_lower_x64: u128,
    sqrt_price_upper_x64: u128,
    liquidity: u128,
    round_up: bool,
) -> Option<u64> {
    if sqrt_price_lower_x64 >= sqrt_price_upper_x64 {
        return Some(0);
    }

    let sqrt_price_diff = sqrt_price_upper_x64.checked_sub(sqrt_price_lower_x64)?;

    // Check if multiplication fits in u128
    if let Some(product) = liquidity.checked_mul(sqrt_price_diff) {
        // Fits in u128, use simple division
        let result = product >> 64;
        let has_remainder = (product & ((1u128 << 64) - 1)) > 0;

        let final_result = if round_up && has_remainder {
            result.checked_add(1)?
        } else {
            result
        };

        if final_result > u64::MAX as u128 {
            return None;
        }
        Some(final_result as u64)
    } else {
        // u128 multiplication overflowed — match on-chain behavior which treats this
        // as ExceedsMax (the swap step will treat it as "can't reach target")
        None
    }
}

/// Get the output amount for a swap given liquidity and price range
/// For a_to_b: returns token B output (amount_1)
/// For b_to_a: returns token A output (amount_0)
pub fn get_amount_out_for_liquidity(
    sqrt_price_current_x64: u128,
    sqrt_price_target_x64: u128,
    liquidity: u128,
    a_to_b: bool,
) -> Option<u64> {
    let (sqrt_lower, sqrt_upper) = if sqrt_price_current_x64 <= sqrt_price_target_x64 {
        (sqrt_price_current_x64, sqrt_price_target_x64)
    } else {
        (sqrt_price_target_x64, sqrt_price_current_x64)
    };

    if a_to_b {
        // Output is token B
        get_delta_amount_1_unsigned(sqrt_lower, sqrt_upper, liquidity, false)
    } else {
        // Output is token A
        get_delta_amount_0_unsigned(sqrt_lower, sqrt_upper, liquidity, false)
    }
}

/// Get the input amount needed for a swap given liquidity and price range
/// For a_to_b: returns token A input (amount_0)
/// For b_to_a: returns token B input (amount_1)
pub fn get_amount_in_for_liquidity(
    sqrt_price_current_x64: u128,
    sqrt_price_target_x64: u128,
    liquidity: u128,
    a_to_b: bool,
) -> Option<u64> {
    let (sqrt_lower, sqrt_upper) = if sqrt_price_current_x64 <= sqrt_price_target_x64 {
        (sqrt_price_current_x64, sqrt_price_target_x64)
    } else {
        (sqrt_price_target_x64, sqrt_price_current_x64)
    };

    if a_to_b {
        // Input is token A
        get_delta_amount_0_unsigned(sqrt_lower, sqrt_upper, liquidity, true)
    } else {
        // Input is token B
        get_delta_amount_1_unsigned(sqrt_lower, sqrt_upper, liquidity, true)
    }
}
