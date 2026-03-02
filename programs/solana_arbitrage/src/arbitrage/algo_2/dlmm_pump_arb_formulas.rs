/// ═══════════════════════════════════════════════════════════════════════════════
/// DLMM <-> Pump AMM Arbitrage: Mathematical Formulas & Implementation Guide
/// ═══════════════════════════════════════════════════════════════════════════════
///
/// This file documents the exact mathematical formulas for detecting and
/// optimally sizing arbitrage between Meteora DLMM and Pump AMM pools that
/// share a common token pair (e.g., SOL/MEMECOIN).
///
///
/// ═══════════════════════════════════════════════════════════════════════════════
/// 1. INDIVIDUAL DEX SWAP FORMULAS
/// ═══════════════════════════════════════════════════════════════════════════════
///
/// ─── 1A. PUMP AMM (Constant Product x*y=k) ───────────────────────────────────
///
/// State variables:
///   X = base_vault_amount   (base token, e.g. MEMECOIN)
///   Y = quote_vault_amount  (quote token, e.g. SOL)
///   f_pump = 1 - fee_rate   (fee factor, market-cap dependent, see fee table)
///
/// Price:
///   P_pump = Y / X           (quote per base, i.e. SOL per MEMECOIN)
///   P_pump_inv = X / Y       (base per quote, i.e. MEMECOIN per SOL)
///
/// DIRECTION A: Buy base (input = quote SOL, output = base MEMECOIN)
///   Fee is applied ON INPUT (before swap):
///
///     Δy_eff = Δy * f_pump                                  (effective input after fee)
///     ΔX_out = X - (X * Y) / (Y + Δy_eff)
///            = X * Δy_eff / (Y + Δy_eff)                    (simplified)
///
///   where Δy = amount of SOL input
///
/// DIRECTION B: Sell base (input = base MEMECOIN, output = quote SOL)
///   Fee is applied ON OUTPUT (after swap):
///
///     ΔY_raw = Y - (X * Y) / (X + Δx)
///            = Y * Δx / (X + Δx)                            (simplified)
///     ΔY_out = ΔY_raw * f_pump                              (output after fee)
///
///   where Δx = amount of MEMECOIN input
///
/// INVERSE (given desired output, find required input):
///   Buy base:  Δy_required = Y * ΔX_out / ((X - ΔX_out) * f_pump)
///   Sell base: Δx_required = X * ΔY_out / ((Y * f_pump) - ΔY_out)
///
/// Fee table (market_cap_sol = min(P_pump, P_pump_inv) * 1_000_000):
///   ≤ 420 SOL   → 1.25%  |  ≤ 9,820  → 1.00%  |  ≤ 49,120 → 0.60%
///   ≤ 1,470     → 1.20%  |  ≤ 14,740 → 0.95%  |  ≤ 58,940 → 0.52%
///   ≤ 2,460     → 1.15%  |  ≤ 19,650 → 0.90%  |  ≤ 63,860 → 0.50%
///   ≤ 3,440     → 1.10%  |  ≤ 24,560 → 0.85%  |  ≤ 68,770 → 0.47%
///   ≤ 4,420     → 1.05%  |  ≤ 29,470 → 0.80%  |  ≤ 73,681 → 0.45%
///                         |  ≤ 34,380 → 0.75%  |  ≤ 78,590 → 0.42%
///                         |  ≤ 39,300 → 0.70%  |  ≤ 83,500 → 0.40%
///                         |  ≤ 44,210 → 0.65%  |  ≤ 88,400 → 0.37%
///                         |                     |  ≤ 93,330 → 0.35%
///                         |                     |  ≤ 98,240 → 0.32%
///                         |                     |  > 98,240 → 0.30%
///
///
/// ─── 1B. METEORA DLMM (Discrete Liquidity Bins) ──────────────────────────────
///
/// State variables:
///   active_id  = current active bin index
///   bin_step   = tick size in basis points (1–400 bps)
///   bins[i]    = { amount_x, amount_y } for each bin
///   f_dlmm    = total_fee_rate / FEE_PRECISION  (base_fee + variable_fee)
///
/// Bin price (Q64.64 fixed-point):
///   price_i = (1 + bin_step / 10000) ^ active_id
///
/// Single-bin swap (swap_for_y = true, i.e., input = base X, output = quote Y):
///   fee = ceil(amount_in * total_fee_rate / (FEE_PRECISION - total_fee_rate))
///   amount_in_after_fee = amount_in - fee
///   amount_out = floor(price * amount_in_after_fee / 2^64)
///
/// Single-bin swap (swap_for_y = false, i.e., input = quote Y, output = base X):
///   fee = ceil(amount_in * total_fee_rate / (FEE_PRECISION - total_fee_rate))
///   amount_in_after_fee = amount_in - fee
///   amount_out = floor(amount_in_after_fee * 2^64 / price)
///
/// Multi-bin quote_exact_in:
///   The swap iterates through bins consuming liquidity:
///   amount_left = amount_in
///   total_out = 0
///   while amount_left > 0:
///       cap = max capacity of current bin
///       consumed = min(amount_left, cap + fee(cap))
///       fee = extract fee from consumed
///       out = convert (consumed - fee) at bin price
///       total_out += out
///       amount_left -= consumed
///       move to next bin if amount_left > 0
///
/// IMPORTANT: For small amounts that stay within a single active bin, DLMM
/// behaves approximately linearly:
///
///   amount_out ≈ amount_in * P_dlmm * f_dlmm
///
/// where P_dlmm is the active bin price and f_dlmm = (1 - fee_rate).
/// This linear approximation is valid as long as the amount doesn't
/// exhaust the active bin's liquidity.
///
///
/// ═══════════════════════════════════════════════════════════════════════════════
/// 2. ARBITRAGE OPPORTUNITY DETECTION
/// ═══════════════════════════════════════════════════════════════════════════════
///
/// For a token pair BASE/QUOTE (e.g., MEMECOIN/SOL) listed on both DLMM and
/// Pump AMM, there are two possible arbitrage directions:
///
/// ─── Direction 1: Buy on Pump, Sell on DLMM ──────────────────────────────────
///
///   Path: SOL → [Pump AMM buy base] → MEMECOIN → [DLMM sell base] → SOL
///
///   Condition (simplified, ignoring multi-bin):
///     P_pump_buy_effective < P_dlmm_sell_effective
///
///   where:
///     P_pump_buy_effective = Y / (X * f_pump)       (SOL cost per MEMECOIN on Pump)
///     P_dlmm_sell_effective = P_dlmm * f_dlmm       (SOL received per MEMECOIN on DLMM)
///
///   Profitable when:
///     P_dlmm * f_dlmm * X * f_pump / Y > 1
///
///   Or equivalently using the edge model:
///     scaled_price_pump_buy * scaled_price_dlmm_sell > PRICE_SCALE^2
///
/// ─── Direction 2: Buy on DLMM, Sell on Pump ──────────────────────────────────
///
///   Path: SOL → [DLMM buy base] → MEMECOIN → [Pump AMM sell base] → SOL
///
///   Condition:
///     P_dlmm_buy_effective < P_pump_sell_effective
///
///   where:
///     P_dlmm_buy_effective = 1 / (P_dlmm_inv * f_dlmm)   (SOL cost per MEMECOIN on DLMM)
///     P_pump_sell_effective = P_pump * f_pump              (SOL received per MEMECOIN on Pump)
///
///
/// ═══════════════════════════════════════════════════════════════════════════════
/// 3. PROFIT FUNCTION
/// ═══════════════════════════════════════════════════════════════════════════════
///
/// ─── Direction 1: Pump buy → DLMM sell ───────────────────────────────────────
///
///   Given input Δy (SOL), the profit is:
///
///     mid = X * Δy * f_pump / (Y + Δy * f_pump)          (MEMECOIN from Pump)
///     out = DLMM_quote_exact_in(mid, swap_for_y=true)     (SOL from DLMM)
///     profit = out - Δy
///
///   For small amounts (single-bin DLMM approximation):
///     mid = X * Δy * f_pump / (Y + Δy * f_pump)
///     out ≈ mid * P_dlmm * f_dlmm
///     profit ≈ X * Δy * f_pump * P_dlmm * f_dlmm / (Y + Δy * f_pump) - Δy
///
/// ─── Direction 2: DLMM buy → Pump sell ───────────────────────────────────────
///
///   Given input Δy (SOL), the profit is:
///
///     mid = DLMM_quote_exact_in(Δy, swap_for_y=false)    (MEMECOIN from DLMM)
///     out_raw = Y * mid / (X + mid)                       (SOL raw from Pump)
///     out = out_raw * f_pump                               (SOL after Pump fee)
///     profit = out - Δy
///
///   For small amounts (single-bin DLMM approximation):
///     mid ≈ Δy * P_dlmm_inv * f_dlmm
///     out_raw = Y * mid / (X + mid)
///     out = out_raw * f_pump
///     profit ≈ Y * Δy * P_dlmm_inv * f_dlmm * f_pump / (X + Δy * P_dlmm_inv * f_dlmm) - Δy
///
///
/// ═══════════════════════════════════════════════════════════════════════════════
/// 4. OPTIMAL AMOUNT IN (Closed-Form for Single-Bin Approximation)
/// ═══════════════════════════════════════════════════════════════════════════════
///
/// The profit function is concave (unimodal), so we can find the maximum
/// analytically by taking the derivative and setting it to zero.
///
/// ─── Direction 1: Pump first (buy base) → DLMM second (sell base) ────────────
///
///   profit(Δy) = X * Δy * f_pump * P_dlmm * f_dlmm / (Y + Δy * f_pump) - Δy
///
///   Let: A = f_pump, B = P_dlmm * f_dlmm, k = X * Y
///
///   d(profit)/d(Δy) = k * A * B / (Y + Δy * A)^2 - 1 = 0
///
///   Solving:  (Y + Δy* * A)^2 = k * A * B
///             Y + Δy* * A = √(k * A * B)
///             Δy* = (√(X * Y * f_pump * P_dlmm * f_dlmm) - Y) / f_pump
///
///   ┌──────────────────────────────────────────────────────────────────────┐
///   │                                                                      │
///   │   Δy* = (√(X · Y · f_pump · P_dlmm · f_dlmm) − Y) / f_pump        │
///   │                                                                      │
///   │   where X = pump base reserve, Y = pump quote reserve               │
///   │         f_pump = 1 - pump_fee_rate                                  │
///   │         P_dlmm = DLMM price (base→quote direction)                 │
///   │         f_dlmm = 1 - dlmm_fee_rate                                 │
///   │                                                                      │
///   └──────────────────────────────────────────────────────────────────────┘
///
///   Special case (Pump selling base, i.e., input is base token into Pump):
///   When the Pump AMM leg inputs base (fee on output instead of input):
///
///     Δy* = √(X · Y · f_pump · P_dlmm · f_dlmm) − X
///
///   (no division by f_pump because fee is on output, not input)
///
///
/// ─── Direction 2: DLMM first (buy base) → Pump second (sell base) ────────────
///
///   profit(Δy) ≈ Y * Δy * P_dlmm_inv * f_dlmm * f_pump / (X + Δy * P_dlmm_inv * f_dlmm) - Δy
///
///   Let: a = P_dlmm_inv * f_dlmm (DLMM effective rate), B = f_pump
///
///   d(profit)/d(Δy) = X * Y * a * B / (X + Δy * a)^2 - 1 = 0
///
///   Solving:  (X + Δy* * a)^2 = X * Y * a * B
///             X + Δy* * a = √(X * Y * a * B)
///
///   ┌──────────────────────────────────────────────────────────────────────┐
///   │                                                                      │
///   │   Δy* = (√(X · Y · a · f_pump) − X) / a                            │
///   │                                                                      │
///   │   where a = P_dlmm_inv · f_dlmm                                    │
///   │         X = pump base reserve, Y = pump quote reserve               │
///   │         f_pump = 1 - pump_fee_rate                                  │
///   │                                                                      │
///   └──────────────────────────────────────────────────────────────────────┘
///
///   Special case (Pump selling base, i.e., Pump input is base token):
///
///     Δy* = (√(X · Y · a · f_pump) − X) / a
///     (same formula, but a uses the DLMM price for the appropriate direction)
///
///
/// ═══════════════════════════════════════════════════════════════════════════════
/// 5. PIECEWISE-LINEAR MULTI-BIN: EXACT ANALYTICAL SOLUTION (NO ITERATION)
/// ═══════════════════════════════════════════════════════════════════════════════
///
/// KEY INSIGHT: DLMM is NOT a curve — it's a staircase of constant-price
/// limit orders. Each bin converts at a fixed price. The output function
/// G(input) is therefore PIECEWISE LINEAR:
///
///   G(Δ) = Σ min(Δ - consumed_so_far, capacity_k) * slope_k
///
/// where slope_k = price_k * f_dlmm (for swap_for_y=true)
///    or slope_k = (1/price_k) * f_dlmm (for swap_for_y=false)
///
/// Since slopes decrease as we move to worse bins (prices get worse),
/// G(Δ) is CONCAVE and piecewise linear. Combined with the concave Pump
/// output, the total profit function is still concave — but now we can
/// solve it EXACTLY per segment without any iterative search.
///
/// ─── 5A. Building the Bin Segment Table ──────────────────────────────────────
///
/// Walk the bins ONCE (same as `sum_bin_array_raw` already does at init) and
/// extract for each non-empty bin in swap order:
///
///   struct BinSegment {
///       cum_in:  f64,   // cumulative DLMM input consumed by all prior bins
///       cum_out: f64,   // cumulative DLMM output from all prior bins
///       slope:   f64,   // output per unit input in THIS bin (after fee)
///       cap_in:  f64,   // max additional input this bin can absorb
///   }
///
/// For swap_for_y = true (input = base X, output = quote Y), traversing
/// bins in DECREASING id order starting from active_id:
///
///   price_k = (1 + bin_step/10000)^bin_id_k   (in f64)
///   slope_k = price_k * f_dlmm
///   cap_out_k = bin[k].amount_y               (available Y liquidity)
///   cap_in_k  = cap_out_k / price_k           (how much X input fills this bin)
///
/// For swap_for_y = false (input = quote Y, output = base X), traversing
/// bins in INCREASING id order:
///
///   slope_k = (1.0 / price_k) * f_dlmm
///   cap_out_k = bin[k].amount_x               (available X liquidity)
///   cap_in_k  = cap_out_k * price_k           (how much Y input fills this bin)
///
/// Total bins to walk: only the non-empty ones in 2 bin arrays (max ~140
/// bins, typically 2-10 with liquidity). This is O(bins) ONCE.
///
///
/// ─── 5B. Analytical Optimal Per Segment ──────────────────────────────────────
///
/// *** DIRECTION 1: Pump buy → DLMM sell ***
///
/// The mid token entering DLMM at segment k:
///   mid(Δy) = X * Δy * f_pump / (Y + Δy * f_pump)
///
/// DLMM output on segment k:
///   out_k(Δy) = cum_out_k + slope_k * (mid(Δy) - cum_in_k)
///
/// Profit on segment k:
///   π_k(Δy) = cum_out_k + slope_k * (mid(Δy) - cum_in_k) - Δy
///
/// Setting dπ_k/dΔy = 0:
///   slope_k * d(mid)/d(Δy) = 1
///   slope_k * X * Y * f_pump / (Y + Δy * f_pump)² = 1
///
/// ┌───────────────────────────────────────────────────────────────────────┐
/// │                                                                       │
/// │   Δy*_k = (√(X · Y · f_pump · slope_k) − Y) / f_pump               │
/// │                                                                       │
/// │   Same formula as single-bin, just with slope_k for each bin!        │
/// │                                                                       │
/// │   Valid when: cum_in_k ≤ mid(Δy*_k) ≤ cum_in_k + cap_in_k          │
/// │                                                                       │
/// └───────────────────────────────────────────────────────────────────────┘
///
/// *** DIRECTION 2: DLMM buy → Pump sell ***
///
/// DLMM output (mid tokens) on segment k is linear in Δy:
///   mid_k(Δy) = cum_mid_k + slope_k * (Δy - cum_dy_k)
///
/// Pump sell output:
///   out(mid) = Y * mid / (X + mid) * f_pump
///
/// Setting derivative to 0, with m = mid_k(Δy):
///   slope_k * X * Y * f_pump / (X + m)² = 1
///   (X + m)² = slope_k * X * Y * f_pump
///
/// ┌───────────────────────────────────────────────────────────────────────┐
/// │                                                                       │
/// │   m*_k  = √(slope_k · X · Y · f_pump) − X                          │
/// │   Δy*_k = cum_dy_k + (m*_k − cum_mid_k) / slope_k                  │
/// │                                                                       │
/// │   Valid when: cum_dy_k ≤ Δy*_k ≤ cum_dy_k + cap_in_k               │
/// │                                                                       │
/// └───────────────────────────────────────────────────────────────────────┘
///
///
/// ─── 5C. The Complete Algorithm ──────────────────────────────────────────────
///
///   1. Extract bin segments from the 2 bin arrays (once, O(bins))
///
///   2. For each segment k:
///      a. Compute Δy*_k using the per-segment formula above
///      b. Compute mid(Δy*_k) and check it falls within [cum_in_k, cum_in_k + cap_in_k]
///      c. If valid, evaluate profit at Δy*_k
///      d. If Δy*_k falls OUTSIDE the segment, the optimal on this segment
///         is at the nearest boundary — evaluate at both segment endpoints
///
///   3. Return the Δy with maximum profit across all segments
///
/// Complexity: O(num_non_empty_bins) — typically 2-10 evaluations.
/// No golden section search. No quote_exact_in calls. Pure closed-form.
///
///
/// ─── 5D. Why This Is Exact ───────────────────────────────────────────────────
///
/// The profit function π(Δy) is the composition of:
///   - Pump output mid(Δy): concave, smooth (constant product)
///   - DLMM output G(mid): concave, piecewise linear
///   - Minus identity: -Δy (linear)
///
/// The composition of concave functions where the inner is smooth and the
/// outer is piecewise linear gives a piecewise concave function. On each
/// linear segment of G, the profit is smooth and concave (single critical
/// point). The global maximum is either at one of these per-segment optima
/// or at a segment boundary (kink point).
///
/// By evaluating all segment optima + boundaries, we find the EXACT global
/// maximum — same result as `quote_exact_in` would give, but without
/// ever calling it.
///
///
/// ─── 5E. Bin Price Computation ───────────────────────────────────────────────
///
/// To get per-bin prices without Q64 math (for the segment table), use:
///
///   price_base = 1.0 + bin_step as f64 / 10000.0
///
/// Then incrementally from the active bin:
///   For swap_for_y (decreasing bin_id):  price_{k+1} = price_k / price_base
///   For !swap_for_y (increasing bin_id): price_{k+1} = price_k * price_base
///
/// Active bin price is already cached as `self.price` in MeteoraDlmm.
///
///
/// ─── 5F. Implementation in Existing Codebase ─────────────────────────────────
///
/// In `MeteoraDlmm::new()`, the code already iterates bins in
/// `sum_bin_array_raw()` to compute buy_max/sell_max. The segment table
/// can be built in the SAME loop — just additionally store:
///   - per-bin amount_x, amount_y (already read)
///   - per-bin price (incremental from active bin price)
///
/// Store as: `pub buy_segments: Vec<BinSegment>` and
///           `pub sell_segments: Vec<BinSegment>` in MeteoraDlmm.
///
/// Then in `analytical_hint_amm_dlmm()`, instead of computing a single
/// Δy* with one price, iterate the segments and pick the best.
/// This completely replaces the hybrid_search fallback.
///
///
/// ═══════════════════════════════════════════════════════════════════════════════
/// 6. MAXIMUM PROFIT AT OPTIMAL
/// ═══════════════════════════════════════════════════════════════════════════════
///
/// Direction 1 (Pump buy → DLMM sell), at Δy*:
///
///   profit_max = X * Δy* * f_pump * P_dlmm * f_dlmm / (Y + Δy* * f_pump) - Δy*
///
///   Substituting Δy* = (√(k · f_pump · P_dlmm · f_dlmm) − Y) / f_pump:
///
///   profit_max = (√(X · Y · f_pump · P_dlmm · f_dlmm) − Y)^2 / (Y · f_pump)
///
///   Quick profitability check (no sqrt needed):
///     profitable ⟺ X · Y · f_pump · P_dlmm · f_dlmm > Y^2
///                 ⟺ X · f_pump · P_dlmm · f_dlmm > Y
///                 ⟺ X · f_pump · P_dlmm · f_dlmm / Y > 1
///
///   This is the "effective price ratio": if > 1, there's profit.
///
///
/// ═══════════════════════════════════════════════════════════════════════════════
/// 7. IMPLEMENTATION STRATEGY (How to integrate into existing codebase)
/// ═══════════════════════════════════════════════════════════════════════════════
///
/// The existing codebase already has all the infrastructure needed. Here's how
/// the DLMM <-> Pump AMM arbitrage fits in:
///
/// A) PATH DETECTION (in simple_method.rs / optimized_method.rs):
///    - Cross-arbitrage (2-hop) naturally discovers DLMM <-> Pump pairs
///    - Edge pairs where edge1.program = PumpAmm and edge2.program = MeteoraDLMM
///      (or vice versa) on the same token pair
///
/// B) OPTIMAL AMOUNT (in optimal_amount_in_v2.rs):
///    - `analytical_hint_amm_dlmm()` already handles Pump AMM + DLMM pairs
///    - It computes the closed-form Δy* from Section 4 above
///    - Falls back to hybrid_search (grid + golden section) when:
///      * The hint is None (e.g., both are DLMM)
///      * The mid_amount exceeds active bin capacity
///
/// C) EXECUTION (via ProgramMeta trait):
///    - Both MeteoraDlmm and PumpAmm implement ProgramMeta
///    - swap_base_in / invoke_swap_base_in handle the actual swaps
///    - The arbitrage executor calls them in sequence
///
/// The formulas in Section 4 are exactly what `analytical_hint_amm_dlmm`
/// computes. The key insight is the mapping:
///
///   r_in  = pump reserve on input side
///   r_out = pump reserve on output side
///   k     = r_in * r_out
///   f_amm = f_pump (pump fee factor)
///   p_dlmm = DLMM active bin price (in the swap direction)
///   f_dlmm = DLMM fee factor
///
///   Standard:    Δy* = (√(k · f_amm · p_dlmm · f_dlmm) − r_in) / f_amm
///   Pump sell:   Δy* = √(k · f_amm · p_dlmm · f_dlmm) − r_in
///   DLMM first:  Δy* = (√(k · b · f_amm) − r_in) / b   where b = p_dlmm · f_dlmm
///
///
/// ═══════════════════════════════════════════════════════════════════════════════
/// 8. NUMERICAL EXAMPLE
/// ═══════════════════════════════════════════════════════════════════════════════
///
/// Pool state:
///   Pump AMM: X = 500,000,000 MEME (6 dec), Y = 50 SOL (9 dec = 50_000_000_000)
///   DLMM: P_dlmm = 0.00012 SOL/MEME, f_dlmm = 0.997 (0.3% fee)
///   Pump fee: market_cap ≈ 50 SOL → fee = 1.25%, f_pump = 0.9875
///
/// Check profitability:
///   X * f_pump * P_dlmm * f_dlmm / Y
///   = 500e6 * 0.9875 * 0.00012 * 0.997 / 50e9
///   = 59,221.5 / 50e9
///   = 0.001184  ← NOT profitable (< 1)
///
/// If P_dlmm were 0.00015 and Y = 40 SOL:
///   = 500e6 * 0.9875 * 0.00015 * 0.997 / 40e9
///   = 73,905.9 / 40e9
///   = 0.001848  ← Still not profitable
///
/// Typical profitable scenario (price dislocation):
///   Pump: X = 1,000,000 (6 dec), Y = 100 SOL
///   P_pump = 100 / 1,000,000 = 0.0001 SOL/MEME
///   DLMM:  P_dlmm = 0.00015 SOL/MEME (50% higher!)
///
///   X * f_pump * P_dlmm * f_dlmm / Y
///   = 1,000,000 * 0.9875 * 0.00015 * 0.997 / 100
///   = 147.7 / 100 = 1.477  ← PROFITABLE
///
///   Optimal Δy* = (√(1e6 * 100 * 0.9875 * 0.00015 * 0.997) − 100) / 0.9875
///               = (√(14,770.3) − 100) / 0.9875
///               = (121.5 − 100) / 0.9875
///               = 21.8 SOL
///
///   Expected profit ≈ (√(k·f·P·f) − Y)^2 / (Y · f_pump)
///                    = (21.5)^2 / (100 * 0.9875)
///                    ≈ 4.68 SOL

