use crate::compat::AccountInfo;
use crate::programs::orca::libraries::{tick_math, liquidity_math, swap_math};
use super::FD;

/// Initialized tick data for whirlpool simulation.
#[derive(Clone, Copy, Debug)]
pub struct WhirlpoolTick {
    pub tick_index: i32,
    pub liquidity_net: i128,
}

/// Tick array format tag for lazy reading.
#[derive(Clone, Copy, Debug)]
enum WpTickFormat {
    /// Fixed-size TickArray: 88 ticks × 113 bytes each, starting at offset 12.
    Simple,
    /// DynamicTickArray: variable-length, tag-based encoding starting at offset 60.
    /// Bitmap (u128) indicates which of the 88 slots are initialized.
    Dynamic { bitmap: u128 },
}

/// Pointer to one on-chain Whirlpool TickArray account.
#[derive(Clone, Copy, Debug)]
struct WpTickSource {
    acc_idx: u16,
    start_tick_index: i32,
    format: WpTickFormat,
}

const MAX_WP_SOURCES: usize = 3;

// TickArraySimple layout constants
const DISC_LEN: usize = 8;
const SIMPLE_TICK_OFF: usize = DISC_LEN + 4; // 12: disc + start_tick_index
const TICK_LEN: usize = 113;
const TICK_INIT_OFF: usize = 0;  // bool (1 byte)
const TICK_LIQ_NET_OFF: usize = 1;  // i128 (16 bytes)
const TICK_CNT: usize = 88;
const SIMPLE_MIN_LEN: usize = SIMPLE_TICK_OFF + TICK_LEN; // at least 1 tick

// DynamicTickArray layout constants
const DYN_BITMAP_OFF: usize = DISC_LEN + 4 + 32; // 44: disc + start + whirlpool
const DYN_TICK_OFF: usize = DYN_BITMAP_OFF + 16;  // 60: after bitmap
const DYN_INIT_DATA_LEN: usize = 112; // tag(1) + data(112) = 113, but data portion is 112

// Discriminators
const SIMPLE_DISC: [u8; 8] = [69, 97, 189, 190, 110, 7, 66, 187];
const DYNAMIC_DISC: [u8; 8] = [17, 216, 246, 142, 225, 199, 218, 56];

/// Lightweight Whirlpool pool for arbitrage checking.
///
/// On-chain: ticks are read lazily from TickArray account data via `sources`.
/// Tests: ticks stored inline in `ticks_storage` (behind `#[cfg(test)]`).
///
/// Within each tick range the Whirlpool is equivalent to a constant-product AMM:
///   v_a = L << 64 / sqrt_P,  v_b = L * sqrt_P >> 64
#[derive(Clone, Debug)]
pub struct WhirlpoolPool {
    pub sqrt_price: u128,       // Q64.64
    pub liquidity: u128,
    pub tick_current_index: i32,
    pub tick_spacing: u16,
    pub fee_rate: u32,          // hundredths of basis point, denominator 1_000_000
    sources: [WpTickSource; MAX_WP_SOURCES],
    source_count: u8,
    #[cfg(test)]
    ticks_storage: [WhirlpoolTick; 128],
    #[cfg(test)]
    tick_count: usize,
}

