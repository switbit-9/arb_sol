use super::amm_sim::AmmPool;
use super::optimizer::optimal_amm_amm;
use super::ArbitrageResult;

// ── AMM ↔ AMM Arbitrage ──

/// Check arbitrage: buy base on pool_a, sell base on pool_b.
///
/// Pools must be pre-oriented so that base = mid_token and quote = start_token.
/// Use `AmmPool::flipped()` at the call site when the pool's native orientation
/// doesn't match the arb direction.
#[inline(never)]
pub fn check_amm_amm(pool_a: &AmmPool, pool_b: &AmmPool, max_amount_in: u64) -> ArbitrageResult {
    debug_eprintln!(
        "[check_amm_amm] pool_a: base={} quote={} buy_fi={} buy_fo={} sell_fi={} sell_fo={} | pool_b: base={} quote={} buy_fi={} buy_fo={} sell_fi={} sell_fo={} | max_in={}",
        pool_a.base_vault, pool_a.quote_vault,
        pool_a.buy_input_fee, pool_a.buy_output_fee, pool_a.sell_input_fee, pool_a.sell_output_fee,
        pool_b.base_vault, pool_b.quote_vault,
        pool_b.buy_input_fee, pool_b.buy_output_fee, pool_b.sell_input_fee, pool_b.sell_output_fee,
        max_amount_in
    );

    let (mut amt, mut profit) = optimal_amm_amm(
        pool_a.quote_vault,
        pool_a.base_vault,
        pool_a.buy_input_fee,
        pool_a.buy_output_fee,
        pool_b.base_vault,
        pool_b.quote_vault,
        pool_b.sell_input_fee,
        pool_b.sell_output_fee,
    );
    debug_eprintln!("[check_amm_amm] buy A, sell B: amt={} profit={}", amt, profit);
    if amt > max_amount_in {
        amt = max_amount_in;
        profit = amm_profit_at(pool_a, pool_b, amt);
        debug_eprintln!("[check_amm_amm] clamped: amt={} profit={}", amt, profit);
    }

    if profit > 0 {
        ArbitrageResult::from_pair(amt, profit as i64)
    } else {
        ArbitrageResult::none()
    }
}

/// Compute profit for buy on pool_a, sell on pool_b at a given amount_in.
fn amm_profit_at(pool_a: &AmmPool, pool_b: &AmmPool, amount_in: u64) -> i128 {
    let mid = pool_a.buy_base(amount_in);
    if mid == 0 { return 0; }
    let out = pool_b.sell_base(mid);
    out as i128 - amount_in as i128
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_amm_amm_arb() {
        // Pool A: base is cheap (high base, low quote)
        let pool_a = AmmPool::from_pump(1_000_000_000_000, 10_000_000_000);
        // Pool B: base is expensive (low base, high quote)
        let pool_b = AmmPool::from_pump(100_000_000_000, 50_000_000_000);

        let result = check_amm_amm(&pool_a, &pool_b, u64::MAX);
        assert!(result.profit > 0, "should find arb: profit={}", result.profit);
        assert!(result.amount_in > 0);
    }

    #[test]
    fn test_amm_amm_no_arb() {
        // Same reserves => no arb (fees eat any difference)
        let pool_a = AmmPool::from_pump(1_000_000_000, 500_000_000);
        let pool_b = AmmPool::from_pump(1_000_000_000, 500_000_000);

        let result = check_amm_amm(&pool_a, &pool_b, u64::MAX);
        assert_eq!(result.profit, 0);
    }
}
