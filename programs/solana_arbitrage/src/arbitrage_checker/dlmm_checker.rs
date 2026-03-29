use super::dlmm_sim::DlmmPool;
use super::amm_sim::AmmPool;
use super::optimizer::optimal_linear_vs_cp;
use super::{ArbitrageResult, FEE_PRECISION, FEE_PRECISION_SHL32, SCALE_OFFSET, FD};
use crate::compat::AccountInfo;

const HALF_SHIFT: u8 = 32;


/// Compute DLMM output with full Q64.64 price precision.
/// Replaces the `(price >> 32)` approximation that loses ~0.05% on low-price tokens.
#[inline(always)]
fn dlmm_out_full(gross_in: u128, price: u128, fee_adj: u128, swap_for_y: bool) -> u128 {
    let after_fee = gross_in * fee_adj / FEE_PRECISION;
    if swap_for_y {
        super::dlmm_sim::mul_shr(price, after_fee, SCALE_OFFSET, false).unwrap_or(0)
    } else {
        super::dlmm_sim::shl_div(after_fee, price, SCALE_OFFSET, false).unwrap_or(0)
    }
}

// ── DLMM ↔ DLMM Arbitrage ──

/// Check arbitrage between two DLMM pools sharing a common token.
///
/// Path: input_token -[pool_a swap_for_y_a]-> mid_token -[pool_b swap_for_y_b]-> output_token
///
/// Both DLMM sides are linear within a bin (fixed price per bin).
/// Uses direct rate math instead of full swap simulations.
#[inline(never)]
pub fn check_dlmm_dlmm(
    pool_a: &DlmmPool,
    swap_for_y_a: bool,
    pool_b: &DlmmPool,
    swap_for_y_b: bool,
    max_amount_in: u64,
    accounts: &[AccountInfo],
) -> ArbitrageResult {
    debug_eprintln!(
        "[check_dlmm_dlmm] pool_a: active_id={} bins={} swap_for_y={} | pool_b: active_id={} bins={} swap_for_y={} | max_in={}",
        pool_a.active_id, pool_a.bin_range(swap_for_y_a).1 - pool_a.bin_range(swap_for_y_a).0, swap_for_y_a,
        pool_b.active_id, pool_b.bin_range(swap_for_y_b).1 - pool_b.bin_range(swap_for_y_b).0, swap_for_y_b,
        max_amount_in
    );
    let mut total_in: u64 = 0;
    let mut total_out: u64 = 0;

    let mut cursor_a = match pool_a.start_cursor(swap_for_y_a) { Some(c) => c, None => { debug_eprintln!("[check_dlmm_dlmm] no start_cursor for pool_a"); return ArbitrageResult::none() } };
    let mut cursor_b = match pool_b.start_cursor(swap_for_y_b) { Some(c) => c, None => { debug_eprintln!("[check_dlmm_dlmm] no start_cursor for pool_b"); return ArbitrageResult::none() } };
    let mut vol_a = pool_a.initial_vol_acc();
    let mut vol_b = pool_b.initial_vol_acc();
    let mut first_a = true;
    let mut first_b = true;

    // Constant-fee fast path: skip volatility tracking when variable_fee_control == 0
    let const_fee_a = pool_a.variable_fee_control == 0;
    let cached_fee_rate_a = if const_fee_a { FEE_PRECISION - pool_a.get_base_fee() } else { 0 };
    let const_fee_b = pool_b.variable_fee_control == 0;
    let cached_fee_rate_b = if const_fee_b { FEE_PRECISION - pool_b.get_base_fee() } else { 0 };

    // Remaining capacity in current bins (in input/mid token units, before fee)
    let mut cap_a_remaining: u128 = 0; // input tokens to drain pool_a's current bin
    let mut cap_b_remaining_mid: u128 = 0; // mid tokens pool_b's current bin can accept
    let mut rate_a_num: u128 = 0; // pool_a: mid_out = input * rate_a_num / rate_a_den
    let mut rate_a_den: u128 = 1;
    let mut rate_b_num: u128 = 0; // pool_b: output = mid * rate_b_num / rate_b_den
    let mut rate_b_den: u128 = 1;
    let mut price_a: u128 = 0;    // full Q64.64 price for pool_a's current bin
    let mut fee_adj_a: u128 = 0;  // FEE_PRECISION - fee_rate for pool_a
    let mut price_b: u128 = 0;    // full Q64.64 price for pool_b's current bin
    let mut fee_adj_b: u128 = 0;  // FEE_PRECISION - fee_rate for pool_b
    let mut need_load_a = true;
    let mut need_load_b = true;

    let max_bins = {
        let (sa, ea) = pool_a.bin_range(swap_for_y_a);
        let (sb, eb) = pool_b.bin_range(swap_for_y_b);
        (ea - sa) + (eb - sb)
    };
    for _ in 0..max_bins {
        // Load pool_a bin if needed
        if need_load_a {
            loop {
                let bin = match pool_a.read_bin(accounts, cursor_a, swap_for_y_a) { Some(b) => b, None => return finish(total_in, total_out) };
                let out_available = if swap_for_y_a { bin.amount_y } else { bin.amount_x };
                if out_available == 0 {
                    cursor_a = match pool_a.advance_cursor(cursor_a, swap_for_y_a) { Some(c) => c, None => return finish(total_in, total_out) };
                    continue;
                }
                let fee_adj = if const_fee_a {
                    cached_fee_rate_a
                } else {
                    if !first_a { vol_a = pool_a.update_volatility_accumulator(vol_a, bin.id); }
                    FEE_PRECISION - pool_a.get_total_fee(vol_a)
                };
                first_a = false;

                let price = bin.price; // pre-computed
                price_a = price;
                fee_adj_a = fee_adj;

                if swap_for_y_a {
                    rate_a_num = (price >> 32) * fee_adj;
                    rate_a_den = FEE_PRECISION_SHL32;
                } else {
                    rate_a_num = fee_adj << 32;
                    rate_a_den = FEE_PRECISION * (price >> 32);
                }

                if rate_a_num == 0 { return finish(total_in, total_out); }
                let input_net = (out_available as u128) * rate_a_den / rate_a_num;
                cap_a_remaining = input_net * FEE_PRECISION / fee_adj + 1;

                need_load_a = false;
                break;
            }
        }

        // Load pool_b bin if needed
        if need_load_b {
            loop {
                let bin = match pool_b.read_bin(accounts, cursor_b, swap_for_y_b) { Some(b) => b, None => return finish(total_in, total_out) };
                let out_available = if swap_for_y_b { bin.amount_y } else { bin.amount_x };
                if out_available == 0 {
                    cursor_b = match pool_b.advance_cursor(cursor_b, swap_for_y_b) { Some(c) => c, None => return finish(total_in, total_out) };
                    continue;
                }
                let fee_adj = if const_fee_b {
                    cached_fee_rate_b
                } else {
                    if !first_b { vol_b = pool_b.update_volatility_accumulator(vol_b, bin.id); }
                    FEE_PRECISION - pool_b.get_total_fee(vol_b)
                };
                first_b = false;

                let price = bin.price;
                price_b = price;
                fee_adj_b = fee_adj;

                if swap_for_y_b {
                    rate_b_num = (price >> 32) * fee_adj;
                    rate_b_den = FEE_PRECISION_SHL32;
                } else {
                    rate_b_num = fee_adj << 32;
                    rate_b_den = FEE_PRECISION * (price >> 32);
                }

                if rate_b_num == 0 { return finish(total_in, total_out); }
                let mid_net = (out_available as u128) * rate_b_den / rate_b_num;
                cap_b_remaining_mid = mid_net * FEE_PRECISION / fee_adj + 1;

                need_load_b = false;
                break;
            }
        }

        let composite_num = rate_a_num as u128 * rate_b_num as u128;
        let composite_den = rate_a_den as u128 * rate_b_den as u128;
        debug_eprintln!(
            "[check_dlmm_dlmm] rate_a={}/{} rate_b={}/{} composite={:.8} cap_a={} cap_b_mid={}",
            rate_a_num, rate_a_den, rate_b_num, rate_b_den,
            composite_num as f64 / composite_den as f64,
            cap_a_remaining, cap_b_remaining_mid
        );
        if composite_num <= composite_den {
            debug_eprintln!("[check_dlmm_dlmm] composite rate <= 1.0, no arb at current bins");
            break;
        }

        let cap_b_as_input = if rate_a_num > 0 {
            cap_b_remaining_mid * rate_a_den / rate_a_num
        } else { 0 };

        let remaining_budget = (max_amount_in as u128).saturating_sub(total_in as u128);
        let fillable = cap_a_remaining.min(cap_b_as_input).min(remaining_budget);
        if fillable == 0 { break; }

        let mid_produced = dlmm_out_full(fillable, price_a, fee_adj_a, swap_for_y_a);
        let output = dlmm_out_full(mid_produced, price_b, fee_adj_b, swap_for_y_b);

        total_in += fillable as u64;
        total_out += output as u64;

        cap_a_remaining -= fillable;
        cap_b_remaining_mid = cap_b_remaining_mid.saturating_sub(mid_produced);

        if remaining_budget <= fillable { break; }

        if cap_a_remaining == 0 {
            let next_a = match pool_a.advance_cursor(cursor_a, swap_for_y_a) { Some(c) => c, None => break };
            // Pre-check: next bin's price with current rate_b — if composite <= 1, no point loading
            if let Some(next_bin_a) = pool_a.read_bin(accounts, next_a, swap_for_y_a) {
                let np = next_bin_a.price >> 32;
                let (nra_num, nra_den) = if swap_for_y_a {
                    (np * fee_adj_a, FEE_PRECISION_SHL32)
                } else {
                    (fee_adj_a << 32, FEE_PRECISION * np)
                };
                if nra_num * rate_b_num <= nra_den * rate_b_den { break; }
            }
            cursor_a = next_a;
            need_load_a = true;
        }
        if cap_b_remaining_mid == 0 {
            let next_b = match pool_b.advance_cursor(cursor_b, swap_for_y_b) { Some(c) => c, None => break };
            // Pre-check: next bin's price with current rate_a — if composite <= 1, no point loading
            if let Some(next_bin_b) = pool_b.read_bin(accounts, next_b, swap_for_y_b) {
                let np = next_bin_b.price >> 32;
                let (nrb_num, nrb_den) = if swap_for_y_b {
                    (np * fee_adj_b, FEE_PRECISION_SHL32)
                } else {
                    (fee_adj_b << 32, FEE_PRECISION * np)
                };
                if rate_a_num * nrb_num <= rate_a_den * nrb_den { break; }
            }
            cursor_b = next_b;
            need_load_b = true;
        }
    }

    debug_eprintln!(
        "[check_dlmm_dlmm] RESULT: total_in={} total_out={} profit={}",
        total_in, total_out, total_out as i64 - total_in as i64
    );
    finish(total_in, total_out)
}

