use crate::compat::AccountInfo;
use crate::programs::raydium_clmm::libraries::{tick_math, liquidity_math, swap_math};
use super::FD;

/// Initialized tick data for CLMM simulation.
#[derive(Clone, Copy, Debug)]
pub struct ClmmTick {
    pub tick_index: i32,
    pub liquidity_net: i128,
}

/// Pointer to one on-chain TickArray account.
#[derive(Clone, Copy, Debug)]
struct TickArraySource {
    acc_idx: u16,           // index into the accounts[] slice
    start_tick_index: i32,  // first tick index of this array
}

const MAX_TICK_SOURCES: usize = 4;

// Raydium CLMM TickArray layout constants (after 8-byte discriminator):
//   [0..32]  pool_id
//   [32..36] start_tick_index (i32)
//   [36..]   ticks[60] (each 168 bytes)
// Per tick: [0..4] unused, [4..20] liquidity_net (i128), [20..36] liquidity_gross (u128)
const TA_DISC: usize = 8;
const TA_TICKS: usize = TA_DISC + 32 + 4; // 44
const TICK_SIZE: usize = 168;
const TICK_CNT: usize = 60;
const LIQ_NET_OFF: usize = 4;
const LIQ_GROSS_OFF: usize = 20;
const MIN_TA_LEN: usize = TA_TICKS + TICK_SIZE * TICK_CNT;

/// Lightweight Raydium CLMM pool for arbitrage checking.
///
/// On-chain: ticks are read lazily from TickArray account data via `sources`.
/// Tests: ticks stored inline in `ticks_storage` (behind `#[cfg(test)]`).
///
/// Within each tick range the CLMM is equivalent to a constant-product AMM:
///   v_a = L << 64 / sqrt_P,  v_b = L * sqrt_P >> 64
#[derive(Clone, Debug)]
pub struct ClmmPool {
    pub sqrt_price: u128,       // Q64.64
    pub liquidity: u128,
    pub tick_current_index: i32,
    pub tick_spacing: u16,
    pub fee_rate: u32,          // denominator 1_000_000
    sources: [TickArraySource; MAX_TICK_SOURCES],
    source_count: u8,
    #[cfg(test)]
    ticks_storage: [ClmmTick; 128],
    #[cfg(test)]
    tick_count: usize,
}

impl ClmmPool {
    /// Create a new ClmmPool with lazy tick sources (on-chain path).
    /// Call `add_tick_source` to register tick array accounts.
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
            sources: [TickArraySource { acc_idx: 0, start_tick_index: 0 }; MAX_TICK_SOURCES],
            source_count: 0,
            #[cfg(test)]
            ticks_storage: [ClmmTick { tick_index: 0, liquidity_net: 0 }; 128],
            #[cfg(test)]
            tick_count: 0,
        }
    }

    /// Test constructor — stores ticks inline for unit tests without account data.
    #[cfg(test)]
    pub fn new(
        sqrt_price: u128,
        liquidity: u128,
        tick_current_index: i32,
        tick_spacing: u16,
        fee_rate: u32,
        ticks: &[ClmmTick],
    ) -> Self {
        let count = ticks.len().min(128);
        let mut storage = [ClmmTick { tick_index: 0, liquidity_net: 0 }; 128];
        storage[..count].copy_from_slice(&ticks[..count]);
        storage[..count].sort_unstable_by_key(|t| t.tick_index);
        Self {
            sqrt_price,
            liquidity,
            tick_current_index,
            tick_spacing,
            fee_rate,
            sources: [TickArraySource { acc_idx: 0, start_tick_index: 0 }; MAX_TICK_SOURCES],
            source_count: 0,
            ticks_storage: storage,
            tick_count: count,
        }
    }

    /// Register a tick array account for lazy reading.
    pub fn add_tick_source(&mut self, acc_idx: usize, start_tick_index: i32) {
        if (self.source_count as usize) < MAX_TICK_SOURCES {
            self.sources[self.source_count as usize] = TickArraySource {
                acc_idx: acc_idx as u16,
                start_tick_index,
            };
            self.source_count += 1;
        }
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

    /// Find the next initialized tick by scanning tick array account data on demand.
    /// For zero_for_one (descending): largest tick_index <= current_tick.
    /// For one_for_zero (ascending): smallest tick_index > current_tick.
    pub fn find_next_tick(&self, current_tick: i32, zero_for_one: bool, accounts: &[AccountInfo]) -> Option<ClmmTick> {
        // Lazy path: scan tick array accounts
        if self.source_count > 0 {
            let mut best: Option<ClmmTick> = None;
            let spacing = self.tick_spacing as i32;

            for si in 0..self.source_count as usize {
                let src = &self.sources[si];
                let idx = src.acc_idx as usize;
                if idx >= accounts.len() { continue; }
                let data = match accounts[idx].try_borrow_data() {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                if data.len() < MIN_TA_LEN { continue; }

                for i in 0..TICK_CNT {
                    let base = TA_TICKS + i * TICK_SIZE;

                    // Quick check: is this tick initialized? (liquidity_gross != 0)
                    let lg_bytes: [u8; 16] = match data[base + LIQ_GROSS_OFF..base + LIQ_GROSS_OFF + 16].try_into() {
                        Ok(b) => b,
                        Err(_) => continue,
                    };
                    if u128::from_le_bytes(lg_bytes) == 0 { continue; }

                    let tick_index = src.start_tick_index + (i as i32) * spacing;

                    // Direction filter
                    if zero_for_one {
                        if tick_index > current_tick { continue; }
                        if let Some(ref b) = best {
                            if tick_index <= b.tick_index { continue; }
                        }
                    } else {
                        if tick_index <= current_tick { continue; }
                        if let Some(ref b) = best {
                            if tick_index >= b.tick_index { continue; }
                        }
                    }

                    let ln_bytes: [u8; 16] = match data[base + LIQ_NET_OFF..base + LIQ_NET_OFF + 16].try_into() {
                        Ok(b) => b,
                        Err(_) => continue,
                    };
                    best = Some(ClmmTick {
                        tick_index,
                        liquidity_net: i128::from_le_bytes(ln_bytes),
                    });
                }
            }
            return best;
        }

        // Test fallback: use stored ticks
        #[cfg(test)]
        {
            let ticks = &self.ticks_storage[..self.tick_count];
            let found = if zero_for_one {
                ticks.iter().rev().find(|t| t.tick_index <= current_tick)
            } else {
                ticks.iter().find(|t| t.tick_index > current_tick)
            };
            return found.copied();
        }

        #[cfg(not(test))]
        None
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
