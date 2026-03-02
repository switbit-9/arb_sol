use crate::arbitrage::base::Edge;
use crate::arbitrage::utils::find_instance_by_pool_id;
use crate::programs::{ProgramInstance, ProgramMeta};
use crate::utils::bot_config::BotConfig;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::pubkey::Pubkey;

/// Configuration for the optimization search - OPTIMIZED FOR LOW CU USAGE
const GOLDEN_RATIO: f64 = 1.618033988749895; // (1 + sqrt(5)) / 2
const MAX_ITERATIONS: usize = 12; // Reduced from 25 to save CU
const CONVERGENCE_THRESHOLD: u64 = 10_000; // 0.1 SOL - larger = fewer iterations
const MIN_SEARCH_AMOUNT: u64 = 1_000; // 0.000001 SOL minimum
const MIN_PROFIT_THRESHOLD: i128 = 50_000; // 0.00005 SOL - skip refinement if below


/// Simulate a full arbitrage path and return the profit (DLMM + AMM)
#[inline]
fn simulate_path<'info>(
    accounts: &[AccountInfo<'info>],
    program_1: &Box<dyn ProgramMeta + 'info>,
    program_2: &Box<dyn ProgramMeta + 'info>,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    amount_in: u64,
    clock: &Clock,
) -> Result<i128> {
    let token_out = program_1.swap_base_in(accounts, input_mint, amount_in, clock)?;
    let sol_out = program_2.swap_base_in(accounts, middle_mint, token_out, clock)?;

    Ok(sol_out as i128 - amount_in as i128)
}

/// Golden section search to find the optimal input amount that maximizes profit
/// Optimized for low CU usage
fn golden_section_search<'info>(
    accounts: &[AccountInfo<'info>],
    program_1: &ProgramInstance<'info>,
    program_2: &ProgramInstance<'info>,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    min_amount: u64,
    max_amount: u64,
    clock: &Clock,
) -> Result<(u64, i128)> {
    if max_amount <= min_amount {
        return Ok((min_amount, i128::MIN));
    }

    let mut a = min_amount;
    let mut b = max_amount;

    // Initial golden section points
    let mut c = b - ((b - a) as f64 / GOLDEN_RATIO) as u64;
    let mut d = a + ((b - a) as f64 / GOLDEN_RATIO) as u64;

    // Evaluate at initial points
    let mut fc = simulate_path(
        accounts,
        program_1,
        program_2,
        input_mint,
        middle_mint,
        c,
        clock,
    )
    .unwrap_or(i128::MIN);

    let mut fd = simulate_path(
        accounts,
        program_1,
        program_2,
        input_mint,
        middle_mint,
        d,
        clock,
    )
    .unwrap_or(i128::MIN);

    // debug_eprintln!(
    //     "Golden search initial: a={}, b={}, c={}, d={}, fc={}, fd={}",
    //     a, b, c, d, fc, fd
    // );

    // Early exit if both initial points are below profit threshold
    // Simplified: fc < 0 && fc < MIN_PROFIT_THRESHOLD is redundant since MIN_PROFIT_THRESHOLD > 0
    if fc < MIN_PROFIT_THRESHOLD && fd < MIN_PROFIT_THRESHOLD {
        // debug_eprintln!("Early exit: both initial points unprofitable");
        return Ok((if fc > fd { c } else { d }, fc.max(fd)));
    }

    let mut consecutive_decreases = 0u8;
    let mut last_best = fc.max(fd);

    for iteration in 0..MAX_ITERATIONS {
        // Avoid unused warning when debug logging is disabled
        let _ = iteration;
        if b - a < CONVERGENCE_THRESHOLD {
            // debug_eprintln!("Converged after {} iterations", iteration);
            break;
        }

        if fc > fd {
            // Maximum is in [a, d]
            b = d;
            d = c;
            fd = fc;
            c = b - ((b - a) as f64 / GOLDEN_RATIO) as u64;
            fc = simulate_path(
                accounts,
                program_1,
                program_2,
                input_mint,
                middle_mint,
                c,
                clock,
            )
            .unwrap_or(i128::MIN);
        } else {
            // Maximum is in [c, b]
            a = c;
            c = d;
            fc = fd;
            d = a + ((b - a) as f64 / GOLDEN_RATIO) as u64;
            fd = simulate_path(
                accounts,
                program_1,
                program_2,
                input_mint,
                middle_mint,
                d,
                clock,
            )
            .unwrap_or(i128::MIN);
        }

        // Early exit if profit is consistently decreasing
        let current_best = fc.max(fd);
        if current_best < last_best {
            consecutive_decreases += 1;
            if consecutive_decreases >= 3 {
                // debug_eprintln!("Early exit: profit decreasing for 3 iterations");
                break;
            }
        } else {
            consecutive_decreases = 0;
        }
        last_best = current_best;
    }

    // Return the midpoint and its profit
    let optimal = (a + b) / 2;
    let profit = simulate_path(
        accounts,
        program_1,
        program_2,
        input_mint,
        middle_mint,
        optimal,
        clock,
    )?;

    #[cfg(any(test, feature = "debug"))]
    {
    debug_eprintln!(
        "Golden search result: optimal={} ({} SOL), profit={} ({} SOL)",
        optimal,
        optimal as f64 / 1_000_000_000.0,
        profit,
        profit as f64 / 1_000_000_000.0
    );
    }

    Ok((optimal, profit))
}

