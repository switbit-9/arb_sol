use crate::programs::orca::libraries::{tick_math, liquidity_math, swap_math};
use super::FD;

/// Initialized tick data for whirlpool simulation.
#[derive(Clone, Copy, Debug)]
pub struct WhirlpoolTick {
    pub tick_index: i32,
    pub liquidity_net: i128,
}

/// Max initialized ticks we store (3 tick arrays x 88 = 264 max, far fewer initialized).
const MAX_TICKS: usize = 128;

/// Lightweight Whirlpool pool for arbitrage checking.
///
/// Stores only essential state: current sqrt_price, liquidity, tick position, fee rate,
/// and a sorted list of initialized ticks with their liquidity deltas.
///
/// Within each tick range (between two initialized ticks), the Whirlpool is
/// mathematically equivalent to a constant-product AMM with virtual reserves:
///   v_a = L << 64 / sqrt_P,  v_b = L * sqrt_P >> 64
#[derive(Clone, Debug)]
pub struct WhirlpoolPool {
    pub sqrt_price: u128,       // Q64.64
    pub liquidity: u128,
    pub tick_current_index: i32,
    pub tick_spacing: u16,
    pub fee_rate: u32,          // hundredths of basis point, denominator 1_000_000
    ticks_storage: [WhirlpoolTick; MAX_TICKS],
    tick_count: usize,
}

impl WhirlpoolPool {
    pub fn new(
        sqrt_price: u128,
        liquidity: u128,
        tick_current_index: i32,
        tick_spacing: u16,
        fee_rate: u32,
        ticks: &[WhirlpoolTick],
    ) -> Self {
        let count = ticks.len().min(MAX_TICKS);
        let mut storage = [WhirlpoolTick { tick_index: 0, liquidity_net: 0 }; MAX_TICKS];
        storage[..count].copy_from_slice(&ticks[..count]);
        storage[..count].sort_unstable_by_key(|t| t.tick_index);
        Self {
            sqrt_price,
            liquidity,
            tick_current_index,
            tick_spacing,
            fee_rate,
            ticks_storage: storage,
            tick_count: count,
        }
    }

    pub fn ticks(&self) -> &[WhirlpoolTick] {
        &self.ticks_storage[..self.tick_count]
    }

    /// Fee factor (FD - fee_rate), denominator FD.
    #[inline]
    pub fn fee_factor(&self) -> u128 {
        FD.saturating_sub(self.fee_rate as u128)
    }

    /// Virtual reserves (v_a, v_b) at given sqrt_price and liquidity.
    /// v_a = L << 64 / sqrt_P (token A),  v_b = L * sqrt_P >> 64 (token B)
    #[inline]
    pub fn virtual_reserves(sqrt_price: u128, liquidity: u128) -> (u64, u64) {
        if liquidity == 0 || sqrt_price == 0 {
            return (0, 0);
        }
        let v_a = ((liquidity as u128) << 64) / sqrt_price;
        let v_b = liquidity
            .checked_mul(sqrt_price)
            .map(|v| v >> 64)
            .unwrap_or(u128::MAX);
        (
            v_a.min(u64::MAX as u128) as u64,
            v_b.min(u64::MAX as u128) as u64,
        )
    }

    /// Find the next initialized tick in the given direction.
    /// For a_to_b (descending): largest tick_index <= current_tick.
    /// For b_to_a (ascending): smallest tick_index > current_tick.
    /// Uses binary search (ticks are sorted by tick_index).
    pub fn find_next_tick(&self, current_tick: i32, a_to_b: bool) -> Option<&WhirlpoolTick> {
        let ticks = self.ticks();
        if ticks.is_empty() { return None; }
        // partition_point returns the first index where tick_index > current_tick
        let idx = ticks.partition_point(|t| t.tick_index <= current_tick);
        if a_to_b {
            // Want largest tick_index <= current_tick → element at idx - 1
            if idx > 0 { Some(&ticks[idx - 1]) } else { None }
        } else {
            // Want smallest tick_index > current_tick → element at idx
            if idx < ticks.len() { Some(&ticks[idx]) } else { None }
        }
    }

    /// Compute sqrt_price at a tick index, clamped to valid range.
    #[inline]
    pub fn sqrt_price_at_tick_clamped(tick_index: i32, a_to_b: bool) -> u128 {
        match tick_math::get_sqrt_price_at_tick(tick_index) {
            Ok(p) => {
                if a_to_b {
                    p.max(tick_math::MIN_SQRT_PRICE_X64 + 1)
                } else {
                    p.min(tick_math::MAX_SQRT_PRICE_X64 - 1)
                }
            }
            Err(_) => {
                if a_to_b {
                    tick_math::MIN_SQRT_PRICE_X64 + 1
                } else {
                    tick_math::MAX_SQRT_PRICE_X64 - 1
                }
            }
        }
    }

