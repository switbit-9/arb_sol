use crate::arbitrage::base::Edge;
use crate::programs::SolarBError;
use anchor_lang::prelude::*;
use std::collections::{HashMap, HashSet};

const MIN_PROFIT: i128 = 40_000;

#[derive(Clone, Debug)]
pub struct ArbitragePath {
    pub edges: Vec<Edge>,
    pub profit: i128,
    pub final_amount: u128,
    pub start_amount: u128,
}

fn calculate_swap_amount(edge: &Edge, amount_in: u128) -> u128 {
    (amount_in as f64 * edge.get_price()) as u128
}

/// Highly efficient iterative check for 2-hop (Cross) Arbitrage.
/// O(E) complexity. Safe for on-chain execution (no recursion).
/// Path: Start -> Token B -> Start
pub fn find_cross_arbitrage_iterative(
    edges: &[&Edge],
    start_amount: u128,
    min_profit: i128,
    start_token: Option<Pubkey>,
) -> Option<ArbitragePath> {
    let mut best_path: Option<ArbitragePath> = None;
    let mut max_profit = 0i128;

    // Group edges by start token for O(1) lookup
    // Map: StartToken -> List of Edges
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
            // Hop 1: Root -> B
            for edge1 in root_edges {
                let token_b = edge1.right.mint_account;
                let amount_b = calculate_swap_amount(edge1, start_amount);

                // Hop 2: B -> Root
                if let Some(b_edges) = adj.get(&token_b) {
                    for edge2 in b_edges {
                        // Ensure we go back to root AND use a different program/market
                        if edge2.right.mint_account == root && edge2.program != edge1.program {
                            // Found 2-hop cycle
                            let final_amount = calculate_swap_amount(edge2, amount_b);
                            let profit = final_amount as i128 - start_amount as i128;

                            // Only update if this path is MORE profitable than current best
                            // This ensures we find the BEST path, not just the first valid one
                            if profit > max_profit && profit >= min_profit {
                                max_profit = profit;
                                best_path = Some(ArbitragePath {
                                    edges: vec![(*edge1).clone(), (*edge2).clone()],
                                    profit,
                                    final_amount,
                                    start_amount,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    best_path
}

/// Optimized O(E) check for 3-hop (Triangular) Arbitrage using Map lookup.
/// Best performance for on-chain execution.
/// Path: Start -> Token B -> Token C -> Start
pub fn find_triangular_arbitrage_iterative<'info>(
    edges: &[&Edge],
    start_amount: u128,
    min_profit: i128,
    start_token: Option<Pubkey>,
) -> Option<ArbitragePath> {
    let mut best_path: Option<ArbitragePath> = None;
    let mut max_profit = 0i128;

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

                if !adj.contains_key(&token_b) {
                    continue;
                }

                // Hop 2: B -> C
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
                                let final_amount = calculate_swap_amount(edge3, amount_c);
                                let profit = final_amount as i128 - start_amount as i128;

                                // Debug logging
                                // msg!("Triangular: profit={}, min_profit={}", profit, min_profit);

                                if profit > max_profit && profit >= min_profit {
                                    max_profit = profit;
                                    best_path = Some(ArbitragePath {
                                        edges: vec![
                                            (*edge1).clone(),
                                            (*edge2).clone(),
                                            (*edge3).clone(),
                                        ],
                                        profit,
                                        final_amount,
                                        start_amount,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    best_path
}

/// Main entry point for arbitrage calculation.
pub fn check_arbitrage(
    edges: &[&Edge],
    start_amount: u128,
    start_token: Option<Pubkey>,
    min_profit: Option<i128>,
) -> Result<ArbitragePath> {
    let min_profit = min_profit.unwrap_or(MIN_PROFIT);

    // 1. Determine Unique Tokens to decide strategy
    let mut unique_tokens = HashSet::new();
    for &edge in edges {
        unique_tokens.insert(edge.left.mint_account);
        unique_tokens.insert(edge.right.mint_account);
    }

    let num_tokens = unique_tokens.len();

    // 2. Strategy Selection
    let arbitrage = if num_tokens <= 2 {
        find_cross_arbitrage_iterative(edges, start_amount, min_profit, start_token)
    } else {
        find_triangular_arbitrage_iterative(edges, start_amount, min_profit, start_token)
    };

    match arbitrage {
        Some(arb) if arb.profit >= MIN_PROFIT => Ok(arb),
        _ => Err(SolarBError::NoProfitFound.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arbitrage::base::{Edge, EdgeSide, Pool};
    use crate::programs::ProgramMeta;
    use anchor_lang::prelude::Pubkey;

    // Mock ProgramMeta implementation for testing
    struct MockProgram {
        id: Pubkey,
    }

    impl MockProgram {
        fn new() -> Self {
            Self {
                id: Pubkey::new_unique(),
            }
        }
    }

    impl ProgramMeta for MockProgram {
        fn get_id(&self) -> &Pubkey {
            &self.id
        }

        fn get_vaults(&self) -> (&AccountInfo<'_>, &AccountInfo<'_>) {
            panic!("Not implemented for test");
        }

        fn swap_base_in(&self, _amount_in: u64, _clock: Clock) -> Result<u64> {
            Ok(0) // Mock implementation
        }

        fn swap_base_out(&self, _amount_in: u64, _clock: Clock) -> Result<u64> {
            Ok(0) // Mock implementation
        }

        fn invoke_swap_base_in<'a>(
            &self,
            _max_amount_in: u64,
            _amount_out: Option<u64>,
            _payer: AccountInfo<'a>,
            _user_mint_1_token_account: AccountInfo<'a>,
            _user_mint_2_token_account: AccountInfo<'a>,
            _mint_1_account: AccountInfo<'a>,
            _mint_2_account: AccountInfo<'a>,
            _mint_1_token_program: AccountInfo<'a>,
            _mint_2_token_program: AccountInfo<'a>,
        ) -> Result<()> {
            Ok(()) // Mock implementation
        }

        fn invoke_swap_base_out<'a>(
            &self,
            _amount_in: u64,
            _min_amount_out: Option<u64>,
            _payer: AccountInfo<'a>,
            _user_mint_1_token_account: AccountInfo<'a>,
            _user_mint_2_token_account: AccountInfo<'a>,
            _mint_1_account: AccountInfo<'a>,
            _mint_2_account: AccountInfo<'a>,
            _mint_1_token_program: AccountInfo<'a>,
            _mint_2_token_program: AccountInfo<'a>,
        ) -> Result<()> {
            Ok(()) // Mock implementation
        }
    }

    #[test]
    fn test_cross_arbitrage_logic() {
        use std::io::Write;
        let stderr: std::io::Stderr = std::io::stderr();
        let mut handle = stderr.lock();

        let sol = Pubkey::new_unique();
        let usdc = Pubkey::new_unique();

        let prog1_struct = MockProgram::new();
        let program1: &dyn ProgramMeta = &prog1_struct;

        let prog2_struct = MockProgram::new();
        let program2: &dyn ProgramMeta = &prog2_struct;

        let prog3_struct = MockProgram::new();
        let program3: &dyn ProgramMeta = &prog3_struct;

        // Market 1: 1 SOL = 100 USDC
        // let pool1_sol = Pool::new(&sol, 1_000_000_000);
        // let pool1_usdc = Pool::new(&usdc, 100_000_000);

        // Edge 1: SOL -> USDC on Prog1. Price = 100 (1 SOL = 100 USDC)
        // Use realistic amounts to avoid unrealistic prices
        let pool_a_left = Pool::new(&sol, 1_000_000_000); // 1 SOL
        let pool_a_right = Pool::new(&usdc, 100_000_000); // 100 USDC (1 SOL = 100 USDC)
        let price_a_lr = if pool_a_left.amount > 0 {
            pool_a_right.amount as f64 / pool_a_left.amount as f64
        } else {
            0.0
        };
        let price_a_rl = if pool_a_right.amount > 0 {
            pool_a_left.amount as f64 / pool_a_right.amount as f64
        } else {
            0.0
        };
        let prog1_id = *program1.get_id();
        let edge1_1_a = Edge::new(
            prog1_id,
            EdgeSide::LeftToRight,
            price_a_lr,
            pool_a_left.clone(),
            pool_a_right.clone(),
        );
        let edge1_1_b = Edge::new(
            prog1_id,
            EdgeSide::RightToLeft,
            price_a_rl,
            pool_a_right.clone(),
            pool_a_left.clone(),
        );

        // Market 2: SOL -> USDC (for Program2-Program3 arbitrage)
        let pool_b_sol_to_usdc_left = Pool::new(&sol, 1_000_000_000); // 1 SOL
        let pool_b_sol_to_usdc_right = Pool::new(&usdc, 100_000_000_000); // 100 USDC
        let price_b_sol_usdc = if pool_b_sol_to_usdc_left.amount > 0 {
            pool_b_sol_to_usdc_right.amount as f64 / pool_b_sol_to_usdc_left.amount as f64
        } else {
            0.0
        };
        let prog2_id = *program2.get_id();
        let edge_2_sol_to_usdc = Edge::new(
            prog2_id,
            EdgeSide::LeftToRight,
            price_b_sol_usdc,
            pool_b_sol_to_usdc_left.clone(),
            pool_b_sol_to_usdc_right.clone(),
        );

        // Market 2: USDC -> SOL (for completing the cycle)
        // Note: This should NOT create a profitable cycle within Program2
        // Use realistic amounts: if 1 SOL = 100 USDC, then 1 USDC = 0.01 SOL
        let pool_b_left = Pool::new(&usdc, 100_000_000); // 100 USDC
        let pool_b_right = Pool::new(&sol, 1_000_000_000); // 1 SOL (realistic: 1 USDC = 0.01 SOL)
        let price_b_lr = if pool_b_left.amount > 0 {
            pool_b_right.amount as f64 / pool_b_left.amount as f64
        } else {
            0.0
        };
        let price_b_rl = if pool_b_right.amount > 0 {
            pool_b_left.amount as f64 / pool_b_right.amount as f64
        } else {
            0.0
        };
        let edge_2_a = Edge::new(
            prog2_id,
            EdgeSide::LeftToRight,
            price_b_lr,
            pool_b_left.clone(),
            pool_b_right.clone(),
        );
        let edge_2_b = Edge::new(
            prog2_id,
            EdgeSide::RightToLeft,
            price_b_rl,
            pool_b_right.clone(),
            pool_b_left.clone(),
        );

        // Market 3: USDC -> SOL (should be better rate than Program2 to make Program2->Program3 profitable)
        // Use a better rate: 1 USDC = 0.011 SOL (slightly better than Program2's 0.01)
        let pool_c_left = Pool::new(&usdc, 100_000_000); // 100 USDC
        let pool_c_right = Pool::new(&sol, 1_100_000_000); // 1.1 SOL (better rate: 1 USDC = 0.011 SOL)
        let price_c_lr = if pool_c_left.amount > 0 {
            pool_c_right.amount as f64 / pool_c_left.amount as f64
        } else {
            0.0
        };
        let price_c_rl = if pool_c_right.amount > 0 {
            pool_c_left.amount as f64 / pool_c_right.amount as f64
        } else {
            0.0
        };
        let prog3_id = *program3.get_id();
        let edge_3_a = Edge::new(
            prog3_id,
            EdgeSide::LeftToRight,
            price_c_lr,
            pool_c_left.clone(),
            pool_c_right.clone(),
        );
        let edge_3_b = Edge::new(
            prog3_id,
            EdgeSide::RightToLeft,
            price_c_rl,
            pool_c_right.clone(),
            pool_c_left.clone(),
        );

        // Put edges in a vector
        // Note: Program2-Program3 should be best because:
        // - Program2: SOL -> USDC (good rate: 1 SOL = 100 USDC)
        // - Program3: USDC -> SOL (good rate: should give more SOL back)
        let edges = vec![
            &edge1_1_a,
            &edge1_1_b,
            &edge_2_sol_to_usdc,
            &edge_2_a,
            &edge_2_b,
            &edge_3_a,
            &edge_3_b,
        ];

        // Start with 1 SOL (1e9)
        let start_amount = 1_000_000_000;

        // First, let's manually check all possible paths to see which should be best
        writeln!(handle, "=== Analyzing All Possible Arbitrage Paths ===").unwrap();
        writeln!(
            handle,
            "Start: {} SOL ({} units)",
            start_amount as f64 / 1e9,
            start_amount
        )
        .unwrap();
        writeln!(handle, "").unwrap();

        // Check Program2 -> Program3 path (should be best)
        let prog2_sol_to_usdc = &edge_2_sol_to_usdc;
        let prog3_edge = &edge_3_a;
        if prog2_sol_to_usdc.right.mint_account == prog3_edge.left.mint_account
            && prog3_edge.right.mint_account == sol
            && prog2_sol_to_usdc.program != prog3_edge.program
        {
            let amount_b = calculate_swap_amount(prog2_sol_to_usdc, start_amount);
            let final_amount = calculate_swap_amount(prog3_edge, amount_b);
            let profit = final_amount as i128 - start_amount as i128;
            writeln!(handle, "Path: Program2 -> Program3 (EXPECTED BEST)").unwrap();
            writeln!(
                handle,
                "  Step 1: {} SOL -> {} USDC (via Program2, price: {})",
                start_amount as f64 / 1e9,
                amount_b as f64 / 1e9,
                prog2_sol_to_usdc.price
            )
            .unwrap();
            writeln!(
                handle,
                "  Step 2: {} USDC -> {} SOL (via Program3, price: {})",
                amount_b as f64 / 1e9,
                final_amount as f64 / 1e9,
                prog3_edge.price
            )
            .unwrap();
            writeln!(handle, "  Profit: {} ({})", profit, profit as f64 / 1e9).unwrap();
            writeln!(handle, "").unwrap();
        }

        // Check Program1 -> Program2 path
        let prog1_edge = &edge1_1_a;
        let prog2_edge = &edge_2_a;
        if prog1_edge.right.mint_account == prog2_edge.left.mint_account
            && prog2_edge.right.mint_account == sol
            && prog1_edge.program != prog2_edge.program
        {
            let amount_b = calculate_swap_amount(prog1_edge, start_amount);
            let final_amount = calculate_swap_amount(prog2_edge, amount_b);
            let profit = final_amount as i128 - start_amount as i128;
            writeln!(handle, "Path: Program1 -> Program2").unwrap();
            writeln!(
                handle,
                "  Step 1: {} SOL -> {} USDC (via Program1, price: {})",
                start_amount as f64 / 1e9,
                amount_b as f64 / 1e9,
                prog1_edge.price
            )
            .unwrap();
            writeln!(
                handle,
                "  Step 2: {} USDC -> {} SOL (via Program2, price: {})",
                amount_b as f64 / 1e9,
                final_amount as f64 / 1e9,
                prog2_edge.price
            )
            .unwrap();
            writeln!(handle, "  Profit: {} ({})", profit, profit as f64 / 1e9).unwrap();
            writeln!(handle, "").unwrap();
        }

        // Check Program1 -> Program3 path
        let prog3_edge = &edge_3_a;
        if prog1_edge.right.mint_account == prog3_edge.left.mint_account
            && prog3_edge.right.mint_account == sol
            && prog1_edge.program != prog3_edge.program
        {
            let amount_b = calculate_swap_amount(prog1_edge, start_amount);
            let final_amount = calculate_swap_amount(prog3_edge, amount_b);
            let profit = final_amount as i128 - start_amount as i128;
            writeln!(handle, "Path: Program1 -> Program3").unwrap();
            writeln!(
                handle,
                "  Step 1: {} SOL -> {} USDC (via Program1, price: {})",
                start_amount as f64 / 1e9,
                amount_b as f64 / 1e9,
                prog1_edge.price
            )
            .unwrap();
            writeln!(
                handle,
                "  Step 2: {} USDC -> {} SOL (via Program3, price: {})",
                amount_b as f64 / 1e9,
                final_amount as f64 / 1e9,
                prog3_edge.price
            )
            .unwrap();
            writeln!(handle, "  Profit: {} ({})", profit, profit as f64 / 1e9).unwrap();
            writeln!(handle, "").unwrap();
        }

        // Note: Program2 -> Program3 can't form a cross arbitrage starting from SOL
        // because both only have USDC -> SOL edges, not SOL -> USDC
        // For Program2-Program3 to work, we'd need:
        // - Program2: SOL -> USDC (currently missing)
        // - Program3: USDC -> SOL (exists)
        // OR start from USDC instead of SOL

        writeln!(handle, "=== Running Algorithm ===").unwrap();
        let result = find_cross_arbitrage_iterative(&edges, start_amount, 0, Some(sol));

        if result.is_none() {
            writeln!(handle, "No arbitrage found!").unwrap();
            writeln!(handle, "  Start amount: {}", start_amount).unwrap();
            writeln!(handle, "  Start token: {:?}", sol).unwrap();
            writeln!(handle, "  Total edges provided: {}", edges.len()).unwrap();
            writeln!(handle, "  Min profit required: {}", 0).unwrap();
            handle.flush().unwrap();
        }

        assert!(result.is_some());
        let arb = result.unwrap();

        // Get program IDs for identification (already defined above as Pubkey values)

        // Force output to stderr with explicit flush
        writeln!(handle, "Arbitrage Result:").unwrap();
        writeln!(handle, "  Profit: {}", arb.profit).unwrap();
        writeln!(handle, "  Final Amount: {}", arb.final_amount).unwrap();
        writeln!(handle, "  Number of edges: {}", arb.edges.len()).unwrap();
        writeln!(handle, "  Start Amount: {}", start_amount).unwrap();
        writeln!(handle, "").unwrap();
        writeln!(handle, "  Selected Path (Best Profit):").unwrap();
        for (i, edge) in arb.edges.iter().enumerate() {
            let direction = match edge.side {
                EdgeSide::LeftToRight => "->",
                EdgeSide::RightToLeft => "<-",
            };

            // Identify which program this edge belongs to
            let program_name = if edge.program == prog1_id {
                "Program1"
            } else if edge.program == prog2_id {
                "Program2"
            } else if edge.program == prog3_id {
                "Program3"
            } else {
                "Unknown"
            };

            writeln!(
                handle,
                "    Edge {}: {:?} {} {:?} (price: {}, {} - ID: {:?})",
                i + 1,
                edge.left.mint_account,
                direction,
                edge.right.mint_account,
                edge.price,
                program_name,
                edge.program
            )
            .unwrap();
        }
        handle.flush().unwrap();

        // Also use dbg! as backup
        dbg!(&arb.profit, &arb.final_amount, &arb.edges.len());

        // Verify that Program2-Program3 was selected (should be most profitable)
        let selected_prog1 = arb.edges[0].program == prog1_id;
        let selected_prog2 = arb.edges[0].program == prog2_id || arb.edges[1].program == prog2_id;
        let selected_prog3 = arb.edges[0].program == prog3_id || arb.edges[1].program == prog3_id;

        writeln!(handle, "").unwrap();
        writeln!(handle, "=== Verification ===").unwrap();
        writeln!(
            handle,
            "  Program2-Program3 selected: {}",
            selected_prog2 && selected_prog3 && !selected_prog1
        )
        .unwrap();
        writeln!(handle, "  Path uses Program1: {}", selected_prog1).unwrap();
        writeln!(handle, "  Path uses Program2: {}", selected_prog2).unwrap();
        writeln!(handle, "  Path uses Program3: {}", selected_prog3).unwrap();
        handle.flush().unwrap();

        assert_eq!(arb.edges.len(), 2);
        // Program2-Program3 should be selected (most profitable)
        assert!(
            selected_prog2 && selected_prog3,
            "Expected Program2-Program3 path, but got different programs"
        );
    }

    #[test]
    fn test_triangular_arbitrage_logic() {
        let token_a = Pubkey::new_unique();
        let token_b = Pubkey::new_unique();
        let token_c = Pubkey::new_unique();

        let prog_struct = MockProgram::new();
        let program: &dyn ProgramMeta = &prog_struct;

        // Path: A -> B -> C -> A
        // A -> B: 2.0
        let pool_ab_left = Pool::new(&token_a, 1_000_000_000);
        let pool_ab_right = Pool::new(&token_b, 2_000_000_000);
        let price_ab = if pool_ab_left.amount > 0 {
            pool_ab_right.amount as f64 / pool_ab_left.amount as f64
        } else {
            0.0
        };
        let prog_id = *program.get_id();
        let edge1 = Edge::new(
            prog_id,
            EdgeSide::LeftToRight,
            price_ab,
            pool_ab_left.clone(),
            pool_ab_right.clone(),
        );

        // B -> C: 3.0
        let pool_bc_left = Pool::new(&token_b, 1_000_000_000);
        let pool_bc_right = Pool::new(&token_c, 3_000_000_000);
        let price_bc = if pool_bc_left.amount > 0 {
            pool_bc_right.amount as f64 / pool_bc_left.amount as f64
        } else {
            0.0
        };
        let edge2 = Edge::new(
            prog_id,
            EdgeSide::LeftToRight,
            price_bc,
            pool_bc_left.clone(),
            pool_bc_right.clone(),
        );

        // C -> A: 0.2 (1/5)
        // Total: 2 * 3 * 0.2 = 1.2 (20% profit)
        let pool_ca_left = Pool::new(&token_c, 10_000_000_000);
        let pool_ca_right = Pool::new(&token_a, 2_000_000_000);
        let price_ca = if pool_ca_left.amount > 0 {
            pool_ca_right.amount as f64 / pool_ca_left.amount as f64
        } else {
            0.0
        };
        let edge3 = Edge::new(
            prog_id,
            EdgeSide::LeftToRight,
            price_ca,
            pool_ca_left.clone(),
            pool_ca_right.clone(),
        );

        let edges = vec![&edge1, &edge2, &edge3];
        let start_amount = 1_000_000_000;

        let result =
            find_triangular_arbitrage_iterative(&edges, start_amount, 40_000, Some(token_a));

        assert!(result.is_some());
        let arb = result.unwrap();
        assert_eq!(arb.final_amount, 1_200_000_000);
        assert_eq!(arb.profit, 200_000_000);
        assert_eq!(arb.edges.len(), 3);
    }
}