/// Hybrid search: OPTIMIZED for low CU usage
/// Reduced grid points and conditional golden section refinement
fn hybrid_search<'info>(
    accounts: &[AccountInfo<'info>],
    program_1: &ProgramInstance<'info>,
    program_2: &ProgramInstance<'info>,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    min_amount: u64,
    max_amount: u64,
    clock: &Clock,
) -> Result<(u64, i128)> {
    // // Quick check: is this even potentially profitable?
    // let (is_profitable, hint_amount) = quick_profit_check(
    //     accounts,
    //     program_1,
    //     program_2,
    //     input_mint,
    //     middle_mint,
    //     min_amount,
    //     max_amount,
    //     clock,
    // );

    // #[cfg(any(test, feature = "debug"))]
    // {
    //     debug_eprintln!(
    //         "Quick check: is_profitable={}, hint_amount={}",
    //         is_profitable, hint_amount
    //     );
    // }

    // if !is_profitable {
    //     #[cfg(any(test, feature = "debug"))]
    //     {
    //         debug_eprintln!("Quick check: not profitable, skipping full search");
    //     }
    //     return Ok((hint_amount, i128::MIN));
    // }

    // Phase 1: Reduced grid search - only 6 points instead of 10
    let grid_points = [0.00001, 0.0001, 0.001, 0.01, 0.05, 0.20, 0.50, 0.7, 1.0];

    let mut best_amount = min_amount;
    let mut best_profit = i128::MIN;
    let mut best_idx = 0usize;
    let mut prev_profit = i128::MIN;
    let mut last_drop: i128 = 0;

    for (idx, &fraction) in grid_points.iter().enumerate() {
        let amount = min_amount + ((max_amount - min_amount) as f64 * fraction) as u64;
        let profit = simulate_path(
            accounts,
            program_1,
            program_2,
            input_mint,
            middle_mint,
            amount,
            clock,
        )
        .unwrap_or(i128::MIN);

        #[cfg(any(test, feature = "debug"))]
        {
        debug_eprintln!(
                "Grid search [{:.0}%]: amount={} ({} SOL), profit={} ({} SOL)",
                fraction * 100.0,
                amount,
                amount as f64 / 1_000_000_000.0,
                profit,
                profit as f64 / 1_000_000_000.0
            );
        }

        if profit > best_profit {
            best_profit = profit;
            best_amount = amount;
            best_idx = idx;
        }

        // Only apply early exit after we've seen a positive profit.
        // At tiny amounts, losses scale with size (fee overhead) and would
        // falsely trigger the accelerating-drop heuristic before we reach
        // the profitable region.
        if best_profit > 0 && prev_profit != i128::MIN && profit < prev_profit {
            let current_drop = prev_profit - profit;
            if last_drop > 0 && current_drop > last_drop {
                break;
            }
            last_drop = current_drop;
        } else if profit >= prev_profit {
            last_drop = 0;
        }
        prev_profit = profit;
    }

    #[cfg(any(test, feature = "debug"))]
    {
        debug_eprintln!(
            "Grid search best: amount={}, profit={}",
            best_amount, best_profit
        );
    }

    // OPTIMIZATION: Skip golden section refinement if profit is too low
    // This saves significant CU for unprofitable paths
    if best_profit < MIN_PROFIT_THRESHOLD {
        #[cfg(any(test, feature = "debug"))]
        {
            debug_eprintln!("Profit below threshold, skipping golden section refinement");
        }
        return Ok((best_amount, best_profit));
    }

    // Phase 2: Refine around the best point using golden section
    let lower_bound = if best_idx > 0 {
        min_amount + ((max_amount - min_amount) as f64 * grid_points[best_idx - 1]) as u64
    } else {
        min_amount
    };

    let upper_bound = if best_idx < grid_points.len() - 1 {
        min_amount + ((max_amount - min_amount) as f64 * grid_points[best_idx + 1]) as u64
    } else {
        max_amount
    };

    // Only refine if the range is large enough to matter
    // if upper_bound - lower_bound < CONVERGENCE_THRESHOLD {
    //     debug_eprintln!("Range too small, skipping golden section refinement");
    //     return Ok((best_amount, best_profit));
    // }

    // Run golden section in the refined region
    let (refined_amount, refined_profit) = golden_section_search(
        accounts,
        program_1,
        program_2,
        input_mint,
        middle_mint,
        lower_bound,
        upper_bound,
        clock,
    )?;

    // Return the better of grid best and refined result
    if refined_profit > best_profit {
        Ok((refined_amount, refined_profit))
    } else {
        Ok((best_amount, best_profit))
    }
}


