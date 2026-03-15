use crate::arb_mode;
use crate::arbitrage::algo_2::ArbitragePath;
use crate::arbitrage::base::Edge;
use crate::programs::{ProgramInstance, ProgramMeta, SolarBError};
use crate::utils::bot_config::BotConfig;
use crate::utils::token::{get_transfer_fees, MintFee};
use anchor_lang::prelude::*;
use anchor_spl::token::spl_token::native_mint::ID as WSOL;

use super::formulas::{analytical_estimate, analytical_estimate_nhop, analytical_optimal_multibin};
use super::pool_model::{extract_pool_model, PoolModel};

/// Minimum search amount in lamports
const MIN_SEARCH_AMOUNT: u64 = 1_000;
/// Max golden section iterations for DLMM refinement
/// 5 iterations narrows a 4x range to ~6.8% — sufficient precision for DLMM paths.
const DLMM_REFINE_ITERATIONS: usize = 5;
/// Convergence threshold for golden section (lamports)
const CONVERGENCE: u64 = 50_000;

/// Integer approximation of x / golden_ratio ≈ x * 0.6180339...
/// Uses x * 6180 / 10000 (accurate to 0.005%, avoids f64 entirely).
#[inline(always)]
fn golden_div(x: u64) -> u64 {
    ((x as u128) * 6180 / 10000) as u64
}

// ─── Simulation helpers ─────────────────────────────────────────────────────

/// Simulate a 2-hop path: pool1(buy) -> pool2(sell) and return profit.
#[inline]
fn simulate_2hop<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance],
    buy_idx: usize,
    sell_idx: usize,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    amount_in: u64,
    clock: &Clock,
    mint_fees: &[(Pubkey, MintFee)],
) -> Result<i128> {
    let (base1, quote1) = instances[buy_idx].get_mints();
    let (in_fee_1, out_fee_1) = get_transfer_fees(input_mint, base1, quote1, mint_fees);
    let middle_amount = instances[buy_idx].swap_base_in(accounts, input_mint, amount_in, in_fee_1, out_fee_1, clock)?;
    let (base2, quote2) = instances[sell_idx].get_mints();
    let (in_fee_2, out_fee_2) = get_transfer_fees(middle_mint, base2, quote2, mint_fees);
    let final_amount = instances[sell_idx].swap_base_in(accounts, middle_mint, middle_amount, in_fee_2, out_fee_2, clock)?;
    Ok(final_amount as i128 - amount_in as i128)
}

/// Golden section search over [hint/2, min(hint*2, max_amount)] range,
/// centered around the analytical optimal amount.
fn golden_section_refine<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance],
    buy_idx: usize,
    sell_idx: usize,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    hint: u64,
    max_amount: u64,
    clock: &Clock,
    mint_fees: &[(Pubkey, MintFee)],
) -> Result<(u64, i128)> {
    let mut a = (hint / 2).max(MIN_SEARCH_AMOUNT);
    let mut b = (hint.saturating_mul(2)).min(max_amount);
    let mut c = b - golden_div(b - a);
    let mut d = a + golden_div(b - a);
    let mut fc = simulate_2hop(
        accounts, instances, buy_idx, sell_idx, input_mint, middle_mint, c, clock, mint_fees,
    )
    .unwrap_or(i128::MIN);
    let mut fd = simulate_2hop(
        accounts, instances, buy_idx, sell_idx, input_mint, middle_mint, d, clock, mint_fees,
    )
    .unwrap_or(i128::MIN);

    for _ in 0..DLMM_REFINE_ITERATIONS {
        if b - a < CONVERGENCE {
            break;
        }
        if fc > fd {
            b = d;
            d = c;
            fd = fc;
            c = b - golden_div(b - a);
            fc = simulate_2hop(
                accounts, instances, buy_idx, sell_idx, input_mint, middle_mint, c, clock, mint_fees,
            )
            .unwrap_or(i128::MIN);
        } else {
            a = c;
            c = d;
            fc = fd;
            d = a + golden_div(b - a);
            fd = simulate_2hop(
                accounts, instances, buy_idx, sell_idx, input_mint, middle_mint, d, clock, mint_fees,
            )
            .unwrap_or(i128::MIN);
        }
    }

    let optimal = (a + b) / 2;
    let profit = simulate_2hop(
        accounts, instances, buy_idx, sell_idx, input_mint, middle_mint, optimal, clock, mint_fees,
    )?;
    Ok((optimal, profit))
}

// ─── Candidate search ───────────────────────────────────────────────────────

/// A candidate found by the analytical search.
struct AnalyticalCandidate {
    buy_idx: usize,  // index into instances
    sell_idx: usize,  // index into instances
    input_mint: Pubkey,
    middle_mint: Pubkey,
    optimal_amount: u64,
    estimated_profit: i128,
    dlmm_capped: bool,
    buy_model: PoolModel,
    sell_model: PoolModel,
}

