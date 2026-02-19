use crate::arbitrage::base::Edge;
use crate::arbitrage::algo_2::path_classifier::{classify_path, PathType};
use crate::programs::{ProgramInstance, ProgramMeta};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::account_info::AccountInfo;

const GOLDEN_RATIO: f64 = 1.618033988749895;
const OPTIMIZER_TOLERANCE: f64 = 100.0; // 100 lamports tolerance
const MAX_ITERATIONS: usize = 50;
const BINARY_SEARCH_MAX_ITERATIONS: usize = 7; // Optimized for on-chain
const GRID_SEARCH_CONVERGENCE: f64 = 0.01; // 1% convergence threshold

/// Result of an optimization attempt
#[derive(Debug, Clone)]
pub struct OptimizationResult {
    pub optimal_amount: u128,
    pub max_profit: i128,
    pub final_amount: u128,
    pub iterations: usize,
    pub converged: bool,
}

/// Calculate profit for a given input amount through a path
pub fn calculate_path_profit<'info>(
    accounts: &[AccountInfo<'info>],
    path: &[(usize, usize)], // (edge_idx, program_idx)
    edges: &[Edge],
    instances: &[ProgramInstance<'info>],
    amount_in: u128,
    clock: Clock,
) -> Result<(u128, i128)> {
    let mut current_amount = amount_in;

    for (edge_idx, program_idx) in path {
        let edge = &edges[*edge_idx];
        let program_instance = &instances[*program_idx];
        let input_mint = edge.left.mint_account;

        let amount_out = match edge.side {
            crate::arbitrage::base::EdgeSide::LeftToRight => {
                program_instance.swap_base_in(accounts, input_mint, current_amount as u64, clock.clone())? as u128
            }
            crate::arbitrage::base::EdgeSide::RightToLeft => {
                program_instance.swap_base_out(accounts, input_mint, current_amount as u64, clock.clone())? as u128
            }
        };

        current_amount = amount_out;
    }

    let profit = current_amount as i128 - amount_in as i128;
    Ok((current_amount, profit))
}

/// Golden ratio search (golden-section search) for finding optimal input amount
/// This is a robust optimization method that doesn't require derivatives
/// and is particularly good for unimodal functions (single peak)
pub fn golden_ratio_search<'info>(
    accounts: &[AccountInfo<'info>],
    path: &[(usize, usize)],
    edges: &[Edge],
    instances: &[ProgramInstance<'info>],
    min_amount: u128,
    max_amount: u128,
    clock: Clock,
) -> Result<OptimizationResult> {
    let mut a = min_amount as f64;
    let mut b = max_amount as f64;
    let mut iterations = 0;
    let mut best_amount = min_amount;
    let mut best_profit = i128::MIN;

    // Golden ratio search maintains two internal points
    let mut x1 = b - (b - a) / GOLDEN_RATIO;
    let mut x2 = a + (b - a) / GOLDEN_RATIO;

    // Evaluate at initial points
    let (_, profit1) = calculate_path_profit(accounts, path, edges, instances, x1 as u128, clock.clone())
        .unwrap_or((0, i128::MIN));
    let (_, profit2) = calculate_path_profit(accounts, path, edges, instances, x2 as u128, clock.clone())
        .unwrap_or((0, i128::MIN));

    let mut f1 = profit1 as f64;
    let mut f2 = profit2 as f64;

    while (b - a).abs() > OPTIMIZER_TOLERANCE && iterations < MAX_ITERATIONS {
        iterations += 1;

        if f1 > f2 {
            // Maximum is in [a, x2]
            b = x2;
            x2 = x1;
            f2 = f1;
            x1 = b - (b - a) / GOLDEN_RATIO;

            if let Ok((_, profit)) = calculate_path_profit(accounts, path, edges, instances, x1 as u128, clock.clone()) {
                f1 = profit as f64;
                if profit > best_profit {
                    best_profit = profit;
                    best_amount = x1 as u128;
                }
            } else {
                f1 = f64::MIN;
            }
        } else {
            // Maximum is in [x1, b]
            a = x1;
            x1 = x2;
            f1 = f2;
            x2 = a + (b - a) / GOLDEN_RATIO;

            if let Ok((_, profit)) = calculate_path_profit(accounts, path, edges, instances, x2 as u128, clock.clone()) {
                f2 = profit as f64;
                if profit > best_profit {
                    best_profit = profit;
                    best_amount = x2 as u128;
                }
            } else {
                f2 = f64::MIN;
            }
        }
    }

    let (final_amount, _) = calculate_path_profit(accounts, path, edges, instances, best_amount, clock)?;

    Ok(OptimizationResult {
        optimal_amount: best_amount,
        max_profit: best_profit,
        final_amount,
        iterations,
        converged: (b - a).abs() <= OPTIMIZER_TOLERANCE,
    })
}

/// Simple gradient ascent optimizer
/// Uses numerical gradients to climb towards maximum profit
pub fn gradient_ascent<'info>(
    accounts: &[AccountInfo<'info>],
    path: &[(usize, usize)],
    edges: &[Edge],
    instances: &[ProgramInstance<'info>],
    initial_amount: u128,
    max_amount: u128,
    clock: Clock,
) -> Result<OptimizationResult> {
    let mut current_amount = initial_amount as f64;
    let max_x = max_amount as f64;
    let mut iterations = 0;
    let mut best_amount = initial_amount;
    let mut best_profit = i128::MIN;

    // Initial evaluation
    if let Ok((_, profit)) = calculate_path_profit(accounts, path, edges, instances, initial_amount, clock.clone()) {
        best_profit = profit;
        best_amount = initial_amount;
    }

    let mut learning_rate = 0.1; // 10% of current amount
    let delta = 1000.0; // Small delta for gradient calculation

    while iterations < MAX_ITERATIONS {
        iterations += 1;

        let x_u128 = current_amount as u128;

        // Calculate gradient using finite differences
        let profit_current = calculate_path_profit(accounts, path, edges, instances, x_u128, clock.clone())
            .map(|(_, p)| p)
            .unwrap_or(i128::MIN);

        let x_forward = (current_amount + delta).min(max_x) as u128;
        let profit_forward = calculate_path_profit(accounts, path, edges, instances, x_forward, clock.clone())
            .map(|(_, p)| p)
            .unwrap_or(i128::MIN);

        // Gradient (approximate derivative)
        let gradient = (profit_forward - profit_current) as f64 / delta;

        // Update best if current is better
        if profit_current > best_profit {
            best_profit = profit_current;
            best_amount = x_u128;
        }

        // Check convergence
        if gradient.abs() < 0.001 {
            break;
        }

        // Adaptive learning rate: reduce if we're oscillating
        if gradient * gradient < 0.0001 {
            learning_rate *= 0.5;
        }

        // Update position in direction of gradient
        let step = learning_rate * current_amount * gradient.signum();
        current_amount = (current_amount + step).max(0.0).min(max_x);

        // If we haven't moved much, we've converged
        if step.abs() < OPTIMIZER_TOLERANCE {
            break;
        }
    }

    let (final_amount, _) = calculate_path_profit(accounts, path, edges, instances, best_amount, clock)?;

    Ok(OptimizationResult {
        optimal_amount: best_amount,
        max_profit: best_profit,
        final_amount,
        iterations,
        converged: iterations < MAX_ITERATIONS,
    })
}

/// Grid search optimizer - divides the search space into a grid and evaluates each point
/// More thorough but slower than other methods
pub fn grid_search<'info>(
    accounts: &[AccountInfo<'info>],
    path: &[(usize, usize)],
    edges: &[Edge],
    instances: &[ProgramInstance<'info>],
    min_amount: u128,
    max_amount: u128,
    num_points: usize,
    clock: Clock,
) -> Result<OptimizationResult> {
    let mut best_amount = min_amount;
    let mut best_profit = i128::MIN;
    let step = (max_amount - min_amount) / num_points as u128;

    for i in 0..=num_points {
        let test_amount = min_amount + (step * i as u128);
        if test_amount > max_amount {
            break;
        }

        if let Ok((_, profit)) = calculate_path_profit(accounts, path, edges, instances, test_amount, clock.clone()) {
            if profit > best_profit {
                best_profit = profit;
                best_amount = test_amount;
            }
        }
    }

    let (final_amount, _) = calculate_path_profit(accounts, path, edges, instances, best_amount, clock)?;

    Ok(OptimizationResult {
        optimal_amount: best_amount,
        max_profit: best_profit,
        final_amount,
        iterations: num_points + 1,
        converged: true,
    })
}

/// Multi-start optimizer: tries multiple initial points and returns the best result
/// Helps avoid local maxima by starting from different positions
pub fn multi_start_optimize<'info>(
    accounts: &[AccountInfo<'info>],
    path: &[(usize, usize)],
    edges: &[Edge],
    instances: &[ProgramInstance<'info>],
    start_amount: u128,
    max_amount: u128,
    clock: Clock,
) -> Result<OptimizationResult> {
    let initial_guesses = vec![
        start_amount / 1000,   // Very small
        start_amount / 100,    // Small
        start_amount / 20,     // Medium-small
        start_amount / 5,      // Medium
        start_amount / 2,      // Medium-large
        start_amount,          // Full
        (start_amount as f64 * 1.5) as u128, // 1.5x
    ];

    let mut best_result = OptimizationResult {
        optimal_amount: start_amount,
        max_profit: i128::MIN,
        final_amount: 0,
        iterations: 0,
        converged: false,
    };

    for guess in initial_guesses {
        let guess_clamped = guess.min(max_amount);

        // Try gradient ascent from this starting point
        if let Ok(result) = gradient_ascent(accounts, path, edges, instances, guess_clamped, max_amount, clock.clone()) {
            if result.max_profit > best_result.max_profit {
                best_result = result;
            }
        }
    }

    Ok(best_result)
}

/// Hybrid optimizer: combines golden ratio search with multi-start
/// Uses coarse grid search to find promising regions, then refines with golden ratio
pub fn hybrid_optimize<'info>(
    accounts: &[AccountInfo<'info>],
    path: &[(usize, usize)],
    edges: &[Edge],
    instances: &[ProgramInstance<'info>],
    min_amount: u128,
    max_amount: u128,
    clock: Clock,
) -> Result<OptimizationResult> {
    // Phase 1: Coarse grid search to find promising regions
    let coarse_result = grid_search(accounts, path, edges, instances, min_amount, max_amount, 10, clock.clone())?;

    // Phase 2: Refine around the best point found using golden ratio search
    let search_width = (max_amount - min_amount) / 5; // Search within 20% of range
    let refined_min = coarse_result.optimal_amount.saturating_sub(search_width).max(min_amount);
    let refined_max = (coarse_result.optimal_amount + search_width).min(max_amount);

    let refined_result = golden_ratio_search(accounts, path, edges, instances, refined_min, refined_max, clock)?;

    // Return the better of the two results
    if refined_result.max_profit > coarse_result.max_profit {
        Ok(refined_result)
    } else {
        Ok(coarse_result)
    }
}

/// Smart optimizer that dispatches to the best algorithm based on path type
/// This is the MAIN ENTRY POINT for path optimization with automatic algorithm selection
///
/// Performance characteristics:
/// - AMM → AMM: Golden ratio search (fast, 10-20 iterations)
/// - AMM → DLMM: Binary search (7 iterations, respects DLMM bins)
/// - DLMM → DLMM: Logarithmic grid (15-20 evaluations)
pub fn optimize_path_smart<'info>(
    accounts: &[AccountInfo<'info>],
    path: &[(usize, usize)],
    edges: &[Edge],
    instances: &[ProgramInstance<'info>],
    min_amount: u128,
    max_amount: u128,
    clock: Clock,
) -> Result<OptimizationResult> {
    // Get edges for this path
    let path_edges: Vec<Edge> = path.iter()
        .map(|(edge_idx, _)| edges[*edge_idx].clone())
        .collect();

    let path_type = classify_path(&path_edges);


    match path_type {
        PathType::AmmToAmm => {
            // Use golden ratio search (works well for smooth AMM curves)
            golden_ratio_search(accounts, path, edges, instances, min_amount, max_amount, clock)
        },
        PathType::AmmToDlmm | PathType::DlmmToAmm => {
            // Use binary search optimized for mixed paths
            binary_search_optimize(accounts, path, edges, instances, min_amount, max_amount, clock)
        },
        PathType::DlmmToDlmm => {
            // Use logarithmic grid search for DLMM-DLMM
            grid_search_logarithmic(accounts, path, edges, instances, min_amount, max_amount, clock)
        },
        PathType::MultiHop => {
            // Use hybrid optimizer for complex paths
            hybrid_optimize(accounts, path, edges, instances, min_amount, max_amount, clock)
        },
    }
}

/// Binary search optimizer optimized for mixed AMM-DLMM paths
/// Uses golden ratio subdivision for better convergence (7-10 iterations max)
///
/// This is ideal for paths with one DLMM pool because:
/// - DLMM has discrete bins, so derivative-based methods don't work well
/// - Binary search respects bin boundaries naturally
/// - Limited iterations keep on-chain compute cost low
pub fn binary_search_optimize<'info>(
    accounts: &[AccountInfo<'info>],
    path: &[(usize, usize)],
    edges: &[Edge],
    instances: &[ProgramInstance<'info>],
    min_amount: u128,
    max_amount: u128,
    clock: Clock,
) -> Result<OptimizationResult> {
    let mut left = min_amount as f64;
    let mut right = max_amount as f64;
    let mut best_amount = min_amount;
    let mut best_profit = i128::MIN;
    let mut iterations = 0;


    while iterations < BINARY_SEARCH_MAX_ITERATIONS {
        iterations += 1;

        // Use golden ratio for better convergence than simple midpoint
        let x1 = right - (right - left) / GOLDEN_RATIO;
        let x2 = left + (right - left) / GOLDEN_RATIO;

        // Evaluate at two golden ratio points
        let profit1 = calculate_path_profit(accounts, path, edges, instances, x1 as u128, clock.clone())
            .map(|(_, p)| p)
            .unwrap_or(i128::MIN);

        let profit2 = calculate_path_profit(accounts, path, edges, instances, x2 as u128, clock.clone())
            .map(|(_, p)| p)
            .unwrap_or(i128::MIN);

        // Update best
        if profit1 > best_profit {
            best_profit = profit1;
            best_amount = x1 as u128;
        }
        if profit2 > best_profit {
            best_profit = profit2;
            best_amount = x2 as u128;
        }

        // Narrow search range toward better profit
        if profit1 > profit2 {
            right = x2;
        } else {
            left = x1;
        }

        // Check convergence
        let range_pct = (right - left) / (max_amount as f64);
        if range_pct < GRID_SEARCH_CONVERGENCE {
            break;
        }
    }

    let (final_amount, _) = calculate_path_profit(accounts, path, edges, instances, best_amount, clock)?;


    Ok(OptimizationResult {
        optimal_amount: best_amount,
        max_profit: best_profit,
        final_amount,
        iterations,
        converged: iterations < BINARY_SEARCH_MAX_ITERATIONS,
    })
}

/// Grid search with logarithmic sampling optimized for DLMM-DLMM paths
///
/// DLMM pools have discrete bins, so we need to:
/// 1. Sample across a wide range (logarithmic spacing)
/// 2. Cover potential bin boundaries
/// 3. Refine around the best point
///
/// This uses ~15-20 evaluations total (coarse + refinement)
pub fn grid_search_logarithmic<'info>(
    accounts: &[AccountInfo<'info>],
    path: &[(usize, usize)],
    edges: &[Edge],
    instances: &[ProgramInstance<'info>],
    min_amount: u128,
    max_amount: u128,
    clock: Clock,
) -> Result<OptimizationResult> {

    // Phase 1: Coarse logarithmic sampling to cover bin boundaries
    // These samples are logarithmically spaced to cover orders of magnitude
    let base_samples = vec![
        min_amount.max(1000),           // Minimum viable
        max_amount / 1000,              // 0.1%
        max_amount / 500,               // 0.2%
        max_amount / 200,               // 0.5%
        max_amount / 100,               // 1%
        max_amount / 50,                // 2%
        max_amount / 20,                // 5%
        max_amount / 10,                // 10%
        max_amount / 5,                 // 20%
        max_amount / 2,                 // 50%
        (max_amount * 3) / 4,           // 75%
        max_amount,                     // 100%
    ];

    let mut best_amount = min_amount;
    let mut best_profit = i128::MIN;
    let mut iterations = 0;

    // Coarse search
    for &amount in &base_samples {
        if amount < min_amount || amount > max_amount {
            continue;
        }
        iterations += 1;

        if let Ok((_, profit)) = calculate_path_profit(
            accounts, path, edges, instances, amount, clock.clone()
        ) {
            if profit > best_profit {
                best_profit = profit;
                best_amount = amount;
                #[cfg(test)]
                {
                    debug_eprintln!("  New best at {}: profit={}", amount, profit);
                }
            }
        }
    }


    // Phase 2: Refine around best point with linear sampling
    let range = best_amount / 10; // +/- 10% of best
    let refinement_samples = vec![
        best_amount.saturating_sub(range),
        best_amount.saturating_sub(range / 2),
        best_amount,
        best_amount.saturating_add(range / 2).min(max_amount),
        best_amount.saturating_add(range).min(max_amount),
    ];

    for &amount in &refinement_samples {
        if amount < min_amount || amount > max_amount {
            continue;
        }
        iterations += 1;

        if let Ok((_, profit)) = calculate_path_profit(
            accounts, path, edges, instances, amount, clock.clone()
        ) {
            if profit > best_profit {
                best_profit = profit;
                best_amount = amount;
                #[cfg(test)]
                {
                    debug_eprintln!("  Refinement improved: amount={}, profit={}", amount, profit);
                }
            }
        }
    }

    let (final_amount, _) = calculate_path_profit(accounts, path, edges, instances, best_amount, clock)?;

    #[cfg(test)]
    {
        debug_eprintln!("  Logarithmic grid result: optimal_amount={}, profit={}, iterations={}", best_amount, best_profit, iterations);
    }

    Ok(OptimizationResult {
        optimal_amount: best_amount,
        max_profit: best_profit,
        final_amount,
        iterations,
        converged: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_golden_ratio_constant() {
        // Verify golden ratio constant
        assert!((GOLDEN_RATIO - 1.618).abs() < 0.001);
    }

    #[test]
    fn test_optimization_result_structure() {
        let result = OptimizationResult {
            optimal_amount: 1000,
            max_profit: 100,
            final_amount: 1100,
            iterations: 10,
            converged: true,
        };

        assert_eq!(result.optimal_amount, 1000);
        assert_eq!(result.max_profit, 100);
        assert!(result.converged);
    }
}

