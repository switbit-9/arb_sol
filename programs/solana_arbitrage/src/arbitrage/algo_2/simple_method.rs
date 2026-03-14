use crate::arbitrage::base::{Edge, EdgeSide, Pool};
use crate::programs::{ProgramInstance, ProgramMeta};
use crate::utils::bot_config::BotConfig;
use anchor_lang::prelude::*;
use std::collections::HashMap;

/// Format a pubkey as short string: first4..last4
#[cfg(any(test, feature = "debug"))]
fn short_key(key: &Pubkey) -> String {
    let s = key.to_string();
    format!("{}..{}", &s[..4], &s[s.len() - 4..])
}

/// Format arbitrage path as a readable route string.
/// Example: `[So11..pump](pool Orca 9xQe..3Zuq) -> [EPjF..USDC](pool Raydium 7kS2..x9fN) -> [So11..pump] | in: 1000000 out: 1005000`
#[cfg(any(test, feature = "debug"))]
fn format_arb_path(edges: &[Edge], amount_in: u64, amount_out: u128) -> String {
    let mut parts = Vec::new();
    for edge in edges {
        parts.push(format!(
            "[{}] --(pool {} @ p={:.6} fee={:.4})--> [{}]",
            short_key(&edge.left.mint_account),
            short_key(&edge.pool_id),
            edge.get_price(),
            edge.fee_factor,
            short_key(&edge.right.mint_account),
        ));
    }
    format!(
        "{} | in: {} out: {}",
        parts.join(" -> "),
        amount_in,
        amount_out
    )
}

/// Build a pool_id -> index lookup map.
fn build_pool_index(
    instances: &[ProgramInstance],
) -> HashMap<Pubkey, usize> {
    instances
        .iter()
        .enumerate()
        .map(|(i, inst)| (*inst.get_pool_id(), i))
        .collect()
}

/// Call fast_quote on the program instance for the given edge.
/// Returns (actual_amount_in, amount_out) — programs clamp to their max amounts.
#[inline]
fn edge_fast_quote(
    edge: &Edge,
    amount_in: u64,
    pool_index: &HashMap<Pubkey, usize>,
    instances: &mut [ProgramInstance],
) -> (u64, u64) {
    pool_index
        .get(&edge.pool_id)
        .and_then(|&idx| instances[idx].fast_quote(edge.left.mint_account, amount_in, 0.0).ok())
        .unwrap_or((0, 0))
}

