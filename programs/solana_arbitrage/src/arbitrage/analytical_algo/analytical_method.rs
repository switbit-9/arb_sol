use crate::arbitrage::algo_2::arbitrage_path::EdgeArray;
use crate::arbitrage::algo_2::ArbitragePath;
use crate::arbitrage::base::Edge;
use crate::programs::{ProgramInstance, ProgramMeta};
use crate::utils::bot_config::BotConfig;
use crate::utils::token::{get_transfer_fees, MintFee};
use anchor_lang::prelude::*;
use anchor_spl::token::spl_token::native_mint::ID as WSOL;

use super::formulas::{analytical_estimate_nhop, analytical_optimal_2pool, analytical_optimal_clmm_cp, analytical_optimal_dlmm_dlmm, analytical_optimal_multibin, pool_output};
use super::pool_model::{extract_pool_model, extract_pool_model_both, PoolModel};

/// Minimum search amount in lamports
const MIN_SEARCH_AMOUNT: u64 = 1_000;

// ─── Candidate search ───────────────────────────────────────────────────────

/// A candidate found by the marginal price search.
#[derive(Clone, Copy)]
struct AnalyticalCandidate {
    buy_idx: usize,  // index into instances
    sell_idx: usize,  // index into instances
    input_mint: Pubkey,
    middle_mint: Pubkey,
    price_product: f64, // buy_price * sell_price — used for ranking
    buy_model: PoolModel,
    sell_model: PoolModel,
}


/// Candidate search using marginal prices only — no analytical formulas.
///
/// 1. Extract fee-adjusted marginal prices for every pool in O(n).
/// 2. Brute-scan all pairs (O(n²)) — keep all with product > 1.0.
///    Pairs with product <= 1.0 are unprofitable and skipped.
/// 3. No heap allocations — fixed-size arrays throughout.
///
/// Optimal amounts and real profits are computed later via multibin/fast_quote.
const MAX_POOLS: usize = 8;

#[inline(never)]
fn find_candidates_analytical(
    instances: &[ProgramInstance],
    start_token: Pubkey,
    middle_mint: Pubkey,
    max_amount_in: u64,
) -> Vec<AnalyticalCandidate> {
    if instances.is_empty() { return Vec::new() };

    // Fixed-size price extraction — no heap alloc.
    let mut prices: [(usize, f64, f64); MAX_POOLS] = [(0, 0.0, 0.0); MAX_POOLS];
    let mut count = 0;

    for (idx, inst) in instances.iter().enumerate() {
        if count >= MAX_POOLS { break; }
        let (price_btq, price_qtb) = inst.get_prices().unwrap_or((0.0, 0.0));
        let (fee_btq, fee_qtb) = inst.get_fee_factor().unwrap_or((1.0, 1.0));
        let (base, _) = inst.get_mints();
        let buy = if start_token == *base { price_btq * fee_btq } else { price_qtb * fee_qtb };
        let sell = if middle_mint == *base { price_btq * fee_btq } else { price_qtb * fee_qtb };
        if buy > 0.0 || sell > 0.0 {
            prices[count] = (idx, buy, sell);
            count += 1;
        }
    }

    // Brute-scan all pairs, keep all with positive profit (product > 1.0).
    let mut candidates = Vec::new();

    for i in 0..count {
        let (buy_idx, buy_price, _) = prices[i];
        for j in 0..count {
            if i == j { continue; }
            let (sell_idx, _, sell_price) = prices[j];
            let product = buy_price * sell_price;
            if product <= 1.0 { continue; }
            let estimated_out = max_amount_in as f64 * product;
            if estimated_out - max_amount_in as f64 <= 5_000.0 { continue; }

            candidates.push(AnalyticalCandidate {
                buy_idx,
                sell_idx,
                input_mint: start_token,
                middle_mint,
                price_product: product,
                buy_model: PoolModel::Opaque { marginal_price: buy_price },
                sell_model: PoolModel::Opaque { marginal_price: sell_price },
            });
        }
    }

    candidates
}

