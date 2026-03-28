use super::clmm_sim::ClmmPool;
use super::amm_sim::AmmPool;
use super::dlmm_sim::DlmmPool;
use super::whirlpool_sim::WhirlpoolPool;
use super::optimizer::{optimal_amm_amm, ternary_search_maximize};
use super::{ArbitrageResult, FD};
use crate::programs::raydium_clmm::libraries::liquidity_math;

#[inline]
fn finish(total_in: u64, total_out: u64) -> ArbitrageResult {
    let profit = total_out as i64 - total_in as i64;
    if profit > 0 {
        ArbitrageResult::from_pair(total_in, profit)
    } else {
        ArbitrageResult::none()
    }
}

// -- CLMM -> AMM Arbitrage --

/// Check arbitrage: buy on Raydium CLMM, sell on AMM CP pool.
///
/// Path: input -[CLMM swap]-> mid -[AMM sell/buy]-> output
///
/// Iterates CLMM tick ranges greedily. Within each tick range the CLMM
/// acts as a constant-product pool with virtual reserves. For each range,
/// uses `optimal_amm_amm` (CLMM virtual reserves as pool A, AMM as pool B).
pub fn check_clmm_to_amm(
    clmm: &ClmmPool,
    zero_for_one: bool,
    amm: &AmmPool,
    amm_sells_base: bool,
    max_amount_in: u64,
) -> ArbitrageResult {
    let mut total_in: u64 = 0;
    let mut total_out: u64 = 0;

    let clmm_fee = clmm.fee_rate as u64;
    let clmm_ff = clmm.fee_factor();

    // AMM direction fees (raw values for optimal_amm_amm)
    let (amm_fi_raw, amm_fo_raw) = if amm_sells_base {
        (amm.sell_input_fee, amm.sell_output_fee)
    } else {
        (amm.buy_input_fee, amm.buy_output_fee)
    };
    let cp_fi = FD - amm_fi_raw as u128;
    let cp_fo = FD - amm_fo_raw as u128;

    // AMM virtual reserves (evolving as mid tokens enter)
    let (mut amm_res_in, mut amm_res_out) = if amm_sells_base {
        (amm.base_vault as u128, amm.quote_vault as u128)
    } else {
        (amm.quote_vault as u128, amm.base_vault as u128)
    };

    // CLMM evolving state
    let mut clmm_sqrt_price = clmm.sqrt_price;
    let mut clmm_liquidity = clmm.liquidity;
    let mut clmm_tick = clmm.tick_current_index;

    for _ in 0..20 {
        if clmm_liquidity == 0 { break; }

        let next = match clmm.find_next_tick(clmm_tick, zero_for_one) {
            Some(t) => *t,
            None => break,
        };

        let sqrt_target = ClmmPool::sqrt_price_at_tick_clamped(next.tick_index, zero_for_one);

        // Virtual reserves for this tick range
        let (v_a, v_b) = ClmmPool::virtual_reserves(clmm_sqrt_price, clmm_liquidity);
        let (clmm_res_in, clmm_res_out) = if zero_for_one { (v_a, v_b) } else { (v_b, v_a) };

        // Net capacity in this tick range (after fee deduction)
        let cap_net = liquidity_math::get_amount_in_for_liquidity(
            clmm_sqrt_price, sqrt_target, clmm_liquidity, zero_for_one,
        )
        .unwrap_or(0);

        if cap_net == 0 || clmm_res_in == 0 || clmm_res_out == 0 {
            // Empty range -- cross tick and continue
            clmm_liquidity = ClmmPool::cross_tick(clmm_liquidity, next.liquidity_net, zero_for_one);
            clmm_sqrt_price = sqrt_target;
            clmm_tick = if zero_for_one { next.tick_index - 1 } else { next.tick_index };
            continue;
        }

        let cap_gross = ((cap_net as u128) * FD / clmm_ff + 1).min(u64::MAX as u128) as u64;
        let remaining = max_amount_in.saturating_sub(total_in);
        if remaining == 0 { break; }

        // Optimal for (CLMM as first CP pool, AMM as second CP pool)
        let (opt_amt, opt_profit) = optimal_amm_amm(
            clmm_res_in,
            clmm_res_out,
            clmm_fee,
            0, // CLMM has no output fee
            amm_res_in.min(u64::MAX as u128) as u64,
            amm_res_out.min(u64::MAX as u128) as u64,
            amm_fi_raw,
            amm_fo_raw,
        );

        if opt_profit <= 0 {
            if cap_net > 1000 { break; }
            clmm_liquidity = ClmmPool::cross_tick(clmm_liquidity, next.liquidity_net, zero_for_one);
            clmm_sqrt_price = sqrt_target;
            clmm_tick = if zero_for_one { next.tick_index - 1 } else { next.tick_index };
            continue;
        }

        let amount_in = opt_amt.min(cap_gross).min(remaining);

        // Simulate CLMM swap (CP formula with virtual reserves)
        let in_eff = (amount_in as u128) * clmm_ff / FD;
        let mid = (clmm_res_out as u128) * in_eff / ((clmm_res_in as u128) + in_eff);

        // Simulate AMM swap
        let mid_eff = mid * cp_fi / FD;
        let raw_out = amm_res_out * mid_eff / (amm_res_in + mid_eff);
        let out = (raw_out * cp_fo / FD) as u64;

        total_in += amount_in;
        total_out += out;

        // Update AMM virtual reserves
        amm_res_in += mid_eff;
        amm_res_out = amm_res_out.saturating_sub(raw_out);

        if amount_in >= cap_gross {
            // Consumed full CLMM range -- cross tick and continue
            clmm_liquidity = ClmmPool::cross_tick(clmm_liquidity, next.liquidity_net, zero_for_one);
            clmm_sqrt_price = sqrt_target;
            clmm_tick = if zero_for_one { next.tick_index - 1 } else { next.tick_index };
        } else {
            // Optimal was within range (or budget limited), done
            break;
        }
    }

    finish(total_in, total_out)
}