// ============================================================================
// Below is the implementation code that can be compiled and used
// ============================================================================

/// Compute the Pump AMM output for a given input (constant product).
///
/// - `is_buy`: true if buying base (input=quote), false if selling base (input=base)
/// - `reserve_in`: reserve on the input side
/// - `reserve_out`: reserve on the output side
/// - `amount_in`: input amount
/// - `fee_factor`: 1.0 - fee_rate
///
/// Returns the output amount after all fees.
pub fn pump_amm_swap(
    is_buy: bool,
    reserve_in: f64,
    reserve_out: f64,
    amount_in: f64,
    fee_factor: f64,
) -> f64 {
    if is_buy {
        // Fee on input
        let effective_in = amount_in * fee_factor;
        reserve_out * effective_in / (reserve_in + effective_in)
    } else {
        // Fee on output
        let raw_out = reserve_out * amount_in / (reserve_in + amount_in);
        raw_out * fee_factor
    }
}

/// Approximate DLMM output for small amounts (single active bin).
///
/// - `amount_in`: input amount
/// - `price`: active bin price in the swap direction
/// - `fee_factor`: 1.0 - fee_rate
pub fn dlmm_single_bin_swap(amount_in: f64, price: f64, fee_factor: f64) -> f64 {
    amount_in * price * fee_factor
}