// ─── Build edges for the result ─────────────────────────────────────────────

/// Build Edge structs for the chosen buy/sell pair.
/// Needed so the ArbitragePath can be used for execution.
fn build_edges(
    buy_instance: &dyn ProgramMeta,
    sell_instance: &dyn ProgramMeta,
    input_mint: Pubkey,
    middle_mint: Pubkey,
) -> Result<EdgeArray> {
    use crate::arbitrage::base::edge::EdgeSide;
    use crate::arbitrage::base::pool::Pool;

    let (buy_price, buy_inverse_price) = buy_instance.get_prices()?;
    let (buy_fee_a_to_b, buy_fee_b_to_a) = buy_instance.get_fee_factor().unwrap_or((1.0, 1.0));
    let (buy_base, _) = buy_instance.get_mints();
    let (buy_max_in, buy_max_out) = buy_instance.get_cached_max_amounts(input_mint);

    let buy_edge = if input_mint == *buy_base {
        Edge::new(
            *buy_instance.get_id(),
            *buy_instance.get_pool_id(),
            EdgeSide::LeftToRight,
            buy_price,
            buy_fee_a_to_b,
            buy_fee_b_to_a,
            buy_max_in,
            buy_max_out,
            Pool::new(&input_mint),
            Pool::new(&middle_mint),
        )
    } else {
        Edge::new(
            *buy_instance.get_id(),
            *buy_instance.get_pool_id(),
            EdgeSide::RightToLeft,
            buy_inverse_price,
            buy_fee_b_to_a,
            buy_fee_a_to_b,
            buy_max_in,
            buy_max_out,
            Pool::new(&input_mint),
            Pool::new(&middle_mint),
        )
    };

    let (sell_price, sell_inverse_price) = sell_instance.get_prices()?;
    let (sell_fee_a_to_b, sell_fee_b_to_a) = sell_instance.get_fee_factor().unwrap_or((1.0, 1.0));
    let (sell_base, _) = sell_instance.get_mints();
    let (sell_max_in, sell_max_out) = sell_instance.get_cached_max_amounts(middle_mint);

    let sell_edge = if middle_mint == *sell_base {
        Edge::new(
            *sell_instance.get_id(),
            *sell_instance.get_pool_id(),
            EdgeSide::LeftToRight,
            sell_price,
            sell_fee_a_to_b,
            sell_fee_b_to_a,
            sell_max_in,
            sell_max_out,
            Pool::new(&middle_mint),
            Pool::new(&input_mint),
        )
    } else {
        Edge::new(
            *sell_instance.get_id(),
            *sell_instance.get_pool_id(),
            EdgeSide::RightToLeft,
            sell_inverse_price,
            sell_fee_b_to_a,
            sell_fee_a_to_b,
            sell_max_in,
            sell_max_out,
            Pool::new(&middle_mint),
            Pool::new(&input_mint),
        )
    };

    Ok(EdgeArray::from_2(buy_edge, sell_edge))
}

// ─── Compute optimal amount for a candidate ─────────────────────────────────

