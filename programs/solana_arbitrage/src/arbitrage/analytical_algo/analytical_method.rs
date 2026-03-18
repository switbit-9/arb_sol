use crate::arbitrage::algo_2::ArbitragePath;
use crate::arbitrage::base::Edge;
use crate::programs::{ProgramInstance, ProgramMeta};
use crate::utils::bot_config::BotConfig;
use crate::utils::token::{get_transfer_fees, MintFee};
use anchor_lang::prelude::*;
use anchor_spl::token::spl_token::native_mint::ID as WSOL;

use super::formulas::{analytical_estimate_nhop, analytical_optimal_multibin, analytical_optimal_2pool, pool_output};
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
/// 1. Extract both-direction PoolModels for every pool upfront in O(n).
/// 2. Check all pairs (O(n²)) — marginal check (buy_price * sell_price > 1.0)
///    eliminates unprofitable pairs. Opaque pools are filtered before the loop.
/// 3. Rank by price product (buy_price * sell_price), keep top 3.
///
/// Optimal amounts and real profits are computed later via multibin/fast_quote.
const MAX_CANDIDATES: usize = 3;

fn find_candidates_analytical(
    instances: &[ProgramInstance],
    config: &BotConfig,
) -> Vec<AnalyticalCandidate> {
    let start_token = config.start_token.unwrap_or(WSOL);

    let Some(first) = instances.first() else { return Vec::new() };
    let (base_mint, quote_mint) = first.get_mints();
    let middle_mint = if *base_mint == start_token { *quote_mint } else { *base_mint };

    // Extract both directions per pool in a single call — get_prices(), get_fee_factor(),
    // and get_vault_amounts() are shared between buy and sell instead of being repeated.
    let models: Vec<(PoolModel, PoolModel)> = instances
        .iter()
        .map(|inst: &ProgramInstance| extract_pool_model_both(inst, start_token, middle_mint))
        .collect();

    // Build price list from marginal_price() already stored in the model — no extra reads.
    let mut prices: Vec<(usize, f64, f64)> = models
        .iter()
        .enumerate()
        .filter_map(|(idx, (buy_model, sell_model))| {
            let buy = buy_model.marginal_price();
            let sell = sell_model.marginal_price();
            if buy > 0.0 || sell > 0.0 { Some((idx, buy, sell)) } else { None }
        })
        .collect();

    // Outer loop: highest buy_price first (cheapest pool to buy middle from).
    prices.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    // Inner loop: separate sort by sell_price descending.
    let mut sell_order = prices.clone();
    sell_order.sort_unstable_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    // Pre-filter Opaque entries so the nested loop never checks them.
    prices.retain(|(idx, _, _)| !matches!(models[*idx].0, PoolModel::Opaque { .. }));
    sell_order.retain(|(idx, _, _)| !matches!(models[*idx].1, PoolModel::Opaque { .. }));

    let mut candidates: Vec<AnalyticalCandidate> = Vec::with_capacity(MAX_CANDIDATES + 1);

    for &(buy_idx, buy_price, _) in &prices {
        if sell_order.first().map_or(true, |e| buy_price * e.2 <= 1.0) { break; }

        let buy_model = models[buy_idx].0;

        for &(sell_idx, _, sell_price) in &sell_order {
            if buy_idx == sell_idx { continue; }
            let product = buy_price * sell_price;
            if product <= 1.0 { break; }

            let sell_model = models[sell_idx].1;

            // Insert into sorted candidates list (descending by price product)
            let pos = candidates.iter().position(|c| product > c.price_product)
                .unwrap_or(candidates.len());
            if pos < MAX_CANDIDATES {
                candidates.insert(pos, AnalyticalCandidate {
                    buy_idx,
                    sell_idx,
                    input_mint: start_token,
                    middle_mint,
                    price_product: product,
                    buy_model,
                    sell_model,
                });
                candidates.truncate(MAX_CANDIDATES);
            }
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
) -> Result<Vec<Edge>> {
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

    Ok(vec![buy_edge, sell_edge])
}

// ─── Compute optimal amount for a candidate ─────────────────────────────────

/// Compute the optimal input amount and estimated profit for a candidate.
///
/// - CP+Linear (or Linear+CP): uses multibin tick-walking for accuracy.
/// - CP+CP: uses closed-form analytical formula (exact for constant-product).
/// - Linear+Linear: maximizes input up to bin capacity (profit is linear).
///
/// Returns `(optimal_amount, estimated_profit)`.
fn compute_optimal_amount<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance],
    candidate: &AnalyticalCandidate,
    max_amount_in: u64,
) -> Option<(u64, i128)> {
    let has_linear = matches!(candidate.buy_model, PoolModel::Linear { .. })
        || matches!(candidate.sell_model, PoolModel::Linear { .. });
    let both_linear = matches!((&candidate.buy_model, &candidate.sell_model),
        (PoolModel::Linear { .. }, PoolModel::Linear { .. }));

    if has_linear && !both_linear {
        // CP + Linear pair: multibin tick-walking for accurate optimal
        let dlmm_instance: &dyn ProgramMeta =
            if matches!(candidate.buy_model, PoolModel::Linear { .. }) {
                &instances[candidate.buy_idx]
            } else {
                &instances[candidate.sell_idx]
            };
        let dlmm_input_mint = if matches!(candidate.buy_model, PoolModel::Linear { .. }) {
            candidate.input_mint
        } else {
            candidate.middle_mint
        };

        let multibin_result = analytical_optimal_multibin(
            accounts,
            dlmm_instance,
            dlmm_input_mint,
            &candidate.buy_model,
            &candidate.sell_model,
            max_amount_in,
        );

        if let Some((amount, profit)) = multibin_result {
            debug_eprintln!(
                "  multibin optimal_amount={:.4} SOL, estimated_profit={:.4} SOL",
                amount as f64 / 1e9, profit as f64 / 1e9
            );
            Some((amount, profit))
        } else {
            // Multibin failed — fall back to analytical_optimal_2pool
            analytical_optimal_2pool(&candidate.buy_model, &candidate.sell_model, max_amount_in)
                .map(|r| r.optimal_amount)
                .filter(|&a| a >= MIN_SEARCH_AMOUNT)
                .map(|a| {
                    let mid = pool_output(&candidate.buy_model, a as f64);
                    let out = pool_output(&candidate.sell_model, mid);
                    (a, (out - a as f64) as i128)
                })
        }
    } else {
        // CP+CP or Linear+Linear: analytical formula gives the optimal directly
        analytical_optimal_2pool(&candidate.buy_model, &candidate.sell_model, max_amount_in)
            .map(|r| r.optimal_amount)
            .filter(|&a| a >= MIN_SEARCH_AMOUNT)
            .map(|a| {
                let mid = pool_output(&candidate.buy_model, a as f64);
                let out = pool_output(&candidate.sell_model, mid);
                (a, (out - a as f64) as i128)
            })
    }
}

