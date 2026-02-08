use super::full_math::MulDiv;
use super::liquidity_math;
use super::sqrt_price_math;
use crate::programs::raydium_clmm::states::FEE_RATE_DENOMINATOR_VALUE;

/// Result of computing a swap step
#[derive(Debug, Clone, Copy, Default)]
pub struct SwapStep {
    /// The sqrt price after the swap step
    pub sqrt_price_next_x64: u128,
    /// The amount of input token consumed
    pub amount_in: u64,
    /// The amount of output token produced
    pub amount_out: u64,
    /// The fee amount
    pub fee_amount: u64,
}

/// Computes the result of swapping some amount in, or amount out, given the parameters of the swap
/// This matches the real Raydium CLMM implementation
pub fn compute_swap_step(
    sqrt_price_current_x64: u128,
    sqrt_price_target_x64: u128,
    liquidity: u128,
    amount_remaining: u64,
    fee_rate: u32,
    is_base_input: bool,
    zero_for_one: bool,
) -> SwapStep {
    if liquidity == 0 || amount_remaining == 0 {
        return SwapStep {
            sqrt_price_next_x64: sqrt_price_current_x64,
            amount_in: 0,
            amount_out: 0,
            fee_amount: 0,
        };
    }

    let mut swap_step = SwapStep::default();

    if is_base_input {
        // Exact input case
        // Deduct fee from input first
        let amount_remaining_less_fee = (amount_remaining as u128)
            .mul_div_floor(
                (FEE_RATE_DENOMINATOR_VALUE - fee_rate) as u128,
                FEE_RATE_DENOMINATOR_VALUE as u128,
            )
            .unwrap_or(0) as u64;

        // Calculate amount_in to reach target price
        let amount_in = calculate_amount_in_range(
            sqrt_price_current_x64,
            sqrt_price_target_x64,
            liquidity,
            zero_for_one,
            is_base_input,
        );

        if let Some(amt_in) = amount_in {
            swap_step.amount_in = amt_in;
        }

        swap_step.sqrt_price_next_x64 =
            if amount_in.is_some() && amount_remaining_less_fee >= swap_step.amount_in {
                sqrt_price_target_x64
            } else {
                sqrt_price_math::get_next_sqrt_price_from_input(
                    sqrt_price_current_x64,
                    liquidity,
                    amount_remaining_less_fee,
                    zero_for_one,
                )
                .unwrap_or(sqrt_price_current_x64)
            };
    } else {
        // Exact output case
        let amount_out = calculate_amount_in_range(
            sqrt_price_current_x64,
            sqrt_price_target_x64,
            liquidity,
            zero_for_one,
            is_base_input,
        );

        if let Some(amt_out) = amount_out {
            swap_step.amount_out = amt_out;
        }

        swap_step.sqrt_price_next_x64 =
            if amount_out.is_some() && amount_remaining >= swap_step.amount_out {
                sqrt_price_target_x64
            } else {
                sqrt_price_math::get_next_sqrt_price_from_output(
                    sqrt_price_current_x64,
                    liquidity,
                    amount_remaining,
                    zero_for_one,
                )
                .unwrap_or(sqrt_price_current_x64)
            };
    }

    // Whether we reached the target price
    let max = sqrt_price_target_x64 == swap_step.sqrt_price_next_x64;

    // Get the input/output amounts when target price is not reached
    if zero_for_one {
        // If max is reached for exact input case, entire amount_in is needed
        if !(max && is_base_input) {
            swap_step.amount_in = liquidity_math::get_delta_amount_0_unsigned(
                swap_step.sqrt_price_next_x64,
                sqrt_price_current_x64,
                liquidity,
                true,
            )
            .unwrap_or(0);
        }
        // If max is reached for exact output case, entire amount_out is needed
        if !(max && !is_base_input) {
            swap_step.amount_out = liquidity_math::get_delta_amount_1_unsigned(
                swap_step.sqrt_price_next_x64,
                sqrt_price_current_x64,
                liquidity,
                false,
            )
            .unwrap_or(0);
        }
    } else {
        if !(max && is_base_input) {
            swap_step.amount_in = liquidity_math::get_delta_amount_1_unsigned(
                sqrt_price_current_x64,
                swap_step.sqrt_price_next_x64,
                liquidity,
                true,
            )
            .unwrap_or(0);
        }
        if !(max && !is_base_input) {
            swap_step.amount_out = liquidity_math::get_delta_amount_0_unsigned(
                sqrt_price_current_x64,
                swap_step.sqrt_price_next_x64,
                liquidity,
                false,
            )
            .unwrap_or(0);
        }
    }

    // For exact output case, cap the output amount to not exceed the remaining output amount
    if !is_base_input && swap_step.amount_out > amount_remaining {
        swap_step.amount_out = amount_remaining;
    }

    swap_step.fee_amount =
        if is_base_input && swap_step.sqrt_price_next_x64 != sqrt_price_target_x64 {
            // We didn't reach the target, so take the remainder of the maximum input as fee
            amount_remaining.saturating_sub(swap_step.amount_in)
        } else {
            // Take percentage as fee
            (swap_step.amount_in as u128)
                .mul_div_ceil(
                    fee_rate as u128,
                    (FEE_RATE_DENOMINATOR_VALUE - fee_rate) as u128,
                )
                .unwrap_or(0) as u64
        };

    swap_step
}

/// Pre-calculate amount_in or amount_out for the specified price range
fn calculate_amount_in_range(
    sqrt_price_current_x64: u128,
    sqrt_price_target_x64: u128,
    liquidity: u128,
    zero_for_one: bool,
    is_base_input: bool,
) -> Option<u64> {
    if is_base_input {
        if zero_for_one {
            liquidity_math::get_delta_amount_0_unsigned(
                sqrt_price_target_x64,
                sqrt_price_current_x64,
                liquidity,
                true,
            )
        } else {
            liquidity_math::get_delta_amount_1_unsigned(
                sqrt_price_current_x64,
                sqrt_price_target_x64,
                liquidity,
                true,
            )
        }
    } else {
        if zero_for_one {
            liquidity_math::get_delta_amount_1_unsigned(
                sqrt_price_target_x64,
                sqrt_price_current_x64,
                liquidity,
                false,
            )
        } else {
            liquidity_math::get_delta_amount_0_unsigned(
                sqrt_price_current_x64,
                sqrt_price_target_x64,
                liquidity,
                false,
            )
        }
    }
}