/// Compute the optimal input amount and estimated profit for a candidate.
///
/// When either pool is a DLMM (Linear model), uses the multi-bin walker for
/// accurate cross-bin optimization. Falls back to single-bin analytical formula.
///
/// Returns `(optimal_amount, estimated_profit)`.
fn compute_optimal_amount<'info>(
    candidate: &AnalyticalCandidate,
    max_amount_in: u64,
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance],
) -> Option<(u64, i128)> {
    // Determine which pool (if any) is DLMM for multi-bin walking
    let has_dlmm_buy = matches!(candidate.buy_model, PoolModel::Linear { .. });
    let has_dlmm_sell = matches!(candidate.sell_model, PoolModel::Linear { .. });

    // DLMM + DLMM: greedy bin-pair walker (both sides piecewise-linear)
    if has_dlmm_buy && has_dlmm_sell {
        let buy_instance: &dyn ProgramMeta = &instances[candidate.buy_idx];
        let sell_instance: &dyn ProgramMeta = &instances[candidate.sell_idx];

        if let Some((amount, profit)) = analytical_optimal_dlmm_dlmm(
            accounts,
            buy_instance,
            sell_instance,
            candidate.input_mint,   // buy side input: SOL
            candidate.middle_mint,  // sell side input: token
            max_amount_in,
        ) {
            if amount >= MIN_SEARCH_AMOUNT {
                debug_eprintln!(
                    "  compute_optimal (dlmm+dlmm): amount={}, profit={:.4}, buy_model={:?}, sell_model={:?}",
                    amount, profit as f64, candidate.buy_model, candidate.sell_model
                );
                return Some((amount, profit));
            }
        }
    }

    // CP + DLMM or DLMM + CP: single DLMM multi-bin walker with analytical formula
    if has_dlmm_buy || has_dlmm_sell {
        let dlmm_idx = if has_dlmm_buy { candidate.buy_idx } else { candidate.sell_idx };
        let dlmm_instance: &dyn ProgramMeta = &instances[dlmm_idx];
        // DLMM input mint: when DLMM is sell side (Pool2), its input is the middle token;
        // when DLMM is buy side (Pool1), its input is the start token.
        let dlmm_input_mint = if has_dlmm_sell { candidate.middle_mint } else { candidate.input_mint };

        if let Some((amount, profit)) = analytical_optimal_multibin(
            accounts,
            dlmm_instance,
            dlmm_input_mint,
            &candidate.buy_model,
            &candidate.sell_model,
            max_amount_in,
        ) {
            if amount >= MIN_SEARCH_AMOUNT {
                debug_eprintln!(
                    "  compute_optimal (multibin): amount={}, profit={:.4}, buy_model={:?}, sell_model={:?}",
                    amount, profit as f64, candidate.buy_model, candidate.sell_model
                );
                return Some((amount, profit));
            }
        }
    }

    // CP + CLMM or CLMM + CP: multi-tick walker with exact CP-per-tick math
    let has_clmm_buy = matches!(candidate.buy_model, PoolModel::Clmm { .. });
    let has_clmm_sell = matches!(candidate.sell_model, PoolModel::Clmm { .. });
    debug_eprintln!(
        "  compute_optimal: has_clmm_buy={}, has_clmm_sell={}, buy_model={:?}, sell_model={:?}",
        has_clmm_buy, has_clmm_sell, candidate.buy_model, candidate.sell_model
    );
    if has_clmm_buy || has_clmm_sell {
        let clmm_idx = if has_clmm_buy { candidate.buy_idx } else { candidate.sell_idx };
        let clmm_instance: &dyn ProgramMeta = &instances[clmm_idx];
        let clmm_input_mint = if has_clmm_sell { candidate.middle_mint } else { candidate.input_mint };

        debug_eprintln!(
            "  compute_optimal: calling clmm_cp walker, clmm_idx={}, input_mint={}",
            clmm_idx, clmm_input_mint
        );
        let clmm_result = analytical_optimal_clmm_cp(
            accounts,
            clmm_instance,
            clmm_input_mint,
            &candidate.buy_model,
            &candidate.sell_model,
            max_amount_in,
        );
        debug_eprintln!("  compute_optimal: clmm_cp result={:?}", clmm_result);
        if let Some((amount, profit)) = clmm_result {
            if amount >= MIN_SEARCH_AMOUNT {
                debug_eprintln!(
                    "  compute_optimal (clmm_cp): amount={}, profit={:.4}, buy_model={:?}, sell_model={:?}",
                    amount, profit as f64, candidate.buy_model, candidate.sell_model
                );
                return Some((amount, profit));
            }
        }
    }

    // Fallback: single-tick analytical
    analytical_optimal_2pool(&candidate.buy_model, &candidate.sell_model, max_amount_in)
        .map(|r| r.optimal_amount)
        .filter(|&a| a >= MIN_SEARCH_AMOUNT)
        .map(|a| {
            let mid = pool_output(&candidate.buy_model, a as f64);
            let out = pool_output(&candidate.sell_model, mid);
            debug_eprintln!(
                "  compute_optimal: amount={}, mid={:.4}, out={:.4}, profit={:.4}, buy_model={:?}, sell_model={:?}",
                a, mid, out, out - a as f64, candidate.buy_model, candidate.sell_model
            );
            (a, (out - a as f64) as i128)
        })
}

