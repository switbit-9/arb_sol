use super::liquidity_math;
use super::sqrt_price_math;

/// Fee rate denominator for Orca Whirlpool (1,000,000 = 100%)
/// Fee rate is in hundredths of a basis point
pub const FEE_RATE_DENOMINATOR: u64 = 1_000_000;

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

/// Compute a swap step within a single tick range
///
/// # Arguments
/// * `sqrt_price_current_x64` - Current sqrt price as Q64.64
/// * `sqrt_price_target_x64` - Target sqrt price (tick boundary or limit)
/// * `liquidity` - Available liquidity in current range
/// * `amount_remaining` - Remaining amount to swap
/// * `fee_rate` - Total fee rate (static + adaptive) in hundredths of basis point (out of 1,000,000)
/// * `is_base_input` - True if amount_remaining is input, false if output
/// * `a_to_b` - True if swapping token A for token B
///
/// # Returns
/// SwapStep with the results of the swap within this tick range
pub fn compute_swap_step(
    sqrt_price_current_x64: u128,
    sqrt_price_target_x64: u128,
    liquidity: u128,
    amount_remaining: u64,
    fee_rate: u32,
    is_base_input: bool,
    a_to_b: bool,
) -> SwapStep {
    if liquidity == 0 || amount_remaining == 0 {
        return SwapStep {
            sqrt_price_next_x64: sqrt_price_target_x64,
            amount_in: 0,
            amount_out: 0,
            fee_amount: 0,
        };
    }

    let fee_rate_64 = fee_rate as u64;

    if is_base_input {
        // Exact input case: we have a specific input amount
        // Deduct fee from input first
        let amount_remaining_less_fee = if fee_rate_64 > 0 {
            let fee_complement = FEE_RATE_DENOMINATOR - fee_rate_64;
            ((amount_remaining as u128) * (fee_complement as u128) / FEE_RATE_DENOMINATOR as u128) as u64
        } else {
            amount_remaining
        };

        // Calculate max amount_in to reach target price
        let amount_in_max = liquidity_math::get_amount_in_for_liquidity(
            sqrt_price_current_x64,
            sqrt_price_target_x64,
            liquidity,
            a_to_b,
        ).unwrap_or(u64::MAX);

        let (sqrt_price_next_x64, amount_in, amount_out, fee_amount) =
            if amount_remaining_less_fee >= amount_in_max {
                // We can reach the target price
                let amount_out = liquidity_math::get_amount_out_for_liquidity(
                    sqrt_price_current_x64,
                    sqrt_price_target_x64,
                    liquidity,
                    a_to_b,
                ).unwrap_or(0);

                let fee_amount = if fee_rate_64 > 0 && amount_in_max > 0 {
                    let amount_in_with_fee = ((amount_in_max as u128) * FEE_RATE_DENOMINATOR as u128)
                        .checked_div(FEE_RATE_DENOMINATOR as u128 - fee_rate_64 as u128)
                        .map(|v| v as u64)
                        .unwrap_or(amount_in_max);
                    amount_in_with_fee.saturating_sub(amount_in_max)
                } else {
                    0
                };

                (sqrt_price_target_x64, amount_in_max, amount_out, fee_amount)
            } else {
                // We can't reach the target price, calculate new price from input
                let sqrt_price_next = sqrt_price_math::get_next_sqrt_price_from_input(
                    sqrt_price_current_x64,
                    liquidity,
                    amount_remaining_less_fee,
                    a_to_b,
                ).unwrap_or(sqrt_price_current_x64);

                let amount_out = liquidity_math::get_amount_out_for_liquidity(
                    sqrt_price_current_x64,
                    sqrt_price_next,
                    liquidity,
                    a_to_b,
                ).unwrap_or(0);

                let fee_amount = amount_remaining.saturating_sub(amount_remaining_less_fee);

                (sqrt_price_next, amount_remaining_less_fee, amount_out, fee_amount)
            };

        SwapStep {
            sqrt_price_next_x64,
            amount_in,
            amount_out,
            fee_amount,
        }
    } else {
        // Exact output case: we want a specific output amount
        // Calculate max amount_out available at target price
        let amount_out_max = liquidity_math::get_amount_out_for_liquidity(
            sqrt_price_current_x64,
            sqrt_price_target_x64,
            liquidity,
            a_to_b,
        ).unwrap_or(0);

        let (sqrt_price_next_x64, amount_in, amount_out) = if amount_remaining >= amount_out_max {
            // We can reach the target price
            let amount_in = liquidity_math::get_amount_in_for_liquidity(
                sqrt_price_current_x64,
                sqrt_price_target_x64,
                liquidity,
                a_to_b,
            ).unwrap_or(0);

            (sqrt_price_target_x64, amount_in, amount_out_max)
        } else {
            // Calculate new price from desired output
            let sqrt_price_next = sqrt_price_math::get_next_sqrt_price_from_output(
                sqrt_price_current_x64,
                liquidity,
                amount_remaining,
                a_to_b,
            ).unwrap_or(sqrt_price_current_x64);

            let amount_in = liquidity_math::get_amount_in_for_liquidity(
                sqrt_price_current_x64,
                sqrt_price_next,
                liquidity,
                a_to_b,
            ).unwrap_or(0);

            (sqrt_price_next, amount_in, amount_remaining)
        };

        // Calculate fee on input
        let fee_amount = if fee_rate_64 > 0 && amount_in > 0 {
            let fee_complement = FEE_RATE_DENOMINATOR - fee_rate_64;
            let amount_in_with_fee = ((amount_in as u128) * FEE_RATE_DENOMINATOR as u128)
                .checked_div(fee_complement as u128)
                .map(|v| v as u64)
                .unwrap_or(amount_in);
            amount_in_with_fee.saturating_sub(amount_in)
        } else {
            0
        };

        SwapStep {
            sqrt_price_next_x64,
            amount_in,
            amount_out,
            fee_amount,
        }
    }
}