#[inline(always)]
fn finish(total_in: u64, total_out: u64) -> ArbitrageResult {
    let profit = total_out as i64 - total_in as i64;
    if profit > 0 {
        ArbitrageResult::from_pair(total_in, profit)
    } else {
        ArbitrageResult::none()
    }
}

// ── DLMM → AMM Arbitrage ──

/// Check arbitrage: buy on DLMM, sell on AMM CP pool.
///
/// Path: quote_in -[DLMM]-> mid -[AMM CP sell/buy]-> quote_out
///
/// For each DLMM bin, the output is linear. Against the AMM CP curve,
/// optimal per-bin is analytical. We iterate bins greedily.
#[inline(never)]
pub fn check_dlmm_to_amm(
    dlmm: &DlmmPool,
    swap_for_y: bool,
    amm: &AmmPool,
    amm_sells_base: bool,
    max_amount_in: u64,
    accounts: &[AccountInfo],
) -> ArbitrageResult {
    debug_eprintln!(
        "[check_dlmm_to_amm] dlmm: active_id={} bins={} swap_for_y={} | amm: base={} quote={} sells_base={} | max_in={}",
        dlmm.active_id, dlmm.bin_range(swap_for_y).1 - dlmm.bin_range(swap_for_y).0, swap_for_y,
        amm.base_vault, amm.quote_vault, amm_sells_base,
        max_amount_in
    );
    let mut total_in: u64 = 0;
    let mut total_out: u64 = 0;

    // Pick fee factors based on AMM direction
    let (cp_fi, cp_fo) = if amm_sells_base {
        // Selling base on AMM: use sell fees
        (FD - amm.sell_input_fee as u128, FD - amm.sell_output_fee as u128)
    } else {
        // Buying base on AMM: use buy fees
        (FD - amm.buy_input_fee as u128, FD - amm.buy_output_fee as u128)
    };

    let (mut virt_in, mut virt_out) = if amm_sells_base {
        (amm.base_vault as u128, amm.quote_vault as u128)
    } else {
        (amm.quote_vault as u128, amm.base_vault as u128)
    };

    debug_eprintln!(
        "[check_dlmm_to_amm] bins_sfy_true={} bins_sfy_false={}",
        dlmm.bin_range(true).1 - dlmm.bin_range(true).0, dlmm.bin_range(false).1 - dlmm.bin_range(false).0
    );
    let mut cursor = match dlmm.start_cursor(swap_for_y) { Some(c) => c, None => { debug_eprintln!("[check_dlmm_to_amm] no start_cursor for dlmm (bins empty for sfy={}))", swap_for_y); return ArbitrageResult::none() } };
    let mut vol_acc = dlmm.initial_vol_acc();
    let mut first_bin = true;

    // Constant-fee fast path: skip volatility tracking when variable_fee_control == 0
    let const_fee = dlmm.variable_fee_control == 0;
    let cached_fee_adj = if const_fee { FEE_PRECISION - dlmm.get_base_fee() } else { 0 };

    let (bin_start, bin_end) = dlmm.bin_range(swap_for_y);
    for _ in 0..(bin_end - bin_start) {
        let bin = match dlmm.read_bin(accounts, cursor, swap_for_y) { Some(b) => b, None => break };

        let bin_out = if swap_for_y { bin.amount_y } else { bin.amount_x };
        if bin_out == 0 {
            debug_eprintln!(
                "[check_dlmm_to_amm] bin[{}] id={} skipped: bin_out=0 (amount_x={} amount_y={} sfy={})",
                cursor, bin.id, bin.amount_x, bin.amount_y, swap_for_y
            );
            cursor = match dlmm.advance_cursor(cursor, swap_for_y) { Some(c) => c, None => break };
            continue;
        }

        let fee_adj = if const_fee {
            cached_fee_adj
        } else {
            if !first_bin {
                vol_acc = dlmm.update_volatility_accumulator(vol_acc, bin.id);
            }
            FEE_PRECISION - dlmm.get_total_fee(vol_acc)
        };
        first_bin = false;

        let price = bin.price;

        let max_in_bin = if swap_for_y {
            let in_raw = ((bin_out as u128) << SCALE_OFFSET) / price + 1;
            let in_with_fee = in_raw.saturating_mul(FEE_PRECISION) / fee_adj + 1;
            in_with_fee.min(u64::MAX as u128) as u64
        } else {
            let in_raw = ((bin_out as u128).saturating_mul(price)) >> SCALE_OFFSET;
            let in_with_fee = (in_raw + 1).saturating_mul(FEE_PRECISION) / fee_adj + 1;
            in_with_fee.min(u64::MAX as u128) as u64
        };

        let (rate_num, rate_den) = if swap_for_y {
            let num = (price >> HALF_SHIFT).saturating_mul(fee_adj);
            let den = FEE_PRECISION_SHL32;
            (num, den)
        } else {
            let num = fee_adj << HALF_SHIFT;
            let den = FEE_PRECISION.saturating_mul(price >> HALF_SHIFT);
            (num, den)
        };

        let remaining = max_amount_in.saturating_sub(total_in);
        if remaining == 0 { break; }
        let capped_max_in = max_in_bin.min(remaining);

        debug_eprintln!(
            "[check_dlmm_to_amm] bin[{}] rate={}/{} capped_max_in={} virt_in={} virt_out={} cp_fi={} cp_fo={}",
            cursor, rate_num, rate_den, capped_max_in, virt_in, virt_out, cp_fi, cp_fo
        );

        let (opt_in, opt_profit) = optimal_linear_vs_cp(
            rate_num,
            rate_den,
            capped_max_in,
            virt_in,
            virt_out,
            cp_fi,
            cp_fo,
        );

        debug_eprintln!(
            "[check_dlmm_to_amm] bin[{}] opt_in={} opt_profit={}",
            cursor, opt_in, opt_profit
        );

        if opt_profit <= 0 {
            if capped_max_in > 1000 {
                break;
            }
            cursor = match dlmm.advance_cursor(cursor, swap_for_y) { Some(c) => c, None => break };
            continue;
        }

        let mid_from_bin = dlmm_out_full(opt_in as u128, price, fee_adj, swap_for_y);
        let mid_u64 = mid_from_bin.min(bin_out as u128) as u64;

        total_in += opt_in;

        // Unified CP swap with both fees
        let in_eff = (mid_u64 as u128) * cp_fi / FD;
        let raw_out = virt_out * in_eff / (virt_in + in_eff);
        let out = (raw_out * cp_fo / FD) as u64;
        total_out += out;

        // Update virtual reserves (effective amounts)
        virt_in += in_eff;
        virt_out = virt_out.saturating_sub(raw_out);

        // Pre-check: peek next DLMM bin price vs current AMM marginal price (post-fill reserves)
        // Composite profitable if: next_dlmm_rate * amm_marginal > 1
        // amm_marginal ≈ virt_out * cp_fi * cp_fo / (virt_in * FD^2)
        // So: next_rate_num * virt_out * cp_fi * cp_fo > next_rate_den * virt_in * FD * FD
        let next_c = match dlmm.advance_cursor(cursor, swap_for_y) { Some(c) => c, None => break };
        if let Some(next_bin) = dlmm.read_bin(accounts, next_c, swap_for_y) {
            let np = next_bin.price >> 32;
            let (nr_num, nr_den) = if swap_for_y {
                (np * fee_adj, FEE_PRECISION_SHL32)
            } else {
                (fee_adj << 32, FEE_PRECISION * np)
            };
            // nr_num/nr_den * virt_out*cp_fi*cp_fo / (virt_in*FD*FD) > 1
            // => nr_num * virt_out * cp_fi * cp_fo > nr_den * virt_in * FD * FD
            let lhs = (nr_num as u128)
                .checked_mul(virt_out)
                .and_then(|v| v.checked_mul(cp_fi))
                .and_then(|v| v.checked_mul(cp_fo));
            let rhs = (nr_den as u128)
                .checked_mul(virt_in)
                .and_then(|v| v.checked_mul(FD))
                .and_then(|v| v.checked_mul(FD));
            if let (Some(l), Some(r)) = (lhs, rhs) {
                if l <= r { break; }
            }
        }
        cursor = next_c;
    }

    debug_eprintln!(
        "[check_dlmm_to_amm] RESULT: total_in={} total_out={} profit={}",
        total_in, total_out, total_out as i64 - total_in as i64
    );
    finish(total_in, total_out)
}