/// Result of the AMM↔DLMM analytical formula.
struct AmmDlmmHint {
    /// Optimal input amount (assuming linear DLMM)
    optimal: u64,
    /// Amount of middle token entering the DLMM at the optimal input
    mid_amount: u64,
    /// Whether the AMM is the first leg (true) or second leg (false)
    amm_is_first: bool,
}

/// AMM↔DLMM analytical formula. Computes optimal input assuming DLMM is linear.
///
/// Formula: Δx* = (√(X·Y·f_amm·P_dlmm·f_dlmm) − X) / f_amm  (standard case)
/// Also returns the mid amount so callers can check against active bin capacity.
fn analytical_hint_amm_dlmm(
    program_1: &dyn ProgramMeta,
    program_2: &dyn ProgramMeta,
    input_mint: Pubkey,
    middle_mint: Pubkey,
) -> Option<AmmDlmmHint> {
    let name_1 = program_1.name();
    let name_2 = program_2.name();

    let (amm, dlmm, amm_is_first): (&dyn ProgramMeta, &dyn ProgramMeta, bool) =
        if name_2 == "MeteoraDLMM" && name_1 != "MeteoraDLMM" {
            (program_1, program_2, true)
        } else if name_1 == "MeteoraDLMM" && name_2 != "MeteoraDLMM" {
            (program_2, program_1, false)
        } else {
            return None;
        };

    let (base_vault, quote_vault) = amm.get_vault_amounts().ok()?;
    let amm_base = amm.get_base_token();
    let amm_input_mint = if amm_is_first { input_mint } else { middle_mint };

    let (r_in, r_out) = if amm_input_mint == amm_base {
        (base_vault as f64, quote_vault as f64)
    } else {
        (quote_vault as f64, base_vault as f64)
    };
    if r_in <= 0.0 || r_out <= 0.0 { return None; }

    let (fee_a_to_b, fee_b_to_a) = amm.get_fee_factor().ok()?;
    let f_amm = if amm_input_mint == amm_base { fee_a_to_b } else { fee_b_to_a };

    let dlmm_input_mint = if amm_is_first { middle_mint } else { input_mint };
    let (dlmm_price_base, dlmm_price_inv) = dlmm.get_prices().ok()?;
    let dlmm_base = dlmm.get_base_token();
    let p_dlmm = if dlmm_input_mint == dlmm_base { dlmm_price_base } else { dlmm_price_inv };

    let (f_dlmm, _) = dlmm.get_fee_factor().ok()?;

    if p_dlmm <= 0.0 || !p_dlmm.is_finite() { return None; }

    let is_pump_sell = amm.name() == "PumpAmm" && amm_input_mint == amm_base;
    let k = r_in * r_out;

    let optimal = if amm_is_first {
        if is_pump_sell {
            (k * f_amm * p_dlmm * f_dlmm).sqrt() - r_in
        } else {
            ((k * f_amm * p_dlmm * f_dlmm).sqrt() - r_in) / f_amm
        }
    } else {
        if is_pump_sell {
            let b = p_dlmm * f_dlmm;
            ((k * b * f_amm).sqrt() - r_in) / b
        } else {
            let a = p_dlmm * f_dlmm * f_amm;
            ((k * a).sqrt() - r_in) / a
        }
    };

    if optimal <= 0.0 || !optimal.is_finite() { return None; }

    // Compute how much middle token enters the DLMM at the analytical optimal
    let dx = optimal;
    let mid = if amm_is_first {
        // AMM output = mid tokens entering DLMM
        if is_pump_sell {
            r_out * dx / (r_in + dx) * f_amm
        } else {
            r_out * dx * f_amm / (r_in + dx * f_amm)
        }
    } else {
        // DLMM is first: input goes directly into DLMM
        dx
    };

    Some(AmmDlmmHint {
        optimal: optimal as u64,
        mid_amount: mid as u64,
        amm_is_first,
    })
}