// -- AMM -> CLMM Arbitrage --

/// Check arbitrage: buy on AMM CP pool, sell on Raydium CLMM.
///
/// Path: input -[AMM buy/sell]-> mid -[CLMM swap]-> output
///
/// Iterates CLMM tick ranges. For each range, uses `optimal_amm_amm`
/// (AMM as pool A, CLMM virtual reserves as pool B). Caps mid tokens
/// by CLMM tick range capacity.
pub fn check_amm_to_clmm(
    amm: &AmmPool,
    amm_buys_base: bool,
    clmm: &ClmmPool,
    zero_for_one: bool,
    max_amount_in: u64,
) -> ArbitrageResult {
    let mut total_in: u64 = 0;
    let mut total_out: u64 = 0;

    // AMM fees
    let (amm_fi_raw, amm_fo_raw) = if amm_buys_base {
        (amm.buy_input_fee, amm.buy_output_fee)
    } else {
        (amm.sell_input_fee, amm.sell_output_fee)
    };
    let fi = FD - amm_fi_raw as u128;
    let fo = FD - amm_fo_raw as u128;

    // AMM reserves (evolving) -- oriented for buy/sell direction
    let (mut amm_res_in, mut amm_res_out) = if amm_buys_base {
        (amm.quote_vault as u128, amm.base_vault as u128)
    } else {
        (amm.base_vault as u128, amm.quote_vault as u128)
    };

    let clmm_fee = clmm.fee_rate as u64;
    let clmm_ff = clmm.fee_factor();

    // CLMM evolving state
    let mut clmm_sqrt_price = clmm.sqrt_price;
    let mut clmm_liquidity = clmm.liquidity;
    let mut clmm_tick = clmm.tick_current_index;

    for _ in 0..20 {
        if clmm_liquidity == 0 { break; }

        let next = match clmm.find_next_tick(clmm_tick, zero_for_one) {
            Some(t) => *t,
            None => break,
        };

        let sqrt_target = ClmmPool::sqrt_price_at_tick_clamped(next.tick_index, zero_for_one);

        let (v_a, v_b) = ClmmPool::virtual_reserves(clmm_sqrt_price, clmm_liquidity);
        // CLMM receives mid tokens as input, produces output tokens
        let (clmm_res_mid, clmm_res_out) = if zero_for_one { (v_a, v_b) } else { (v_b, v_a) };

        // CLMM capacity in mid tokens (net, after CLMM fee deduction)
        let cap_net_mid = liquidity_math::get_amount_in_for_liquidity(
            clmm_sqrt_price, sqrt_target, clmm_liquidity, zero_for_one,
        )
        .unwrap_or(0);

        if cap_net_mid == 0 || clmm_res_mid == 0 || clmm_res_out == 0 {
            clmm_liquidity = ClmmPool::cross_tick(clmm_liquidity, next.liquidity_net, zero_for_one);
            clmm_sqrt_price = sqrt_target;
            clmm_tick = if zero_for_one { next.tick_index - 1 } else { next.tick_index };
            continue;
        }

        let remaining = max_amount_in.saturating_sub(total_in);
        if remaining == 0 { break; }

        // Optimal: AMM is "Pool A" (buy side), CLMM is "Pool B" (sell side)
        let (opt_amt, opt_profit) = optimal_amm_amm(
            amm_res_in.min(u64::MAX as u128) as u64,
            amm_res_out.min(u64::MAX as u128) as u64,
            amm_fi_raw,
            amm_fo_raw,
            clmm_res_mid,  // CLMM input = mid tokens
            clmm_res_out,  // CLMM output
            clmm_fee,      // CLMM fee on input (mid)
            0,              // CLMM has no output fee
        );

        if opt_profit <= 0 {
            if cap_net_mid > 1000 { break; }
            clmm_liquidity = ClmmPool::cross_tick(clmm_liquidity, next.liquidity_net, zero_for_one);
            clmm_sqrt_price = sqrt_target;
            clmm_tick = if zero_for_one { next.tick_index - 1 } else { next.tick_index };
            continue;
        }

        let mut amount_in = opt_amt.min(remaining);

        // Simulate AMM swap to get mid
        let in_eff = (amount_in as u128) * fi / FD;
        let mid_raw = amm_res_out * in_eff / (amm_res_in + in_eff);
        let mut mid = mid_raw * fo / FD;

        // Cap mid by CLMM tick range capacity (gross = net * FD / fee_factor)
        let cap_gross_mid = ((cap_net_mid as u128) * FD / clmm_ff + 1).min(u64::MAX as u128);
        let mut capped = false;
        if mid > cap_gross_mid {
            capped = true;
            // Reverse-compute input that produces cap_gross_mid of mid from AMM
            let target_mid_raw = if fo == FD {
                cap_gross_mid
            } else {
                cap_gross_mid * FD / fo
            };
            if target_mid_raw >= amm_res_out { break; }
            let in_eff_capped = amm_res_in * target_mid_raw / (amm_res_out - target_mid_raw) + 1;
            amount_in = ((in_eff_capped * FD / fi + 1).min(u64::MAX as u128) as u64).min(remaining);

            // Recompute mid with capped input
            let in_eff_c = (amount_in as u128) * fi / FD;
            let mid_raw_c = amm_res_out * in_eff_c / (amm_res_in + in_eff_c);
            mid = mid_raw_c * fo / FD;
        }

        // Simulate CLMM swap with mid tokens
        let mid_eff = mid * clmm_ff / FD;
        let clmm_raw_out = (clmm_res_out as u128) * mid_eff / ((clmm_res_mid as u128) + mid_eff);
        let out = clmm_raw_out as u64;

        if out as i128 <= amount_in as i128 {
            if cap_net_mid > 1000 { break; }
            clmm_liquidity = ClmmPool::cross_tick(clmm_liquidity, next.liquidity_net, zero_for_one);
            clmm_sqrt_price = sqrt_target;
            clmm_tick = if zero_for_one { next.tick_index - 1 } else { next.tick_index };
            continue;
        }

        total_in += amount_in;
        total_out += out;

        // Update AMM reserves
        let in_eff_actual = (amount_in as u128) * fi / FD;
        let mid_raw_actual = amm_res_out * in_eff_actual / (amm_res_in + in_eff_actual);
        amm_res_in += in_eff_actual;
        amm_res_out = amm_res_out.saturating_sub(mid_raw_actual);

        // Advance: cross tick if CLMM range exhausted, else done
        if capped {
            clmm_liquidity = ClmmPool::cross_tick(clmm_liquidity, next.liquidity_net, zero_for_one);
            clmm_sqrt_price = sqrt_target;
            clmm_tick = if zero_for_one { next.tick_index - 1 } else { next.tick_index };
        } else {
            break;
        }
    }

    finish(total_in, total_out)
}

