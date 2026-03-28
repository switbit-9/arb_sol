use super::whirlpool_sim::WhirlpoolPool;
use super::amm_sim::AmmPool;
use super::dlmm_sim::DlmmPool;
use super::optimizer::{optimal_amm_amm, ternary_search_maximize};
use super::{ArbitrageResult, FD};
use crate::programs::orca::libraries::liquidity_math;

#[inline]
fn finish(total_in: u64, total_out: u64) -> ArbitrageResult {
    let profit = total_out as i64 - total_in as i64;
    if profit > 0 {
        ArbitrageResult::from_pair(total_in, profit)
    } else {
        ArbitrageResult::none()
    }
}

// ── Whirlpool → AMM Arbitrage ──

/// Check arbitrage: buy on Whirlpool, sell on AMM CP pool.
///
/// Path: input -[WP swap]-> mid -[AMM sell/buy]-> output
///
/// Iterates WP tick ranges greedily. Within each tick range the Whirlpool
/// acts as a constant-product pool with virtual reserves. For each range,
/// uses `optimal_amm_amm` (WP virtual reserves as pool A, AMM as pool B).
pub fn check_whirlpool_to_amm(
    wp: &WhirlpoolPool,
    a_to_b: bool,
    amm: &AmmPool,
    amm_sells_base: bool,
    max_amount_in: u64,
) -> ArbitrageResult {
    debug_eprintln!(
        "[check_wp_to_amm] wp: sqrt_p={} liq={} tick={} fee={} a_to_b={} | amm: base={} quote={} sells_base={} | max_in={}",
        wp.sqrt_price, wp.liquidity, wp.tick_current_index, wp.fee_rate, a_to_b,
        amm.base_vault, amm.quote_vault, amm_sells_base, max_amount_in
    );

    let mut total_in: u64 = 0;
    let mut total_out: u64 = 0;

    let wp_fee = wp.fee_rate as u64;
    let wp_ff = wp.fee_factor();

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

    // WP evolving state
    let mut wp_sqrt_price = wp.sqrt_price;
    let mut wp_liquidity = wp.liquidity;
    let mut wp_tick = wp.tick_current_index;

    for _ in 0..20 {
        if wp_liquidity == 0 { break; }

        let next = match wp.find_next_tick(wp_tick, a_to_b) {
            Some(t) => *t,
            None => break,
        };

        let sqrt_target = WhirlpoolPool::sqrt_price_at_tick_clamped(next.tick_index, a_to_b);

        // Virtual reserves for this tick range
        let (v_a, v_b) = WhirlpoolPool::virtual_reserves(wp_sqrt_price, wp_liquidity);
        let (wp_res_in, wp_res_out) = if a_to_b { (v_a, v_b) } else { (v_b, v_a) };

        // Net capacity in this tick range (after fee deduction)
        let cap_net = liquidity_math::get_amount_in_for_liquidity(
            wp_sqrt_price, sqrt_target, wp_liquidity, a_to_b,
        )
        .unwrap_or(0);

        if cap_net == 0 || wp_res_in == 0 || wp_res_out == 0 {
            // Empty range — cross tick and continue
            wp_liquidity = WhirlpoolPool::cross_tick(wp_liquidity, next.liquidity_net, a_to_b);
            wp_sqrt_price = sqrt_target;
            wp_tick = if a_to_b { next.tick_index - 1 } else { next.tick_index };
            continue;
        }

        let cap_gross = ((cap_net as u128) * FD / wp_ff + 1).min(u64::MAX as u128) as u64;
        let remaining = max_amount_in.saturating_sub(total_in);
        if remaining == 0 { break; }

        // Optimal for (WP as first CP pool, AMM as second CP pool)
        // WP is "Pool A" (buy side): quote_a = input_reserve, base_a = output_reserve
        // AMM is "Pool B" (sell side)
        let (opt_amt, opt_profit) = optimal_amm_amm(
            wp_res_in,
            wp_res_out,
            wp_fee,
            0, // WP has no output fee
            amm_res_in.min(u64::MAX as u128) as u64,
            amm_res_out.min(u64::MAX as u128) as u64,
            amm_fi_raw,
            amm_fo_raw,
        );

        debug_eprintln!(
            "[check_wp_to_amm] range: tick={} v_in={} v_out={} cap_net={} cap_gross={} opt_amt={} opt_profit={}",
            next.tick_index, wp_res_in, wp_res_out, cap_net, cap_gross, opt_amt, opt_profit
        );

        if opt_profit <= 0 {
            if cap_net > 1000 { break; }
            wp_liquidity = WhirlpoolPool::cross_tick(wp_liquidity, next.liquidity_net, a_to_b);
            wp_sqrt_price = sqrt_target;
            wp_tick = if a_to_b { next.tick_index - 1 } else { next.tick_index };
            continue;
        }

        let amount_in = opt_amt.min(cap_gross).min(remaining);

        // Simulate WP swap (CP formula with virtual reserves)
        let in_eff = (amount_in as u128) * wp_ff / FD;
        let mid = (wp_res_out as u128) * in_eff / ((wp_res_in as u128) + in_eff);

        // Simulate AMM swap
        let mid_eff = mid * cp_fi / FD;
        let raw_out = amm_res_out * mid_eff / (amm_res_in + mid_eff);
        let out = (raw_out * cp_fo / FD) as u64;

        total_in += amount_in;
        total_out += out;

        // Update AMM virtual reserves
        amm_res_in += mid_eff;
        amm_res_out = amm_res_out.saturating_sub(raw_out);

        debug_eprintln!(
            "[check_wp_to_amm] filled: amt={} mid={} out={} total_in={} total_out={}",
            amount_in, mid, out, total_in, total_out
        );

        if amount_in >= cap_gross {
            // Consumed full WP range — cross tick and continue
            wp_liquidity = WhirlpoolPool::cross_tick(wp_liquidity, next.liquidity_net, a_to_b);
            wp_sqrt_price = sqrt_target;
            wp_tick = if a_to_b { next.tick_index - 1 } else { next.tick_index };
        } else {
            // Optimal was within range (or budget limited), done
            break;
        }
    }

    debug_eprintln!(
        "[check_wp_to_amm] RESULT: total_in={} total_out={} profit={}",
        total_in, total_out, total_out as i128 - total_in as i128
    );
    finish(total_in, total_out)
}