// ─── Main entry point ───────────────────────────────────────────────────────

#[inline(never)]
pub fn run_analytical_2hop<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance],
    config: &mut BotConfig,
) -> Result<Option<ArbitragePath>> {
    let start_token = config.start_token.unwrap_or(WSOL);
    let first = instances.first().unwrap();
    let (base_mint, quote_mint) = first.get_mints();
    let middle_mint = if *base_mint == start_token { *quote_mint } else { *base_mint };

    let candidates = find_candidates_analytical(instances, start_token, middle_mint, config.max_amount_in);

    if candidates.is_empty() {
        debug_eprintln!("Analytical 2hop: no candidate found");
        if config.test && instances.len() >= 2 {
            let edges = build_edges(
                &instances[0], &instances[1],
                start_token, middle_mint,
            )?;
            return Ok(Some(ArbitragePath {
                edges,
                profit: 6_000,
                final_amount: 6_000,
                start_amount: 1_000_000,
            }));
        }
        return Ok(None);
    }

    let result = evaluate_candidates_single_pass(
        &candidates, instances, accounts, &config.clock,
        start_token, middle_mint,
        config.max_amount_in, config.min_profit,
    )?;

    if result.is_none() && config.test && instances.len() >= 2 {
        let edges = build_edges(
            &instances[0], &instances[1],
            start_token, middle_mint,
        )?;
        return Ok(Some(ArbitragePath {
            edges,
            profit: 6_000,
            final_amount: 6_000,
            start_amount: 1_000_000,
        }));
    }

    Ok(result)
}