/// Pool with both directional marginal prices for Phase 1 ranking.
struct PoolPrices {
    pool_index: usize,
    buy_price: f64,   // start_token → middle_mint (higher = better to buy from)
    sell_price: f64,   // middle_mint → start_token (higher = better to sell to)
}

/// Ranked index into PoolPrices: [best, second_best] by the given price direction.
struct Ranked {
    best: Option<usize>,
    second: Option<usize>,
}

impl Ranked {
    fn insert(&mut self, idx: usize, score: f64, pools: &[PoolPrices], get_price: fn(&PoolPrices) -> f64) {
        match self.best {
            Some(b) if score <= get_price(&pools[b]) => {
                if self.second.map_or(true, |s| score > get_price(&pools[s])) {
                    self.second = Some(idx);
                }
            }
            _ => {
                self.second = self.best;
                self.best = Some(idx);
            }
        }
    }
}

/// Compute marginal prices for both directions of a pool (buy + sell).
/// Only calls get_prices() + get_fee_factor() — no vault reads, no model extraction.
#[inline]
fn compute_marginal_prices(
    instance: &dyn ProgramMeta,
    start_token: Pubkey,
    middle_mint: Pubkey,
) -> (f64, f64) {
    let (base_mint, _) = instance.get_mints();
    let (price_base_to_quote, price_quote_to_base) = instance.get_prices().unwrap_or((0.0, 0.0));
    let (fee_base_to_quote, fee_quote_to_base) = instance.get_fee_factor().unwrap_or((1.0, 1.0));

    let buy_marginal = if start_token == *base_mint {
        price_base_to_quote * fee_base_to_quote
    } else {
        price_quote_to_base * fee_quote_to_base
    };
    let sell_marginal = if middle_mint == *base_mint {
        price_base_to_quote * fee_base_to_quote
    } else {
        price_quote_to_base * fee_quote_to_base
    };

    (buy_marginal, sell_marginal)
}

