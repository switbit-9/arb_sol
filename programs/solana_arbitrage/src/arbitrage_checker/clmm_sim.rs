use anchor_lang::prelude::AccountInfo;
use crate::programs::raydium_clmm::libraries::{tick_math, liquidity_math, swap_math};
use super::FD;

/// Initialized tick data for CLMM simulation.
#[derive(Clone, Copy, Debug)]
pub struct ClmmTick {
    pub tick_index: i32,
    pub liquidity_net: i128,
}

/// Max initialized ticks we store (2 tick arrays x 60 = 120 max, far fewer initialized).
const MAX_TICKS: usize = 128;
const MAX_CLMM_SOURCES: usize = 4;

// Byte layout: CLMM TickArrayState
const CLMM_TICKS_OFF: usize = 44; // 8 disc + 32 pool_id + 4 start_tick_index
const CLMM_TICK_SIZE: usize = 168;
const CLMM_TICK_COUNT: usize = 60;
const CLMM_LIQ_NET_OFF: usize = 4;   // i128, after tick (i32)
const CLMM_LIQ_GROSS_OFF: usize = 20; // u128, after liquidity_net (i128)

#[derive(Clone, Copy, Debug)]
struct ClmmTickSource {
    acc_idx: usize,
    start_tick: i32,
}

/// Lightweight Raydium CLMM pool for arbitrage checking.
///
/// Stores only essential state: current sqrt_price, liquidity, tick position, fee rate,
/// and either pre-loaded ticks (eager, via `new`) or tick array account references
/// (lazy, via `new_lazy` + `add_tick_source`).
///
/// Within each tick range (between two initialized ticks), the CLMM is
/// mathematically equivalent to a constant-product AMM with virtual reserves:
///   v_a = L << 64 / sqrt_P,  v_b = L * sqrt_P >> 64
#[derive(Clone, Debug)]
pub struct ClmmPool {
    pub sqrt_price: u128,       // Q64.64
    pub liquidity: u128,
    pub tick_current_index: i32,
    pub tick_spacing: u16,
    pub fee_rate: u32,          // denominator 1_000_000
    // Eager path (populated by new())
    ticks_storage: [ClmmTick; MAX_TICKS],
    tick_count: usize,
    // Lazy path (populated by new_lazy + add_tick_source)
    sources: [ClmmTickSource; MAX_CLMM_SOURCES],
    source_count: usize,
}