// -- CLMM <-> CLMM Arbitrage --

/// Check arbitrage between two Raydium CLMM pools sharing a common token.
///
/// Path: input -[pool_a swap]-> mid -[pool_b swap]-> output
///
/// Uses ternary search over the concave profit function.
/// Each evaluation simulates full tick-traversal swaps on both pools.
pub fn check_clmm_clmm(
    pool_a: &ClmmPool,
    zero_for_one_a: bool,
    pool_b: &ClmmPool,
    zero_for_one_b: bool,
    max_amount_in: u64,
) -> ArbitrageResult {
    // Quick marginal check: is there any arb at a small test amount?
    let test_amt = 10_000u64.min(max_amount_in);
    if test_amt == 0 {
        return ArbitrageResult::none();
    }
    let mid = pool_a.quote_exact_in(test_amt, zero_for_one_a);
    let out = pool_b.quote_exact_in(mid, zero_for_one_b);
    if out <= test_amt {
        return ArbitrageResult::none();
    }

    let profit_fn = |amount_in: u64| -> i128 {
        if amount_in == 0 {
            return 0;
        }
        let mid = pool_a.quote_exact_in(amount_in, zero_for_one_a);
        let out = pool_b.quote_exact_in(mid, zero_for_one_b);
        out as i128 - amount_in as i128
    };

    let (amt, profit) = ternary_search_maximize(profit_fn, 1, max_amount_in, 44);

    if profit > 0 {
        ArbitrageResult::from_pair(amt, profit as i64)
    } else {
        ArbitrageResult::none()
    }
}