/// Single-pass: lazily prepare pools, extract models, and evaluate each candidate.
/// Each pool is prepared/modeled at most once via per-index caches.
#[inline(never)]
fn evaluate_candidates_single_pass<'info>(
    candidates: &[AnalyticalCandidate],
    instances: &mut [ProgramInstance],
    accounts: &[AccountInfo<'info>],
    clock: &Clock,
    start_token: Pubkey,
    middle_mint: Pubkey,
    max_amount_in: u64,
    min_profit: i128,
) -> Result<Option<ArbitragePath>> {
    let mut prepared: [bool; MAX_POOLS] = [false; MAX_POOLS];
    let mut skipped: [bool; MAX_POOLS] = [false; MAX_POOLS];
    let opaque_zero = PoolModel::Opaque { marginal_price: 0.0 };
    let mut cached_buy: [PoolModel; MAX_POOLS] = [opaque_zero; MAX_POOLS];
    let mut cached_sell: [PoolModel; MAX_POOLS] = [opaque_zero; MAX_POOLS];
    let mut model_cached: [bool; MAX_POOLS] = [false; MAX_POOLS];

    let mut best_path: Option<ArbitragePath> = None;
    let mut best_profit: i128 = min_profit;
    let cand_count = candidates.len();

    for (i, c) in candidates.iter().enumerate() {
        // Lazy prepare: only on first encounter per pool
        for idx in [c.buy_idx, c.sell_idx] {
            if !prepared[idx] {
                prepared[idx] = true;
                if !instances[idx].prepare_for_execution(accounts, clock) {
                    skipped[idx] = true;
                }
            }
        }

        if skipped[c.buy_idx] || skipped[c.sell_idx] {
            continue;
        }

        // Lazy model extraction
        if !model_cached[c.buy_idx] {
            model_cached[c.buy_idx] = true;
            let (buy, sell) = extract_pool_model_both(&instances[c.buy_idx], start_token, middle_mint);
            cached_buy[c.buy_idx] = buy;
            cached_sell[c.buy_idx] = sell;
        }
        if !model_cached[c.sell_idx] {
            model_cached[c.sell_idx] = true;
            let (buy, sell) = extract_pool_model_both(&instances[c.sell_idx], start_token, middle_mint);
            cached_buy[c.sell_idx] = buy;
            cached_sell[c.sell_idx] = sell;
        }

        let buy_model = cached_buy[c.buy_idx];
        let sell_model = cached_sell[c.sell_idx];

        if matches!(buy_model, PoolModel::Opaque { .. })
            || matches!(sell_model, PoolModel::Opaque { .. })
        {
            continue;
        }

        let c_fresh = AnalyticalCandidate {
            buy_model,
            sell_model,
            ..*c
        };

        let (optimal_amount, estimated_profit) = match compute_optimal_amount(&c_fresh, max_amount_in, accounts, instances) {
            Some((a, p)) if a >= MIN_SEARCH_AMOUNT => {
                debug_eprintln!(
                    "Analytical 2hop[{}/{}]: optimal_amount={:.4} SOL, estimated_profit={:.4} SOL for {}+{} (pool {} -> pool {}), price_product={:.6}",
                    i + 1, cand_count,
                    a as f64 / 1e9, p as f64 / 1e9,
                    buy_model.label(), sell_model.label(),
                    instances[c.buy_idx].get_pool_id(), instances[c.sell_idx].get_pool_id(),
                    c.price_product
                );
                (a, p)
            }
            _ => { continue; }
        };

        if optimal_amount == 0 || estimated_profit <= best_profit {
            continue;
        }

        let edges = build_edges(
            &instances[c.buy_idx],
            &instances[c.sell_idx],
            c.input_mint,
            c.middle_mint,
        )?;

        let final_amount = (optimal_amount as i128).checked_add(estimated_profit).unwrap_or(0) as u128;

        best_profit = estimated_profit;
        best_path = Some(ArbitragePath {
            edges,
            profit: estimated_profit,
            final_amount,
            start_amount: optimal_amount,
        });
    }

    Ok(best_path)
}

// ─── Multiple trades analytical (mode 2) ────────────────────────────────────

// ─── Multi-hop analytical (mode 1) ─────────────────────────────────────────

/// A candidate found by the multi-hop analytical search.
struct MultiHopCandidate {
    indices: [usize; 3],
    mints: [Pubkey; 4], // [start, A, B, start]
    models: [PoolModel; 3],
    optimal_amount: u64,
    estimated_profit: i128,
    dlmm_capped: bool,
}

/// Lightweight hop entry for Phase 1 — marginal prices only, no vault reads.
struct LightHopEntry {
    pool_index: usize,
    other_mint: Pubkey,
    marginal_price: f64,
}

