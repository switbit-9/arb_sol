use crate::arb_mode;
use crate::arbitrage::algo_2::ArbitragePath;
use crate::arbitrage::base::Edge;
use crate::programs::{ProgramInstance, ProgramMeta, SolarBError};
use crate::utils::bot_config::BotConfig;
use anchor_lang::prelude::*;
use anchor_spl::token::spl_token::native_mint::ID as WSOL;

use super::formulas::{analytical_estimate, analytical_estimate_nhop, analytical_optimal_multibin};
use super::pool_model::{extract_pool_model, PoolModel};

/// Minimum search amount in lamports
const MIN_SEARCH_AMOUNT: u64 = 1_000;
/// Golden section ratio
const GOLDEN_RATIO: f64 = 1.618033988749895;
/// Max golden section iterations for DLMM refinement
const DLMM_REFINE_ITERATIONS: usize = 8;
/// Convergence threshold for golden section (lamports)
const CONVERGENCE: u64 = 10_000;

// ─── Simulation helpers ─────────────────────────────────────────────────────

/// Simulate a 2-hop path: pool1(buy) -> pool2(sell) and return profit.
#[inline]
fn simulate_2hop<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance<'info>],
    buy_idx: usize,
    sell_idx: usize,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    amount_in: u64,
    clock: &Clock,
) -> Result<i128> {
    let middle_amount = instances[buy_idx].swap_base_in(accounts, input_mint, amount_in, clock)?;
    let final_amount = instances[sell_idx].swap_base_in(accounts, middle_mint, middle_amount, clock)?;
    Ok(final_amount as i128 - amount_in as i128)
}

