use crate::arbitrage::base::{Edge, EdgeSide, Pool};
use crate::programs::{ProgramInstance, ProgramMeta};
use crate::utils::bot_config::BotConfig;
use anchor_lang::prelude::*;
use std::collections::HashMap;

/// Price scaling factor for fixed-point arithmetic (10^9 for 9 decimal precision)
const PRICE_SCALE: u128 = 1_000_000_000;

/// Calculate swap amount using fixed-point arithmetic: amount_out = amount_in * price * fee_factor
#[inline]
fn calculate_swap_amount(edge: &Edge, amount_in: u64) -> u64 {
    let scaled_price = (edge.get_price() * PRICE_SCALE as f64) as u128;
    let result = (amount_in as u128)
        .saturating_mul(scaled_price)
        / PRICE_SCALE;
    result.min(u64::MAX as u128) as u64
}

/// Highly efficient iterative check for 2-hop (Cross) Arbitrage.
/// O(E) complexity. Safe for on-chain execution (no recursion).
/// Path: Start -> Token B -> Start
pub fn find_cross_arbitrage_iterative<'info>(
    edges: &[&Edge],
    config: &mut BotConfig,
) -> Result<(Vec<Edge>, i128, u128)> {
    let start_token = config.start_token;
    let start_amount = config.max_amount_in;
    let min_profit = config.min_profit;
    debug_eprintln!("start_amount: {:?}", start_amount);
    let mut max_profit = 0;
    let mut best_path: Option<(Vec<Edge>, i128, u128)> = None;

    // adjacency map: start -> edges
    let mut adj: HashMap<Pubkey, Vec<&Edge>> = HashMap::new();
    for &edge in edges {
        adj.entry(edge.left.mint_account)
            .or_insert_with(Vec::new)
            .push(edge);
    }

    let root_tokens: Vec<Pubkey> = if let Some(token) = start_token {
        vec![token]
    } else {
        adj.keys().cloned().collect()
    };

    for root in root_tokens {
        if let Some(root_edges) = adj.get(&root) {
            for edge1 in root_edges {
                let token_b = edge1.right.mint_account;
                let amount_b = calculate_swap_amount(edge1, start_amount);

                if let Some(b_edges) = adj.get(&token_b) {
                    for edge2 in b_edges {
                        let is_same_pool = edge2.pool_id == edge1.pool_id;
                        let returns_to_root = edge2.right.mint_account == root;
                        if !returns_to_root || is_same_pool {
                            continue;
                        }

                        let final_amount = calculate_swap_amount(edge2, amount_b) as u128;
                        let profit = final_amount as i128 - start_amount as i128;
                        if profit < 0 || profit < min_profit {
                            continue;
                        }

                        if profit > max_profit {
                            debug_eprintln!("profit: {:?}", profit);
                            max_profit = profit;
                            let running_edges = vec![(*edge1).clone(), (*edge2).clone()];
                            best_path = Some((running_edges, profit, final_amount));
                        }
                    }
                }
            }
        }
    }

    if let Some((edges, profit, final_amount)) = &best_path {
        #[cfg(any(test, feature = "debug"))]
        {
            let pool_ids: Vec<_> = edges.iter().map(|e| e.pool_id).collect();
            debug_eprintln!(
                "Best path: {:?}, pools: {:?}, profit {}",
                edges, pool_ids, profit
            );
        }
        Ok((edges.clone(), *profit, *final_amount))
    } else {
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!("No profit found");
        Ok((vec![], 0, 0))
    }
}