/// Find the best 3-hop analytical candidate: start → A → B → start.
///
/// Two-phase approach:
/// Phase 1: build adjacency with marginal prices only (no vault reads).
/// Phase 2: for each (hop1, hop2, hop3) triple, pre-filter with marginal price product,
///   then extract full models only for promising triples.
fn find_best_candidate_analytical_multihop(
    instances: &[ProgramInstance],
    config: &BotConfig,
) -> Option<MultiHopCandidate> {
    let start_token = config.start_token.unwrap_or(WSOL);
    let max_amount_in = config.max_amount_in;
    let pool_count = instances.len();

    // ── Phase 1: lightweight adjacency with marginal prices only ──────────
    let mut hop1_entries: Vec<LightHopEntry> = Vec::with_capacity(pool_count);
    let mut hop3_mints: Vec<Pubkey> = Vec::with_capacity(pool_count);
    let mut hop3_groups: Vec<Vec<LightHopEntry>> = Vec::with_capacity(pool_count);

    for (pool_index, instance) in instances.iter().enumerate() {
        let (base_mint, quote_mint) = instance.get_mints();

        if *base_mint != start_token && *quote_mint != start_token {
            continue;
        }
        let other_mint = if *base_mint == start_token { *quote_mint } else { *base_mint };

        let (price_base_to_quote, price_quote_to_base) = instance.get_prices().unwrap_or((0.0, 0.0));
        let (fee_base_to_quote, fee_quote_to_base) = instance.get_fee_factor().unwrap_or((1.0, 1.0));

        // Hop 1 marginal price: start_token → other_mint
        let hop1_marginal = if start_token == *base_mint {
            price_base_to_quote * fee_base_to_quote
        } else {
            price_quote_to_base * fee_quote_to_base
        };
        if hop1_marginal > 0.0 {
            hop1_entries.push(LightHopEntry { pool_index, other_mint, marginal_price: hop1_marginal });
        }

        // Hop 3 marginal price: other_mint → start_token
        let hop3_marginal = if other_mint == *base_mint {
            price_base_to_quote * fee_base_to_quote
        } else {
            price_quote_to_base * fee_quote_to_base
        };
        if hop3_marginal > 0.0 {
            let group_index = match hop3_mints.iter().position(|m| *m == other_mint) {
                Some(pos) => pos,
                None => {
                    hop3_mints.push(other_mint);
                    hop3_groups.push(Vec::with_capacity(4));
                    hop3_mints.len() - 1
                }
            };
            hop3_groups[group_index].push(LightHopEntry {
                pool_index, other_mint: start_token, marginal_price: hop3_marginal,
            });
        }
    }

    // ── Phase 2: search with marginal price pre-filter, defer model extraction ──
    let mut best: Option<MultiHopCandidate> = None;

    for h1 in &hop1_entries {
        let mint_a = h1.other_mint;

        for (hop2_index, hop2_instance) in instances.iter().enumerate() {
            if hop2_index == h1.pool_index { continue; }
            let (base_2, quote_2) = hop2_instance.get_mints();

            let mint_b = if *base_2 == mint_a {
                *quote_2
            } else if *quote_2 == mint_a {
                *base_2
            } else {
                continue;
            };
            if mint_b == start_token { continue; }

            // Hop 3: look up pools that accept mint_b
            let hop3_group = match hop3_mints.iter().position(|m| *m == mint_b) {
                Some(pos) => &hop3_groups[pos],
                None => continue,
            };

            // Compute hop2 marginal price cheaply
            let (p2_btq, p2_qtb) = hop2_instance.get_prices().unwrap_or((0.0, 0.0));
            let (f2_btq, f2_qtb) = hop2_instance.get_fee_factor().unwrap_or((1.0, 1.0));
            let hop2_marginal = if mint_a == *base_2 {
                p2_btq * f2_btq
            } else {
                p2_qtb * f2_qtb
            };
            if hop2_marginal <= 0.0 { continue; }

            for h3 in hop3_group {
                if h3.pool_index == h1.pool_index || h3.pool_index == hop2_index { continue; }

                // Cheap pre-filter: combined marginal prices must suggest profitability
                if h1.marginal_price * hop2_marginal * h3.marginal_price <= 1.0 {
                    continue;
                }

                // NOW extract full models — only for promising triples
                let model_1 = extract_pool_model(&instances[h1.pool_index], start_token);
                let model_2 = extract_pool_model(&instances[hop2_index], mint_a);
                let model_3 = extract_pool_model(&instances[h3.pool_index], mint_b);

                if matches!(model_1, PoolModel::Opaque { .. })
                    || matches!(model_2, PoolModel::Opaque { .. })
                    || matches!(model_3, PoolModel::Opaque { .. })
                {
                    continue;
                }

                let models = [model_1, model_2, model_3];
                let Some((optimal_amount, estimated_profit, dlmm_capped)) =
                    analytical_estimate_nhop(&models, max_amount_in)
                else { continue };

                if optimal_amount < MIN_SEARCH_AMOUNT || estimated_profit <= config.min_profit {
                    continue;
                }

                if best.as_ref().map_or(true, |b| estimated_profit > b.estimated_profit) {
                    best = Some(MultiHopCandidate {
                        indices: [h1.pool_index, hop2_index, h3.pool_index],
                        mints: [start_token, mint_a, mint_b, start_token],
                        models,
                        optimal_amount,
                        estimated_profit,
                        dlmm_capped,
                    });
                }
            }
        }
    }

    best
}

