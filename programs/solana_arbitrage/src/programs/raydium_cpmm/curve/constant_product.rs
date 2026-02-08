//! The Uniswap invariantConstantProductCurve::

// use super::calculator::{RoundDirection, TradingTokenResult};
use crate::programs::raydium_cpmm::utils::CheckedCeilDiv;

/// ConstantProductCurve struct implementing CurveCalculator
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConstantProductCurve;

impl ConstantProductCurve {
    /// Constant product swap ensures x * y = constant
    /// The constant product swap calculation, factored out of its class for reuse.
    ///
    /// This is guaranteed to work for all values such that:
    ///  - 1 <= source_vault_amount * destination_vault_amount <= u128::MAX
    ///  - 1 <= source_amount <= u64::MAX
    pub fn swap_base_input_without_fees(
        input_amount: u128,
        input_vault_amount: u128,
        output_vault_amount: u128,
    ) -> u128 {
        // (x + delta_x) * (y - delta_y) = x * y
        // delta_y = (delta_x * y) / (x + delta_x)
        let numerator = input_amount.checked_mul(output_vault_amount).unwrap();
        let denominator = input_vault_amount.checked_add(input_amount).unwrap();
        let output_amount = numerator.checked_div(denominator).unwrap();
        output_amount
    }

    pub fn swap_base_output_without_fees(
        output_amount: u128,
        input_vault_amount: u128,
        output_vault_amount: u128,
    ) -> u128 {
        // (x + delta_x) * (y - delta_y) = x * y
        // delta_x = (x * delta_y) / (y - delta_y)
        let numerator = input_vault_amount.checked_mul(output_amount).unwrap();
        let denominator = output_vault_amount.checked_sub(output_amount).unwrap();
        let input_amount = numerator.checked_ceil_div(denominator).unwrap();
        input_amount
    }

}

// #[cfg(test)]
// mod tests {
//     // use {
//     //     super::*,
//     //     crate::programs::raydium_cpmm::curve::calculator::{
//     //         test::{
//     //             check_curve_value_from_swap, check_pool_value_from_deposit,
//     //             check_pool_value_from_withdraw, total_and_intermediate,
//     //         },
//     //         RoundDirection, TradeDirection,
//     //     },
//     //     proptest::prelude::*,
//     // };

//     // fn check_pool_token_rate(
//     //     token_a: u128,
//     //     token_b: u128,
//     //     deposit: u128,
//     //     supply: u128,
//     //     expected_a: u128,
//     //     expected_b: u128,
//     // ) {
//     //     let results = ConstantProductCurve::lp_tokens_to_trading_tokens(
//     //         deposit,
//     //         supply,
//     //         token_a,
//     //         token_b,
//     //         RoundDirection::Ceiling,
//     //     )
//     //     .unwrap();
//     //     assert_eq!(results.token_0_amount, expected_a);
//     //     assert_eq!(results.token_1_amount, expected_b);
//     // }

//     #[test]
//     fn trading_token_conversion() {
//         check_pool_token_rate(2, 49, 5, 10, 1, 25);
//         check_pool_token_rate(100, 202, 5, 101, 5, 10);
//         check_pool_token_rate(5, 501, 2, 10, 1, 101);
//     }

//     #[test]
//     fn fail_trading_token_conversion() {
//         let results = ConstantProductCurve::lp_tokens_to_trading_tokens(
//             5,
//             10,
//             u128::MAX,
//             0,
//             RoundDirection::Floor,
//         );
//         assert!(results.is_none());
//         let results = ConstantProductCurve::lp_tokens_to_trading_tokens(
//             5,
//             10,
//             0,
//             u128::MAX,
//             RoundDirection::Floor,
//         );
//         assert!(results.is_none());
//     }

//     fn test_truncation(
//         source_amount: u128,
//         swap_source_amount: u128,
//         swap_destination_amount: u128,
//         expected_source_amount_swapped: u128,
//         expected_destination_amount_swapped: u128,
//     ) {
//         let invariant = swap_source_amount * swap_destination_amount;
//         let destination_amount_swapped = ConstantProductCurve::swap_base_input_without_fees(
//             source_amount,
//             swap_source_amount,
//             swap_destination_amount,
//         );
//         assert_eq!(source_amount, expected_source_amount_swapped);
//         assert_eq!(
//             destination_amount_swapped,
//             expected_destination_amount_swapped
//         );
//         let new_invariant = (swap_source_amount + source_amount)
//             * (swap_destination_amount - destination_amount_swapped);
//         assert!(new_invariant >= invariant);
//     }