/// Golden section search over [hint/2, min(hint*2, max_amount)] range,
/// centered around the analytical optimal amount.
fn golden_section_refine<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance<'info>],
    buy_idx: usize,
    sell_idx: usize,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    hint: u64,
    max_amount: u64,
    clock: &Clock,
) -> Result<(u64, i128)> {
    let mut a = (hint / 2).max(MIN_SEARCH_AMOUNT);
    let mut b = (hint.saturating_mul(2)).min(max_amount);
    let mut c = b - ((b - a) as f64 / GOLDEN_RATIO) as u64;
    let mut d = a + ((b - a) as f64 / GOLDEN_RATIO) as u64;
    let mut fc = simulate_2hop(
        accounts, instances, buy_idx, sell_idx, input_mint, middle_mint, c, clock,
    )
    .unwrap_or(i128::MIN);
    let mut fd = simulate_2hop(
        accounts, instances, buy_idx, sell_idx, input_mint, middle_mint, d, clock,
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
            c = b - ((b - a) as f64 / GOLDEN_RATIO) as u64;
            fc = simulate_2hop(
                accounts, instances, buy_idx, sell_idx, input_mint, middle_mint, c, clock,
            )
            .unwrap_or(i128::MIN);
        } else {
            a = c;
            c = d;
            fc = fd;
            d = a + ((b - a) as f64 / GOLDEN_RATIO) as u64;
            fd = simulate_2hop(
                accounts, instances, buy_idx, sell_idx, input_mint, middle_mint, d, clock,
            )
            .unwrap_or(i128::MIN);
        }
    }

    let optimal = (a + b) / 2;
    let profit = simulate_2hop(
        accounts, instances, buy_idx, sell_idx, input_mint, middle_mint, optimal, clock,
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

/// Pre-extracted pool model with its instance index, direction, and marginal price.
struct PoolEntry {
    idx: usize,
    model: PoolModel,
    marginal_price: f64,
}

/// Find top-2 entries by marginal_price from a slice. Returns (best, second_best).
#[inline]
fn top2_by_marginal_price(entries: &[PoolEntry]) -> [Option<usize>; 2] {
    let mut best: Option<(usize, f64)> = None;
    let mut second: Option<(usize, f64)> = None;

    for (i, entry) in entries.iter().enumerate() {
        let score = entry.marginal_price;
        match best {
            Some((_, bp)) if score <= bp => {
                if second.map_or(true, |(_, sp)| score > sp) {
                    second = Some((i, score));
                }
            }
            _ => {
                second = best;
                best = Some((i, score));
            }
        }
    }

    [best.map(|(i, _)| i), second.map(|(i, _)| i)]
}

/// Build edges from instance pairs and find the best candidate using
/// pure analytical formulas — no fast_quote, no swap_base_in.
///
/// O(n) approach: extract all pool models in a single pass, group by middle_mint,
/// then test only the top-2 buy × top-2 sell combos per group (≤ 4 pairs).
fn find_best_candidate_analytical<'info>(
    instances: &[ProgramInstance<'info>],
    config: &BotConfig,
) -> Option<AnalyticalCandidate> {
    let start_token = config.start_token.unwrap_or(WSOL);
    let max_amount_in = config.max_amount_in;

    // ── Single pass: extract models, group by middle_mint ────────────────
    // Each instance that involves start_token produces:
    //   - a buy entry (input=start_token, output=middle_mint)
    //   - a sell entry (input=middle_mint, output=start_token)
    let mut group_mints: Vec<Pubkey> = Vec::new();
    let mut buy_groups: Vec<Vec<PoolEntry>> = Vec::new();
    let mut sell_groups: Vec<Vec<PoolEntry>> = Vec::new();

    for (i, inst) in instances.iter().enumerate() {
        let (base, quote) = inst.get_mints();

        let middle_mint = if *base == start_token {
            *quote
        } else if *quote == start_token {
            *base
        } else {
            continue;
        };

        // Find or create the group for this middle_mint
        let group_idx = if let Some(pos) = group_mints.iter().position(|m| *m == middle_mint) {
            pos
        } else {
            group_mints.push(middle_mint);
            buy_groups.push(Vec::new());
            sell_groups.push(Vec::new());
            group_mints.len() - 1
        };

        // Buy direction: start_token → middle_mint
        let buy_model = extract_pool_model(inst, start_token);
        if !matches!(buy_model, PoolModel::Opaque { .. }) {
            let mp = buy_model.marginal_price();
            buy_groups[group_idx].push(PoolEntry { idx: i, model: buy_model, marginal_price: mp });
        }

        // Sell direction: middle_mint → start_token
        let sell_model = extract_pool_model(inst, middle_mint);
        if !matches!(sell_model, PoolModel::Opaque { .. }) {
            let mp = sell_model.marginal_price();
            sell_groups[group_idx].push(PoolEntry { idx: i, model: sell_model, marginal_price: mp });
        }
    }

    // ── Per group: top-2 buy × top-2 sell, test ≤ 4 combos ─────────────
    let mut best: Option<AnalyticalCandidate> = None;

    for (g, middle_mint) in group_mints.iter().enumerate() {
        let buys = &buy_groups[g];
        let sells = &sell_groups[g];
        if buys.is_empty() || sells.is_empty() {
            continue;
        }

        let top_buys = top2_by_marginal_price(buys);
        let top_sells = top2_by_marginal_price(sells);

        for buy_local in top_buys.into_iter().flatten() {
            let buy = &buys[buy_local];
            for sell_local in top_sells.into_iter().flatten() {
                let sell = &sells[sell_local];

                // Cannot use the same pool for both legs
                if buy.idx == sell.idx {
                    continue;
                }

                let Some((opt_amount, est_profit, dlmm_capped)) =
                    analytical_estimate(&buy.model, &sell.model, max_amount_in)
                else {
                    continue;
                };

                if opt_amount < MIN_SEARCH_AMOUNT || est_profit <= config.min_profit {
                    continue;
                }

                let price_diff_pct = ((buy.marginal_price - sell.marginal_price) / sell.marginal_price) * 100.0;

                debug_eprintln!("");
                debug_eprintln!("");
                msg!(
                    "d={:.2} p={:.5} in={:.5} | f={:.4} {} | f={:.4} {}",
                    price_diff_pct,
                    est_profit as f64 / 1_000_000_000.0,
                    opt_amount as f64 / 1_000_000_000.0,
                    buy.model.fee(),
                    &instances[buy.idx].get_pool_id().to_string()[..1],
                    sell.model.fee(),
                    &instances[sell.idx].get_pool_id().to_string()[..1],
                );
                debug_eprintln!("");
                debug_eprintln!("");


                if best.as_ref().map_or(true, |b| est_profit > b.estimated_profit) {
                    best = Some(AnalyticalCandidate {
                        buy_idx: buy.idx,
                        sell_idx: sell.idx,
                        input_mint: start_token,
                        middle_mint: *middle_mint,
                        optimal_amount: opt_amount,
                        estimated_profit: est_profit,
                        dlmm_capped,
                        buy_model: buy.model.clone(),
                        sell_model: sell.model.clone(),
                    });
                }
            }
        }
    }

    best
}

/// Find the best candidate within a group of instance indices sharing the same token.
/// Uses top-2 pruning by marginal price to test ≤ 4 combos instead of O(k²).
fn find_best_candidate_for_group<'info>(
    instances: &[ProgramInstance<'info>],
    group: &[usize],
    start_token: Pubkey,
    middle_mint: Pubkey,
    max_amount_in: u64,
    min_profit: i128,
) -> Option<AnalyticalCandidate> {
    // Extract models + marginal prices for buy and sell directions
    let mut buy_entries: Vec<PoolEntry> = Vec::new();
    let mut sell_entries: Vec<PoolEntry> = Vec::new();

    for &i in group {
        let buy_model = extract_pool_model(&instances[i], start_token);
        if matches!(buy_model, PoolModel::Opaque { .. }) {
            debug_eprintln!("  pair: buy_pool[{}]={} -> Opaque, skip", i, instances[i].name());
        } else {
            let mp = buy_model.marginal_price();
            buy_entries.push(PoolEntry { idx: i, model: buy_model, marginal_price: mp });
        }

        let sell_model = extract_pool_model(&instances[i], middle_mint);
        if matches!(sell_model, PoolModel::Opaque { .. }) {
            debug_eprintln!("  pair: sell_pool[{}]={} -> Opaque, skip", i, instances[i].name());
        } else {
            let mp = sell_model.marginal_price();
            sell_entries.push(PoolEntry { idx: i, model: sell_model, marginal_price: mp });
        }
    }

    if buy_entries.is_empty() || sell_entries.is_empty() {
        return None;
    }

    let top_buys = top2_by_marginal_price(&buy_entries);
    let top_sells = top2_by_marginal_price(&sell_entries);

    let mut best: Option<AnalyticalCandidate> = None;

    for buy_local in top_buys.into_iter().flatten() {
        let buy = &buy_entries[buy_local];
        for sell_local in top_sells.into_iter().flatten() {
            let sell = &sell_entries[sell_local];

            if buy.idx == sell.idx {
                continue;
            }

            let estimate = analytical_estimate(&buy.model, &sell.model, max_amount_in);

            match &estimate {
                Some((optimal_amount, estimated_profit, dlmm_capped)) => {
                    debug_eprintln!(
                        "  pair: buy_pool[{}]={} sell_pool[{}]={} -> optimal_amount={} estimated_profit={} dlmm_capped={}",
                        buy.idx, buy.model.label(), sell.idx, sell.model.label(),
                        *optimal_amount as f64 / 1e9, *estimated_profit as f64 / 1e9, dlmm_capped
                    );
                }
                None => {
                    debug_eprintln!(
                        "  pair: buy_pool[{}]={} sell_pool[{}]={} -> analytical_estimate=None",
                        buy.idx, buy.model.label(), sell.idx, sell.model.label()
                    );
                }
            }

            let Some((opt_amount, est_profit, dlmm_capped)) = estimate else {
                continue;
            };

            if opt_amount < MIN_SEARCH_AMOUNT || est_profit <= min_profit {
                debug_eprintln!("  pair: filtered (optimal_amount={} < MIN or estimated_profit={} <= min_profit={})",
                    opt_amount, est_profit, min_profit);
                continue;
            }

            if best.as_ref().map_or(true, |b| est_profit > b.estimated_profit) {
                best = Some(AnalyticalCandidate {
                    buy_idx: buy.idx,
                    sell_idx: sell.idx,
                    input_mint: start_token,
                    middle_mint,
                    optimal_amount: opt_amount,
                    estimated_profit: est_profit,
                    dlmm_capped,
                    buy_model: buy.model.clone(),
                    sell_model: sell.model.clone(),
                });
            }
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

    let (buy_price, buy_inv_price) = buy_instance.get_prices()?;
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
            buy_inv_price,
            buy_fee_b_to_a,
            buy_fee_a_to_b,
            buy_max_in,
            buy_max_out,
            Pool::new(&input_mint),
            Pool::new(&middle_mint),
        )
    };

    let (sell_price, sell_inv_price) = sell_instance.get_prices()?;
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
            sell_inv_price,
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
    instances: &mut [ProgramInstance<'info>],
    candidate: &AnalyticalCandidate,
    config: &BotConfig,
    log_prefix: &str,
) -> Result<Option<ArbitragePath>> {
    #[cfg(any(test, feature = "debug"))]
    {
        let buy_inst = &instances[candidate.buy_idx];
        let sell_inst = &instances[candidate.sell_idx];
        let (buy_price, _) = buy_inst.get_prices().unwrap_or((0.0, 0.0));
        let (sell_price, _) = sell_inst.get_prices().unwrap_or((0.0, 0.0));
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
        )?
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
/// - Mode 0 (SINGLE_PAIR_MULTI_MARKET): 2-hop pair search across all instances
/// - Mode 1 (MULTI_HOP_CHAIN): 3-hop chain search with N-pool closed-form
/// - Mode 2 (MULTIPLE_TRADES): group by token, run 2-hop per group, pick best
#[inline(never)]
pub fn run_arbitrage_analytical<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance<'info>],
    config: &mut BotConfig,
) -> Result<Option<ArbitragePath>> {
    match config.mode {
        arb_mode::SINGLE_PAIR_MULTI_MARKET => {
            run_analytical_2hop(accounts, instances, config)
        }
        arb_mode::MULTI_HOP_CHAIN => {
            run_analytical_multihop(accounts, instances, config)
        }
        arb_mode::MULTIPLE_TRADES => {
            run_analytical_multi_trade(accounts, instances, config)
        }
        _ => Err(error!(SolarBError::InvalidMode)),
    }
}

/// 2-hop analytical arbitrage (mode 0: single pair, multi-market).
///
/// 1. Iterates all instance pairs, extracts PoolModels
/// 2. Uses analytical formulas to find optimal amount + estimated profit per pair
/// 3. Picks best candidate by estimated profit
/// 4. Validates with swap_base_in (1 call, or 8-iter golden section if DLMM-capped)
/// 5. Returns ArbitragePath
#[inline(never)]
fn run_analytical_2hop<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance<'info>],
    config: &mut BotConfig,
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

    validate_and_build_path(accounts, instances, &candidate, config, "Analytical 2hop")
}

// ─── Multiple trades analytical (mode 2) ────────────────────────────────────

/// Mode 2: group instances by their non-start token, run 2-hop per group, pick best.
#[inline(never)]
fn run_analytical_multi_trade<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance<'info>],
    config: &mut BotConfig,
) -> Result<Option<ArbitragePath>> {
    let start_token = config.start_token.unwrap_or(WSOL);

    // Group instance indices by their "other" mint (the one that's not start_token)
    let mut group_mints: Vec<Pubkey> = Vec::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();

    for (idx, inst) in instances.iter().enumerate() {
        let (base, quote) = inst.get_mints();
        let other_mint = if *base == start_token {
            *quote
        } else if *quote == start_token {
            *base
        } else {
            continue;
        };

        if let Some(pos) = group_mints.iter().position(|m| *m == other_mint) {
            groups[pos].push(idx);
        } else {
            group_mints.push(other_mint);
            groups.push(vec![idx]);
        }
    }

    debug_eprintln!("[ANA-MULTI]: {} token groups", groups.len());

    // Collect profit-positive candidates from all groups
    let mut candidates: Vec<AnalyticalCandidate> = Vec::new();

    for (group_idx, group) in groups.iter().enumerate() {
        if group.len() < 2 {
            continue;
        }

        debug_eprintln!(
            "[ANA-MULTI]: evaluating group {} (mint={}, {} pools)",
            group_idx, group_mints[group_idx], group.len()
        );

        if let Some(candidate) = find_best_candidate_for_group(
            instances, group, start_token, group_mints[group_idx],
            config.max_amount_in, config.min_profit,
        ) {
            debug_eprintln!(
                "[ANA-MULTI]: {} candidate: buy_model={} sell_model={} optimal_amount={} estimated_profit={} dlmm_capped={}",
                group_idx, candidate.buy_model.label(), candidate.sell_model.label(),
                candidate.optimal_amount as f64 / 1e9, candidate.estimated_profit as f64 / 1e9, candidate.dlmm_capped
            );
            if candidate.estimated_profit > 0 {
                candidates.push(candidate);
            }
        } else {
            debug_eprintln!("Analytical multi-trade: group {} no candidate found", group_idx);
        }
    }

    if candidates.is_empty() {
        debug_eprintln!("Analytical multi-trade: no candidate found");
        return Ok(None);
    }

    // Sort best first
    candidates.sort_by(|a, b| b.estimated_profit.cmp(&a.estimated_profit));

    // Try each: if best validates with profit, return it; otherwise try next
    for candidate in candidates.iter() {
        debug_eprintln!(
            "Analytical multi-trade: buy_model={}, sell_model={}, optimal_amount={}, estimated_profit={}",
            candidate.buy_model.label(),
            candidate.sell_model.label(),
            candidate.optimal_amount as f64 / 1_000_000_000.0,
            candidate.estimated_profit as f64 / 1_000_000_000.0
        );

        if let Some(path) = validate_and_build_path(accounts, instances, candidate, config, "Analytical multi-trade")? {
            if path.profit > 0 {
                return Ok(Some(path));
            }
            debug_eprintln!("Analytical multi-trade: candidate validated but profit={}, trying next...", path.profit);
            continue;
        }
        debug_eprintln!("Analytical multi-trade: candidate rejected, trying next...");
    }

    debug_eprintln!("Analytical multi-trade: all candidates rejected");
    Ok(None)
}

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

