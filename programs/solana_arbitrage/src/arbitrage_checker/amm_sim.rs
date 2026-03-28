/// Fee denominator for AMM integer fee math (millionths).
const FEE_DENOM: u128 = 1_000_000;

/// Fee tier thresholds for PumpAmm: (threshold, fee_in_millionths)
/// market_cap_proxy = min_vault * 1_000_000
/// compared against threshold * max_vault
const PUMP_TIERS: [(u128, u64); 24] = [
    (420, 12500),
    (1470, 12000),
    (2460, 11500),
    (3440, 11000),
    (4420, 10500),
    (9820, 10000),
    (14740, 9500),
    (19650, 9000),
    (24560, 8500),
    (29470, 8000),
    (34380, 7500),
    (39300, 7000),
    (44210, 6500),
    (49120, 6000),
    (54030, 5500),
    (58940, 5200),
    (63860, 5000),
    (68770, 4700),
    (73681, 4500),
    (78590, 4200),
    (83500, 4000),
    (88400, 3700),
    (93330, 3500),
    (98240, 3200),
];

/// Generic constant-product AMM pool with per-direction input/output fees.
///
/// Works for both PumpAmm and Raydium CPMM (and any x*y=k AMM).
/// All fees are in millionths (e.g. 12500 = 1.25%), FEE_DENOM = 1_000_000.
///
/// Each swap direction has separate input and output fees:
/// - Buy base (quote→base): `buy_input_fee` deducted before swap, `buy_output_fee` after
/// - Sell base (base→quote): `sell_input_fee` deducted before swap, `sell_output_fee` after
///
/// For Pump: buy=(fee,0), sell=(0,fee) — one fee is always 0.
/// For CPMM: depends on `is_creator_fee_on_input` per direction.
#[derive(Clone, Debug)]
pub struct AmmPool {
    pub base_vault: u64,
    pub quote_vault: u64,
    pub buy_input_fee: u64,
    pub buy_output_fee: u64,
    pub sell_input_fee: u64,
    pub sell_output_fee: u64,
}

impl AmmPool {
    /// Construct from PumpAmm vault amounts. Fee tier is auto-detected.
    /// Pump: buy has fee on input, sell has fee on output.
    pub fn from_pump(base_vault: u64, quote_vault: u64) -> Self {
        let fee = get_pump_fee_tier(base_vault, quote_vault);
        Self::from_pump_with_fee(base_vault, quote_vault, fee)
    }

    /// Construct from PumpAmm vault amounts with an explicit fee (millionths).
    /// Use this when the actual fee_numerator is known from the pool instance.
    pub fn from_pump_with_fee(base_vault: u64, quote_vault: u64, fee: u64) -> Self {
        Self {
            base_vault,
            quote_vault,
            buy_input_fee: fee,
            buy_output_fee: 0,
            sell_input_fee: 0,
            sell_output_fee: fee,
        }
    }

    /// Construct from Raydium AMM pool parameters.
    /// Fee is input-only, same rate both directions. Pass 0 for now (fee parsing TODO).
    pub fn from_raydium_amm(base_vault: u64, quote_vault: u64, fee_millionths: u64) -> Self {
        Self {
            base_vault,
            quote_vault,
            buy_input_fee: fee_millionths,
            buy_output_fee: 0,
            sell_input_fee: fee_millionths,
            sell_output_fee: 0,
        }
    }

    /// Construct from Meteora DAMM V1 pool parameters.
    ///
    /// DAMM V1 fee model: trade fee is deducted from input (protocol fee is a subset,
    /// not additional). Same fee rate both directions, no output fee.
    pub fn from_damm_v1(
        base_vault: u64,
        quote_vault: u64,
        trade_fee_numerator: u64,
        trade_fee_denominator: u64,
    ) -> Self {
        // Convert fraction to millionths: fee_millionths = num * 1_000_000 / den
        let fee_millionths = if trade_fee_denominator == 0 {
            0
        } else {
            (trade_fee_numerator as u128 * FEE_DENOM / trade_fee_denominator as u128) as u64
        };
        Self {
            base_vault,
            quote_vault,
            buy_input_fee: fee_millionths,
            buy_output_fee: 0,
            sell_input_fee: fee_millionths,
            sell_output_fee: 0,
        }
    }

