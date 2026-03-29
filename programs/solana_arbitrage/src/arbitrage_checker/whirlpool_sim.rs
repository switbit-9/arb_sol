use anchor_lang::prelude::AccountInfo;
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
const MAX_WP_SOURCES: usize = 3;

// Byte layout: simple (v1) tick array
const SIMPLE_TICKS_OFF: usize = 12; // 8 disc + 4 start_tick_index
const SIMPLE_TICK_SIZE: usize = 113;
const SIMPLE_TICK_COUNT: usize = 88;

// Byte layout: dynamic (v2) tick array
const DYN_TICKS_OFF: usize = 60; // 8 disc + 4 start + 32 whirlpool + 16 bitmap
const DYN_TICK_INIT_SIZE: usize = 113; // 1 tag + 112 data

#[derive(Clone, Copy, Debug)]
struct WpTickSource {
    acc_idx: usize,
    start_tick: i32,
    is_dynamic: bool,
}

/// Lightweight Whirlpool pool for arbitrage checking.
///
/// Stores only essential state: current sqrt_price, liquidity, tick position, fee rate,
/// and either pre-loaded ticks (eager, via `new`) or tick array account references
/// (lazy, via `new_lazy` + `add_*_source`).
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
    // Eager path (populated by new())
    ticks_storage: [WhirlpoolTick; MAX_TICKS],
    tick_count: usize,
    // Lazy path (populated by new_lazy + add_*_source)
    sources: [WpTickSource; MAX_WP_SOURCES],
    source_count: usize,
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
            sources: [WpTickSource { acc_idx: 0, start_tick: 0, is_dynamic: false }; MAX_WP_SOURCES],
            source_count: 0,
        }
    }

    /// Create a pool with no ticks. Register tick array sources with
    /// `add_simple_source` / `add_dynamic_source`, then ticks are read
    /// lazily from account data during `find_next_tick`.
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
            ticks_storage: [WhirlpoolTick { tick_index: 0, liquidity_net: 0 }; MAX_TICKS],
            tick_count: 0,
            sources: [WpTickSource { acc_idx: 0, start_tick: 0, is_dynamic: false }; MAX_WP_SOURCES],
            source_count: 0,
        }
    }

    /// Register a simple (v1) tick array source for lazy loading.
    pub fn add_simple_source(&mut self, acc_idx: usize, start_tick: i32) {
        if self.source_count < MAX_WP_SOURCES {
            self.sources[self.source_count] = WpTickSource { acc_idx, start_tick, is_dynamic: false };
            self.source_count += 1;
        }
    }

    /// Register a dynamic (v2) tick array source for lazy loading.
    pub fn add_dynamic_source(&mut self, acc_idx: usize, start_tick: i32, _bitmap: u128) {
        if self.source_count < MAX_WP_SOURCES {
            self.sources[self.source_count] = WpTickSource { acc_idx, start_tick, is_dynamic: true };
            self.source_count += 1;
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
    ///
    /// Eager path (tick_count > 0): binary search on pre-loaded ticks.
    /// Lazy path: reads tick data on demand from account data.
    pub fn find_next_tick(&self, current_tick: i32, a_to_b: bool, accounts: &[AccountInfo]) -> Option<WhirlpoolTick> {
        if self.tick_count > 0 {
            return self.find_next_tick_eager(current_tick, a_to_b);
        }
        self.find_next_tick_lazy(current_tick, a_to_b, accounts)
    }

    fn find_next_tick_eager(&self, current_tick: i32, a_to_b: bool) -> Option<WhirlpoolTick> {
        let ticks = self.ticks();
        if ticks.is_empty() { return None; }
        let idx = ticks.partition_point(|t| t.tick_index <= current_tick);
        if a_to_b {
            if idx > 0 { Some(ticks[idx - 1]) } else { None }
        } else {
            if idx < ticks.len() { Some(ticks[idx]) } else { None }
        }
    }

    fn find_next_tick_lazy(&self, current_tick: i32, a_to_b: bool, accounts: &[AccountInfo]) -> Option<WhirlpoolTick> {
        let mut best: Option<WhirlpoolTick> = None;
        let spacing = self.tick_spacing as i32;

        for si in 0..self.source_count {
            let s = &self.sources[si];
            if s.acc_idx >= accounts.len() { continue; }
            let data = match accounts[s.acc_idx].try_borrow_data() {
                Ok(d) => d,
                Err(_) => continue,
            };

            if s.is_dynamic {
                Self::scan_dynamic(&data, s.start_tick, spacing, current_tick, a_to_b, &mut best);
            } else {
                Self::scan_simple(&data, s.start_tick, spacing, current_tick, a_to_b, &mut best);
            }
        }

        best
    }

    /// Scan a simple (v1) tick array for the next initialized tick.
    /// Uses directed scan: for a_to_b scans backwards from the highest valid slot,
    /// for b_to_a scans forwards from the lowest valid slot.
    fn scan_simple(data: &[u8], start_tick: i32, spacing: i32, current_tick: i32, a_to_b: bool, best: &mut Option<WhirlpoolTick>) {
        if data.len() < SIMPLE_TICKS_OFF { return; }

        if a_to_b {
            // Want largest tick_index <= current_tick. Scan backwards.
            let max_slot = if current_tick < start_tick {
                return; // no slot has tick_index <= current_tick
            } else {
                (((current_tick - start_tick) / spacing) as usize).min(SIMPLE_TICK_COUNT - 1)
            };

            for i in (0..=max_slot).rev() {
                let off = SIMPLE_TICKS_OFF + i * SIMPLE_TICK_SIZE;
                if off + SIMPLE_TICK_SIZE > data.len() { continue; }
                if data[off] == 0 { continue; }

                let tick_index = start_tick + (i as i32) * spacing;
                // Already guaranteed tick_index <= current_tick by max_slot bound
                if best.map_or(false, |b| tick_index <= b.tick_index) {
                    break; // this and all remaining are worse
                }

                if let Ok(bytes) = <[u8; 16]>::try_from(&data[off + 1..off + 17]) {
                    *best = Some(WhirlpoolTick {
                        tick_index,
                        liquidity_net: i128::from_le_bytes(bytes),
                    });
                }
                break; // first found scanning backwards is the best from this source
            }
        } else {
            // Want smallest tick_index > current_tick. Scan forwards.
            let min_slot = if current_tick < start_tick {
                0
            } else {
                (((current_tick - start_tick) / spacing) as usize + 1).min(SIMPLE_TICK_COUNT)
            };
            if min_slot >= SIMPLE_TICK_COUNT { return; }

            for i in min_slot..SIMPLE_TICK_COUNT {
                let off = SIMPLE_TICKS_OFF + i * SIMPLE_TICK_SIZE;
                if off + SIMPLE_TICK_SIZE > data.len() { break; }
                if data[off] == 0 { continue; }

                let tick_index = start_tick + (i as i32) * spacing;
                if best.map_or(false, |b| tick_index >= b.tick_index) {
                    break; // this and all remaining are worse
                }

                if let Ok(bytes) = <[u8; 16]>::try_from(&data[off + 1..off + 17]) {
                    *best = Some(WhirlpoolTick {
                        tick_index,
                        liquidity_net: i128::from_le_bytes(bytes),
                    });
                }
                break;
            }
        }
    }

    /// Scan a dynamic (v2) tick array for the next initialized tick.
    /// Must walk sequentially due to variable-length encoding.
    fn scan_dynamic(data: &[u8], start_tick: i32, spacing: i32, current_tick: i32, a_to_b: bool, best: &mut Option<WhirlpoolTick>) {
        let mut cursor = DYN_TICKS_OFF;
        let mut found_in_source: Option<WhirlpoolTick> = None;

        for i in 0..SIMPLE_TICK_COUNT {
            if cursor >= data.len() { break; }

            if data[cursor] == 0 {
                cursor += 1;
                continue;
            }
            // Initialized tick
            if cursor + DYN_TICK_INIT_SIZE > data.len() { break; }

            let tick_index = start_tick + (i as i32) * spacing;

            if a_to_b {
                // Want largest tick_index <= current_tick
                if tick_index > current_tick {
                    // Past the boundary — done scanning this source
                    break;
                }
                if let Ok(bytes) = <[u8; 16]>::try_from(&data[cursor + 1..cursor + 17]) {
                    found_in_source = Some(WhirlpoolTick {
                        tick_index,
                        liquidity_net: i128::from_le_bytes(bytes),
                    });
                    // Keep going — a later slot may have a larger valid tick_index
                }
            } else {
                // Want smallest tick_index > current_tick
                if tick_index > current_tick {
                    if let Ok(bytes) = <[u8; 16]>::try_from(&data[cursor + 1..cursor + 17]) {
                        found_in_source = Some(WhirlpoolTick {
                            tick_index,
                            liquidity_net: i128::from_le_bytes(bytes),
                        });
                    }
                    break; // first valid ascending is the best from this source
                }
            }

            cursor += DYN_TICK_INIT_SIZE;
        }

        // Merge with overall best
        if let Some(found) = found_in_source {
            let dominated = match best {
                Some(b) if a_to_b => found.tick_index <= b.tick_index,
                Some(b) => found.tick_index >= b.tick_index,
                None => false,
            };
            if !dominated {
                *best = Some(found);
            }
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
    pub fn quote_exact_in(&self, amount_in: u64, a_to_b: bool, accounts: &[AccountInfo]) -> u64 {
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

            let next_tick_data = match self.find_next_tick(tick, a_to_b, accounts) {
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
        let out_a_to_b = pool.quote_exact_in(1_000_000, true, &[]);
        let out_b_to_a = pool.quote_exact_in(1_000_000, false, &[]);
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
        let t = pool.find_next_tick(0, true, &[]).unwrap();
        assert_eq!(t.tick_index, -1000);
        // b_to_a from tick 0: should find tick 1000
        let t = pool.find_next_tick(0, false, &[]).unwrap();
        assert_eq!(t.tick_index, 1000);
    }

    #[test]
    fn test_cross_tick() {
        assert_eq!(WhirlpoolPool::cross_tick(1000, 500, false), 1500);
        assert_eq!(WhirlpoolPool::cross_tick(1000, 500, true), 500);
        assert_eq!(WhirlpoolPool::cross_tick(100, 200, true), 0); // underflow → 0
    }
}
