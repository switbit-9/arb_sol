use crate::arbitrage::algo_2::ArbitragePath;
use crate::arbitrage::base::Edge;
use crate::programs::{ProgramInstance, ProgramMeta};
use crate::utils::bot_config::BotConfig;
use anchor_lang::prelude::*;
use anchor_spl::token::spl_token::native_mint::ID as WSOL;

use super::formulas::analytical_estimate;
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
    program_1: &dyn ProgramMeta,
    program_2: &dyn ProgramMeta,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    amount_in: u64,
    clock: &Clock,
) -> Result<i128> {
    let mid_out = program_1.swap_base_in(accounts, input_mint, amount_in, clock)?;
    let final_out = program_2.swap_base_in(accounts, middle_mint, mid_out, clock)?;
    Ok(final_out as i128 - amount_in as i128)
}

/// Small golden section search to refine around the analytical hint.
/// Used when DLMM active bin capacity is the binding constraint.
fn golden_section_refine<'info>(
    accounts: &[AccountInfo<'info>],
    program_1: &dyn ProgramMeta,
    program_2: &dyn ProgramMeta,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    hint: u64,
    max_amount: u64,
    clock: &Clock,
) -> Result<(u64, i128)> {
    let lower = (hint / 2).max(MIN_SEARCH_AMOUNT);
    let upper = ((hint as u128 * 3 / 2) as u64).min(max_amount);

    if upper <= lower {
        let profit = simulate_2hop(
            accounts, program_1, program_2, input_mint, middle_mint, hint, clock,
        )?;
        return Ok((hint, profit));
    }

    let mut a = lower;
    let mut b = upper;
    let mut c = b - ((b - a) as f64 / GOLDEN_RATIO) as u64;
    let mut d = a + ((b - a) as f64 / GOLDEN_RATIO) as u64;
    let mut fc = simulate_2hop(
        accounts, program_1, program_2, input_mint, middle_mint, c, clock,
    )
    .unwrap_or(i128::MIN);
    let mut fd = simulate_2hop(
        accounts, program_1, program_2, input_mint, middle_mint, d, clock,
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
                accounts, program_1, program_2, input_mint, middle_mint, c, clock,
            )
            .unwrap_or(i128::MIN);
        } else {
            a = c;
            c = d;
            fc = fd;
            d = a + ((b - a) as f64 / GOLDEN_RATIO) as u64;
            fd = simulate_2hop(
                accounts, program_1, program_2, input_mint, middle_mint, d, clock,
            )
            .unwrap_or(i128::MIN);
        }
    }

    let optimal = (a + b) / 2;
    let profit = simulate_2hop(
        accounts, program_1, program_2, input_mint, middle_mint, optimal, clock,
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

/// Build edges from instance pairs and find the best candidate using
/// pure analytical formulas — no fast_quote, no swap_base_in.
///
/// For each pair of instances (i, j) where i != j and they share a token pair
/// with a common root token (SOL), extract PoolModels and compute the
/// closed-form optimal + estimated profit. Keep the best.
fn find_best_candidate_analytical<'info>(
    instances: &[ProgramInstance<'info>],
    config: &BotConfig,
) -> Option<AnalyticalCandidate> {
    let start_token = config.start_token.unwrap_or(WSOL);
    let max_amount_in = config.max_amount_in;

    let mut best: Option<AnalyticalCandidate> = None;

    for (i, inst_buy) in instances.iter().enumerate() {
        let (base_buy, quote_buy) = inst_buy.get_mints();

        // Determine if this instance can serve as the buy leg (start_token → middle_token)
        // Buy leg: we input start_token and get middle_token out
        let middle_mint = if *base_buy == start_token {
            // Input = base (start_token), output = quote
            *quote_buy
        } else if *quote_buy == start_token {
            // Input = quote (start_token), output = base
            *base_buy
        } else {
            continue; // This pool doesn't involve start_token
        };

        // Extract pool model for buy direction
        let buy_model = extract_pool_model(inst_buy.as_ref(), start_token);
        if matches!(buy_model, PoolModel::Opaque) {
            continue;
        }

        for (j, inst_sell) in instances.iter().enumerate() {
            if i == j {
                continue;
            }

            let (base_sell, quote_sell) = inst_sell.get_mints();

            // Sell leg must: input middle_token, output start_token
            let has_pair = (*base_sell == middle_mint && *quote_sell == start_token)
                || (*quote_sell == middle_mint && *base_sell == start_token);
            if !has_pair {
                continue;
            }

            // Extract pool model for sell direction (input = middle_mint)
            let sell_model = extract_pool_model(inst_sell.as_ref(), middle_mint);
            if matches!(sell_model, PoolModel::Opaque) {
                continue;
            }

            // Compute analytical optimal + estimated profit
            let Some((opt_amount, est_profit, dlmm_capped)) =
                analytical_estimate(&buy_model, &sell_model, max_amount_in)
            else {
                continue;
            };

            if opt_amount < MIN_SEARCH_AMOUNT || est_profit <= config.min_profit {
                continue;
            }

            // Keep best by estimated profit
            if best.as_ref().map_or(true, |b| est_profit > b.estimated_profit) {
                best = Some(AnalyticalCandidate {
                    buy_idx: i,
                    sell_idx: j,
                    input_mint: start_token,
                    middle_mint,
                    optimal_amount: opt_amount,
                    estimated_profit: est_profit,
                    dlmm_capped,
                    buy_model: buy_model.clone(),
                    sell_model: sell_model.clone(),
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

// ─── Main entry point ───────────────────────────────────────────────────────

/// Fully independent analytical arbitrage: finds candidates AND computes optimal
/// amounts using closed-form formulas. No dependency on algo_2.
///
/// 1. Iterates all instance pairs, extracts PoolModels
/// 2. Uses analytical formulas to find optimal amount + estimated profit per pair
/// 3. Picks best candidate by estimated profit
/// 4. Validates with swap_base_in (1 call, or 8-iter golden section if DLMM-capped)
/// 5. Returns ArbitragePath
#[inline(never)]
pub fn run_arbitrage_analytical<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance<'info>],
    config: &mut BotConfig,
) -> Result<Option<ArbitragePath>> {
    let candidate = find_best_candidate_analytical(instances, config);

    let Some(cand) = candidate else {
        msg!("Ana: no candidate");
        return Ok(None);
    };

    msg!(
        "Ana: m1={}, m2={}, est_opt={}, est_p={}",
        cand.buy_model.label(),
        cand.sell_model.label(),
        cand.optimal_amount,
        cand.estimated_profit
    );

    #[cfg(any(test, feature = "debug"))]
    {
        let buy_inst = instances[cand.buy_idx].as_ref();
        let sell_inst = instances[cand.sell_idx].as_ref();
        let (buy_price, _) = buy_inst.get_prices().unwrap_or((0.0, 0.0));
        let (sell_price, _) = sell_inst.get_prices().unwrap_or((0.0, 0.0));
        let buy_fee = buy_inst.get_fee_factor().unwrap_or((1.0, 1.0)).0;
        let sell_fee = sell_inst.get_fee_factor().unwrap_or((1.0, 1.0)).0;
        debug_eprintln!("");
        debug_eprintln!(
            "Best candidate | profit: {} SOL | in: {} out: {}",
            cand.estimated_profit as f64 / 1_000_000_000.0,
            cand.optimal_amount as f64 / 1_000_000_000.0,
            (cand.optimal_amount as i128 + cand.estimated_profit) as f64 / 1_000_000_000.0,
        );
        debug_eprintln!(
            "  {} -> {} (pool {} @ p={:.6} fee={:.4})",
            cand.input_mint,
            cand.middle_mint,
            buy_inst.get_pool_id(),
            buy_price,
            buy_fee,
        );
        debug_eprintln!(
            "  {} -> {} (pool {} @ p={:.6} fee={:.4})",
            cand.middle_mint,
            cand.input_mint,
            sell_inst.get_pool_id(),
            sell_price,
            sell_fee,
        );
        debug_eprintln!("");
    }

    let program_1 = &instances[cand.buy_idx];
    let program_2 = &instances[cand.sell_idx];

    // Validate / refine with actual swap simulation
    let (optimal_amount, profit) = if cand.dlmm_capped {
        golden_section_refine(
            accounts,
            program_1.as_ref(),
            program_2.as_ref(),
            cand.input_mint,
            cand.middle_mint,
            cand.optimal_amount,
            config.max_amount_in,
            &config.clock,
        )?
    } else {
        let profit = simulate_2hop(
            accounts,
            program_1.as_ref(),
            program_2.as_ref(),
            cand.input_mint,
            cand.middle_mint,
            cand.optimal_amount,
            &config.clock,
        )?;
        (cand.optimal_amount, profit)
    };

    msg!("Ana: final opt={}, profit={}", optimal_amount, profit);

    if !config.test && (profit <= 0 || optimal_amount == 0) {
        msg!("Ana: rejected, profit={}", profit);
        return Ok(None);
    }

    let edges = build_edges(
        program_1.as_ref(),
        program_2.as_ref(),
        cand.input_mint,
        cand.middle_mint,
    )?;

    let final_amount = (optimal_amount as i128).checked_add(profit).unwrap_or(0) as u128;

    Ok(Some(ArbitragePath {
        edges,
        profit,
        final_amount,
        start_amount: optimal_amount,
    }))
}