// ── AMM → DLMM Arbitrage ──

/// Check arbitrage: buy on AMM CP pool, sell on DLMM.
///
/// Path: quote_in -[AMM buy base]-> base -[DLMM sell base]-> quote_out
///
/// The AMM side is a CP curve, the DLMM side is piecewise linear across bins.
#[inline(never)]
pub fn check_amm_to_dlmm(
    amm: &AmmPool,
    amm_buys_base: bool,
    dlmm: &DlmmPool,
    swap_for_y: bool,
    max_amount_in: u64,
    accounts: &[AccountInfo],
) -> ArbitrageResult {
    analytical_amm_to_dlmm(amm, amm_buys_base, dlmm, swap_for_y, max_amount_in, accounts)
}

/// Analytical per-bin approach for AMM→DLMM arb.
///
/// For the AMM buy side:
///   in_eff = amount_in * fi / FD
///   mid_raw = virt_base * in_eff / (virt_quote + in_eff)
///   mid = mid_raw * fo / FD
///
/// Composite out(amount_in) is concave. Derivative approach:
///   d(out)/d(amount_in) = dlmm_rate * fo * virt_base * virt_quote * fi / (FD^2 * (virt_quote + in_eff)^2)
///
/// Setting = 1:
///   (virt_quote + in_eff)^2 = dlmm_rate_num * fo * fi * virt_base * virt_quote / (dlmm_rate_den * FD^2)
///   in_eff = sqrt(K) - virt_quote
///   amount_in = in_eff * FD / fi
fn analytical_amm_to_dlmm(
    amm: &AmmPool,
    amm_buys_base: bool,
    dlmm: &DlmmPool,
    swap_for_y: bool,
    max_amount_in: u64,
    accounts: &[AccountInfo],
) -> ArbitrageResult {
    debug_eprintln!(
        "[amm_to_dlmm] amm: base={} quote={} buys_base={} | dlmm: active_id={} bins={} swap_for_y={} | max_in={}",
        amm.base_vault, amm.quote_vault, amm_buys_base,
        dlmm.active_id, dlmm.bin_range(swap_for_y).1 - dlmm.bin_range(swap_for_y).0, swap_for_y,
        max_amount_in
    );
    let mut total_in: u64 = 0;
    let mut total_out: u64 = 0;

    // AMM buy fees
    let (fi, fo) = if amm_buys_base {
        (FD - amm.buy_input_fee as u128, FD - amm.buy_output_fee as u128)
    } else {
        (FD - amm.sell_input_fee as u128, FD - amm.sell_output_fee as u128)
    };

    // virt_base = reserve of what the AMM outputs (mid token), virt_quote = reserve receiving input.
    // When amm_buys_base: input=quote, output=base → virt_base=base, virt_quote=quote.
    // When !amm_buys_base: input=base, output=quote → virt_base=quote, virt_quote=base.
    let (mut virt_base, mut virt_quote) = if amm_buys_base {
        (amm.base_vault as u128, amm.quote_vault as u128)
    } else {
        (amm.quote_vault as u128, amm.base_vault as u128)
    };

    let mut cursor = match dlmm.start_cursor(swap_for_y) { Some(c) => c, None => { debug_eprintln!("[amm_to_dlmm] no start_cursor"); return ArbitrageResult::none() } };
    let mut vol_acc = dlmm.initial_vol_acc();
    let mut first_bin = true;

    // Constant-fee fast path: skip volatility tracking when variable_fee_control == 0
    let const_fee = dlmm.variable_fee_control == 0;
    let cached_fee_adj = if const_fee { FEE_PRECISION - dlmm.get_base_fee() } else { 0 };

    let (bin_start, bin_end) = dlmm.bin_range(swap_for_y);
    for _ in 0..(bin_end - bin_start) {
        let bin = match dlmm.read_bin(accounts, cursor, swap_for_y) { Some(b) => b, None => break };

        let bin_out_available = if swap_for_y { bin.amount_y } else { bin.amount_x };
        if bin_out_available == 0 {
            cursor = match dlmm.advance_cursor(cursor, swap_for_y) { Some(c) => c, None => break };
            continue;
        }

        let fee_adj = if const_fee {
            cached_fee_adj
        } else {
            if !first_bin {
                vol_acc = dlmm.update_volatility_accumulator(vol_acc, bin.id);
            }
            FEE_PRECISION - dlmm.get_total_fee(vol_acc)
        };
        first_bin = false;

        let price = bin.price;

        let (dlmm_rate_num, dlmm_rate_den): (u128, u128) = if swap_for_y {
            let num = (price >> HALF_SHIFT).saturating_mul(fee_adj);
            let den = FEE_PRECISION << HALF_SHIFT;
            (num, den)
        } else {
            let num = fee_adj << HALF_SHIFT;
            let den = FEE_PRECISION.saturating_mul(price >> HALF_SHIFT);
            (num, den)
        };

        // K = dlmm_rate_num * fo * fi * virt_base * virt_quote / (dlmm_rate_den * FD^2)
        let den = FD * FD * dlmm_rate_den;
        if den == 0 { break; }

        // Marginal rate check: dlmm_rate * fo * virt_base * fi > virt_quote * FD^2 * dlmm_rate_den
        {
            let lhs = dlmm_rate_num
                .checked_mul(fo)
                .and_then(|v| v.checked_mul(virt_base))
                .and_then(|v| v.checked_mul(fi));
            let rhs = dlmm_rate_den
                .checked_mul(virt_quote)
                .and_then(|v| v.checked_mul(FD))
                .and_then(|v| v.checked_mul(FD));
            if let (Some(l), Some(r)) = (lhs, rhs) {
                if l <= r {
                    debug_eprintln!("[amm_to_dlmm] bin[{}] marginal rate <= 1, early break", cursor);
                    break;
                }
            }
        }

        // sqrt(K) — 5 terms / den
        let sqrt_k: u128 = {
            let k_u128 = dlmm_rate_num
                .checked_mul(fo)
                .and_then(|v| v.checked_mul(fi))
                .and_then(|v| v.checked_mul(virt_base))
                .and_then(|v| v.checked_mul(virt_quote));
            if let Some(num) = k_u128 {
                super::optimizer::isqrt_u128(num / den)
            } else {
                super::wide_math::sqrt_mul5_div(dlmm_rate_num, fo, fi, virt_base, virt_quote, den)
            }
        };

        if sqrt_k <= virt_quote {
            debug_eprintln!(
                "[amm_to_dlmm] bin[{}] sqrt_k <= virt_quote, no arb — break",
                cursor
            );
            break;
        }

        let in_eff_opt = sqrt_k - virt_quote;
        let mut amount_in: u64 = (in_eff_opt * FD / fi).min(u64::MAX as u128) as u64;
        debug_eprintln!(
            "[amm_to_dlmm] bin[{}] id={} dlmm_rate={}/{} virt_base={} virt_quote={} amount_in={} bin_out={}",
            cursor, bin.id, dlmm_rate_num, dlmm_rate_den, virt_base, virt_quote, amount_in, bin_out_available
        );

        let remaining = max_amount_in.saturating_sub(total_in);
        if remaining == 0 { break; }
        amount_in = amount_in.min(remaining);

        let in_eff_u128 = (amount_in as u128) * fi / FD;
        let mid_raw = virt_base * in_eff_u128 / (virt_quote + in_eff_u128);
        let mid = mid_raw * fo / FD;

        // Cap mid by bin capacity
        let max_mid_for_bin = if swap_for_y {
            if price == 0 || fee_adj == 0 { break; }
            let num_wide = {
                let ab = super::wide_math::wmul(bin_out_available as u128, FEE_PRECISION);
                super::wide_math::U256 {
                    hi: (ab.hi << SCALE_OFFSET) | (ab.lo >> (128 - SCALE_OFFSET)),
                    lo: ab.lo << SCALE_OFFSET,
                }
            };
            let den_wide = super::wide_math::wmul(price, fee_adj);
            let den_u128 = if den_wide.hi == 0 { den_wide.lo } else { u128::MAX };
            super::wide_math::wdiv(num_wide, den_u128) + 1
        } else {
            if fee_adj == 0 { break; }
            let num_val = super::wide_math::mul4_div(
                bin_out_available as u128, FEE_PRECISION, price, 1,
                fee_adj << SCALE_OFFSET,
            );
            num_val + 1
        };

        if mid > max_mid_for_bin {
            if max_mid_for_bin >= virt_base {
                break;
            }
            // Reverse: find amount_in that produces max_mid_for_bin of mid
            // mid = mid_raw * fo / FD → mid_raw = mid * FD / fo
            let mid_raw_capped = if fo == FD { max_mid_for_bin } else { max_mid_for_bin * FD / fo };
            let in_eff_capped = mid_raw_capped * virt_quote / (virt_base - mid_raw_capped) + 1;
            amount_in = (in_eff_capped * FD / fi + 1).min(u64::MAX as u128) as u64;
        }

        let in_eff_actual = (amount_in as u128) * fi / FD;
        let mid_raw_actual = virt_base * in_eff_actual / (virt_quote + in_eff_actual);
        let mid_actual = mid_raw_actual * fo / FD;

        let dlmm_out = {
            let out = dlmm_out_full(mid_actual, price, fee_adj, swap_for_y);
            (out.min(bin_out_available as u128)) as u64
        };

        if dlmm_out as i128 <= amount_in as i128 {
            if bin_out_available > 1000 {
                break;
            }
            cursor = match dlmm.advance_cursor(cursor, swap_for_y) { Some(c) => c, None => break };
            continue;
        }

        total_in += amount_in;
        total_out += dlmm_out;

        // Update virtual reserves (effective amounts entering/leaving the CP)
        virt_quote += in_eff_actual;
        virt_base = virt_base.saturating_sub(mid_raw_actual);

        // Pre-check: peek next DLMM bin price vs current AMM marginal (post-fill reserves)
        // AMM marginal = fi * fo * virt_base / (virt_quote * FD^2)
        // Profitable if: amm_marginal * next_dlmm_rate > 1
        // => fi * fo * virt_base * next_dlmm_num > virt_quote * FD^2 * next_dlmm_den
        let next_c = match dlmm.advance_cursor(cursor, swap_for_y) { Some(c) => c, None => break };
        if let Some(next_bin) = dlmm.read_bin(accounts, next_c, swap_for_y) {
            let np = next_bin.price >> 32;
            let (nd_num, nd_den): (u128, u128) = if swap_for_y {
                (np * fee_adj, FEE_PRECISION << 32)
            } else {
                (fee_adj << 32, FEE_PRECISION * np)
            };
            let lhs = nd_num
                .checked_mul(fo)
                .and_then(|v| v.checked_mul(virt_base))
                .and_then(|v| v.checked_mul(fi));
            let rhs = nd_den
                .checked_mul(virt_quote)
                .and_then(|v| v.checked_mul(FD))
                .and_then(|v| v.checked_mul(FD));
            if let (Some(l), Some(r)) = (lhs, rhs) {
                if l <= r { break; }
            }
        }
        cursor = next_c;
    }

    let profit = total_out as i64 - total_in as i64;
    debug_eprintln!(
        "[amm_to_dlmm] RESULT: total_in={} total_out={} profit={}",
        total_in, total_out, profit
    );
    if profit > 0 {
        ArbitrageResult::from_pair(total_in, profit)
    } else {
        ArbitrageResult::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::dlmm_sim::DlmmBin;
    use crate::compat::Pubkey;

    fn make_test_accounts(bins: &[DlmmBin], bin_array_index: i64) -> (Vec<u8>, Vec<u8>) {
        let d1 = super::super::dlmm_sim::make_test_bin_array_data(bins, bin_array_index);
        let d2 = super::super::dlmm_sim::make_test_bin_array_data(bins, bin_array_index);
        (d1, d2)
    }

    fn make_dlmm_pool(bins: &[DlmmBin]) -> DlmmPool {
        let active_id = bins.first().map(|b| b.id).unwrap_or(0);
        let bin_array_index = (active_id as i64) / 70;
        super::super::dlmm_sim::make_test_dlmm_pool(active_id, 10, 10, 0, 0, 0, 0, 0, 0, bins, bin_array_index)
    }

    #[test]
    fn test_dlmm_amm_arb() {
        let one = 1u128 << 64;
        let bins = [DlmmBin {
            id: 0,
            amount_x: 10_000_000_000,
            amount_y: 0,
            price: one,
        }];
        let dlmm = make_dlmm_pool(&bins);
        let bin_array_index = (bins[0].id as i64) / 70;
        let (mut d1, mut d2) = make_test_accounts(&bins, bin_array_index);
        let key1 = Pubkey::default();
        let key2 = Pubkey::default();
        let owner = Pubkey::default();
        let mut l1 = 0u64;
        let mut l2 = 0u64;
        let acc1 = AccountInfo::new(&key1, false, false, &mut l1, &mut d1, &owner, false, 0);
        let acc2 = AccountInfo::new(&key2, false, false, &mut l2, &mut d2, &owner, false, 0);
        let accounts = [acc1, acc2];

        let amm = AmmPool::from_pump(
            5_000_000_000,
            50_000_000_000,
        );

        let result = check_dlmm_to_amm(&dlmm, false, &amm, true, u64::MAX, &accounts);
        assert!(
            result.profit > 0,
            "should find arb: profit={}, amount_in={}",
            result.profit,
            result.amount_in
        );
    }

    #[test]
    fn test_amm_to_dlmm_arb() {
        let one = 1u128 << 64;
        let amm = AmmPool::from_pump(1_000_000_000_000, 10_000_000);

        let bins = [DlmmBin {
            id: 0,
            amount_x: 0,
            amount_y: 10_000_000_000,
            price: one,
        }];
        let bin_array_index = (bins[0].id as i64) / 70;
        let dlmm = super::super::dlmm_sim::make_test_dlmm_pool(
            0, 100, 10, 0, 0, 0, 0, 0, 0, &bins, bin_array_index,
        );
        let (mut d1, mut d2) = make_test_accounts(&bins, bin_array_index);
        let key1 = Pubkey::default();
        let key2 = Pubkey::default();
        let owner = Pubkey::default();
        let mut l1 = 0u64;
        let mut l2 = 0u64;
        let acc1 = AccountInfo::new(&key1, false, false, &mut l1, &mut d1, &owner, false, 0);
        let acc2 = AccountInfo::new(&key2, false, false, &mut l2, &mut d2, &owner, false, 0);
        let accounts = [acc1, acc2];

        let result = check_amm_to_dlmm(&amm, true, &dlmm, true, u64::MAX, &accounts);
        assert!(
            result.profit > 0,
            "should find arb: profit={}, amount_in={}",
            result.profit,
            result.amount_in
        );
    }

    #[test]
    fn test_dlmm_dlmm_no_arb() {
        let one = 1u128 << 64;
        let bins_a = [DlmmBin {
            id: 0,
            amount_x: 0,
            amount_y: 1_000_000_000,
            price: one,
        }];
        let pool_a = make_dlmm_pool(&bins_a);
        let bins_b = [DlmmBin {
            id: 0,
            amount_x: 1_000_000_000,
            amount_y: 0,
            price: one,
        }];
        let pool_b = make_dlmm_pool(&bins_b);

        let bin_array_index = 0i64;
        let (mut da1, mut da2) = make_test_accounts(&bins_a, bin_array_index);
        let (mut db1, mut db2) = make_test_accounts(&bins_b, bin_array_index);
        let key1 = Pubkey::default();
        let key2 = Pubkey::default();
        let key3 = Pubkey::default();
        let key4 = Pubkey::default();
        let owner = Pubkey::default();
        let mut l1 = 0u64;
        let mut l2 = 0u64;
        let mut l3 = 0u64;
        let mut l4 = 0u64;
        let acc1 = AccountInfo::new(&key1, false, false, &mut l1, &mut da1, &owner, false, 0);
        let acc2 = AccountInfo::new(&key2, false, false, &mut l2, &mut da2, &owner, false, 0);
        let acc3 = AccountInfo::new(&key3, false, false, &mut l3, &mut db1, &owner, false, 0);
        let acc4 = AccountInfo::new(&key4, false, false, &mut l4, &mut db2, &owner, false, 0);
        let accounts = [acc1, acc2, acc3, acc4];

        let result = check_dlmm_dlmm(&pool_a, true, &pool_b, false, u64::MAX, &accounts);
        assert!(
            result.profit <= 0,
            "same price should have no arb: profit={}",
            result.profit
        );
    }
}