/// Highly efficient iterative check for 2-hop (Cross) Arbitrage.
/// O(E) complexity. Safe for on-chain execution (no recursion).
/// Path: Start -> Token B -> Start
pub fn find_cross_arbitrage_iterative(
    edges: &[&Edge],
    instances: &mut [ProgramInstance],
    config: &mut BotConfig,
) -> Result<(Vec<Edge>, i128, u128)> {
    let start_token = config.start_token;
    let start_amount = config.max_amount_in;
    let min_profit = config.min_profit;
    debug_eprintln!("[ARB] Start amount: {:.9} SOL ({})", start_amount as f64 / 1_000_000_000.0, start_amount);
    let mut max_profit = 0;
    let mut best_path: Option<(Vec<Edge>, i128, u128)> = None;

    // Track best pair for display (even if profit < 0)
    let mut display_max_profit = i128::MIN;
    let mut best_buy_price = 0.0_f64;
    let mut best_sell_price = 0.0_f64;
    let mut best_buy_fee = 0.0_f64;
    let mut best_sell_fee = 0.0_f64;

    let pool_index = build_pool_index(instances);

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
                let (_, amount_b) = edge_fast_quote(edge1, start_amount, &pool_index, instances);

                if let Some(b_edges) = adj.get(&token_b) {
                    for edge2 in b_edges {
                        let is_same_pool = edge2.pool_id == edge1.pool_id;
                        let returns_to_root = edge2.right.mint_account == root;
                        if !returns_to_root || is_same_pool {
                            continue;
                        }

                        let (_, final_out) = edge_fast_quote(edge2, amount_b, &pool_index, instances);
                        let final_amount = final_out as u128;
                        let profit = final_amount as i128 - start_amount as i128;

                        if profit > display_max_profit {
                            display_max_profit = profit;
                            let p1 = edge1.get_price();
                            best_buy_price = if p1 > 1.0 { 1.0 / p1 } else { p1 };
                            let p2: f64 = edge2.get_price();
                            best_sell_price = if p2 > 1.0 { 1.0 / p2 } else { p2 };
                            best_buy_fee = edge1.fee_factor;
                            best_sell_fee = edge2.fee_factor;
                        }

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

    let min_price = best_buy_price.min(best_sell_price);
    let max_price = best_buy_price.max(best_sell_price);
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
    msg!(
        "P={:.4}, F={:.4}, F={:.4}, {:.4})",
        price_diff_pct,
        1.0 - max_fee,
        1.0 - min_fee,
        display_max_profit as f64 / 1_000_000_000.0,
    );
    debug_eprintln!("");

    if let Some((edges, profit, final_amount)) = &best_path {
        #[cfg(any(test, feature = "debug"))]
        {
            debug_eprintln!("");
            debug_eprintln!(
                "Best Cross Arb | profit: {} SOL | {}",
                (*profit as f64 / 1_000_000_000.0),
                format_arb_path(edges, start_amount, *final_amount)
            );
            debug_eprintln!("");
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
pub fn find_triangular_arbitrage_iterative(
    edges: &[&Edge],
    instances: &mut [ProgramInstance],
    config: &mut BotConfig,
) -> Result<(Vec<Edge>, i128, u128)> {
    let start_token = config.start_token;
    let start_amount = config.max_amount_in;
    let min_profit = config.min_profit;

    let mut best_path: Option<(Vec<Edge>, i128, u128)> = None;
    let mut max_profit = i128::MIN; // Start with minimum to allow tracking best path

    let pool_index = build_pool_index(instances);

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
                let (_, amount_b) = edge_fast_quote(edge1, start_amount, &pool_index, instances);

                // Hop 2: B -> C (single lookup instead of contains_key + get)
                if let Some(b_edges) = adj.get(&token_b) {
                    for edge2 in b_edges {
                        let token_c = edge2.right.mint_account;

                        // Optimization: Don't go back to root immediately (that's cross arb)
                        if token_c == root {
                            continue;
                        }

                        let (_, amount_c) = edge_fast_quote(edge2, amount_b, &pool_index, instances);

                        // Hop 3: C -> Root (Optimized Lookup)
                        // Instead of iterating adj[token_c] and filtering for 'root',
                        // we directly look up edges (token_c, root)
                        if let Some(third_leg_edges) = pair_map.get(&(token_c, root)) {
                            for edge3 in third_leg_edges {
                                // Found 3-hop cycle
                                let (_, final_out) = edge_fast_quote(edge3, amount_c, &pool_index, instances);
                                let final_amount = final_out as u128;
                                let profit = final_amount as i128 - start_amount as i128;

                                // Debug logging
                                #[cfg(any(test, feature = "debug"))]
                                {
                                    debug_eprintln!(
                                        "Triangular: profit={}, min_profit={}",
                                        profit,
                                        min_profit
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
            debug_eprintln!("");
            debug_eprintln!(
                "Best Triangular Arb | profit: {} | {}",
                profit,
                format_arb_path(edges, start_amount, *final_amount)
            );
            debug_eprintln!("");
        }
        Ok((edges.clone(), *profit, *final_amount))
    } else {
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!("No profit found");
        Ok((vec![], 0, 0))
    }
}

/// Generate edges for a single program instance.
pub fn generate_edges(program: &ProgramInstance) -> Result<Vec<Edge>> {
    let (price, inverse_price) = program.get_prices()?;

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

    // Get cached max amounts for each direction
    let (buy_max_in, buy_max_out) = program.get_cached_max_amounts(*base_mint);
    let (sell_max_in, sell_max_out) = program.get_cached_max_amounts(*quote_mint);

    let mut edges = Vec::with_capacity(2);

    // edge_1: base→quote (input=base). Skip if pool has no output liquidity in this direction.
    if program.has_output_liquidity(*base_mint) {
        edges.push(Edge::new(
            program_id,
            pool_id,
            EdgeSide::LeftToRight,
            price,
            fee_a_to_b,
            fee_b_to_a,
            buy_max_in,
            buy_max_out,
            base_pool.clone(),
            quote_pool.clone(),
        ));
    }

    // edge_2: quote→base (input=quote). Skip if pool has no output liquidity in this direction.
    if program.has_output_liquidity(*quote_mint) {
        edges.push(Edge::new(
            program_id,
            pool_id,
            EdgeSide::RightToLeft,
            inverse_price,
            fee_b_to_a,
            fee_a_to_b,
            sell_max_in,
            sell_max_out,
            quote_pool,
            base_pool,
        ));
    }

    Ok(edges)
}

/// Generate edges for all program instances.
pub fn get_edges(instances: &[ProgramInstance]) -> Result<Vec<Edge>> {
    // Pre-allocate capacity: each instance generates 2 edges
    let mut edges = Vec::with_capacity(instances.len() * 2);
    for instance in instances {
        let instance_edges = generate_edges(instance)?;
        edges.extend(instance_edges);
    }
    Ok(edges)
}

pub fn check_arbitrage(
    instances: &mut [ProgramInstance],
    config: &mut BotConfig,
) -> Result<(Vec<Edge>, i128, u128)> {
    let edges = get_edges(instances)?;
    let edge_refs = edges.iter().collect::<Vec<_>>();
    if config.mints == 2 {
        find_cross_arbitrage_iterative(&edge_refs, instances, config)
    } else {
        find_triangular_arbitrage_iterative(&edge_refs, instances, config)
    }
}