//     #[test]
//     fn constant_product_swap_rounding() {
//         let tests: &[(u128, u128, u128, u128, u128)] = &[
//             // spot: 10 * 70b / ~4m = 174,999.99
//             (10, 4_000_000, 70_000_000_000, 10, 174_999),
//             // spot: 20 * 1 / 3.000 = 6.6667 (source can be 18 to get 6 dest.)
//             (20, 30_000 - 20, 10_000, 20, 6),
//             // spot: 19 * 1 / 2.999 = 6.3334 (source can be 18 to get 6 dest.)
//             (19, 30_000 - 20, 10_000, 19, 6),
//             // spot: 18 * 1 / 2.999 = 6.0001
//             (18, 30_000 - 20, 10_000, 18, 6),
//             // spot: 10 * 3 / 2.0010 = 14.99
//             (10, 20_000, 30_000, 10, 14),
//             // spot: 10 * 3 / 2.0001 = 14.999
//             (10, 20_000 - 9, 30_000, 10, 14),
//             // spot: 10 * 3 / 2.0000 = 15
//             (10, 20_000 - 10, 30_000, 10, 15),
//             // spot: 100 * 3 / 6.001 = 49.99 (source can be 99 to get 49 dest.)
//             (100, 60_000, 30_000, 100, 49),
//             // spot: 99 * 3 / 6.001 = 49.49
//             (99, 60_000, 30_000, 99, 49),
//             // spot: 98 * 3 / 6.001 = 48.99 (source can be 97 to get 48 dest.)
//             (98, 60_000, 30_000, 98, 48),
//         ];
//         for (
//             source_amount,
//             swap_source_amount,
//             swap_destination_amount,
//             expected_source_amount,
//             expected_destination_amount,
//         ) in tests.iter()
//         {
//             test_truncation(
//                 *source_amount,
//                 *swap_source_amount,
//                 *swap_destination_amount,
//                 *expected_source_amount,
//                 *expected_destination_amount,
//             );
//         }
//     }

//     proptest! {
//         #[test]
//         fn curve_value_does_not_decrease_from_swap(
//             source_token_amount in 1..u64::MAX,
//             swap_source_amount in 1..u64::MAX,
//             swap_destination_amount in 1..u64::MAX,
//         ) {
//             check_curve_value_from_swap(
//                 source_token_amount as u128,
//                 swap_source_amount as u128,
//                 swap_destination_amount as u128,
//                 TradeDirection::ZeroForOne
//             );
//         }
//     }

//     proptest! {
//         #[test]
//         fn curve_value_does_not_decrease_from_deposit(
//             pool_token_amount in 1..u64::MAX,
//             pool_token_supply in 1..u64::MAX,
//             swap_token_a_amount in 1..u64::MAX,
//             swap_token_b_amount in 1..u64::MAX,
//         ) {
//             let pool_token_amount = pool_token_amount as u128;
//             let pool_token_supply = pool_token_supply as u128;
//             let swap_token_a_amount = swap_token_a_amount as u128;
//             let swap_token_b_amount = swap_token_b_amount as u128;
//             // Make sure we will get at least one trading token out for each
//             // side, otherwise the calculation fails
//             prop_assume!(pool_token_amount * swap_token_a_amount / pool_token_supply >= 1);
//             prop_assume!(pool_token_amount * swap_token_b_amount / pool_token_supply >= 1);
//             check_pool_value_from_deposit(
//                 pool_token_amount,
//                 pool_token_supply,
//                 swap_token_a_amount,
//                 swap_token_b_amount,
//             );
//         }
//     }

//     proptest! {
//         #[test]
//         fn curve_value_does_not_decrease_from_withdraw(
//             (pool_token_supply, pool_token_amount) in total_and_intermediate(u64::MAX),
//             swap_token_a_amount in 1..u64::MAX,
//             swap_token_b_amount in 1..u64::MAX,
//         ) {
//             let pool_token_amount = pool_token_amount as u128;
//             let pool_token_supply = pool_token_supply as u128;
//             let swap_token_a_amount = swap_token_a_amount as u128;
//             let swap_token_b_amount = swap_token_b_amount as u128;
//             // Make sure we will get at least one trading token out for each
//             // side, otherwise the calculation fails
//             prop_assume!(pool_token_amount * swap_token_a_amount / pool_token_supply >= 1);
//             prop_assume!(pool_token_amount * swap_token_b_amount / pool_token_supply >= 1);
//             check_pool_value_from_withdraw(
//                 pool_token_amount,
//                 pool_token_supply,
//                 swap_token_a_amount,
//                 swap_token_b_amount,
//             );
//         }
//     }
// }
