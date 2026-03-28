pub mod dlmm_sim;
pub mod amm_sim;
pub mod whirlpool_sim;
pub mod clmm_sim;
pub mod optimizer;
pub mod dlmm_checker;
pub mod amm_checker;
pub mod whirlpool_checker;
pub mod clmm_checker;
pub mod wide_math;
pub mod runner;

pub const FEE_PRECISION: u128 = 1_000_000_000;
pub const FEE_PRECISION_SHL32: u128 = FEE_PRECISION << 32;
pub const SCALE_OFFSET: u8 = 64;
pub const FD: u128 = 1_000_000; // AMM fee denominator

/// One leg of an arbitrage: which pool instance and which swap direction.
#[derive(Clone, Copy, Debug)]
pub struct Hop {
    /// Index into the `instances` slice.
    pub instance_idx: u8,
    /// true = base→quote (LeftToRight), false = quote→base (RightToLeft).
    pub left_to_right: bool,
}

/// Result of an arbitrage check: optimal input, profit, and the hop sequence.
#[derive(Clone, Debug)]
pub struct ArbitrageResult {
    /// Optimal input amount (in lamports / smallest unit of input token).
    /// 0 if no profitable arb found.
    pub amount_in: u64,
    /// Signed profit in lamports. Positive = profitable. 0 = no arb.
    pub profit: i64,
    /// Ordered swap legs. Only hops[..hop_count] are valid.
    pub hops: [Hop; 3],
    /// Number of valid hops (2 for 2-hop, 3 for 3-hop).
    pub hop_count: u8,
}

const EMPTY_HOP: Hop = Hop { instance_idx: 0, left_to_right: true };

impl ArbitrageResult {
    pub fn none() -> Self {
        Self {
            amount_in: 0,
            profit: 0,
            hops: [EMPTY_HOP; 3],
            hop_count: 0,
        }
    }

    /// Checker-internal: create a result with only amount_in/profit set.
    /// The runner fills in hops after comparing all pairs.
    pub fn from_pair(amount_in: u64, profit: i64) -> Self {
        Self {
            amount_in,
            profit,
            hops: [EMPTY_HOP; 3],
            hop_count: 0,
        }
    }

    pub fn is_profitable(&self) -> bool {
        self.profit > 0
    }
}