// ── AMM → Whirlpool Arbitrage ──

/// Check arbitrage: buy on AMM CP pool, sell on Whirlpool.
///
/// Path: input -[AMM buy/sell]-> mid -[WP swap]-> output
///
/// Iterates WP tick ranges. For each range, uses `optimal_amm_amm`
/// (AMM as pool A, WP virtual reserves as pool B). Caps mid tokens
/// by WP tick range capacity.
pub fn check_amm_to_whirlpool(
    amm: &AmmPool,
    amm_buys_base: bool,
    wp: &WhirlpoolPool,
    a_to_b: bool,
    max_amount_in: u64,
) -> ArbitrageResult {
    debug_eprintln!(
        "[check_amm_to_wp] amm: base={} quote={} buys_base={} | wp: sqrt_p={} liq={} tick={} fee={} a_to_b={} | max_in={}",
        amm.base_vault, amm.quote_vault, amm_buys_base,
        wp.sqrt_price, wp.liquidity, wp.tick_current_index, wp.fee_rate, a_to_b, max_amount_in
    );

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

    // AMM reserves (evolving) — oriented for buy/sell direction
    // "in" = what user puts in, "out" = what AMM gives (mid tokens)
    let (mut amm_res_in, mut amm_res_out) = if amm_buys_base {
        (amm.quote_vault as u128, amm.base_vault as u128)
    } else {
        (amm.base_vault as u128, amm.quote_vault as u128)
    };

    let wp_fee = wp.fee_rate as u64;
    let wp_ff = wp.fee_factor();

    // WP evolving state
    let mut wp_sqrt_price = wp.sqrt_price;
    let mut wp_liquidity = wp.liquidity;
    let mut wp_tick = wp.tick_current_index;

    for _ in 0..20 {
        if wp_liquidity == 0 { break; }

        let next = match wp.find_next_tick(wp_tick, a_to_b) {
            Some(t) => *t,
            None => break,
        };

        let sqrt_target = WhirlpoolPool::sqrt_price_at_tick_clamped(next.tick_index, a_to_b);

        let (v_a, v_b) = WhirlpoolPool::virtual_reserves(wp_sqrt_price, wp_liquidity);
        // WP receives mid tokens as input, produces output tokens
        let (wp_res_mid, wp_res_out) = if a_to_b { (v_a, v_b) } else { (v_b, v_a) };

        // WP capacity in mid tokens (net, after WP fee deduction)
        let cap_net_mid = liquidity_math::get_amount_in_for_liquidity(
            wp_sqrt_price, sqrt_target, wp_liquidity, a_to_b,
        )
        .unwrap_or(0);

        if cap_net_mid == 0 || wp_res_mid == 0 || wp_res_out == 0 {
            wp_liquidity = WhirlpoolPool::cross_tick(wp_liquidity, next.liquidity_net, a_to_b);
            wp_sqrt_price = sqrt_target;
            wp_tick = if a_to_b { next.tick_index - 1 } else { next.tick_index };
            continue;
        }

        let remaining = max_amount_in.saturating_sub(total_in);
        if remaining == 0 { break; }

        // Optimal: AMM is "Pool A" (buy side), WP is "Pool B" (sell side)
        let (opt_amt, opt_profit) = optimal_amm_amm(
            amm_res_in.min(u64::MAX as u128) as u64,
            amm_res_out.min(u64::MAX as u128) as u64,
            amm_fi_raw,
            amm_fo_raw,
            wp_res_mid,  // WP input = mid tokens
            wp_res_out,  // WP output
            wp_fee,      // WP fee on input (mid)
            0,           // WP has no output fee
        );

        debug_eprintln!(
            "[check_amm_to_wp] range: tick={} wp_mid={} wp_out={} cap_net_mid={} opt_amt={} opt_profit={}",
            next.tick_index, wp_res_mid, wp_res_out, cap_net_mid, opt_amt, opt_profit
        );

        if opt_profit <= 0 {
            if cap_net_mid > 1000 { break; }
            wp_liquidity = WhirlpoolPool::cross_tick(wp_liquidity, next.liquidity_net, a_to_b);
            wp_sqrt_price = sqrt_target;
            wp_tick = if a_to_b { next.tick_index - 1 } else { next.tick_index };
            continue;
        }

        let mut amount_in = opt_amt.min(remaining);

        // Simulate AMM swap to get mid
        let in_eff = (amount_in as u128) * fi / FD;
        let mid_raw = amm_res_out * in_eff / (amm_res_in + in_eff);
        let mut mid = mid_raw * fo / FD;

        // Cap mid by WP tick range capacity (gross = net * FD / fee_factor)
        let cap_gross_mid = ((cap_net_mid as u128) * FD / wp_ff + 1).min(u64::MAX as u128);
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

        // Simulate WP swap with mid tokens
        let mid_eff = mid * wp_ff / FD;
        let wp_raw_out = (wp_res_out as u128) * mid_eff / ((wp_res_mid as u128) + mid_eff);
        let out = wp_raw_out as u64;

        if out as i128 <= amount_in as i128 {
            if cap_net_mid > 1000 { break; }
            wp_liquidity = WhirlpoolPool::cross_tick(wp_liquidity, next.liquidity_net, a_to_b);
            wp_sqrt_price = sqrt_target;
            wp_tick = if a_to_b { next.tick_index - 1 } else { next.tick_index };
            continue;
        }

        total_in += amount_in;
        total_out += out;

        // Update AMM reserves
        let in_eff_actual = (amount_in as u128) * fi / FD;
        let mid_raw_actual = amm_res_out * in_eff_actual / (amm_res_in + in_eff_actual);
        amm_res_in += in_eff_actual;
        amm_res_out = amm_res_out.saturating_sub(mid_raw_actual);

        debug_eprintln!(
            "[check_amm_to_wp] filled: amt={} mid={} out={} total_in={} total_out={}",
            amount_in, mid, out, total_in, total_out
        );

        // Advance: cross tick if WP range exhausted, else done
        if capped {
            wp_liquidity = WhirlpoolPool::cross_tick(wp_liquidity, next.liquidity_net, a_to_b);
            wp_sqrt_price = sqrt_target;
            wp_tick = if a_to_b { next.tick_index - 1 } else { next.tick_index };
        } else {
            break;
        }
    }

    debug_eprintln!(
        "[check_amm_to_wp] RESULT: total_in={} total_out={} profit={}",
        total_in, total_out, total_out as i128 - total_in as i128
    );
    finish(total_in, total_out)
}

