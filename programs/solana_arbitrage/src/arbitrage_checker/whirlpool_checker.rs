use crate::compat::AccountInfo;
use super::whirlpool_sim::WhirlpoolPool;
use super::amm_sim::AmmPool;
use super::dlmm_sim::DlmmPool;
use super::optimizer::optimal_amm_amm;
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
#[inline(never)]
pub fn check_whirlpool_to_amm(
    wp: &WhirlpoolPool,
    a_to_b: bool,
    amm: &AmmPool,
    amm_sells_base: bool,
    max_amount_in: u64,
    accounts: &[AccountInfo],
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

        let next = match wp.find_next_tick(wp_tick, a_to_b, accounts) {
            Some(t) => t,
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
#[inline(never)]
pub fn check_amm_to_whirlpool(
    amm: &AmmPool,
    amm_buys_base: bool,
    wp: &WhirlpoolPool,
    a_to_b: bool,
    max_amount_in: u64,
    accounts: &[AccountInfo],
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

        let next = match wp.find_next_tick(wp_tick, a_to_b, accounts) {
            Some(t) => t,
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
#[inline(never)]
pub fn check_whirlpool_whirlpool(
    _pool_a: &WhirlpoolPool,
    _a_to_b_a: bool,
    _pool_b: &WhirlpoolPool,
    _a_to_b_b: bool,
    _max_amount_in: u64,
    _accounts: &[AccountInfo],
) -> ArbitrageResult {
    // Disabled: ternary search too expensive for now
    ArbitrageResult::none()
}

// ── Whirlpool ↔ DLMM Arbitrage ──

/// Check arbitrage: buy on Whirlpool, sell on DLMM.
#[inline(never)]
pub fn check_whirlpool_to_dlmm(
    _wp: &WhirlpoolPool,
    _a_to_b: bool,
    _dlmm: &DlmmPool,
    _swap_for_y: bool,
    _max_amount_in: u64,
    _accounts: &[AccountInfo],
) -> ArbitrageResult {
    // Disabled: ternary search too expensive for now
    ArbitrageResult::none()
}

/// Check arbitrage: buy on DLMM, sell on Whirlpool.
#[inline(never)]
pub fn check_dlmm_to_whirlpool(
    _dlmm: &DlmmPool,
    _swap_for_y: bool,
    _wp: &WhirlpoolPool,
    _a_to_b: bool,
    _max_amount_in: u64,
    _accounts: &[AccountInfo],
) -> ArbitrageResult {
    // Disabled: ternary search too expensive for now
    ArbitrageResult::none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::whirlpool_sim::WhirlpoolTick;

    fn make_wp_cheap_base() -> WhirlpoolPool {
        let sqrt_price = 1_844_674_407_370_955_161u128;
        WhirlpoolPool::new(
            sqrt_price,
            100_000_000_000_000,
            -23028,
            10,
            3000,
            &[
                WhirlpoolTick { tick_index: -30000, liquidity_net: 100_000_000_000_000 },
                WhirlpoolTick { tick_index: -10000, liquidity_net: -100_000_000_000_000 },
            ],
        )
    }

    fn make_wp_expensive_base() -> WhirlpoolPool {
        let sqrt_price = 4_125_206_105_282_697_830u128;
        WhirlpoolPool::new(
            sqrt_price,
            50_000_000_000_000,
            -14979,
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
        let wp = make_wp_cheap_base();
        let amm = AmmPool::from_pump(500_000_000_000, 50_000_000_000);

        let result = check_whirlpool_to_amm(&wp, true, &amm, true, u64::MAX, &[]);
        eprintln!("wp_to_amm: profit={} amount_in={}", result.profit, result.amount_in);
    }

    #[test]
    fn test_amm_to_wp_arb() {
        let amm = AmmPool::from_pump(1_000_000_000_000, 10_000_000);
        let wp = make_wp_expensive_base();

        let result = check_amm_to_whirlpool(&amm, true, &wp, true, u64::MAX, &[]);
        eprintln!("amm_to_wp: profit={} amount_in={}", result.profit, result.amount_in);
    }

    #[test]
    fn test_wp_wp_arb() {
        let pool_cheap = make_wp_cheap_base();
        let pool_expensive = make_wp_expensive_base();

        let result = check_whirlpool_whirlpool(
            &pool_cheap,
            true,
            &pool_expensive,
            false,
            u64::MAX,
            &[],
        );
        eprintln!("wp_wp: profit={} amount_in={}", result.profit, result.amount_in);
    }

    /// Precision test: verify the inline CP formula matches quote_exact_in.
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
            let pool = WhirlpoolPool::new(
                *sqrt_price, *liquidity, *tick, 10, 3000,
                &[
                    WhirlpoolTick { tick_index: tick - 5000, liquidity_net: *liquidity as i128 },
                    WhirlpoolTick { tick_index: tick + 5000, liquidity_net: -(*liquidity as i128) },
                ],
            );

            let exact_out = pool.quote_exact_in(*amount_in, true, &[]);

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

            assert!(
                pct_err < 0.001 || diff <= 5,
                "CP divergence too large: exact={} cp={} diff={} err={:.6}%",
                exact_out, cp_out, diff, pct_err
            );
        }
    }

    /// End-to-end precision test: checker profit vs independent simulation.
    #[test]
    fn test_wp_to_amm_profit_precision() {
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
        let amm = AmmPool::from_pump(500_000_000_000, 50_000_000_000);

        let result = check_whirlpool_to_amm(&wp, true, &amm, true, u64::MAX, &[]);
        if result.profit <= 0 { return; }

        let mid = wp.quote_exact_in(result.amount_in, true, &[]);
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

        let result = check_amm_to_whirlpool(&amm, true, &wp, true, u64::MAX, &[]);
        if result.profit <= 0 { return; }

        let mid = amm.buy_base(result.amount_in);
        let out = wp.quote_exact_in(mid, true, &[]);
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
        let sqrt_price = 1u128 << 64;
        let pool_a = WhirlpoolPool::new(
            sqrt_price, 1_000_000_000_000, 0, 10, 3000,
            &[
                WhirlpoolTick { tick_index: -1000, liquidity_net: 1_000_000_000_000 },
                WhirlpoolTick { tick_index: 1000, liquidity_net: -1_000_000_000_000 },
            ],
        );
        let pool_b = WhirlpoolPool::new(
            sqrt_price, 1_000_000_000_000, 0, 10, 3000,
            &[
                WhirlpoolTick { tick_index: -1000, liquidity_net: 1_000_000_000_000 },
                WhirlpoolTick { tick_index: 1000, liquidity_net: -1_000_000_000_000 },
            ],
        );

        let result = check_whirlpool_whirlpool(&pool_a, true, &pool_b, false, u64::MAX, &[]);
        assert!(
            result.profit <= 0,
            "same price should have no arb: profit={}",
            result.profit
        );
    }
}