impl WhirlpoolPool {
    /// Create a new WhirlpoolPool with lazy tick sources (on-chain path).
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
            sources: [WpTickSource {
                acc_idx: 0,
                start_tick_index: 0,
                format: WpTickFormat::Simple,
            }; MAX_WP_SOURCES],
            source_count: 0,
            #[cfg(test)]
            ticks_storage: [WhirlpoolTick { tick_index: 0, liquidity_net: 0 }; 128],
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
        ticks: &[WhirlpoolTick],
    ) -> Self {
        let count = ticks.len().min(128);
        let mut storage = [WhirlpoolTick { tick_index: 0, liquidity_net: 0 }; 128];
        storage[..count].copy_from_slice(&ticks[..count]);
        storage[..count].sort_unstable_by_key(|t| t.tick_index);
        Self {
            sqrt_price,
            liquidity,
            tick_current_index,
            tick_spacing,
            fee_rate,
            sources: [WpTickSource {
                acc_idx: 0,
                start_tick_index: 0,
                format: WpTickFormat::Simple,
            }; MAX_WP_SOURCES],
            source_count: 0,
            ticks_storage: storage,
            tick_count: count,
        }
    }

    /// Register a Simple (fixed) TickArray source.
    pub fn add_simple_source(&mut self, acc_idx: usize, start_tick_index: i32) {
        if (self.source_count as usize) < MAX_WP_SOURCES {
            self.sources[self.source_count as usize] = WpTickSource {
                acc_idx: acc_idx as u16,
                start_tick_index,
                format: WpTickFormat::Simple,
            };
            self.source_count += 1;
        }
    }

    /// Register a DynamicTickArray source with its tick bitmap.
    pub fn add_dynamic_source(&mut self, acc_idx: usize, start_tick_index: i32, bitmap: u128) {
        if (self.source_count as usize) < MAX_WP_SOURCES {
            self.sources[self.source_count as usize] = WpTickSource {
                acc_idx: acc_idx as u16,
                start_tick_index,
                format: WpTickFormat::Dynamic { bitmap },
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

    /// Find the next initialized tick by scanning tick array account data on demand.
    /// For a_to_b (descending): largest tick_index <= current_tick.
    /// For b_to_a (ascending): smallest tick_index > current_tick.
    pub fn find_next_tick(&self, current_tick: i32, a_to_b: bool, accounts: &[AccountInfo]) -> Option<WhirlpoolTick> {
        // Lazy path: scan tick array accounts
        if self.source_count > 0 {
            let mut best: Option<WhirlpoolTick> = None;
            let spacing = self.tick_spacing as i32;

            for si in 0..self.source_count as usize {
                let src = &self.sources[si];
                let idx = src.acc_idx as usize;
                if idx >= accounts.len() { continue; }
                let data = match accounts[idx].try_borrow_data() {
                    Ok(d) => d,
                    Err(_) => continue,
                };

                match src.format {
                    WpTickFormat::Simple => {
                        if data.len() < SIMPLE_MIN_LEN { continue; }
                        for i in 0..TICK_CNT {
                            let off = SIMPLE_TICK_OFF + i * TICK_LEN;
                            if off + TICK_LEN > data.len() { break; }

                            // Check initialized flag
                            if data[off + TICK_INIT_OFF] == 0 { continue; }

                            let tick_index = src.start_tick_index + (i as i32) * spacing;

                            // Direction filter
                            if a_to_b {
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

                            let ln_bytes: [u8; 16] = match data[off + TICK_LIQ_NET_OFF..off + TICK_LIQ_NET_OFF + 16].try_into() {
                                Ok(b) => b,
                                Err(_) => continue,
                            };
                            best = Some(WhirlpoolTick {
                                tick_index,
                                liquidity_net: i128::from_le_bytes(ln_bytes),
                            });
                        }
                    }
                    WpTickFormat::Dynamic { bitmap } => {
                        if data.len() < DYN_TICK_OFF + TICK_CNT { continue; } // minimum: 88 uninit bytes

                        for i in 0..TICK_CNT {
                            // Check bitmap for initialized
                            if (bitmap >> i) & 1 == 0 { continue; }

                            let tick_index = src.start_tick_index + (i as i32) * spacing;

                            // Direction filter
                            if a_to_b {
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

                            // Compute offset: 60 + i + count_init_before_i * 112
                            let mask = if i == 0 { 0u128 } else { (1u128 << i) - 1 };
                            let init_before = (bitmap & mask).count_ones() as usize;
                            let off = DYN_TICK_OFF + i + init_before * DYN_INIT_DATA_LEN;
                            // skip tag byte (1), then liquidity_net at 0..16
                            let ln_start = off + 1;
                            if ln_start + 16 > data.len() { continue; }

                            let ln_bytes: [u8; 16] = match data[ln_start..ln_start + 16].try_into() {
                                Ok(b) => b,
                                Err(_) => continue,
                            };
                            best = Some(WhirlpoolTick {
                                tick_index,
                                liquidity_net: i128::from_le_bytes(ln_bytes),
                            });
                        }
                    }
                }
            }
            return best;
        }

        // Test fallback: use stored ticks
        #[cfg(test)]
        {
            let ticks = &self.ticks_storage[..self.tick_count];
            if ticks.is_empty() { return None; }
            let idx = ticks.partition_point(|t| t.tick_index <= current_tick);
            let found = if a_to_b {
                if idx > 0 { Some(&ticks[idx - 1]) } else { None }
            } else {
                if idx < ticks.len() { Some(&ticks[idx]) } else { None }
            };
            return found.copied();
        }

        #[cfg(not(test))]
        None
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