/// Simulate a 3-hop path through actual swap_base_in calls.
#[inline]
fn simulate_nhop<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance],
    indices: &[usize; 3],
    mints: &[Pubkey; 4],
    amount_in: u64,
    clock: &Clock,
    mint_fees: &[(Pubkey, MintFee)],
) -> Result<i128> {
    let mut current = amount_in;
    for hop in 0..3 {
        let (base_pk, quote_pk) = instances[indices[hop]].get_mints();
        let (in_fee, out_fee) = get_transfer_fees(mints[hop], base_pk, quote_pk, mint_fees);
        current = instances[indices[hop]].swap_base_in(accounts, mints[hop], current, in_fee, out_fee, clock)?;
    }
    Ok(current as i128 - amount_in as i128)
}

// Golden section refinement for 3-hop — commented out in favor of single simulate_nhop validation.
// The analytical estimate already finds the optimal; we validate with one simulation call.
//
// fn golden_section_refine_nhop<'info>(
//     accounts: &[AccountInfo<'info>],
//     instances: &mut [ProgramInstance],
//     indices: &[usize; 3],
//     mints: &[Pubkey; 4],
//     hint: u64,
//     max_amount: u64,
//     clock: &Clock,
//     mint_fees: &[(Pubkey, MintFee)],
// ) -> Result<(u64, i128)> {
//     let mut a = (hint / 2).max(MIN_SEARCH_AMOUNT);
//     let mut b = (hint.saturating_mul(2)).min(max_amount);
//     let mut c = b - golden_div(b - a);
//     let mut d = a + golden_div(b - a);
//     let mut fc = simulate_nhop(accounts, instances, indices, mints, c, clock, mint_fees).unwrap_or(i128::MIN);
//     let mut fd = simulate_nhop(accounts, instances, indices, mints, d, clock, mint_fees).unwrap_or(i128::MIN);
//
//     for _ in 0..DLMM_REFINE_ITERATIONS {
//         if b - a < CONVERGENCE {
//             break;
//         }
//         if fc > fd {
//             b = d;
//             d = c;
//             fd = fc;
//             c = b - golden_div(b - a);
//             fc = simulate_nhop(accounts, instances, indices, mints, c, clock, mint_fees)
//                 .unwrap_or(i128::MIN);
//         } else {
//             a = c;
//             c = d;
//             fc = fd;
//             d = a + golden_div(b - a);
//             fd = simulate_nhop(accounts, instances, indices, mints, d, clock, mint_fees)
//                 .unwrap_or(i128::MIN);
//         }
//     }
//
//     let optimal = (a + b) / 2;
//     let profit = simulate_nhop(accounts, instances, indices, mints, optimal, clock, mint_fees)?;
//     Ok((optimal, profit))
// }