// -- CLMM <-> DLMM Arbitrage --

/// Check arbitrage: buy on CLMM, sell on DLMM.
/// Uses ternary search over the concave profit function.
pub fn check_clmm_to_dlmm(
    clmm: &ClmmPool,
    zero_for_one: bool,
    dlmm: &DlmmPool,
    swap_for_y: bool,
    max_amount_in: u64,
    accounts: &[anchor_lang::prelude::AccountInfo],
) -> ArbitrageResult {
    let test_amt = 10_000u64.min(max_amount_in);
    if test_amt == 0 { return ArbitrageResult::none(); }
    let mid = clmm.quote_exact_in(test_amt, zero_for_one);
    let (out, _fee) = dlmm.quote_exact_in(accounts, mid, swap_for_y).unwrap_or((0, 0));
    if out <= test_amt { return ArbitrageResult::none(); }

    let profit_fn = |amount_in: u64| -> i128 {
        if amount_in == 0 { return 0; }
        let mid = clmm.quote_exact_in(amount_in, zero_for_one);
        let (out, _) = dlmm.quote_exact_in(accounts, mid, swap_for_y).unwrap_or((0, 0));
        out as i128 - amount_in as i128
    };

    let (amt, profit) = ternary_search_maximize(profit_fn, 1, max_amount_in, 44);
    if profit > 0 {
        ArbitrageResult::from_pair(amt, profit as i64)
    } else {
        ArbitrageResult::none()
    }
}

/// Check arbitrage: buy on DLMM, sell on CLMM.
/// Uses ternary search over the concave profit function.
pub fn check_dlmm_to_clmm(
    dlmm: &DlmmPool,
    swap_for_y: bool,
    clmm: &ClmmPool,
    zero_for_one: bool,
    max_amount_in: u64,
    accounts: &[anchor_lang::prelude::AccountInfo],
) -> ArbitrageResult {
    let test_amt = 10_000u64.min(max_amount_in);
    if test_amt == 0 { return ArbitrageResult::none(); }
    let (mid, _fee) = dlmm.quote_exact_in(accounts, test_amt, swap_for_y).unwrap_or((0, 0));
    let out = clmm.quote_exact_in(mid, zero_for_one);
    if out <= test_amt { return ArbitrageResult::none(); }

    let profit_fn = |amount_in: u64| -> i128 {
        if amount_in == 0 { return 0; }
        let (mid, _) = dlmm.quote_exact_in(accounts, amount_in, swap_for_y).unwrap_or((0, 0));
        let out = clmm.quote_exact_in(mid, zero_for_one);
        out as i128 - amount_in as i128
    };

    let (amt, profit) = ternary_search_maximize(profit_fn, 1, max_amount_in, 44);
    if profit > 0 {
        ArbitrageResult::from_pair(amt, profit as i64)
    } else {
        ArbitrageResult::none()
    }
}