/// Check if an arbitrage opportunity exists (no sqrt needed).
///
/// Direction 1: Pump buy → DLMM sell
///   pump_base_reserve * f_pump * p_dlmm_sell * f_dlmm > pump_quote_reserve
///
/// Direction 2: DLMM buy → Pump sell
///   pump_quote_reserve * f_pump * p_dlmm_buy_inv * f_dlmm > pump_base_reserve
///
/// Returns (dir1_profitable, dir2_profitable, dir1_ratio, dir2_ratio)
pub fn check_arbitrage_opportunity(
    pump_base_reserve: f64,   // X
    pump_quote_reserve: f64,  // Y
    f_pump: f64,              // 1 - pump_fee_rate
    p_dlmm_base_to_quote: f64,  // DLMM price: base→quote
    p_dlmm_quote_to_base: f64,  // DLMM price: quote→base (inverse)
    f_dlmm: f64,             // 1 - dlmm_fee_rate
) -> (bool, bool, f64, f64) {
    // Direction 1: Buy base on Pump (input=quote), sell base on DLMM (swap_for_y=true)
    // Pump sells MEMECOIN for SOL, DLMM buys MEMECOIN for SOL
    let ratio_1 = pump_base_reserve * f_pump * p_dlmm_base_to_quote * f_dlmm
        / pump_quote_reserve;

    // Direction 2: Buy base on DLMM (swap_for_y=false), sell base on Pump (input=base)
    // DLMM sells MEMECOIN for SOL, Pump buys MEMECOIN for SOL
    let ratio_2 = pump_quote_reserve * f_pump * p_dlmm_quote_to_base * f_dlmm
        / pump_base_reserve;

    (ratio_1 > 1.0, ratio_2 > 1.0, ratio_1, ratio_2)
}