// ── Whirlpool ↔ Whirlpool Arbitrage ──

/// Check arbitrage between two Whirlpool pools sharing a common token.
///
/// Path: input -[pool_a swap]-> mid -[pool_b swap]-> output
///
/// Uses ternary search over the concave profit function.
/// Each evaluation simulates full tick-traversal swaps on both pools.
pub fn check_whirlpool_whirlpool(
    pool_a: &WhirlpoolPool,
    a_to_b_a: bool,
    pool_b: &WhirlpoolPool,
    a_to_b_b: bool,
    max_amount_in: u64,
) -> ArbitrageResult {
    debug_eprintln!(
        "[check_wp_wp] pool_a: sqrt_p={} liq={} tick={} fee={} a_to_b={} | pool_b: sqrt_p={} liq={} tick={} fee={} a_to_b={} | max_in={}",
        pool_a.sqrt_price, pool_a.liquidity, pool_a.tick_current_index, pool_a.fee_rate, a_to_b_a,
        pool_b.sqrt_price, pool_b.liquidity, pool_b.tick_current_index, pool_b.fee_rate, a_to_b_b,
        max_amount_in
    );

    // Quick marginal check: is there any arb at a small test amount?
    let test_amt = 10_000u64.min(max_amount_in);
    if test_amt == 0 {
        return ArbitrageResult::none();
    }
    let mid = pool_a.quote_exact_in(test_amt, a_to_b_a);
    let out = pool_b.quote_exact_in(mid, a_to_b_b);
    if out <= test_amt {
        debug_eprintln!("[check_wp_wp] no marginal arb at test_amt={}: out={}", test_amt, out);
        return ArbitrageResult::none();
    }

    let profit_fn = |amount_in: u64| -> i128 {
        if amount_in == 0 {
            return 0;
        }
        let mid = pool_a.quote_exact_in(amount_in, a_to_b_a);
        let out = pool_b.quote_exact_in(mid, a_to_b_b);
        out as i128 - amount_in as i128
    };

    let (amt, profit) = ternary_search_maximize(profit_fn, 1, max_amount_in, 44);

    debug_eprintln!(
        "[check_wp_wp] RESULT: amount_in={} profit={}",
        amt, profit
    );

    if profit > 0 {
        ArbitrageResult::from_pair(amt, profit as i64)
    } else {
        ArbitrageResult::none()
    }
}