/// Find optimal amount for 2-hop path using hybrid search
pub fn find_optimal_amount<'info>(
    program_1: &ProgramInstance<'info>,
    program_2: &ProgramInstance<'info>,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    accounts: &[AccountInfo<'info>],
    config: &mut BotConfig,
) -> Result<(u64, i128)> {

    let program_1_max_in = program_1.get_max_amount_in(accounts, input_mint).unwrap_or(0);
    let program_1_max_out = program_1.get_max_amount_out(accounts, input_mint).unwrap_or(0);
    let program_2_max_in = program_2.get_max_amount_in(accounts, middle_mint).unwrap_or(0);
    let program_2_max_out = program_2.get_max_amount_out(accounts, middle_mint).unwrap_or(0);

    // If program_1 can't produce any output tokens, the path is dead
    if program_1_max_out == 0 {
        msg!("OptAmt: p1 max_out=0, dead");
        return Ok((0, 0));
    }

    let max_amount = config.max_amount_in
        .min(program_1_max_in)
        .min(program_2_max_out);
    let min_amount = MIN_SEARCH_AMOUNT;

    msg!("OpAm: max_in={}, p1_in={}, p1_out={}, p2_in={}, p2_out={}, eff_max={}",
        config.max_amount_in, program_1_max_in, program_1_max_out,
        program_2_max_in, program_2_max_out, max_amount);

    #[cfg(test)]
    {
        debug_eprintln!(
            "PROGRAM 1: MAX SOL IN {:?} -> MAX TOKEN OUT {:?}",
            program_1_max_in as f64 / 1_000_000_000.0,
            program_1_max_out as f64 / 1_000_000.0
        );
        debug_eprintln!(
            "PROGRAM 2: MAX TOKEN IN {:?} -> MAX SOL OUT {:?}",
            program_2_max_in as f64 / 1_000_000.0,
            program_2_max_out as f64 / 1_000_000_000.0
        );
        debug_eprintln!("max_amount: {:?}", max_amount as f64 / 1_000_000_000.0);
    }

    if max_amount <= min_amount {
        msg!("OptAmt: max {} <= min {}, skip", max_amount, min_amount);
        return Ok((0, 0));
    }

    let (optimal_amount, profit) = hybrid_search(
        accounts,
        program_1,
        program_2,
        input_mint,
        middle_mint,
        min_amount,
        max_amount,
        &config.clock,
    )?;
    msg!("OptAmt: result in={}, profit={}", optimal_amount, profit);
    debug_eprintln!("optimal_amount: {:?}", optimal_amount as f64 / 1_000_000_000.0);
    Ok((optimal_amount, profit))
}