/// Optimized O(E) check for 3-hop (Triangular) Arbitrage using Map lookup.
/// Best performance for on-chain execution.
/// Path: Start -> Token B -> Token C -> Start
pub fn find_triangular_arbitrage_iterative<'info>(
    edges: &[&Edge],
    config: &mut BotConfig,
) -> Result<(Vec<Edge>, i128, u128)> {
    let start_token = config.start_token;
    let start_amount = config.max_amount_in;
    let min_profit = config.min_profit;

    let mut best_path: Option<(Vec<Edge>, i128, u128)> = None;
    let mut max_profit = i128::MIN; // Start with minimum to allow tracking best path

    // 1. Build Adjacency List (Start -> [Edges])
    let mut adj: HashMap<Pubkey, Vec<&Edge>> = HashMap::new();

    // 2. Build Edge Map ((Start, End) -> List[Edge]) for O(1) lookup
    let mut pair_map: HashMap<(Pubkey, Pubkey), Vec<&Edge>> = HashMap::new();

    for &edge in edges {
        let start = edge.left.mint_account;
        let end = edge.right.mint_account;

        adj.entry(start).or_insert_with(Vec::new).push(edge);
        pair_map
            .entry((start, end))
            .or_insert_with(Vec::new)
            .push(edge);
    }

    let root_tokens: Vec<Pubkey> = if let Some(token) = start_token {
        vec![token]
    } else {
        adj.keys().cloned().collect()
    };

    for root in root_tokens {
        if let Some(root_edges) = adj.get(&root) {
            // Hop 1: Root -> B
            for edge1 in root_edges {
                let token_b = edge1.right.mint_account;
                let amount_b = calculate_swap_amount(edge1, start_amount);

                // Hop 2: B -> C (single lookup instead of contains_key + get)
                if let Some(b_edges) = adj.get(&token_b) {
                    for edge2 in b_edges {
                        let token_c = edge2.right.mint_account;

                        // Optimization: Don't go back to root immediately (that's cross arb)
                        if token_c == root {
                            continue;
                        }

                        let amount_c = calculate_swap_amount(edge2, amount_b);

                        // Hop 3: C -> Root (Optimized Lookup)
                        // Instead of iterating adj[token_c] and filtering for 'root',
                        // we directly look up edges (token_c, root)
                        if let Some(third_leg_edges) = pair_map.get(&(token_c, root)) {
                            for edge3 in third_leg_edges {
                                // Found 3-hop cycle
                                let final_amount = calculate_swap_amount(edge3, amount_c) as u128;
                                let profit = final_amount as i128 - start_amount as i128;

                                // Debug logging
                                #[cfg(any(test, feature = "debug"))]
                                {
                                debug_eprintln!(
                                    "Triangular: profit={}, min_profit={}",
                                    profit, min_profit
                                );
                                }

                                if profit > max_profit && profit >= min_profit {
                                    #[cfg(any(test, feature = "debug"))]
                                    {
                                    debug_eprintln!(
                                        "Found Triangular Arb: profit={}, final={}, start={}, min_profit={}",
                                        profit,
                                        final_amount,
                                        start_amount,
                                        min_profit
                                    );
                                    }
                                    max_profit = profit;
                                    let running_edges =
                                        vec![(*edge1).clone(), (*edge2).clone(), (*edge3).clone()];
                                    best_path = Some((running_edges, profit, final_amount));
                                } else {
                                    #[cfg(any(test, feature = "debug"))]
                                    {
                                    debug_eprintln!(
                                        "Ignored Triangular Arb: profit={}, final={}, start={}, min_profit={}",
                                        profit,
                                        final_amount,
                                        start_amount,
                                        min_profit
                                    );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some((edges, profit, final_amount)) = &best_path {
        #[cfg(any(test, feature = "debug"))]
        {
            let pool_ids: Vec<_> = edges.iter().map(|e| e.pool_id).collect();
            debug_eprintln!(
                "Best path: {:?}, pools: {:?}, profit {}",
                edges, pool_ids, profit
            );
        }
        Ok((edges.clone(), *profit, *final_amount))
    } else {
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!("No profit found");
        Ok((vec![], 0, 0))
    }
}

/// Generate edges for a single program instance.
pub fn generate_edges<'info>(program: &ProgramInstance<'info>) -> Result<Vec<Edge>> {
    // Get prices from the program's get_prices method
    let prices = program.get_prices()?;
    let price = prices.0;
    let inverse_price = prices.1;

    // Get directional fee factors: (fee_a_to_b, fee_b_to_a)
    let (fee_a_to_b, fee_b_to_a) = program.get_fee_factor().unwrap_or((0.0, 0.0));

    // Get vault accounts to extract mints
    let (base_mint, quote_mint) = program.get_mints();

    // Create Pool objects with just the mints (amounts are not stored in Pool anymore)
    let base_pool = Pool::new(&base_mint);
    let quote_pool = Pool::new(&quote_mint);
    let program_id = *program.get_id();
    let pool_id = *program.get_pool_id();

    #[cfg(any(test, feature = "debug"))]
    {
    debug_eprintln!("================================================");
    debug_eprintln!(
        "Gen Edges: {:?} Pool={} Base={} Quote={} P={} IP={} F={} IF={}",
        program_id,
        pool_id,
        base_mint,
        quote_mint,
        price,
        inverse_price,
        fee_a_to_b,
        fee_b_to_a
    );
    }

    let edge_1 = Edge::new(
        program_id,
        pool_id,
        EdgeSide::LeftToRight,
        price,
        fee_a_to_b,
        fee_b_to_a,
        base_pool.clone(),
        quote_pool.clone(),
    );
    let edge_2 = Edge::new(
        program_id,
        pool_id,
        EdgeSide::RightToLeft,
        inverse_price,
        fee_b_to_a,
        fee_a_to_b,
        quote_pool, // Move instead of clone
        base_pool,  // Move instead of clone
    );

    Ok(vec![edge_1, edge_2])
}

/// Generate edges for all program instances.
pub fn get_edges<'info>(instances: &[ProgramInstance<'info>]) -> Result<Vec<Edge>> {
    // Pre-allocate capacity: each instance generates 2 edges
    let mut edges = Vec::with_capacity(instances.len() * 2);
    for instance in instances {
        let instance_edges = generate_edges(instance)?;
        edges.extend(instance_edges);
    }
    Ok(edges)
}


pub fn check_arbitrage<'info>(
    instances: &[ProgramInstance<'info>],
    config: &mut BotConfig,
) -> Result<(Vec<Edge>, i128, u128)> {
    let edges = get_edges(instances)?;
    let edge_refs = edges.iter().collect::<Vec<_>>();
    if config.mints == 2 {
        find_cross_arbitrage_iterative(&edge_refs, config)
    } else {
        find_triangular_arbitrage_iterative(&edge_refs, config)
    }
}