// ── Whirlpool ↔ DLMM Arbitrage ──

/// Check arbitrage: buy on Whirlpool, sell on DLMM.
/// Uses ternary search over the concave profit function.
pub fn check_whirlpool_to_dlmm(
    wp: &WhirlpoolPool,
    a_to_b: bool,
    dlmm: &DlmmPool,
    swap_for_y: bool,
    max_amount_in: u64,
) -> ArbitrageResult {
    let test_amt = 10_000u64.min(max_amount_in);
    if test_amt == 0 { return ArbitrageResult::none(); }
    let mid = wp.quote_exact_in(test_amt, a_to_b);
    let (out, _fee) = dlmm.quote_exact_in(mid, swap_for_y).unwrap_or((0, 0));
    if out <= test_amt { return ArbitrageResult::none(); }

    let profit_fn = |amount_in: u64| -> i128 {
        if amount_in == 0 { return 0; }
        let mid = wp.quote_exact_in(amount_in, a_to_b);
        let (out, _) = dlmm.quote_exact_in(mid, swap_for_y).unwrap_or((0, 0));
        out as i128 - amount_in as i128
    };

    let (amt, profit) = ternary_search_maximize(profit_fn, 1, max_amount_in, 44);
    if profit > 0 {
        ArbitrageResult::from_pair(amt, profit as i64)
    } else {
        ArbitrageResult::none()
    }
}

