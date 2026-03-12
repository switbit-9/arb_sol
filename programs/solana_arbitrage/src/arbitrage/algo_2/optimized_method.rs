use crate::arbitrage::base::Edge;
use crate::programs::{ProgramInstance, ProgramMeta};
use crate::utils::bot_config::BotConfig;
use anchor_lang::prelude::*;

/// Sorted array of (pool_id, index) pairs for O(log n) lookups.
/// Replaces HashMap to avoid SipHash overhead in Solana's BPF/SBF runtime.
struct PoolIndex(Vec<(Pubkey, usize)>);

impl PoolIndex {
    /// Build the index by collecting (pool_id, original_index) pairs and sorting by pool_id.
    fn build(instances: &[ProgramInstance]) -> Self {
        let mut entries: Vec<(Pubkey, usize)> = instances
            .iter()
            .enumerate()
            .map(|(i, inst)| (*inst.get_pool_id(), i))
            .collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Self(entries)
    }

    /// Binary search for pool_id; returns the original index into `instances`.
    #[inline]
    fn find(&self, pool_id: &Pubkey) -> Option<usize> {
        let idx = self.0.partition_point(|(pid, _)| *pid < *pool_id);
        if idx < self.0.len() && self.0[idx].0 == *pool_id {
            Some(self.0[idx].1)
        } else {
            None
        }
    }
}

/// Simulate a swap on a single edge using the pool's own math (fast_quote).
/// 1. Look up the pool instance via binary search on pool_id.
/// 2. Call the program-specific fast_quote which returns (actual_in, amount_out).
///    Programs may clamp actual_in to their own max swap limits.
/// profit_pct is the arb cycle's profit fraction — DLMM uses it for bin-crossing decisions.
/// Returns (0, 0) if the pool is not found or the quote fails.
#[inline]
fn edge_fast_quote(
    edge: &Edge,
    amount_in: u64,
    instances: &mut [ProgramInstance],
    pool_index: &PoolIndex,
    profit_pct: f64,
) -> (u64, u64) {
    pool_index
        .find(&edge.pool_id)
        .and_then(|idx| instances[idx].fast_quote(edge.left.mint_account, amount_in, profit_pct).ok())
        .unwrap_or((0, 0))
}

/// Find the top-2 edges by `scaled_price_with_fee` in a single O(n) pass.
/// This limits the inner loop to at most 2x2 = 4 (buy, sell) combos per token pair,
/// keeping the overall algorithm O(E) instead of O(E^2).
/// Returns (best, second_best) — either may be None if not enough edges.
#[inline]
fn top2_by_price<'a>(edges: &[&'a Edge]) -> (Option<&'a Edge>, Option<&'a Edge>) {
    let mut best: Option<&Edge> = None;
    let mut second: Option<&Edge> = None;

    for &edge in edges {
        let score = edge.scaled_price_with_fee;
        match best {
            // Current edge is worse than best — check if it beats second
            Some(b) if score <= b.scaled_price_with_fee => {
                if second.map_or(true, |s| score > s.scaled_price_with_fee) {
                    second = Some(edge);
                }
            }
            // Current edge is new best — demote old best to second
            _ => {
                second = best;
                best = Some(edge);
            }
        }
    }

    (best, second)
}