/// Two-phase candidate search using analytical formulas.
///
/// Phase 1 (O(n), cheap): compute both directional marginal prices per pool,
///   track best/second-best buy and sell in a single pass.
/// Phase 2 (≤ 2 pairs): try (best_buy, best_sell) first — if same pool, fall back
///   to the better of (best_buy, 2nd_sell) or (2nd_buy, best_sell).
///   Only extract full PoolModel for the pair actually tested.
fn find_best_candidate_analytical(
    instances: &[ProgramInstance],
    config: &BotConfig,
) -> Option<AnalyticalCandidate> {
    let start_token = config.start_token.unwrap_or(WSOL);
    let max_amount_in = config.max_amount_in;

    // Resolve the middle_mint from the first instance
    let (base_mint, quote_mint) = instances.first()?.get_mints();
    let middle_mint = if *base_mint == start_token { *quote_mint } else { *base_mint };

    // Phase 1: single pass — compute marginal prices, track top-2 buy and sell
    let mut pools: Vec<PoolPrices> = Vec::with_capacity(instances.len());
    let mut buy_rank = Ranked { best: None, second: None };
    let mut sell_rank = Ranked { best: None, second: None };

    for (pool_index, instance) in instances.iter().enumerate() {
        let (buy_marginal, sell_marginal) = compute_marginal_prices(instance, start_token, middle_mint);

        if buy_marginal <= 0.0 && sell_marginal <= 0.0 {
            continue;
        }

        let idx = pools.len();
        pools.push(PoolPrices { pool_index, buy_price: buy_marginal, sell_price: sell_marginal });

        if buy_marginal > 0.0 {
            buy_rank.insert(idx, buy_marginal, &pools, |p| p.buy_price);
        }
        if sell_marginal > 0.0 {
            sell_rank.insert(idx, sell_marginal, &pools, |p| p.sell_price);
        }
    }

    let best_buy = buy_rank.best?;
    let best_sell = sell_rank.best?;

    // Phase 2: build candidate pairs — try best first, fall back on collision
    // If best_buy and best_sell are different pools, that's the only pair to check.
    // If same pool, try both alternatives and pick the better one.
    let pairs: Vec<(usize, usize)> = if pools[best_buy].pool_index != pools[best_sell].pool_index {
        vec![(best_buy, best_sell)]
    } else {
        let mut fallbacks = Vec::with_capacity(2);
        if let Some(sb) = buy_rank.second {
            fallbacks.push((sb, best_sell));
        }
        if let Some(ss) = sell_rank.second {
            fallbacks.push((best_buy, ss));
        }
        fallbacks
    };

    let mut best: Option<AnalyticalCandidate> = None;

    for (bi, si) in pairs {
        let buy = &pools[bi];
        let sell = &pools[si];

        if buy.buy_price * sell.sell_price <= 1.0 {
            continue;
        }

        let buy_model = extract_pool_model(&instances[buy.pool_index], start_token);
        let sell_model = extract_pool_model(&instances[sell.pool_index], middle_mint);

        if matches!(buy_model, PoolModel::Opaque { .. }) || matches!(sell_model, PoolModel::Opaque { .. }) {
            continue;
        }

        let Some((optimal_amount, estimated_profit, dlmm_capped)) =
            analytical_estimate(&buy_model, &sell_model, max_amount_in)
        else {
            continue;
        };

        if optimal_amount < MIN_SEARCH_AMOUNT || estimated_profit <= config.min_profit {
            continue;
        }

        if best.as_ref().map_or(true, |b| estimated_profit > b.estimated_profit) {
            best = Some(AnalyticalCandidate {
                buy_idx: buy.pool_index,
                sell_idx: sell.pool_index,
                input_mint: start_token,
                middle_mint,
                optimal_amount,
                estimated_profit,
                dlmm_capped,
                buy_model,
                sell_model,
            });
        }
    }

    best
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

// ─── Shared validate / refine / build ────────────────────────────────────────

/// Validate an analytical candidate with actual swap simulation, optionally
/// refine via multi-bin or golden-section search, then build the ArbitragePath.
///
/// This is the shared second half of both `run_analytical_2hop` and
/// `run_analytical_multi_trade`.
fn validate_and_build_path<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance],
    candidate: &AnalyticalCandidate,
    config: &BotConfig,
    log_prefix: &str,
    mint_fees: &[(Pubkey, MintFee)],
) -> Result<Option<ArbitragePath>> {
    #[cfg(any(test, feature = "debug"))]
    {
        let buy_inst = &instances[candidate.buy_idx];
        let sell_inst = &instances[candidate.sell_idx];
        let buy_price = buy_inst.get_prices().unwrap_or((0.0, 0.0)).0;
        let sell_price = sell_inst.get_prices().unwrap_or((0.0, 0.0)).0;
        let buy_fee = buy_inst.get_fee_factor().unwrap_or((1.0, 1.0)).0;
        let sell_fee = sell_inst.get_fee_factor().unwrap_or((1.0, 1.0)).0;
        debug_eprintln!("");
        debug_eprintln!(
            "[{}] Best candidate | estimated_profit: {} SOL | input: {} SOL | output: {} SOL",
            log_prefix,
            candidate.estimated_profit as f64 / 1_000_000_000.0,
            candidate.optimal_amount as f64 / 1_000_000_000.0,
            (candidate.optimal_amount as i128 + candidate.estimated_profit) as f64 / 1_000_000_000.0,
        );
        debug_eprintln!(
            "  buy:  {} -> {} (pool {} @ price={:.6} fee={:.4})",
            candidate.input_mint,
            candidate.middle_mint,
            buy_inst.get_pool_id(),
            buy_price,
            buy_fee,
        );
        debug_eprintln!(
            "  sell: {} -> {} (pool {} @ price={:.6} fee={:.4})",
            candidate.middle_mint,
            candidate.input_mint,
            sell_inst.get_pool_id(),
            sell_price,
            sell_fee,
        );
        debug_eprintln!("");
    }

    let has_dlmm = matches!(candidate.buy_model, PoolModel::Linear { .. })
        || matches!(candidate.sell_model, PoolModel::Linear { .. });

    let buy_idx = candidate.buy_idx;
    let sell_idx = candidate.sell_idx;

    let (optimal_amount, profit) = if candidate.dlmm_capped || has_dlmm {
        let dlmm_instance: &dyn ProgramMeta =
            if matches!(candidate.buy_model, PoolModel::Linear { .. }) {
                &instances[buy_idx]
            } else {
                &instances[sell_idx]
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
            config.max_amount_in,
        );

        if let Some((multibin_amount, multibin_profit)) = multibin_result {
            if multibin_profit > candidate.estimated_profit {
                debug_eprintln!(
                    "{}: multibin optimal_amount={}, estimated_profit={}",
                    log_prefix, multibin_amount, multibin_profit
                );
                let profit = simulate_2hop(
                    accounts,
                    instances,
                    buy_idx,
                    sell_idx,
                    candidate.input_mint,
                    candidate.middle_mint,
                    multibin_amount,
                    &config.clock,
                    mint_fees,
                )?;
                (multibin_amount, profit)
            } else {
                golden_section_refine(
                    accounts,
                    instances,
                    buy_idx,
                    sell_idx,
                    candidate.input_mint,
                    candidate.middle_mint,
                    candidate.optimal_amount,
                    config.max_amount_in,
                    &config.clock,
                    mint_fees,
                )?
            }
        } else {
            golden_section_refine(
                accounts,
                instances,
                buy_idx,
                sell_idx,
                candidate.input_mint,
                candidate.middle_mint,
                candidate.optimal_amount,
                config.max_amount_in,
                &config.clock,
                mint_fees,
            )?
        }
    } else {
        // Pure CP+CP: analytical formula gives exact optimal — validate with
        // a single simulate_2hop instead of ~18 swap_base_in calls via golden section.
        let profit = simulate_2hop(
            accounts,
            instances,
            buy_idx,
            sell_idx,
            candidate.input_mint,
            candidate.middle_mint,
            candidate.optimal_amount,
            &config.clock,
            mint_fees,
        )?;
        (candidate.optimal_amount, profit)
    };

    // msg!("{} optimal_in={}, profit={}", log_prefix, optimal_amount, profit);

    if !config.test && (profit <= 0 || optimal_amount == 0) {
        debug_eprintln!("{}: rejected, profit={}", log_prefix, profit);
        return Ok(None);
    }

    let edges = build_edges(
        &instances[buy_idx],
        &instances[sell_idx],
        candidate.input_mint,
        candidate.middle_mint,
    )?;



    let final_amount = (optimal_amount as i128).checked_add(profit).unwrap_or(0) as u128;

    Ok(Some(ArbitragePath {
        edges,
        profit,
        final_amount,
        start_amount: optimal_amount,
    }))
}