/// Closed-form optimal input for Direction 1: Pump buy → DLMM sell.
///
/// Δy* = (√(X · Y · f_pump · P_dlmm · f_dlmm) − Y) / f_pump
///
/// Returns None if not profitable.
pub fn optimal_amount_pump_buy_dlmm_sell(
    pump_base_reserve: f64,       // X
    pump_quote_reserve: f64,      // Y
    f_pump: f64,                  // 1 - pump_fee_rate
    p_dlmm_base_to_quote: f64,   // DLMM active bin price (base → quote)
    f_dlmm: f64,                 // 1 - dlmm_fee_rate
) -> Option<f64> {
    let k = pump_base_reserve * pump_quote_reserve;
    let discriminant = k * f_pump * p_dlmm_base_to_quote * f_dlmm;

    if discriminant <= pump_quote_reserve * pump_quote_reserve {
        return None; // Not profitable
    }

    let sqrt_disc = discriminant.sqrt();
    let optimal = (sqrt_disc - pump_quote_reserve) / f_pump;

    if optimal <= 0.0 || !optimal.is_finite() {
        None
    } else {
        Some(optimal)
    }
}

/// Closed-form optimal input for Direction 2: DLMM buy → Pump sell.
///
/// Δy* = (√(X · Y · a · f_pump) − X) / a
/// where a = P_dlmm_inv · f_dlmm (DLMM effective conversion rate quote→base)
///
/// Returns None if not profitable.
pub fn optimal_amount_dlmm_buy_pump_sell(
    pump_base_reserve: f64,       // X
    pump_quote_reserve: f64,      // Y
    f_pump: f64,                  // 1 - pump_fee_rate
    p_dlmm_quote_to_base: f64,   // DLMM inverse price (quote → base)
    f_dlmm: f64,                 // 1 - dlmm_fee_rate
) -> Option<f64> {
    let a = p_dlmm_quote_to_base * f_dlmm;
    if a <= 0.0 { return None; }

    let k = pump_base_reserve * pump_quote_reserve;
    let discriminant = k * a * f_pump;

    if discriminant <= pump_base_reserve * pump_base_reserve {
        return None; // Not profitable
    }

    let sqrt_disc = discriminant.sqrt();
    let optimal = (sqrt_disc - pump_base_reserve) / a;

    if optimal <= 0.0 || !optimal.is_finite() {
        None
    } else {
        Some(optimal)
    }
}