    /// Construct from Meteora DAMM V2 pool parameters.
    ///
    /// DAMM V2 is concentrated liquidity with a single range, which behaves
    /// as constant-product with virtual reserves derived from sqrt_price and liquidity:
    ///   virtual_base  = L / sqrt_price_q64
    ///   virtual_quote = L * sqrt_price_q64 / 2^128
    ///
    /// Fee placement depends on `collect_fee_mode`:
    ///   0 (BothToken): fee always on output
    ///   1 (OnlyB):     B→A (buy_base) fee on input, A→B (sell_base) fee on output
    pub fn from_damm_v2(
        sqrt_price: u128,
        liquidity: u128,
        fee_rate: f64,
        collect_fee_mode: u8,
    ) -> Self {
        let sq = sqrt_price as f64;
        let l = liquidity as f64;
        let q128: f64 = 18446744073709551616.0 * 18446744073709551616.0; // 2^128

        let virtual_base = (l / sq) as u64;
        let virtual_quote = (l * sq / q128) as u64;

        let fee_millionths = (fee_rate * FEE_DENOM as f64) as u64;

        let (buy_in, buy_out, sell_in, sell_out) = match collect_fee_mode {
            1 => (fee_millionths, 0, 0, fee_millionths), // OnlyB
            _ => (0, fee_millionths, 0, fee_millionths),  // BothToken
        };

        Self {
            base_vault: virtual_base,
            quote_vault: virtual_quote,
            buy_input_fee: buy_in,
            buy_output_fee: buy_out,
            sell_input_fee: sell_in,
            sell_output_fee: sell_out,
        }
    }

    /// Construct from Raydium CPMM pool parameters.
    ///
    /// `base_vault` / `quote_vault` should already have protocol+fund fees subtracted
    /// (i.e. the adjusted vault amounts from `get_swap_amounts`).
    ///
    /// CPMM fee model:
    /// - `trade_fee_rate`: always deducted from input
    /// - `creator_fee_rate`: on input or output depending on `is_creator_fee_on_input`
    pub fn from_cpmm(
        base_vault: u64,
        quote_vault: u64,
        trade_fee_rate: u64,
        creator_fee_rate: u64,
        buy_creator_fee_on_input: bool,
        sell_creator_fee_on_input: bool,
    ) -> Self {
        let (buy_in, buy_out) = if buy_creator_fee_on_input {
            (trade_fee_rate + creator_fee_rate, 0)
        } else {
            (trade_fee_rate, creator_fee_rate)
        };
        let (sell_in, sell_out) = if sell_creator_fee_on_input {
            (trade_fee_rate + creator_fee_rate, 0)
        } else {
            (trade_fee_rate, creator_fee_rate)
        };
        Self {
            base_vault,
            quote_vault,
            buy_input_fee: buy_in,
            buy_output_fee: buy_out,
            sell_input_fee: sell_in,
            sell_output_fee: sell_out,
        }
    }

    /// Return an oriented copy with base↔quote and buy↔sell swapped.
    /// Use this when the arb's mid_token is the pool's quote (not base).
    /// After flipping, "buy base" = spend start_token, get mid_token,
    /// and "sell base" = spend mid_token, get start_token — matching
    /// what check_amm_amm / check_*_to_amm expect.
    pub fn flipped(&self) -> Self {
        Self {
            base_vault: self.quote_vault,
            quote_vault: self.base_vault,
            buy_input_fee: self.sell_input_fee,
            buy_output_fee: self.sell_output_fee,
            sell_input_fee: self.buy_input_fee,
            sell_output_fee: self.buy_output_fee,
        }
    }

    /// Buy base tokens with quote tokens.
    /// Applies `buy_input_fee` before swap, `buy_output_fee` after.
    pub fn buy_base(&self, quote_amount_in: u64) -> u64 {
        if quote_amount_in == 0 {
            return 0;
        }
        let base_r = self.base_vault as u128;
        let quote_r = self.quote_vault as u128;
        let fi = FEE_DENOM - self.buy_input_fee as u128;
        let fo = FEE_DENOM - self.buy_output_fee as u128;

        let in_after_fee = (quote_amount_in as u128) * fi / FEE_DENOM;
        let denom = quote_r + in_after_fee;
        if denom == 0 {
            return 0;
        }
        let raw_out = base_r.saturating_sub(base_r * quote_r / denom);
        let out = raw_out * fo / FEE_DENOM;
        out.min(self.base_vault as u128) as u64
    }

    /// Sell base tokens for quote tokens.
    /// Applies `sell_input_fee` before swap, `sell_output_fee` after.
    pub fn sell_base(&self, base_amount_in: u64) -> u64 {
        if base_amount_in == 0 {
            return 0;
        }
        let base_r = self.base_vault as u128;
        let quote_r = self.quote_vault as u128;
        let fi = FEE_DENOM - self.sell_input_fee as u128;
        let fo = FEE_DENOM - self.sell_output_fee as u128;

        let in_after_fee = (base_amount_in as u128) * fi / FEE_DENOM;
        let denom = base_r + in_after_fee;
        if denom == 0 {
            return 0;
        }
        let raw_out = quote_r.saturating_sub(base_r * quote_r / denom);
        let out = (raw_out * fo / FEE_DENOM) as u64;
        out.min(self.quote_vault as u64)
    }