// -- CLMM <-> Whirlpool Arbitrage --

/// Check arbitrage: buy on CLMM, sell on Whirlpool.
/// Uses ternary search over the concave profit function.
pub fn check_clmm_to_whirlpool(
    clmm: &ClmmPool,
    zero_for_one: bool,
    wp: &WhirlpoolPool,
    a_to_b: bool,
    max_amount_in: u64,
) -> ArbitrageResult {
    let test_amt = 10_000u64.min(max_amount_in);
    if test_amt == 0 { return ArbitrageResult::none(); }
    let mid = clmm.quote_exact_in(test_amt, zero_for_one);
    let out = wp.quote_exact_in(mid, a_to_b);
    if out <= test_amt { return ArbitrageResult::none(); }

    let profit_fn = |amount_in: u64| -> i128 {
        if amount_in == 0 { return 0; }
        let mid = clmm.quote_exact_in(amount_in, zero_for_one);
        let out = wp.quote_exact_in(mid, a_to_b);
        out as i128 - amount_in as i128
    };

    let (amt, profit) = ternary_search_maximize(profit_fn, 1, max_amount_in, 44);
    if profit > 0 {
        ArbitrageResult::from_pair(amt, profit as i64)
    } else {
        ArbitrageResult::none()
    }
}

/// Check arbitrage: buy on Whirlpool, sell on CLMM.
/// Uses ternary search over the concave profit function.
pub fn check_whirlpool_to_clmm(
    wp: &WhirlpoolPool,
    a_to_b: bool,
    clmm: &ClmmPool,
    zero_for_one: bool,
    max_amount_in: u64,
) -> ArbitrageResult {
    let test_amt = 10_000u64.min(max_amount_in);
    if test_amt == 0 { return ArbitrageResult::none(); }
    let mid = wp.quote_exact_in(test_amt, a_to_b);
    let out = clmm.quote_exact_in(mid, zero_for_one);
    if out <= test_amt { return ArbitrageResult::none(); }

    let profit_fn = |amount_in: u64| -> i128 {
        if amount_in == 0 { return 0; }
        let mid = wp.quote_exact_in(amount_in, a_to_b);
        let out = clmm.quote_exact_in(mid, zero_for_one);
        out as i128 - amount_in as i128
    };

    let (amt, profit) = ternary_search_maximize(profit_fn, 1, max_amount_in, 44);
    if profit > 0 {
        ArbitrageResult::from_pair(amt, profit as i64)
    } else {
        ArbitrageResult::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::clmm_sim::ClmmTick;

    fn make_clmm_cheap_base() -> ClmmPool {
        // CLMM at price=0.01 (base is cheap relative to quote)
        // sqrt(0.01) = 0.1, in Q64.64: 0.1 * 2^64
        let sqrt_price = 1_844_674_407_370_955_161u128;
        ClmmPool::new(
            sqrt_price,
            100_000_000_000_000, // high liquidity
            -23028,              // tick for price ~0.01
            10,
            3000, // 0.3%
            &[
                ClmmTick { tick_index: -30000, liquidity_net: 100_000_000_000_000 },
                ClmmTick { tick_index: -10000, liquidity_net: -100_000_000_000_000 },
            ],
        )
    }

    fn make_clmm_expensive_base() -> ClmmPool {
        // CLMM at price=0.05 (base is expensive relative to quote)
        // sqrt(0.05) ~ 0.2236, in Q64.64: 0.2236 * 2^64
        let sqrt_price = 4_125_206_105_282_697_830u128;
        ClmmPool::new(
            sqrt_price,
            50_000_000_000_000,
            -14979, // tick for price ~0.05
            10,
            3000,
            &[
                ClmmTick { tick_index: -20000, liquidity_net: 50_000_000_000_000 },
                ClmmTick { tick_index: -5000, liquidity_net: -50_000_000_000_000 },
            ],
        )
    }

    #[test]
    fn test_clmm_to_amm_arb() {
        // CLMM has cheap base, AMM has expensive base -> buy on CLMM, sell on AMM
        let clmm = make_clmm_cheap_base();
        let amm = AmmPool::from_pump(500_000_000_000, 50_000_000_000);

        let result = check_clmm_to_amm(&clmm, true, &amm, true, u64::MAX);
        eprintln!("clmm_to_amm: profit={} amount_in={}", result.profit, result.amount_in);
    }

    #[test]
    fn test_amm_to_clmm_arb() {
        // AMM has cheap base, CLMM has expensive base -> buy on AMM, sell on CLMM
        let amm = AmmPool::from_pump(1_000_000_000_000, 10_000_000);
        let clmm = make_clmm_expensive_base();

        let result = check_amm_to_clmm(&amm, true, &clmm, true, u64::MAX);
        eprintln!("amm_to_clmm: profit={} amount_in={}", result.profit, result.amount_in);
    }

    #[test]
    fn test_clmm_clmm_arb() {
        let pool_cheap = make_clmm_cheap_base();
        let pool_expensive = make_clmm_expensive_base();

        let result = check_clmm_clmm(
            &pool_cheap,
            true,  // zero_for_one on cheap pool
            &pool_expensive,
            false, // one_for_zero on expensive pool
            u64::MAX,
        );
        eprintln!("clmm_clmm: profit={} amount_in={}", result.profit, result.amount_in);
    }

    /// Precision test: verify the inline CP formula matches quote_exact_in
    /// (which uses exact U256 on-chain math) within acceptable tolerance.
    #[test]
    fn test_cp_vs_exact_precision() {
        let test_cases: Vec<(u128, u128, i32, u64)> = vec![
            (1u128 << 64, 1_000_000_000_000, 0, 1_000_000),
            (1u128 << 64, 1_000_000_000_000, 0, 1_000_000_000),
            (1u128 << 64, 1_000_000_000_000, 0, 100_000_000_000),
            (1_844_674_407_370_955_161, 100_000_000_000_000, -23028, 500_000_000),
            (4_125_206_105_282_697_830, 50_000_000_000_000, -14979, 1_000_000_000),
            (18_446_744_073_709_551_616 * 10, 500_000_000_000, 23028, 2_000_000_000),
        ];

        for (sqrt_price, liquidity, tick, amount_in) in &test_cases {
            let pool = ClmmPool::new(
                *sqrt_price, *liquidity, *tick, 10, 3000,
                &[
                    ClmmTick { tick_index: tick - 5000, liquidity_net: *liquidity as i128 },
                    ClmmTick { tick_index: tick + 5000, liquidity_net: -(*liquidity as i128) },
                ],
            );

            // Method 1: quote_exact_in (uses compute_swap_step with U256 math)
            let exact_out = pool.quote_exact_in(*amount_in, true);

            // Method 2: inline CP formula (what the checker uses)
            let clmm_ff = pool.fee_factor();
            let (v_a, v_b) = ClmmPool::virtual_reserves(*sqrt_price, *liquidity);
            let in_eff = (*amount_in as u128) * clmm_ff / FD;
            let cp_out = (v_b as u128) * in_eff / ((v_a as u128) + in_eff);

            let diff = (exact_out as i128 - cp_out as i128).unsigned_abs();
            let max_out = exact_out.max(cp_out as u64);
            let pct_err = if max_out > 0 {
                (diff as f64 / max_out as f64) * 100.0
            } else {
                0.0
            };

            eprintln!(
                "price={:.6} amt={} exact={} cp={} diff={} err={:.6}%",
                (*sqrt_price as f64 / (1u128 << 64) as f64).powi(2),
                amount_in, exact_out, cp_out, diff, pct_err
            );

            assert!(
                pct_err < 0.001 || diff <= 5,
                "CP divergence too large: exact={} cp={} diff={} err={:.6}%",
                exact_out, cp_out, diff, pct_err
            );
        }
    }

    /// End-to-end precision test: verify checker profit matches
    /// independent simulation using quote_exact_in + AmmPool methods.
    #[test]
    fn test_clmm_to_amm_profit_precision() {
        let clmm = ClmmPool::new(
            4_125_206_105_282_697_830, // price ~0.05
            50_000_000_000_000,
            -14979,
            10,
            3000,
            &[
                ClmmTick { tick_index: -20000, liquidity_net: 50_000_000_000_000 },
                ClmmTick { tick_index: -5000, liquidity_net: -50_000_000_000_000 },
            ],
        );
        let amm = AmmPool::from_pump(500_000_000_000, 50_000_000_000);

        let result = check_clmm_to_amm(&clmm, true, &amm, true, u64::MAX);
        if result.profit <= 0 { return; }

        // Independent verification: simulate with quote_exact_in + sell_base
        let mid = clmm.quote_exact_in(result.amount_in, true);
        let out = amm.sell_base(mid);
        let verified_profit = out as i64 - result.amount_in as i64;

        let diff = (result.profit as i64 - verified_profit).unsigned_abs();
        let pct_err = if result.profit > 0 {
            (diff as f64 / result.profit as f64) * 100.0
        } else {
            0.0
        };

        eprintln!(
            "CLMM->AMM: checker_profit={} verified_profit={} diff={} err={:.4}%",
            result.profit, verified_profit, diff, pct_err
        );

        assert!(
            pct_err < 0.01 || diff <= 100,
            "Profit divergence too large: checker={} verified={} diff={} err={:.4}%",
            result.profit, verified_profit, diff, pct_err
        );
    }

    /// End-to-end precision test for AMM->CLMM
    #[test]
    fn test_amm_to_clmm_profit_precision() {
        let amm = AmmPool::from_pump(1_000_000_000_000, 10_000_000);
        let clmm = ClmmPool::new(
            4_125_206_105_282_697_830,
            50_000_000_000_000,
            -14979,
            10,
            3000,
            &[
                ClmmTick { tick_index: -20000, liquidity_net: 50_000_000_000_000 },
                ClmmTick { tick_index: -5000, liquidity_net: -50_000_000_000_000 },
            ],
        );

        let result = check_amm_to_clmm(&amm, true, &clmm, true, u64::MAX);
        if result.profit <= 0 { return; }

        // Independent verification
        let mid = amm.buy_base(result.amount_in);
        let out = clmm.quote_exact_in(mid, true);
        let verified_profit = out as i64 - result.amount_in as i64;

        let diff = (result.profit as i64 - verified_profit).unsigned_abs();
        let pct_err = if result.profit > 0 {
            (diff as f64 / result.profit as f64) * 100.0
        } else {
            0.0
        };

        eprintln!(
            "AMM->CLMM: checker_profit={} verified_profit={} diff={} err={:.4}%",
            result.profit, verified_profit, diff, pct_err
        );

        assert!(
            pct_err < 0.01 || diff <= 100,
            "Profit divergence too large: checker={} verified={} diff={} err={:.4}%",
            result.profit, verified_profit, diff, pct_err
        );
    }

    #[test]
    fn test_clmm_clmm_no_arb_same_price() {
        // Two pools at the same price -- no arb after fees
        let sqrt_price = 1u128 << 64; // price = 1.0
        let pool_a = ClmmPool::new(
            sqrt_price,
            1_000_000_000_000,
            0,
            10,
            3000,
            &[
                ClmmTick { tick_index: -1000, liquidity_net: 1_000_000_000_000 },
                ClmmTick { tick_index: 1000, liquidity_net: -1_000_000_000_000 },
            ],
        );
        let pool_b = ClmmPool::new(
            sqrt_price,
            1_000_000_000_000,
            0,
            10,
            3000,
            &[
                ClmmTick { tick_index: -1000, liquidity_net: 1_000_000_000_000 },
                ClmmTick { tick_index: 1000, liquidity_net: -1_000_000_000_000 },
            ],
        );

        let result = check_clmm_clmm(&pool_a, true, &pool_b, false, u64::MAX);
        assert!(
            result.profit <= 0,
            "same price should have no arb: profit={}",
            result.profit
        );
    }
}