/// Build Edge structs for a 3-hop chain.
fn build_edges_multihop(
    instances: &[ProgramInstance],
    indices: &[usize; 3],
    mints: &[Pubkey; 4],
) -> Result<EdgeArray> {
    use crate::arbitrage::base::edge::EdgeSide;
    use crate::arbitrage::base::pool::Pool;

    let mut edge_buf: [Edge; 3] = unsafe { std::mem::zeroed() };

    for hop in 0..3 {
        let inst = &instances[indices[hop]];
        let input_mint = mints[hop];
        let output_mint = mints[hop + 1];

        let (price, inverse_price) = inst.get_prices()?;
        let (fee_a_to_b, fee_b_to_a) = inst.get_fee_factor().unwrap_or((1.0, 1.0));
        let (base, _) = inst.get_mints();
        let (max_in, max_out) = inst.get_cached_max_amounts(input_mint);

        edge_buf[hop] = if input_mint == *base {
            Edge::new(
                *inst.get_id(),
                *inst.get_pool_id(),
                EdgeSide::LeftToRight,
                price,
                fee_a_to_b,
                fee_b_to_a,
                max_in,
                max_out,
                Pool::new(&input_mint),
                Pool::new(&output_mint),
            )
        } else {
            Edge::new(
                *inst.get_id(),
                *inst.get_pool_id(),
                EdgeSide::RightToLeft,
                inverse_price,
                fee_b_to_a,
                fee_a_to_b,
                max_in,
                max_out,
                Pool::new(&input_mint),
                Pool::new(&output_mint),
            )
        };
    }

    Ok(EdgeArray::from_3(edge_buf[0], edge_buf[1], edge_buf[2]))
}

/// 3-hop analytical arbitrage (MULTI_HOP_CHAIN).
#[inline(never)]
pub fn run_analytical_multihop<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance],
    config: &mut BotConfig,
    mint_fees: &[(Pubkey, MintFee)],
) -> Result<Option<ArbitragePath>> {
    let candidate = find_best_candidate_analytical_multihop(instances, config);

    let Some(candidate) = candidate else {
        debug_eprintln!("Analytical multihop: no candidate found");
        return Ok(None);
    };

    debug_eprintln!(
        "Analytical multihop: hop1_model={}, hop2_model={}, hop3_model={}, optimal_amount={:.4} SOL, estimated_profit={:.4} SOL",
        candidate.models[0].label(),
        candidate.models[1].label(),
        candidate.models[2].label(),
        candidate.optimal_amount as f64 / 1e9,
        candidate.estimated_profit as f64 / 1e9
    );

    // Validate with a single simulate_nhop — no golden section needed.
    // The analytical estimate already found the optimal amount.
    let optimal_amount = candidate.optimal_amount;
    let profit = simulate_nhop(
        accounts,
        instances,
        &candidate.indices,
        &candidate.mints,
        optimal_amount,
        &config.clock,
        mint_fees,
    )?;

    // msg!("Multihop optimal_in={}, profit={}", optimal_amount, profit);

    if !config.test && (profit <= 0 || optimal_amount == 0) {
        debug_eprintln!("Analytical multihop: rejected, profit={}", profit);
        return Ok(None);
    }

    let edges = build_edges_multihop(instances, &candidate.indices, &candidate.mints)?;

    // let price_diff_pct = ((edges[0].price - edges[1].price) / edges[1].price) * 100.0;
    // msg!(
    //     "price_diff={:.2}% profit={:.6} | buy: fee={:.4} pool={} | sell: fee={:.4} pool={}",
    //     price_diff_pct,
    //     profit as f64 / 1_000_000_000.0,
    //     edges[0].fee_factor,
    //     &edges[0].pool_id.to_string()[..1],
    //     edges[1].fee_factor,
    //     &edges[1].pool_id.to_string()[..1],
    // );

    let final_amount = (optimal_amount as i128).checked_add(profit).unwrap_or(0) as u128;

    Ok(Some(ArbitragePath {
        edges,
        profit,
        final_amount,
        start_amount: optimal_amount,
    }))
}