// ─── Main entry point ───────────────────────────────────────────────────────

/// Fully independent analytical arbitrage with mode dispatch.
///
/// 2-hop analytical arbitrage (single pair, multi-market).
///
/// 1. Iterates all instance pairs, extracts PoolModels
/// 2. Uses analytical formulas to find optimal amount + estimated profit per pair
/// 3. Picks best candidate by estimated profit
/// 4. Validates with swap_base_in (1 call, or 8-iter golden section if DLMM-capped)
/// 5. Returns ArbitragePath
#[inline(never)]
pub fn run_analytical_2hop<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance],
    config: &mut BotConfig,
    mint_fees: &[(Pubkey, MintFee)],
) -> Result<Option<ArbitragePath>> {
    let candidate = find_best_candidate_analytical(instances, config);

    let Some(candidate) = candidate else {
        debug_eprintln!("Analytical 2hop: no candidate found");
        return Ok(None);
    };

    debug_eprintln!(
        "Analytical 2hop: buy_model={}, sell_model={}, optimal_amount={}, estimated_profit={}",
        candidate.buy_model.label(),
        candidate.sell_model.label(),
        candidate.optimal_amount,
        candidate.estimated_profit
    );

    validate_and_build_path(accounts, instances, &candidate, config, "Analytical 2hop", mint_fees)
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

/// Golden section refinement for a 3-hop path.
fn golden_section_refine_nhop<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance],
    indices: &[usize; 3],
    mints: &[Pubkey; 4],
    hint: u64,
    max_amount: u64,
    clock: &Clock,
    mint_fees: &[(Pubkey, MintFee)],
) -> Result<(u64, i128)> {
    let mut a = (hint / 2).max(MIN_SEARCH_AMOUNT);
    let mut b = (hint.saturating_mul(2)).min(max_amount);
    let mut c = b - golden_div(b - a);
    let mut d = a + golden_div(b - a);
    let mut fc = simulate_nhop(accounts, instances, indices, mints, c, clock, mint_fees).unwrap_or(i128::MIN);
    let mut fd = simulate_nhop(accounts, instances, indices, mints, d, clock, mint_fees).unwrap_or(i128::MIN);

    for _ in 0..DLMM_REFINE_ITERATIONS {
        if b - a < CONVERGENCE {
            break;
        }
        if fc > fd {
            b = d;
            d = c;
            fd = fc;
            c = b - golden_div(b - a);
            fc = simulate_nhop(accounts, instances, indices, mints, c, clock, mint_fees)
                .unwrap_or(i128::MIN);
        } else {
            a = c;
            c = d;
            fc = fd;
            d = a + golden_div(b - a);
            fd = simulate_nhop(accounts, instances, indices, mints, d, clock, mint_fees)
                .unwrap_or(i128::MIN);
        }
    }

    let optimal = (a + b) / 2;
    let profit = simulate_nhop(accounts, instances, indices, mints, optimal, clock, mint_fees)?;
    Ok((optimal, profit))
}

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
        "Analytical multihop: hop1_model={}, hop2_model={}, hop3_model={}, optimal_amount={}, estimated_profit={}",
        candidate.models[0].label(),
        candidate.models[1].label(),
        candidate.models[2].label(),
        candidate.optimal_amount,
        candidate.estimated_profit
    );

    // Validate / refine with actual swap simulation
    let (optimal_amount, profit) = golden_section_refine_nhop(
        accounts,
        instances,
        &candidate.indices,
        &candidate.mints,
        candidate.optimal_amount,
        config.max_amount_in,
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