/// Check arbitrage: buy on DLMM, sell on Whirlpool.
/// Uses ternary search over the concave profit function.
pub fn check_dlmm_to_whirlpool(
    dlmm: &DlmmPool,
    swap_for_y: bool,
    wp: &WhirlpoolPool,
    a_to_b: bool,
    max_amount_in: u64,
) -> ArbitrageResult {
    let test_amt = 10_000u64.min(max_amount_in);
    if test_amt == 0 { return ArbitrageResult::none(); }
    let (mid, _fee) = dlmm.quote_exact_in(test_amt, swap_for_y).unwrap_or((0, 0));
    let out = wp.quote_exact_in(mid, a_to_b);
    if out <= test_amt { return ArbitrageResult::none(); }

    let profit_fn = |amount_in: u64| -> i128 {
        if amount_in == 0 { return 0; }
        let (mid, _) = dlmm.quote_exact_in(amount_in, swap_for_y).unwrap_or((0, 0));
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::whirlpool_sim::WhirlpoolTick;

    fn make_wp_cheap_base() -> WhirlpoolPool {
        // WP at price=0.01 (base is cheap relative to quote)
        // sqrt(0.01) = 0.1, in Q64.64: 0.1 * 2^64 ≈ 1_844_674_407_370_955_161
        let sqrt_price = 1_844_674_407_370_955_161u128;
        WhirlpoolPool::new(
            sqrt_price,
            100_000_000_000_000, // high liquidity
            -23028,              // tick for price ~0.01
            10,
            3000, // 0.3%
            &[
                WhirlpoolTick { tick_index: -30000, liquidity_net: 100_000_000_000_000 },
                WhirlpoolTick { tick_index: -10000, liquidity_net: -100_000_000_000_000 },
            ],
        )
    }

    fn make_wp_expensive_base() -> WhirlpoolPool {
        // WP at price=0.05 (base is expensive relative to quote)
        // sqrt(0.05) ≈ 0.2236, in Q64.64: 0.2236 * 2^64 ≈ 4_125_206_105_282_697_830
        let sqrt_price = 4_125_206_105_282_697_830u128;
        WhirlpoolPool::new(
            sqrt_price,
            50_000_000_000_000,
            -14979, // tick for price ~0.05
            10,
            3000,
            &[
                WhirlpoolTick { tick_index: -20000, liquidity_net: 50_000_000_000_000 },
                WhirlpoolTick { tick_index: -5000, liquidity_net: -50_000_000_000_000 },
            ],
        )
    }

    #[test]
    fn test_wp_to_amm_arb() {
        // WP has cheap base, AMM has expensive base → buy on WP, sell on AMM
        let wp = make_wp_cheap_base();
        let amm = AmmPool::from_pump(500_000_000_000, 50_000_000_000);

        let result = check_whirlpool_to_amm(&wp, true, &amm, true, u64::MAX);
        // There should be arb if WP price differs enough from AMM price
        eprintln!("wp_to_amm: profit={} amount_in={}", result.profit, result.amount_in);
    }

    #[test]
    fn test_amm_to_wp_arb() {
        // AMM has cheap base, WP has expensive base → buy on AMM, sell on WP
        let amm = AmmPool::from_pump(1_000_000_000_000, 10_000_000);
        let wp = make_wp_expensive_base();

        let result = check_amm_to_whirlpool(&amm, true, &wp, true, u64::MAX);
        eprintln!("amm_to_wp: profit={} amount_in={}", result.profit, result.amount_in);
    }

    #[test]
    fn test_wp_wp_arb() {
        let pool_cheap = make_wp_cheap_base();
        let pool_expensive = make_wp_expensive_base();

        // Buy base on cheap pool (a_to_b=false means buy base/sell quote on WP)
        // Then sell base on expensive pool
        let result = check_whirlpool_whirlpool(
            &pool_cheap,
            true,  // a_to_b on cheap pool
            &pool_expensive,
            false, // b_to_a on expensive pool
            u64::MAX,
        );
        eprintln!("wp_wp: profit={} amount_in={}", result.profit, result.amount_in);
    }

    /// Precision test: verify the inline CP formula matches quote_exact_in
    /// (which uses exact U256 on-chain math) within acceptable tolerance.
    #[test]
    fn test_cp_vs_exact_precision() {
        // Test various amounts at different price points
        let test_cases: Vec<(u128, u128, i32, u64)> = vec![
            // (sqrt_price, liquidity, tick, amount_in)
            (1u128 << 64, 1_000_000_000_000, 0, 1_000_000),          // price=1, small
            (1u128 << 64, 1_000_000_000_000, 0, 1_000_000_000),      // price=1, medium
            (1u128 << 64, 1_000_000_000_000, 0, 100_000_000_000),    // price=1, large
            (1_844_674_407_370_955_161, 100_000_000_000_000, -23028, 500_000_000), // price~0.01
            (4_125_206_105_282_697_830, 50_000_000_000_000, -14979, 1_000_000_000), // price~0.05
            (18_446_744_073_709_551_616 * 10, 500_000_000_000, 23028, 2_000_000_000), // price~100
        ];

        for (sqrt_price, liquidity, tick, amount_in) in &test_cases {
            let pool = WhirlpoolPool::new(
                *sqrt_price, *liquidity, *tick, 10, 3000,
                &[
                    WhirlpoolTick { tick_index: tick - 5000, liquidity_net: *liquidity as i128 },
                    WhirlpoolTick { tick_index: tick + 5000, liquidity_net: -(*liquidity as i128) },
                ],
            );

            // Method 1: quote_exact_in (uses compute_swap_step with U256 math)
            let exact_out = pool.quote_exact_in(*amount_in, true);

            // Method 2: inline CP formula (what the checker uses)
            let wp_ff = pool.fee_factor();
            let (v_a, v_b) = WhirlpoolPool::virtual_reserves(*sqrt_price, *liquidity);
            let in_eff = (*amount_in as u128) * wp_ff / FD;
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

            // Must be within 0.001% for any real trade
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
    fn test_wp_to_amm_profit_precision() {
        let wp = WhirlpoolPool::new(
            4_125_206_105_282_697_830, // price ~0.05
            50_000_000_000_000,
            -14979,
            10,
            3000,
            &[
                WhirlpoolTick { tick_index: -20000, liquidity_net: 50_000_000_000_000 },
                WhirlpoolTick { tick_index: -5000, liquidity_net: -50_000_000_000_000 },
            ],
        );
        let amm = AmmPool::from_pump(500_000_000_000, 50_000_000_000);

        let result = check_whirlpool_to_amm(&wp, true, &amm, true, u64::MAX);
        if result.profit <= 0 { return; }

        // Independent verification: simulate with quote_exact_in + sell_base
        let mid = wp.quote_exact_in(result.amount_in, true);
        let out = amm.sell_base(mid);
        let verified_profit = out as i64 - result.amount_in as i64;

        let diff = (result.profit as i64 - verified_profit).unsigned_abs();
        let pct_err = if result.profit > 0 {
            (diff as f64 / result.profit as f64) * 100.0
        } else {
            0.0
        };

        eprintln!(
            "WP→AMM: checker_profit={} verified_profit={} diff={} err={:.4}%",
            result.profit, verified_profit, diff, pct_err
        );

        assert!(
            pct_err < 0.01 || diff <= 100,
            "Profit divergence too large: checker={} verified={} diff={} err={:.4}%",
            result.profit, verified_profit, diff, pct_err
        );
    }

    /// End-to-end precision test for AMM→WP
    #[test]
    fn test_amm_to_wp_profit_precision() {
        let amm = AmmPool::from_pump(1_000_000_000_000, 10_000_000);
        let wp = WhirlpoolPool::new(
            4_125_206_105_282_697_830,
            50_000_000_000_000,
            -14979,
            10,
            3000,
            &[
                WhirlpoolTick { tick_index: -20000, liquidity_net: 50_000_000_000_000 },
                WhirlpoolTick { tick_index: -5000, liquidity_net: -50_000_000_000_000 },
            ],
        );

        let result = check_amm_to_whirlpool(&amm, true, &wp, true, u64::MAX);
        if result.profit <= 0 { return; }

        // Independent verification
        let mid = amm.buy_base(result.amount_in);
        let out = wp.quote_exact_in(mid, true);
        let verified_profit = out as i64 - result.amount_in as i64;

        let diff = (result.profit as i64 - verified_profit).unsigned_abs();
        let pct_err = if result.profit > 0 {
            (diff as f64 / result.profit as f64) * 100.0
        } else {
            0.0
        };

        eprintln!(
            "AMM→WP: checker_profit={} verified_profit={} diff={} err={:.4}%",
            result.profit, verified_profit, diff, pct_err
        );

        assert!(
            pct_err < 0.01 || diff <= 100,
            "Profit divergence too large: checker={} verified={} diff={} err={:.4}%",
            result.profit, verified_profit, diff, pct_err
        );
    }

    #[test]
    fn test_wp_wp_no_arb_same_price() {
        // Two pools at the same price — no arb after fees
        let sqrt_price = 1u128 << 64; // price = 1.0
        let pool_a = WhirlpoolPool::new(
            sqrt_price,
            1_000_000_000_000,
            0,
            10,
            3000,
            &[
                WhirlpoolTick { tick_index: -1000, liquidity_net: 1_000_000_000_000 },
                WhirlpoolTick { tick_index: 1000, liquidity_net: -1_000_000_000_000 },
            ],
        );
        let pool_b = WhirlpoolPool::new(
            sqrt_price,
            1_000_000_000_000,
            0,
            10,
            3000,
            &[
                WhirlpoolTick { tick_index: -1000, liquidity_net: 1_000_000_000_000 },
                WhirlpoolTick { tick_index: 1000, liquidity_net: -1_000_000_000_000 },
            ],
        );

        let result = check_whirlpool_whirlpool(&pool_a, true, &pool_b, false, u64::MAX);
        assert!(
            result.profit <= 0,
            "same price should have no arb: profit={}",
            result.profit
        );
    }
}