// ─── Main entry point ───────────────────────────────────────────────────────

/// 2-hop analytical arbitrage (single pair, multi-market).
///
/// Pipeline:
/// 1. Marginal price filter → rank by price product → top 3 candidates
/// 2. Compute optimal amount per candidate (multibin for DLMM, analytical for CP)
/// 3. fast_quote for real profit estimation and ranking
/// 4. Build path for best candidate; run_simulation in lib.rs validates before execution
#[inline(never)]
pub fn run_analytical_2hop<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance],
    config: &mut BotConfig,
    _mint_fees: &[(Pubkey, MintFee)],
) -> Result<Option<ArbitragePath>> {
    let candidates = find_candidates_analytical(instances, config);

    if candidates.is_empty() {
        debug_eprintln!("Analytical 2hop: no candidate found");
        return Ok(None);
    }

    // Prepare all unique candidate pools upfront (loads full pool data, computes max amounts).
    {
        let mut prepared_indices: [usize; MAX_CANDIDATES * 2] = [usize::MAX; MAX_CANDIDATES * 2];
        let mut count = 0;
        for c in &candidates {
            for idx in [c.buy_idx, c.sell_idx] {
                if !prepared_indices[..count].contains(&idx) {
                    instances[idx].prepare_for_execution(accounts, &config.clock);
                    prepared_indices[count] = idx;
                    count += 1;
                }
            }
        }
    }

    // For each candidate: compute optimal amount and estimated profit.
    // Track the best by estimated profit.
    let mut best_path: Option<ArbitragePath> = None;
    let mut best_profit: i128 = config.min_profit;

    for (i, c) in candidates.iter().enumerate() {
        let (optimal_amount, estimated_profit) = match compute_optimal_amount(accounts, instances, c, config.max_amount_in) {
            Some((a, p)) if a >= MIN_SEARCH_AMOUNT => {
                debug_eprintln!(
                    "Analytical 2hop[{}/{}]: optimal_amount={:.4} SOL, estimated_profit={:.4} SOL for {}+{} (pool {} -> pool {}), price_product={:.6}",
                    i + 1, candidates.len(),
                    a as f64 / 1e9, p as f64 / 1e9,
                    c.buy_model.label(), c.sell_model.label(),
                    instances[c.buy_idx].get_pool_id(), instances[c.sell_idx].get_pool_id(),
                    c.price_product
                );
                (a, p)
            }
            _ => {
                debug_eprintln!(
                    "Analytical 2hop[{}/{}]: no optimal amount for {}+{} (pool {} -> pool {}), price_product={:.6}",
                    i + 1, candidates.len(), c.buy_model.label(), c.sell_model.label(),
                    instances[c.buy_idx].get_pool_id(), instances[c.sell_idx].get_pool_id(),
                    c.price_product
                );
                continue;
            }
        };


        if !config.test && (estimated_profit <= best_profit || optimal_amount == 0) {
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
) -> Result<Vec<Edge>> {
    use crate::arbitrage::base::edge::EdgeSide;
    use crate::arbitrage::base::pool::Pool;

    let mut edges = Vec::with_capacity(3);

    for hop in 0..3 {
        let inst = &instances[indices[hop]];
        let input_mint = mints[hop];
        let output_mint = mints[hop + 1];

        let (price, inverse_price) = inst.get_prices()?;
        let (fee_a_to_b, fee_b_to_a) = inst.get_fee_factor().unwrap_or((1.0, 1.0));
        let (base, _) = inst.get_mints();
        let (max_in, max_out) = inst.get_cached_max_amounts(input_mint);

        let edge = if input_mint == *base {
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

        edges.push(edge);
    }

    Ok(edges)
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