/// Find the best 3-hop analytical candidate: start → A → B → start.
fn find_best_candidate_analytical_multihop<'info>(
    instances: &[ProgramInstance<'info>],
    config: &BotConfig,
) -> Option<MultiHopCandidate> {
    let start_token = config.start_token.unwrap_or(WSOL);
    let max_amount_in = config.max_amount_in;
    let mut best: Option<MultiHopCandidate> = None;

    for (i, inst_1) in instances.iter().enumerate() {
        let (base_1, quote_1) = inst_1.get_mints();

        // Hop 1: start_token → mint_a
        let mint_a = if *base_1 == start_token {
            *quote_1
        } else if *quote_1 == start_token {
            *base_1
        } else {
            continue;
        };

        let model_1 = extract_pool_model(inst_1, start_token);
        if matches!(model_1, PoolModel::Opaque { .. }) {
            continue;
        }

        for (j, inst_2) in instances.iter().enumerate() {
            if j == i {
                continue;
            }
            let (base_2, quote_2) = inst_2.get_mints();

            // Hop 2: mint_a → mint_b (mint_b must not be start_token)
            let mint_b = if *base_2 == mint_a {
                *quote_2
            } else if *quote_2 == mint_a {
                *base_2
            } else {
                continue;
            };
            if mint_b == start_token {
                continue; // That's a 2-hop, handled by mode 0/2
            }

            let model_2 = extract_pool_model(inst_2, mint_a);
            if matches!(model_2, PoolModel::Opaque { .. }) {
                continue;
            }

            for (k, inst_3) in instances.iter().enumerate() {
                if k == i || k == j {
                    continue;
                }
                let (base_3, quote_3) = inst_3.get_mints();

                // Hop 3: mint_b → start_token
                let has_pair = (*base_3 == mint_b && *quote_3 == start_token)
                    || (*quote_3 == mint_b && *base_3 == start_token);
                if !has_pair {
                    continue;
                }

                let model_3 = extract_pool_model(inst_3, mint_b);
                if matches!(model_3, PoolModel::Opaque { .. }) {
                    continue;
                }

                let models = [model_1.clone(), model_2.clone(), model_3.clone()];
                let Some((opt_amount, est_profit, dlmm_capped)) =
                    analytical_estimate_nhop(&models, max_amount_in)
                else {
                    continue;
                };

                if opt_amount < MIN_SEARCH_AMOUNT || est_profit <= config.min_profit {
                    continue;
                }

                if best.as_ref().map_or(true, |b| est_profit > b.estimated_profit) {
                    best = Some(MultiHopCandidate {
                        indices: [i, j, k],
                        mints: [start_token, mint_a, mint_b, start_token],
                        models,
                        optimal_amount: opt_amount,
                        estimated_profit: est_profit,
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
    instances: &mut [ProgramInstance<'info>],
    indices: &[usize; 3],
    mints: &[Pubkey; 4],
    amount_in: u64,
    clock: &Clock,
) -> Result<i128> {
    let mut current = amount_in;
    for hop in 0..3 {
        current = instances[indices[hop]].swap_base_in(accounts, mints[hop], current, clock)?;
    }
    Ok(current as i128 - amount_in as i128)
}

/// Golden section refinement for a 3-hop path.
fn golden_section_refine_nhop<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance<'info>],
    indices: &[usize; 3],
    mints: &[Pubkey; 4],
    hint: u64,
    max_amount: u64,
    clock: &Clock,
) -> Result<(u64, i128)> {
    let mut a = (hint / 2).max(MIN_SEARCH_AMOUNT);
    let mut b = (hint.saturating_mul(2)).min(max_amount);
    let mut c = b - ((b - a) as f64 / GOLDEN_RATIO) as u64;
    let mut d = a + ((b - a) as f64 / GOLDEN_RATIO) as u64;
    let mut fc = simulate_nhop(accounts, instances, indices, mints, c, clock).unwrap_or(i128::MIN);
    let mut fd = simulate_nhop(accounts, instances, indices, mints, d, clock).unwrap_or(i128::MIN);

    for _ in 0..DLMM_REFINE_ITERATIONS {
        if b - a < CONVERGENCE {
            break;
        }
        if fc > fd {
            b = d;
            d = c;
            fd = fc;
            c = b - ((b - a) as f64 / GOLDEN_RATIO) as u64;
            fc = simulate_nhop(accounts, instances, indices, mints, c, clock)
                .unwrap_or(i128::MIN);
        } else {
            a = c;
            c = d;
            fc = fd;
            d = a + ((b - a) as f64 / GOLDEN_RATIO) as u64;
            fd = simulate_nhop(accounts, instances, indices, mints, d, clock)
                .unwrap_or(i128::MIN);
        }
    }

    let optimal = (a + b) / 2;
    let profit = simulate_nhop(accounts, instances, indices, mints, optimal, clock)?;
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

        let (price, inv_price) = inst.get_prices()?;
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
                inv_price,
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

/// 3-hop analytical arbitrage (mode 1: MULTI_HOP_CHAIN).
#[inline(never)]
fn run_analytical_multihop<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance<'info>],
    config: &mut BotConfig,
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