/// Optimized O(E) check for 2-hop (Cross) Arbitrage.
///
/// Uses per-program `fast_quote` (each program's own simplified swap math)
/// instead of a naive linear model.
///
/// Returns the single best candidate path by estimated profit, if any.
/// The caller should run `find_optimal_amount_in_v2` on it to find the real optimum.
///
/// Path: Root -> Token B -> Root
pub fn find_cross_arbitrage_optimized<'info>(
    edges: &[&Edge],
    instances: &mut [ProgramInstance<'info>],
    config: &mut BotConfig,
) -> Result<Option<(Vec<Edge>, i128, u128)>> {
    let start_token = config.start_token;
    let start_amount = config.max_amount_in;
    let min_profit = config.min_profit;
    debug_eprintln!("[ARB] Start amount: {:.9} SOL", start_amount as f64 / 1_000_000_000.0);

    // ── Index construction ──────────────────────────────────────────────
    // Build a sorted pool index for O(log n) pool_id → instance lookups.
    let pool_index = PoolIndex::build(instances);

    // Will hold the single best arbitrage path found.
    let mut best_candidate: Option<(Vec<Edge>, i128, u128)> = None;

    // Tracking variables for the best price spread seen (for logging, even if unprofitable).
    let mut display_max_profit = i128::MIN;
    let mut best_buy_price = 0.0_f64;
    let mut best_sell_price = 0.0_f64;
    let mut best_buy_fee = 0.0_f64;
    let mut best_sell_fee = 0.0_f64;

    // ── Sort edges by (left_mint, right_mint) ───────────────────────────
    // This enables binary search for "all edges from token A to token B"
    // without allocating per-token HashMaps (avoids SipHash in BPF).
    let mut sorted_edges: Vec<&Edge> = edges.to_vec();
    sorted_edges.sort_unstable_by(|a, b| {
        a.left.mint_account.cmp(&b.left.mint_account)
            .then_with(|| a.right.mint_account.cmp(&b.right.mint_account))
    });

    // ── Determine which tokens to use as the "root" (start/end of the cycle) ──
    // If a specific start_token is configured, only try that one.
    // Otherwise, try every unique source token as a potential root.
    let root_tokens: Vec<Pubkey> = if let Some(token) = start_token {
        vec![token]
    } else {
        let mut tokens: Vec<Pubkey> = sorted_edges.iter().map(|e| e.left.mint_account).collect();
        tokens.dedup();
        tokens
    };

    // ── Main loop: for each root token, find profitable Root → B → Root cycles ──
    for root in &root_tokens {
        // Binary-search sorted_edges for the contiguous block where left_mint == root.
        // These are all possible "buy" edges (root → some token B).
        let root_start = sorted_edges.partition_point(|e| e.left.mint_account < *root);
        let root_end = root_start + sorted_edges[root_start..].partition_point(|e| e.left.mint_account == *root);
        let root_edges = &sorted_edges[root_start..root_end];

        // Walk through root_edges grouped by destination token (contiguous because
        // edges are sorted by (left_mint, right_mint)). No Vec allocation needed.
        let mut gi = 0;
        while gi < root_edges.len() {
            let dest = root_edges[gi].right.mint_account; // "token B"
            // Advance gj to the end of this destination group
            let mut gj = gi + 1;
            while gj < root_edges.len() && root_edges[gj].right.mint_account == dest {
                gj += 1;
            }

            // Skip self-loops (root → root)
            if dest != *root {
                // buy_group: all edges root → token_B (across different pools)
                let buy_group = &root_edges[gi..gj];

                // Find sell edges: token_B → root, using two binary searches:
                //  1. Find all edges where left_mint == dest (token B's outgoing edges)
                let sell_start = sorted_edges.partition_point(|e| e.left.mint_account < dest);
                let sell_group_end = sell_start + sorted_edges[sell_start..].partition_point(|e| e.left.mint_account == dest);
                let token_b_edges = &sorted_edges[sell_start..sell_group_end];
                //  2. Within those, narrow to right_mint == root (edges that return to root)
                let sell_r_start = token_b_edges.partition_point(|e| e.right.mint_account < *root);
                let sell_r_end = sell_r_start + token_b_edges[sell_r_start..].partition_point(|e| e.right.mint_account == *root);
                let sell_group = &token_b_edges[sell_r_start..sell_r_end];

                // Only proceed if there's at least one sell edge back to root
                if !sell_group.is_empty() {
                    // Prune to top-2 by price in each group to cap combos at 2x2 = 4
                    let (buy1, buy2) = top2_by_price(buy_group);
                    let (sell1, sell2) = top2_by_price(sell_group);


                    const PRICE_SCALE_SQ: u128 = 1_000_000_000u128 * 1_000_000_000u128;

                    for buy_opt in [buy1, buy2] {
                        let buy = match buy_opt {
                            Some(e) => e,
                            None => continue,
                        };

                        // Pre-check: does ANY sell edge form a linearly profitable pair?
                        // Avoids the buy fast_quote call entirely when no sell edge can work.
                        let best_sell_scaled = [sell1, sell2].iter().filter_map(|s| {
                            let sell = (*s)?;
                            if sell.pool_id == buy.pool_id { return None; }
                            Some(sell.scaled_price_with_fee)
                        }).max().unwrap_or(0);
                        if buy.scaled_price_with_fee.saturating_mul(best_sell_scaled) <= PRICE_SCALE_SQ {
                            continue;
                        }

                        for sell_opt in [sell1, sell2] {
                            let sell = match sell_opt {
                                Some(e) => e,
                                None => continue,
                            };

                            // Cannot use the same pool for both legs (no self-arbitrage)
                            if buy.pool_id == sell.pool_id {
                                continue;
                            }

                            // Early exit: if linear price model can't profit, real quote can't either
                            // (fast_quote always returns ≤ linear due to slippage)
                            let product = buy.scaled_price_with_fee
                                .saturating_mul(sell.scaled_price_with_fee);
                            if product <= PRICE_SCALE_SQ {
                                continue;
                            }

                            // Cycle profit % from linear prices (e.g. 0.02 = 2%)
                            let profit_pct = (product as f64 / PRICE_SCALE_SQ as f64) - 1.0;

                            #[cfg(any(test, feature = "debug"))]
                            {
                                let linear_profit_bps = product.saturating_sub(PRICE_SCALE_SQ)
                                    .saturating_mul(10_000) / PRICE_SCALE_SQ;
                                debug_eprintln!("");
                                debug_eprintln!("[ARB] Linear profit: {}.{:02}%", linear_profit_bps / 100, linear_profit_bps % 100);
                            }

                            // ── Leg 1 (Buy): swap start_amount of root → token B ──
                            let (buy_actual_in, amount_out) = edge_fast_quote(buy, start_amount, instances, &pool_index, profit_pct);
                            debug_eprintln!(
                                "[ARB] Buy:  {:.9} SOL ({}) -> {:.6} tokens ({})",
                                buy_actual_in as f64 / 1_000_000_000.0, buy_actual_in,
                                amount_out as f64 / 1_000_000.0, amount_out
                            );
                            if buy_actual_in == 0 || amount_out == 0 {
                                continue;
                            }
  


                            // ── Leg 2 (Sell): swap amount_b of token B → root ──
                            // The sell pool may clamp amount_b down to its own max.
                            let (sell_actual_in, final_out) = edge_fast_quote(sell, amount_out, instances, &pool_index, profit_pct);
                            debug_eprintln!(
                                "[ARB] Sell: {:.6} tokens ({}) -> {:.9} SOL ({})",
                                sell_actual_in as f64 / 1_000_000.0, sell_actual_in,
                                final_out as f64 / 1_000_000_000.0, final_out
                            );
                            // ── Profit calculation ──
                            // effective_amount = how much root we actually "spent" on the portion
                            // of token B that the sell pool accepted.
                            let mut effective_amount = buy_actual_in as u128;
                            if sell_actual_in < amount_out {
                                // Sell pool couldn't take all of amount_b — re-quote buy leg
                                // with scaled-down input to account for CP curve convexity.
                                // Linear scaling overestimates cost (the first N% of output is
                                // cheaper than N% of input on a CP curve).
                                let scaled_input = (buy_actual_in as u128)
                                    .saturating_mul(sell_actual_in as u128)
                                    / (amount_out as u128);
                                if scaled_input == 0 {
                                    continue;
                                }
                                let (re_actual_in, _) = edge_fast_quote(
                                    buy, scaled_input as u64, instances, &pool_index, profit_pct,
                                );
                                effective_amount = re_actual_in as u128;
                                if effective_amount == 0 {
                                    continue;
                                }
                            }

                            let final_amount = final_out as u128;
                            // profit = what we got back minus what we put in
                            let profit = final_amount as i128 - effective_amount as i128;

                            // Track best price spread for logging (even if unprofitable)
                            if profit > display_max_profit {
                                display_max_profit = profit;
                                // Normalize prices to [0, 1] range for display
                                let p1 = buy.get_price();
                                best_buy_price = if p1 > 1.0 { 1.0 / p1 } else { p1 };
                                let p2 = sell.get_price();
                                best_sell_price = if p2 > 1.0 { 1.0 / p2 } else { p2 };
                                best_buy_fee = buy.fee_factor;
                                best_sell_fee = sell.fee_factor;
                            }

                            // If full-amount fast_quote is unprofitable but the linear
                            // model shows significant spread, still accept the candidate.
                            // The grid search / golden section will find the optimal
                            // (smaller) amount where slippage doesn't eat the spread.
                            if profit < 0 || profit < min_profit {
                                if profit_pct > 0.03 {
                                    // Use a conservative linear estimate as placeholder profit.
                                    // The optimizer will compute the real optimal.
                                    let est_profit = (profit_pct * effective_amount as f64 * 0.25) as i128;
                                    if best_candidate.as_ref().map_or(true, |c| est_profit > c.1) {
                                        let path = vec![(*buy).clone(), (*sell).clone()];
                                        best_candidate = Some((path, est_profit, final_amount));
                                    }
                                }
                                continue;
                            }

                            debug_eprintln!(
                                "[ARB] +++ Profit: {:.9} SOL ({:.4}%) | Effective in: {:.9} SOL",
                                profit as f64 / 1_000_000_000.0,
                                (profit as f64 / effective_amount as f64) * 100.0,
                                effective_amount as f64 / 1_000_000_000.0
                            );
                            debug_eprintln!("");

                            // Keep only the single best candidate
                            if best_candidate.as_ref().map_or(true, |c| profit > c.1) {
                                let path = vec![(*buy).clone(), (*sell).clone()];
                                best_candidate = Some((path, profit, final_amount));
                            }
                        }
                    }
                }
            }

            // Advance to the next destination group
            gi = gj;
        }
    }

    // ── Logging: report the best price spread and fees seen this scan ──
    // Compute % price difference between the best buy and sell edges.
    // This indicates how much "raw" spread exists before fees eat into it.
    let min_price = best_buy_price.min(best_sell_price);
    let max_price = best_buy_price.max(best_sell_price);
    // Assign fees so max_fee corresponds to the higher-priced (sell) side
    let (max_fee, min_fee) = if best_sell_price >= best_buy_price {
        (best_sell_fee, best_buy_fee)
    } else {
        (best_buy_fee, best_sell_fee)
    };
    let price_diff_pct = if min_price > 0.0 {
        ((max_price - min_price) / min_price) * 100.0
    } else {
        0.0
    };
    debug_eprintln!("");
    // msg!(
    //     "P={:.4}, F={:.4}, F={:.4}, {:.4})",
    //     price_diff_pct,
    //     1.0 - max_fee,
    //     1.0 - min_fee,
    //     display_max_profit as f64 / 1_000_000_000.0,
    // );
    debug_eprintln!("");

    #[cfg(any(test, feature = "debug"))]
    {
        if let Some((ref path_edges, profit, final_amount)) = best_candidate {
            debug_eprintln!("");
            debug_eprintln!(
                "Best candidate | profit: {} SOL | in: {} out: {}",
                profit as f64 / 1_000_000_000.0,
                start_amount / 1_000_000_000,
                final_amount / 1_000_000_000,
            );
            for edge in path_edges {
                debug_eprintln!(
                    "  {} -> {} (pool {} @ p={:.6} fee={:.4})",
                    edge.left.mint_account,
                    edge.right.mint_account,
                    edge.pool_id,
                    edge.price,
                    edge.fee_factor,
                );
            }
            debug_eprintln!("");
        } else {
            debug_eprintln!("No profit found");
        }
    }

    Ok(best_candidate)
}