/// Simulate N-hop arbitrage path and return the profit
/// Works for any number of edges (2-hop, 3-hop, etc.)
#[inline]
fn simulate_n_hop_path<'info>(
    accounts: &[AccountInfo<'info>],
    edges: &[Edge],
    instances: &[ProgramInstance<'info>],
    amount_in: u64,
    clock: &Clock,
) -> Result<i128> {
    let mut current_amount = amount_in;

    for edge in edges.iter() {
        let instance = find_instance_by_pool_id(instances, &edge.pool_id)?;
        current_amount = instance.swap_base_in(
            accounts,
            edge.left.mint_account,
            current_amount,
            clock,
        )?;
    }

    Ok(current_amount as i128 - amount_in as i128)
}

/// Quick profitability check for N-hop path
fn quick_profit_check_n_hop<'info>(
    accounts: &[AccountInfo<'info>],
    edges: &[Edge],
    instances: &[ProgramInstance<'info>],
    min_amount: u64,
    max_amount: u64,
    clock: &Clock,
) -> (bool, u64) {
    let test_points: [u64; 4] = [
        min_amount + ((max_amount - min_amount) / 10),
        (min_amount + max_amount) / 2,
        max_amount - ((max_amount - min_amount) / 10),
        max_amount,
    ];

    for &amount in &test_points {
        let profit = simulate_n_hop_path(accounts, edges, instances, amount, clock)
            .unwrap_or(i128::MIN);

        if profit > MIN_PROFIT_THRESHOLD {
            return (true, amount);
        }
    }

    (false, min_amount)
}

/// Golden section search for N-hop path
fn golden_section_search_n_hop<'info>(
    accounts: &[AccountInfo<'info>],
    edges: &[Edge],
    instances: &[ProgramInstance<'info>],
    min_amount: u64,
    max_amount: u64,
    clock: &Clock,
) -> Result<(u64, i128)> {
    if max_amount <= min_amount {
        return Ok((min_amount, i128::MIN));
    }

    let mut a = min_amount;
    let mut b = max_amount;

    let mut c = b - ((b - a) as f64 / GOLDEN_RATIO) as u64;
    let mut d = a + ((b - a) as f64 / GOLDEN_RATIO) as u64;

    let mut fc = simulate_n_hop_path(accounts, edges, instances, c, clock)
        .unwrap_or(i128::MIN);
    let mut fd = simulate_n_hop_path(accounts, edges, instances, d, clock)
        .unwrap_or(i128::MIN);

    if fc < MIN_PROFIT_THRESHOLD && fd < MIN_PROFIT_THRESHOLD {
        return Ok((if fc > fd { c } else { d }, fc.max(fd)));
    }

    let mut consecutive_decreases = 0u8;
    let mut last_best = fc.max(fd);

    for _iteration in 0..MAX_ITERATIONS {
        if b - a < CONVERGENCE_THRESHOLD {
            break;
        }

        if fc > fd {
            b = d;
            d = c;
            fd = fc;
            c = b - ((b - a) as f64 / GOLDEN_RATIO) as u64;
            fc = simulate_n_hop_path(accounts, edges, instances, c, clock)
                .unwrap_or(i128::MIN);
        } else {
            a = c;
            c = d;
            fc = fd;
            d = a + ((b - a) as f64 / GOLDEN_RATIO) as u64;
            fd = simulate_n_hop_path(accounts, edges, instances, d, clock)
                .unwrap_or(i128::MIN);
        }

        let current_best = fc.max(fd);
        if current_best < last_best {
            consecutive_decreases += 1;
            if consecutive_decreases >= 3 {
                break;
            }
        } else {
            consecutive_decreases = 0;
        }
        last_best = current_best;
    }

    let optimal = (a + b) / 2;
    let profit = simulate_n_hop_path(accounts, edges, instances, optimal, clock)?;

    Ok((optimal, profit))
}

/// Hybrid search for N-hop path
fn hybrid_search_n_hop<'info>(
    accounts: &[AccountInfo<'info>],
    edges: &[Edge],
    instances: &[ProgramInstance<'info>],
    min_amount: u64,
    max_amount: u64,
    clock: &Clock,
) -> Result<(u64, i128)> {
    let (is_profitable, hint_amount) = quick_profit_check_n_hop(
        accounts, edges, instances, min_amount, max_amount, clock,
    );

    if !is_profitable {
        return Ok((hint_amount, i128::MIN));
    }

    let grid_points = [0.05, 0.20, 0.40, 0.60, 0.80, 1.0];

    let mut best_amount = min_amount;
    let mut best_profit = i128::MIN;
    let mut best_idx = 0usize;
    let mut prev_profit = i128::MIN;
    let mut last_drop: i128 = 0;

    for (idx, &fraction) in grid_points.iter().enumerate() {
        let amount = min_amount + ((max_amount - min_amount) as f64 * fraction) as u64;
        let profit = simulate_n_hop_path(accounts, edges, instances, amount, clock)
            .unwrap_or(i128::MIN);

        if profit > best_profit {
            best_profit = profit;
            best_amount = amount;
            best_idx = idx;
        }

        // Only apply early exit after we've seen a positive profit.
        if best_profit > 0 && prev_profit != i128::MIN && profit < prev_profit {
            let current_drop = prev_profit - profit;
            if last_drop > 0 && current_drop > last_drop {
                break;
            }
            last_drop = current_drop;
        } else if profit >= prev_profit {
            last_drop = 0;
        }
        prev_profit = profit;
    }

    if best_profit < MIN_PROFIT_THRESHOLD {
        return Ok((best_amount, best_profit));
    }

    let lower_bound = if best_idx > 0 {
        min_amount + ((max_amount - min_amount) as f64 * grid_points[best_idx - 1]) as u64
    } else {
        min_amount
    };

    let upper_bound = if best_idx < grid_points.len() - 1 {
        min_amount + ((max_amount - min_amount) as f64 * grid_points[best_idx + 1]) as u64
    } else {
        max_amount
    };

    let (refined_amount, refined_profit) = golden_section_search_n_hop(
        accounts, edges, instances, lower_bound, upper_bound, clock,
    )?;

    if refined_profit > best_profit {
        Ok((refined_amount, refined_profit))
    } else {
        Ok((best_amount, best_profit))
    }
}

/// Find optimal amount for N-hop path
fn find_optimal_amount_n_hop<'info>(
    edges: &[Edge],
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance<'info>],
    config: &BotConfig,
) -> Result<(u64, i128)> {
    if edges.is_empty() {
        return Ok((0, 0));
    }

    // Get max input from first program
    let first_instance = find_instance_by_pool_id(instances, &edges[0].pool_id)?;
    let input_mint = edges[0].left.mint_account;
    let first_max_in = first_instance
        .get_max_amount_in(accounts, input_mint)
        .unwrap_or(0);

    let max_amount = config.max_amount_in.min(first_max_in);
    let min_amount = MIN_SEARCH_AMOUNT;

    if max_amount <= min_amount {
        return Ok((0, 0));
    }

    hybrid_search_n_hop(
        accounts, edges, instances, min_amount, max_amount, &config.clock,
    )
}

/// Main entry point to find optimal amount in for any arbitrage path
/// Unified version that handles 2-hop, 3-hop, or N-hop paths
pub fn find_optimal_amount_in_v2<'info>(
    edges: &[Edge],
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance<'info>],
    config: &mut BotConfig,
) -> Result<(u64, i128)> {
    if edges.len() < 2 {
        return Ok((0, 0));
    }

    // For 2-hop paths, use the optimized legacy path (more CU efficient)
    if edges.len() == 2 {
        let first_pool_id = edges[0].pool_id;
        let second_pool_id = edges[1].pool_id;
        let input_mint = edges[0].left.mint_account;
        let middle_mint = edges[0].right.mint_account;

        let first_instance = find_instance_by_pool_id(instances, &first_pool_id)?;
        let second_instance = find_instance_by_pool_id(instances, &second_pool_id)?;

        return find_optimal_amount(
            first_instance,
            second_instance,
            input_mint,
            middle_mint,
            accounts,
            config,
        );
    }

    // For 3+ hop paths, use the unified N-hop approach
    find_optimal_amount_n_hop(edges, accounts, instances, config)
}

/// Alias for backwards compatibility
pub fn find_optimal_amount_in_v3<'info>(
    edges: &[Edge],
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance<'info>],
    config: &mut BotConfig,
) -> Result<(u64, i128)> {
    find_optimal_amount_in_v2(edges, accounts, instances, config)
}