/// Compute the expected profit at a given input amount.
///
/// Direction 1: Pump buy → DLMM sell
/// Direction 2: DLMM buy → Pump sell
///
/// `is_direction_1`: true = Pump first, false = DLMM first
/// `amount_in`: SOL input amount
///
/// Uses single-bin DLMM approximation.
pub fn estimate_profit(
    is_direction_1: bool,
    amount_in: f64,
    pump_base_reserve: f64,
    pump_quote_reserve: f64,
    f_pump: f64,
    p_dlmm: f64,          // DLMM price in the swap direction
    f_dlmm: f64,
) -> f64 {
    if is_direction_1 {
        // Pump buy → DLMM sell
        let mid = pump_amm_swap(true, pump_quote_reserve, pump_base_reserve, amount_in, f_pump);
        let out = dlmm_single_bin_swap(mid, p_dlmm, f_dlmm);
        out - amount_in
    } else {
        // DLMM buy → Pump sell
        let mid = dlmm_single_bin_swap(amount_in, p_dlmm, f_dlmm);
        let out = pump_amm_swap(false, pump_base_reserve, pump_quote_reserve, mid, f_pump);
        out - amount_in
    }
}

/// Golden section search refinement for cases where single-bin approximation
/// is insufficient (large trades spanning multiple DLMM bins).
///
/// `eval_fn`: closure that takes amount_in (u64) and returns profit (i128)
/// `min_amount`: lower bound of search
/// `max_amount`: upper bound of search
///
/// Returns (optimal_amount, profit)
pub fn golden_section_refine<F>(
    eval_fn: F,
    min_amount: u64,
    max_amount: u64,
) -> (u64, i128)
where
    F: Fn(u64) -> i128,
{
    const PHI: f64 = 1.618033988749895;
    const MAX_ITER: usize = 12;
    const CONVERGENCE: u64 = 10_000;

    let mut a = min_amount;
    let mut b = max_amount;

    let mut c = b - ((b - a) as f64 / PHI) as u64;
    let mut d = a + ((b - a) as f64 / PHI) as u64;
    let mut fc = eval_fn(c);
    let mut fd = eval_fn(d);

    for _ in 0..MAX_ITER {
        if b - a < CONVERGENCE {
            break;
        }
        if fc > fd {
            b = d;
            d = c;
            fd = fc;
            c = b - ((b - a) as f64 / PHI) as u64;
            fc = eval_fn(c);
        } else {
            a = c;
            c = d;
            fc = fd;
            d = a + ((b - a) as f64 / PHI) as u64;
            fd = eval_fn(d);
        }
    }

    let optimal = (a + b) / 2;
    let profit = eval_fn(optimal);
    (optimal, profit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profitability_check() {
        // Pump: X=1,000,000 MEME, Y=100 SOL
        // DLMM: P=0.00015 SOL/MEME (50% higher than Pump's 0.0001)
        let (d1, d2, r1, r2) = check_arbitrage_opportunity(
            1_000_000.0,  // X
            100.0,        // Y
            0.9875,       // f_pump (1.25% fee)
            0.00015,      // P_dlmm base→quote
            1.0 / 0.00015, // P_dlmm quote→base
            0.997,        // f_dlmm (0.3% fee)
        );
        assert!(d1, "Direction 1 should be profitable, ratio={}", r1);
        assert!(!d2, "Direction 2 should not be profitable, ratio={}", r2);
    }

    #[test]
    fn test_optimal_amount_direction_1() {
        let opt = optimal_amount_pump_buy_dlmm_sell(
            1_000_000.0,  // X
            100.0,        // Y
            0.9875,       // f_pump
            0.00015,      // P_dlmm
            0.997,        // f_dlmm
        );
        assert!(opt.is_some());
        let amount = opt.unwrap();
        // Should be around 21.8 SOL based on our example
        assert!(amount > 15.0 && amount < 30.0, "optimal={}", amount);
    }

    #[test]
    fn test_profit_at_optimal() {
        let opt = optimal_amount_pump_buy_dlmm_sell(
            1_000_000.0, 100.0, 0.9875, 0.00015, 0.997,
        ).unwrap();

        let profit = estimate_profit(
            true, opt,
            1_000_000.0, 100.0, 0.9875, 0.00015, 0.997,
        );
        assert!(profit > 0.0, "Profit should be positive at optimal, got {}", profit);

        // Profit should be less at half and double the optimal
        let profit_half = estimate_profit(
            true, opt / 2.0,
            1_000_000.0, 100.0, 0.9875, 0.00015, 0.997,
        );
        let profit_double = estimate_profit(
            true, opt * 2.0,
            1_000_000.0, 100.0, 0.9875, 0.00015, 0.997,
        );
        assert!(profit > profit_half, "Optimal should beat half");
        assert!(profit > profit_double, "Optimal should beat double");
    }

    #[test]
    fn test_no_arb_when_prices_aligned() {
        // Same effective price on both → no arb
        let (d1, d2, _, _) = check_arbitrage_opportunity(
            1_000_000.0,
            100.0,
            0.9875,
            0.0001,   // P_dlmm = Pump price exactly
            10_000.0,
            0.997,
        );
        assert!(!d1, "Direction 1 should not be profitable");
        assert!(!d2, "Direction 2 should not be profitable");
    }

    #[test]
    fn test_golden_section_refine() {
        // Simple quadratic profit: -(x - 500)^2 + 10000
        let (opt, profit) = golden_section_refine(
            |x| -(x as i128 - 500).pow(2) + 10000,
            0,
            1000,
        );
        assert!((opt as i64 - 500).abs() < 100, "Should find ~500, got {}", opt);
        assert!(profit > 9000, "Profit should be near 10000, got {}", profit);
    }
}