    /// Inverse: how much quote input needed to buy `base_amount_out` base tokens?
    pub fn quote_for_base_out(&self, base_amount_out: u64) -> Option<u64> {
        if base_amount_out == 0 {
            return Some(0);
        }
        let base_r = self.base_vault as u128;
        let quote_r = self.quote_vault as u128;
        let fi = FEE_DENOM - self.buy_input_fee as u128;
        let fo = FEE_DENOM - self.buy_output_fee as u128;
        let out = base_amount_out as u128;

        // Reverse output fee
        let raw_out = if fo == FEE_DENOM { out } else { (out * FEE_DENOM + fo - 1) / fo };
        if raw_out >= base_r { return None; }

        // Reverse CP
        let numer = quote_r.checked_mul(raw_out)?;
        let denom = base_r.checked_sub(raw_out)?;
        let in_after_fee = numer / denom + 1;

        // Reverse input fee
        let in_before_fee = (in_after_fee * FEE_DENOM + fi - 1) / fi;
        Some(in_before_fee as u64)
    }

    /// Inverse: how much base input needed to receive `quote_amount_out` quote tokens?
    pub fn base_for_quote_out(&self, quote_amount_out: u64) -> Option<u64> {
        if quote_amount_out == 0 {
            return Some(0);
        }
        let base_r = self.base_vault as u128;
        let quote_r = self.quote_vault as u128;
        let fi = FEE_DENOM - self.sell_input_fee as u128;
        let fo = FEE_DENOM - self.sell_output_fee as u128;

        // Reverse output fee
        let raw_out = if fo == FEE_DENOM {
            quote_amount_out as u128
        } else {
            ((quote_amount_out as u128) * FEE_DENOM + fo - 1) / fo
        };
        if raw_out >= quote_r { return None; }

        // Reverse CP
        let numer = base_r.checked_mul(raw_out)?;
        let denom = quote_r.checked_sub(raw_out)?;
        let in_after_fee = numer / denom + 1;

        // Reverse input fee
        let in_before_fee = if fi == FEE_DENOM { in_after_fee } else { (in_after_fee * FEE_DENOM + fi - 1) / fi };
        Some(in_before_fee as u64)
    }
}

pub fn get_pump_fee_tier(base_vault: u64, quote_vault: u64) -> u64 {
    let min_v = (base_vault as u128).min(quote_vault as u128);
    let max_v = (base_vault as u128).max(quote_vault as u128);
    let mcap_num = min_v.saturating_mul(1_000_000);

    for &(threshold, fee) in &PUMP_TIERS {
        if mcap_num <= threshold.saturating_mul(max_v) {
            return fee;
        }
    }
    3000 // 0.30% default
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fee_tier_small_pool() {
        let fee = get_pump_fee_tier(1_000_000, 1_000_000_000);
        assert_eq!(fee, 12000);
    }

    #[test]
    fn test_fee_tier_balanced_pool() {
        let fee = get_pump_fee_tier(1_000_000_000, 1_000_000_000);
        assert_eq!(fee, 3000);
    }

    #[test]
    fn test_buy_sell_roundtrip() {
        let pool = AmmPool::from_pump(1_000_000_000_000, 50_000_000_000);
        let base_out = pool.buy_base(1_000_000_000);
        assert!(base_out > 0);

        let pool2 = AmmPool::from_pump(
            pool.base_vault - base_out,
            pool.quote_vault + 1_000_000_000,
        );
        let quote_out = pool2.sell_base(base_out);
        assert!(quote_out < 1_000_000_000);
        assert!(quote_out > 0);
    }

    #[test]
    fn test_inverse_buy() {
        let pool = AmmPool::from_pump(1_000_000_000_000, 50_000_000_000);
        let quote_needed = pool.quote_for_base_out(1_000).unwrap();
        let base_got = pool.buy_base(quote_needed as u64);
        assert!(
            base_got >= 1_000,
            "inverse should be conservative: got {} expected >= 1000",
            base_got
        );
    }

    #[test]
    fn test_cpmm_pool_creation() {
        let pool = AmmPool::from_cpmm(1_000_000_000, 500_000_000, 25000, 1000, true, true);
        assert_eq!(pool.buy_input_fee, 26000);
        assert_eq!(pool.buy_output_fee, 0);
        assert_eq!(pool.sell_input_fee, 26000);
        assert_eq!(pool.sell_output_fee, 0);

        let pool2 = AmmPool::from_cpmm(1_000_000_000, 500_000_000, 25000, 1000, false, false);
        assert_eq!(pool2.buy_input_fee, 25000);
        assert_eq!(pool2.buy_output_fee, 1000);
        assert_eq!(pool2.sell_input_fee, 25000);
        assert_eq!(pool2.sell_output_fee, 1000);
    }
}