    /// Cross an initialized tick: update liquidity based on direction.
    #[inline]
    pub fn cross_tick(liquidity: u128, liquidity_net: i128, a_to_b: bool) -> u128 {
        if a_to_b {
            liquidity_math::add_delta(liquidity, -liquidity_net).unwrap_or(0)
        } else {
            liquidity_math::add_delta(liquidity, liquidity_net).unwrap_or(liquidity)
        }
    }

    /// Quote exact-in swap simulation across tick ranges.
    /// Uses the same math as the on-chain Whirlpool swap.
    pub fn quote_exact_in(&self, amount_in: u64, a_to_b: bool) -> u64 {
        if amount_in == 0 {
            return 0;
        }
        let mut remaining = amount_in;
        let mut total_out = 0u64;
        let mut sqrt_price = self.sqrt_price;
        let mut liquidity = self.liquidity;
        let mut tick = self.tick_current_index;

        for _ in 0..20 {
            if remaining == 0 || liquidity == 0 {
                break;
            }

            let next_tick_data = match self.find_next_tick(tick, a_to_b) {
                Some(t) => t,
                None => break,
            };
            let next_tick_index = next_tick_data.tick_index;
            let liq_net = next_tick_data.liquidity_net;

            let sqrt_target = Self::sqrt_price_at_tick_clamped(next_tick_index, a_to_b);

            let step = swap_math::compute_swap_step(
                sqrt_price,
                sqrt_target,
                liquidity,
                remaining,
                self.fee_rate,
                true,
                a_to_b,
            );

            remaining = remaining
                .saturating_sub(step.amount_in)
                .saturating_sub(step.fee_amount);
            total_out += step.amount_out;
            sqrt_price = step.sqrt_price_next_x64;

            if step.sqrt_price_next_x64 == sqrt_target {
                liquidity = Self::cross_tick(liquidity, liq_net, a_to_b);
                tick = if a_to_b {
                    next_tick_index - 1
                } else {
                    next_tick_index
                };
            } else {
                break;
            }
        }
        total_out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pool_price_1() -> WhirlpoolPool {
        // Pool at price=1.0 (sqrt_price = 1 << 64), tick=0
        let sqrt_price = 1u128 << 64;
        WhirlpoolPool::new(
            sqrt_price,
            1_000_000_000_000, // L = 1e12
            0,
            10,    // tick_spacing
            3000,  // fee_rate = 0.3%
            &[
                WhirlpoolTick { tick_index: -1000, liquidity_net: 1_000_000_000_000 },
                WhirlpoolTick { tick_index: 1000, liquidity_net: -1_000_000_000_000 },
            ],
        )
    }

    #[test]
    fn test_virtual_reserves() {
        let sqrt_price = 1u128 << 64; // price = 1.0
        let liquidity = 1_000_000_000u128;
        let (v_a, v_b) = WhirlpoolPool::virtual_reserves(sqrt_price, liquidity);
        assert_eq!(v_a, 1_000_000_000);
        assert_eq!(v_b, 1_000_000_000);
    }

    #[test]
    fn test_virtual_reserves_zero() {
        let (v_a, v_b) = WhirlpoolPool::virtual_reserves(0, 100);
        assert_eq!(v_a, 0);
        assert_eq!(v_b, 0);
        let (v_a, v_b) = WhirlpoolPool::virtual_reserves(100, 0);
        assert_eq!(v_a, 0);
        assert_eq!(v_b, 0);
    }

    #[test]
    fn test_quote_basic() {
        let pool = make_pool_price_1();
        let out = pool.quote_exact_in(1_000_000, true);
        assert!(out > 0, "should produce output");
        assert!(out < 1_000_000, "fees + slippage reduce output");
    }

    #[test]
    fn test_quote_zero() {
        let pool = make_pool_price_1();
        assert_eq!(pool.quote_exact_in(0, true), 0);
    }

    #[test]
    fn test_quote_symmetry() {
        let pool = make_pool_price_1();
        let out_a_to_b = pool.quote_exact_in(1_000_000, true);
        let out_b_to_a = pool.quote_exact_in(1_000_000, false);
        // At price=1.0 with symmetric liquidity, outputs should be similar
        let diff = (out_a_to_b as i64 - out_b_to_a as i64).abs();
        assert!(
            diff < 1000,
            "a_to_b={} b_to_a={} diff={}",
            out_a_to_b,
            out_b_to_a,
            diff
        );
    }

    #[test]
    fn test_find_next_tick() {
        let pool = make_pool_price_1();
        // a_to_b from tick 0: should find tick -1000
        let t = pool.find_next_tick(0, true).unwrap();
        assert_eq!(t.tick_index, -1000);
        // b_to_a from tick 0: should find tick 1000
        let t = pool.find_next_tick(0, false).unwrap();
        assert_eq!(t.tick_index, 1000);
    }

    #[test]
    fn test_cross_tick() {
        assert_eq!(WhirlpoolPool::cross_tick(1000, 500, false), 1500);
        assert_eq!(WhirlpoolPool::cross_tick(1000, 500, true), 500);
        assert_eq!(WhirlpoolPool::cross_tick(100, 200, true), 0); // underflow → 0
    }
}