impl ClmmPool {
    pub fn new(
        sqrt_price: u128,
        liquidity: u128,
        tick_current_index: i32,
        tick_spacing: u16,
        fee_rate: u32,
        ticks: &[ClmmTick],
    ) -> Self {
        let count = ticks.len().min(MAX_TICKS);
        let mut storage = [ClmmTick { tick_index: 0, liquidity_net: 0 }; MAX_TICKS];
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
            sources: [ClmmTickSource { acc_idx: 0, start_tick: 0 }; MAX_CLMM_SOURCES],
            source_count: 0,
        }
    }

    /// Create a pool with no ticks. Register tick array sources with
    /// `add_tick_source`, then ticks are read lazily from account data
    /// during `find_next_tick`.
    pub fn new_lazy(
        sqrt_price: u128,
        liquidity: u128,
        tick_current_index: i32,
        tick_spacing: u16,
        fee_rate: u32,
    ) -> Self {
        Self {
            sqrt_price,
            liquidity,
            tick_current_index,
            tick_spacing,
            fee_rate,
            ticks_storage: [ClmmTick { tick_index: 0, liquidity_net: 0 }; MAX_TICKS],
            tick_count: 0,
            sources: [ClmmTickSource { acc_idx: 0, start_tick: 0 }; MAX_CLMM_SOURCES],
            source_count: 0,
        }
    }

    /// Register a tick array source for lazy loading.
    pub fn add_tick_source(&mut self, acc_idx: usize, start_tick: i32) {
        if self.source_count < MAX_CLMM_SOURCES {
            self.sources[self.source_count] = ClmmTickSource { acc_idx, start_tick };
            self.source_count += 1;
        }
    }

    pub fn ticks(&self) -> &[ClmmTick] {
        &self.ticks_storage[..self.tick_count]
    }

    /// Fee factor (FD - fee_rate), denominator FD.
    #[inline]
    pub fn fee_factor(&self) -> u128 {
        FD.saturating_sub(self.fee_rate as u128)
    }

    /// Virtual reserves (v_a, v_b) at given sqrt_price and liquidity.
    /// v_a = L << 64 / sqrt_P (token 0),  v_b = L * sqrt_P >> 64 (token 1)
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
    /// For zero_for_one (descending): largest tick_index <= current_tick.
    /// For one_for_zero (ascending): smallest tick_index > current_tick.
    ///
    /// Eager path (tick_count > 0): searches pre-loaded ticks.
    /// Lazy path: reads tick data on demand from account data.
    pub fn find_next_tick(&self, current_tick: i32, zero_for_one: bool, accounts: &[AccountInfo]) -> Option<ClmmTick> {
        if self.tick_count > 0 {
            return self.find_next_tick_eager(current_tick, zero_for_one);
        }
        self.find_next_tick_lazy(current_tick, zero_for_one, accounts)
    }

    fn find_next_tick_eager(&self, current_tick: i32, zero_for_one: bool) -> Option<ClmmTick> {
        let ticks = self.ticks();
        if zero_for_one {
            ticks.iter().rev().find(|t| t.tick_index <= current_tick).copied()
        } else {
            ticks.iter().find(|t| t.tick_index > current_tick).copied()
        }
    }

    fn find_next_tick_lazy(&self, current_tick: i32, zero_for_one: bool, accounts: &[AccountInfo]) -> Option<ClmmTick> {
        let mut best: Option<ClmmTick> = None;

        for si in 0..self.source_count {
            let s = &self.sources[si];
            if s.acc_idx >= accounts.len() { continue; }
            let data = match accounts[s.acc_idx].try_borrow_data() {
                Ok(d) => d,
                Err(_) => continue,
            };

            Self::scan_clmm_ticks(&data, current_tick, zero_for_one, &mut best);
        }

        best
    }

    /// Scan a CLMM tick array for the next initialized tick.
    /// CLMM TickState stores tick_index explicitly per slot, so we read it directly.
    fn scan_clmm_ticks(data: &[u8], current_tick: i32, zero_for_one: bool, best: &mut Option<ClmmTick>) {
        if data.len() < CLMM_TICKS_OFF { return; }

        for i in 0..CLMM_TICK_COUNT {
            let base = CLMM_TICKS_OFF + i * CLMM_TICK_SIZE;
            if base + CLMM_TICK_SIZE > data.len() { break; }

            // Check liquidity_gross != 0 (initialized tick)
            let lg_off = base + CLMM_LIQ_GROSS_OFF;
            let lg_bytes = match <[u8; 16]>::try_from(&data[lg_off..lg_off + 16]) {
                Ok(b) => b,
                Err(_) => continue,
            };
            if u128::from_le_bytes(lg_bytes) == 0 { continue; }

            let tick_index = match <[u8; 4]>::try_from(&data[base..base + 4]) {
                Ok(b) => i32::from_le_bytes(b),
                Err(_) => continue,
            };

            let valid = if zero_for_one { tick_index <= current_tick } else { tick_index > current_tick };
            if !valid { continue; }

            let better = match best {
                Some(b) => if zero_for_one { tick_index > b.tick_index } else { tick_index < b.tick_index },
                None => true,
            };
            if !better { continue; }

            if let Ok(bytes) = <[u8; 16]>::try_from(&data[base + CLMM_LIQ_NET_OFF..base + CLMM_LIQ_NET_OFF + 16]) {
                *best = Some(ClmmTick {
                    tick_index,
                    liquidity_net: i128::from_le_bytes(bytes),
                });
            }
        }
    }

    /// Compute sqrt_price at a tick index, clamped to valid range.
    #[inline]
    pub fn sqrt_price_at_tick_clamped(tick_index: i32, zero_for_one: bool) -> u128 {
        match tick_math::get_sqrt_price_at_tick(tick_index) {
            Ok(p) => {
                if zero_for_one {
                    p.max(tick_math::MIN_SQRT_PRICE_X64 + 1)
                } else {
                    p.min(tick_math::MAX_SQRT_PRICE_X64 - 1)
                }
            }
            Err(_) => {
                if zero_for_one {
                    tick_math::MIN_SQRT_PRICE_X64 + 1
                } else {
                    tick_math::MAX_SQRT_PRICE_X64 - 1
                }
            }
        }
    }

    /// Cross an initialized tick: update liquidity based on direction.
    #[inline]
    pub fn cross_tick(liquidity: u128, liquidity_net: i128, zero_for_one: bool) -> u128 {
        if zero_for_one {
            liquidity_math::add_delta(liquidity, -liquidity_net).unwrap_or(0)
        } else {
            liquidity_math::add_delta(liquidity, liquidity_net).unwrap_or(liquidity)
        }
    }

    /// Quote exact-in swap simulation across tick ranges.
    /// Uses the same math as the on-chain Raydium CLMM swap.
    pub fn quote_exact_in(&self, amount_in: u64, zero_for_one: bool, accounts: &[AccountInfo]) -> u64 {
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

            let next_tick_data = match self.find_next_tick(tick, zero_for_one, accounts) {
                Some(t) => t,
                None => break,
            };
            let next_tick_index = next_tick_data.tick_index;
            let liq_net = next_tick_data.liquidity_net;

            let sqrt_target = Self::sqrt_price_at_tick_clamped(next_tick_index, zero_for_one);

            let step = swap_math::compute_swap_step(
                sqrt_price,
                sqrt_target,
                liquidity,
                remaining,
                self.fee_rate,
                true,
                zero_for_one,
            );

            remaining = remaining
                .saturating_sub(step.amount_in)
                .saturating_sub(step.fee_amount);
            total_out += step.amount_out;
            sqrt_price = step.sqrt_price_next_x64;

            if step.sqrt_price_next_x64 == sqrt_target {
                liquidity = Self::cross_tick(liquidity, liq_net, zero_for_one);
                tick = if zero_for_one {
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

    fn make_pool_price_1() -> ClmmPool {
        // Pool at price=1.0 (sqrt_price = 1 << 64), tick=0
        let sqrt_price = 1u128 << 64;
        ClmmPool::new(
            sqrt_price,
            1_000_000_000_000, // L = 1e12
            0,
            10,    // tick_spacing
            3000,  // fee_rate = 0.3%
            &[
                ClmmTick { tick_index: -1000, liquidity_net: 1_000_000_000_000 },
                ClmmTick { tick_index: 1000, liquidity_net: -1_000_000_000_000 },
            ],
        )
    }

    #[test]
    fn test_virtual_reserves() {
        let sqrt_price = 1u128 << 64; // price = 1.0
        let liquidity = 1_000_000_000u128;
        let (v_a, v_b) = ClmmPool::virtual_reserves(sqrt_price, liquidity);
        assert_eq!(v_a, 1_000_000_000);
        assert_eq!(v_b, 1_000_000_000);
    }

    #[test]
    fn test_virtual_reserves_zero() {
        let (v_a, v_b) = ClmmPool::virtual_reserves(0, 100);
        assert_eq!(v_a, 0);
        assert_eq!(v_b, 0);
        let (v_a, v_b) = ClmmPool::virtual_reserves(100, 0);
        assert_eq!(v_a, 0);
        assert_eq!(v_b, 0);
    }

    #[test]
    fn test_quote_basic() {
        let pool = make_pool_price_1();
        let out = pool.quote_exact_in(1_000_000, true, &[]);
        assert!(out > 0, "should produce output");
        assert!(out < 1_000_000, "fees + slippage reduce output");
    }

    #[test]
    fn test_quote_zero() {
        let pool = make_pool_price_1();
        assert_eq!(pool.quote_exact_in(0, true, &[]), 0);
    }

    #[test]
    fn test_quote_symmetry() {
        let pool = make_pool_price_1();
        let out_z = pool.quote_exact_in(1_000_000, true, &[]);
        let out_o = pool.quote_exact_in(1_000_000, false, &[]);
        // At price=1.0 with symmetric liquidity, outputs should be similar
        let diff = (out_z as i64 - out_o as i64).abs();
        assert!(
            diff < 1000,
            "zero_for_one={} one_for_zero={} diff={}",
            out_z, out_o, diff
        );
    }

    #[test]
    fn test_find_next_tick() {
        let pool = make_pool_price_1();
        // zero_for_one from tick 0: should find tick -1000
        let t = pool.find_next_tick(0, true, &[]).unwrap();
        assert_eq!(t.tick_index, -1000);
        // one_for_zero from tick 0: should find tick 1000
        let t = pool.find_next_tick(0, false, &[]).unwrap();
        assert_eq!(t.tick_index, 1000);
    }

    #[test]
    fn test_cross_tick() {
        assert_eq!(ClmmPool::cross_tick(1000, 500, false), 1500);
        assert_eq!(ClmmPool::cross_tick(1000, 500, true), 500);
        assert_eq!(ClmmPool::cross_tick(100, 200, true), 0); // underflow -> 0
    }
}
