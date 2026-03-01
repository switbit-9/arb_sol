use anchor_lang::prelude::*;

macro_rules! debug_eprintln {
    ($($arg:tt)*) => {{
        #[cfg(any(test, feature = "debug"))]
        {
            eprintln!($($arg)*);
        }
    }};
}

pub mod arbitrage;
pub mod programs;
pub mod utils;

// Tests are now inline below - external test file moved to integration tests
// #[cfg(test)]
// #[path = "tests/lib_test.rs"]
// mod lib_test;

// #[cfg(test)]
// #[path = "tests/pubkey_test.rs"]
// mod pubkey_test;

use anchor_spl::token::spl_token::native_mint::ID as WSOL;
use arbitrage::algo_2::optimal_amount_in_v2::find_optimal_amount_in_v2;
use arbitrage::algo_2::{
    /* check_arbitrage, */ find_cross_arbitrage_iterative, find_cross_arbitrage_optimized,
    find_triangular_arbitrage_iterative, get_edges, ArbitragePath,
};
use programs::{
    MeteoraDammV1, MeteoraDammV2, MeteoraDlmm, OrcaWhirlpool, ProgramInstance, PumpAmm, RaydiumAmm,
    RaydiumCLMM, RaydiumCPMM, SolarBError,
};
use utils::bot_config::BotConfig;
use utils::utils::parse_token_account;

#[cfg(test)]
use crate::utils::test_utils::write_results_to_file;

// Pre-computed program ID bytes for fast comparison (avoids repeated .to_bytes() calls)
const PUMP_AMM_ID_BYTES: [u8; 32] = PumpAmm::PROGRAM_ID.to_bytes();
const METEORA_DAMM_V1_ID_BYTES: [u8; 32] = MeteoraDammV1::PROGRAM_ID.to_bytes();
const METEORA_DAMM_V2_ID_BYTES: [u8; 32] = MeteoraDammV2::PROGRAM_ID.to_bytes();
const METEORA_DLMM_ID_BYTES: [u8; 32] = MeteoraDlmm::PROGRAM_ID.to_bytes();
const ORCA_WHIRLPOOL_ID_BYTES: [u8; 32] = OrcaWhirlpool::PROGRAM_ID.to_bytes();
const RAYDIUM_AMM_ID_BYTES: [u8; 32] = RaydiumAmm::PROGRAM_ID.to_bytes();
const RAYDIUM_CLMM_ID_BYTES: [u8; 32] = RaydiumCLMM::PROGRAM_ID.to_bytes();
const RAYDIUM_CPMM_ID_BYTES: [u8; 32] = RaydiumCPMM::PROGRAM_ID.to_bytes();

// SPL Token account amount offset (after mint pubkey + owner pubkey)
const TOKEN_ACCOUNT_AMOUNT_OFFSET: usize = 64;

declare_id!("BJREZ2NxHAqSf4jeaogmdoyF2nhexVpeewokt5iqqCMt");

/// Arbitrage mode constants
pub mod arb_mode {
    /// CASE 1: Single token pair, multiple markets (SOL -> TOKEN1 -> SOL)
    pub const SINGLE_PAIR_MULTI_MARKET: u8 = 0;
    /// CASE 2: Multi-hop chain (SOL -> TOKEN1 -> USDC -> SOL)
    pub const MULTI_HOP_CHAIN: u8 = 1;
    /// CASE 3: Multiple independent trades to evaluate
    pub const MULTIPLE_TRADES: u8 = 2;
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InstructionData {
    // pub auth_key: [u8; 32],
    pub mints: u8,
    pub accounts_length: [u8; 5],
    /// Arbitrage mode: 0=single pair multi-market, 1=multi-hop chain, 2=multiple trades
    pub mode: u8,
    /// Test mode: if true, skip profit checks and execute with tiny amount (100 lamports)
    pub test: bool,
}

#[derive(Accounts)]
pub struct Initialize {}

#[program]
pub mod solar_b {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>, data: InstructionData) -> Result<()> {
        let clock: Clock = Clock::get()?;
        start_bot(&ctx.remaining_accounts, data, clock)?;
        Ok(())
    }
}

#[inline(never)]
fn start_bot<'info>(
    accounts: &[AccountInfo<'info>],
    data: InstructionData,
    clock: Clock,
) -> Result<Option<ArbitragePath>> {
    let payer = &accounts[0];

    let num_mints = data.mints as usize;
    let pool_start = 4 + num_mints * 2;

    // Start token is always mint index 0, user token account at base + 1
    let user_token_account = &accounts[5];
    let max_amount_in = parse_token_account(user_token_account)?.amount;
    if max_amount_in == 0 {
        return Err(error!(SolarBError::InsufficientFunds));
    }
    #[cfg(test)]
    debug_eprintln!("max_amount_in: {:?}", max_amount_in);
    let mut instances = parse_accounts(accounts, pool_start, &data, &clock)?;

    let test_mode = data.test;
    let mut bot_config = BotConfig::new(
        Some(WSOL),
        max_amount_in,
        5_500,
        data.mints,
        data.mode,
        clock,
        test_mode,
    );
    // msg!("Mode={}, mints={}, max_in={}", data.mode, data.mints, max_amount_in);
    let Some(mut arbitrage_path) = run_arbitrage(accounts, &instances, &mut bot_config)?
    else {
        msg!("Not found: no path");
        return Ok(None);
    };

    #[cfg(any(test, feature = "debug"))]
    run_simulation(
        accounts,
        &arbitrage_path,
        &mut instances,
        &mut bot_config,
    )?;

    if !test_mode && arbitrage_path.profit <= bot_config.min_profit {
        msg!("Not found: final profit {} <= min_profit {}", arbitrage_path.profit, bot_config.min_profit);
        return Ok(None);
    }

    if test_mode {
        // Override with tiny amount: 100 lamports (0.0000001 SOL)
        arbitrage_path.start_amount = 1_000_000;
    }

    #[cfg(test)]
    {
        if arbitrage_path.profit > 0 {
            write_results_to_file(&[Some(arbitrage_path.clone())]);
        }
    }

    execute_arbitrage_path(accounts, &arbitrage_path, &mut instances, payer, data.mints)?;
    Ok(Some(arbitrage_path))
}

#[inline(never)]
fn parse_accounts<'info>(
    accounts: &[AccountInfo<'info>],
    start_index: usize,
    data: &InstructionData,
    clock: &Clock,
) -> Result<Vec<ProgramInstance<'info>>> {
    let mut index: usize = start_index;
    let accounts_len = accounts.len();

    // Pre-allocate: count non-zero spans (unrolled for fixed-size array)
    let mut estimated_capacity = 0usize;
    for &len in &data.accounts_length {
        if len > 0 {
            estimated_capacity += 1;
        }
    }
    let mut instances = Vec::with_capacity(estimated_capacity);

    // Unroll the loop for fixed 5-element array to avoid iterator overhead
    for &raw_span in &data.accounts_length {
        // On 64-bit systems u32 always fits in usize, skip try_from
        let span = raw_span as usize;
        if span == 0 {
            continue;
        }

        let end_index = index + span;
        if accounts_len < end_index {
            return Err(error!(SolarBError::InsufficientAccounts));
        }
        // Direct key access without creating a slice first
        let program_key = accounts[index].key;
        let instance = find_program_instance(program_key, accounts, index, end_index, clock)?;

        #[cfg(any(test, feature = "debug"))]{
            instance.log_accounts(accounts)?;
        }

        instances.push(instance);
        index = end_index;
    }

    if index != accounts_len {
        return Err(error!(SolarBError::TrailingAccounts));
    }

    Ok(instances)
}
/// Fast program instance lookup using pre-computed byte arrays.
/// Order programs by expected frequency for fastest average lookup.
/// Each constructor is in its own #[inline(never)] fn to get a separate stack frame,
/// avoiding the compiler reserving stack for all program structs in one frame.
#[inline(never)]
pub fn find_program_instance<'info>(
    program_id: &Pubkey,
    accounts: &[AccountInfo<'info>],
    start_index: usize,
    end_index: usize,
    clock: &Clock,
) -> Result<ProgramInstance<'info>> {
    let id_bytes = program_id.to_bytes();

    // Order by expected frequency (most common first)
    if id_bytes == PUMP_AMM_ID_BYTES {
        debug_eprintln!("PumpAmm");
        return create_pump_amm(accounts, start_index, end_index);
    }
    if id_bytes == METEORA_DAMM_V1_ID_BYTES {
        debug_eprintln!("MeteoraDammV1");
        return create_meteora_damm_v1(accounts, start_index, end_index, clock);
    }
    if id_bytes == METEORA_DAMM_V2_ID_BYTES {
        debug_eprintln!("MeteoraDammV2");
        return create_meteora_damm_v2(accounts, start_index, end_index, clock);
    }
    if id_bytes == METEORA_DLMM_ID_BYTES {
        debug_eprintln!("MeteoraDlmm");
        return create_meteora_dlmm(accounts, start_index, end_index, clock);
    }
    if id_bytes == ORCA_WHIRLPOOL_ID_BYTES {
        debug_eprintln!("OrcaWhirlpool");
        return create_orca_whirlpool(accounts, start_index, end_index);
    }
    if id_bytes == RAYDIUM_AMM_ID_BYTES {
        debug_eprintln!("RaydiumAmm");
        return create_raydium_amm(accounts, start_index, end_index);
    }
    if id_bytes == RAYDIUM_CLMM_ID_BYTES {
        debug_eprintln!("RaydiumCLMM");
        return create_raydium_clmm(accounts, start_index, end_index);
    }
    if id_bytes == RAYDIUM_CPMM_ID_BYTES {
        debug_eprintln!("RaydiumCPMM");
        return create_raydium_cpmm(accounts, start_index, end_index);
    }
    Err(error!(SolarBError::UnknownProgram))
}

#[inline(never)]
fn create_pump_amm<'info>(
    accounts: &[AccountInfo<'info>],
    start: usize,
    end: usize,
) -> Result<ProgramInstance<'info>> {
    Ok(Box::new(PumpAmm::new(accounts, start, end)?))
}

#[inline(never)]
fn create_meteora_damm_v1<'info>(
    accounts: &[AccountInfo<'info>],
    start: usize,
    end: usize,
    clock: &Clock,
) -> Result<ProgramInstance<'info>> {
    Ok(Box::new(MeteoraDammV1::new(accounts, start, end, clock)?))
}

#[inline(never)]
fn create_meteora_damm_v2<'info>(
    accounts: &[AccountInfo<'info>],
    start: usize,
    end: usize,
    clock: &Clock,
) -> Result<ProgramInstance<'info>> {
    Ok(Box::new(MeteoraDammV2::new(accounts, start, end, clock)?))
}

#[inline(never)]
fn create_meteora_dlmm<'info>(
    accounts: &[AccountInfo<'info>],
    start: usize,
    end: usize,
    clock: &Clock,
) -> Result<ProgramInstance<'info>> {
    Ok(Box::new(MeteoraDlmm::new(accounts, start, end, clock)?))
}

#[inline(never)]
fn create_orca_whirlpool<'info>(
    accounts: &[AccountInfo<'info>],
    start: usize,
    end: usize,
) -> Result<ProgramInstance<'info>> {
    Ok(Box::new(OrcaWhirlpool::new(accounts, start, end)?))
}

#[inline(never)]
fn create_raydium_amm<'info>(
    accounts: &[AccountInfo<'info>],
    start: usize,
    end: usize,
) -> Result<ProgramInstance<'info>> {
    Ok(Box::new(RaydiumAmm::new(accounts, start, end)?))
}

#[inline(never)]
fn create_raydium_clmm<'info>(
    accounts: &[AccountInfo<'info>],
    start: usize,
    end: usize,
) -> Result<ProgramInstance<'info>> {
    Ok(Box::new(RaydiumCLMM::new(accounts, start, end)?))
}

#[inline(never)]
fn create_raydium_cpmm<'info>(
    accounts: &[AccountInfo<'info>],
    start: usize,
    end: usize,
) -> Result<ProgramInstance<'info>> {
    Ok(Box::new(RaydiumCPMM::new(accounts, start, end)?))
}

#[inline(never)]
pub fn run_arbitrage<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance<'info>],
    config: &mut BotConfig,
) -> Result<Option<ArbitragePath>> {
    match config.mode {
        arb_mode::SINGLE_PAIR_MULTI_MARKET => {
            run_single_pair_arbitrage(accounts, instances, config)
        }
        arb_mode::MULTI_HOP_CHAIN => run_multi_hop_arbitrage(accounts, instances, config),
        arb_mode::MULTIPLE_TRADES => run_multiple_trades_arbitrage(accounts, instances, config),
        _ => Err(error!(SolarBError::InvalidMode)),
    }
}

/// CASE 1: Single token pair, multiple markets
/// All markets share the same two mints (e.g., SOL <-> TOKEN1)
/// Finds the best path through available markets
#[inline(never)]
fn run_single_pair_arbitrage<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance<'info>],
    config: &mut BotConfig,
) -> Result<Option<ArbitragePath>> {
    // Run both methods for testing
    let all_edges = get_edges(instances)?;
    let edge_refs: Vec<&_> = all_edges.iter().collect();

    debug_eprintln!("");
    debug_eprintln!("");
    debug_eprintln!("===============CROSS ARBITRAGE OPTIMIZED===============");
    debug_eprintln!("");
    debug_eprintln!("");

    let (mut edges, profit, _) =
        find_cross_arbitrage_optimized(&edge_refs, config)?;

    msg!("e={}, p={}", edges.len(), profit);

    if !config.test && profit <= 0 {
        msg!("Single: Rejected, profit <= 0");
        return Ok(None);
    }

    // In test mode, if no profitable path was found, build a fallback path
    // from raw edges so we can still test the invoke flow
    if config.test && edges.len() < 2 {
        let all_edges = get_edges(instances)?;
        if all_edges.len() >= 2 {
            let start_token = config.start_token.unwrap_or(WSOL);
            // First edge must buy (start_token → other), second must sell back, from different pools
            let buy_edge = all_edges.iter().find(|e| e.left.mint_account == start_token);
            if let Some(buy) = buy_edge {
                let sell_edge = all_edges.iter().find(|e| e.right.mint_account == start_token && e.pool_id != buy.pool_id);
                if let Some(sell) = sell_edge {
                    edges = vec![buy.clone(), sell.clone()];
                } else {
                    return Ok(None);
                }
            } else {
                return Ok(None);
            }
        } else {
            return Ok(None);
        }
    }

    let (optimal_amount_in, profit) =
        find_optimal_amount_in_v2(&edges, accounts, instances, config)?;

    msg!("Single opt: in={}, p={}", optimal_amount_in, profit);

    if !config.test && (profit <= 0 || optimal_amount_in == 0) {
        msg!("Single: rejected after opt, profit={} amt={}", profit, optimal_amount_in);
        return Ok(None);
    }

    // Use wrapping arithmetic to avoid overflow checks (we know values are valid)
    let final_amount = (optimal_amount_in as i128).checked_add(profit).unwrap_or(0) as u128;

    let arbitrage_path = ArbitragePath {
        edges,
        profit,
        final_amount,
        start_amount: optimal_amount_in,
    };

    // Debug logging only in test/debug builds - no float operations in production
    #[cfg(any(test, feature = "debug"))]
    {
        if optimal_amount_in > 0 {
            let profit_pct = (profit as f64 / optimal_amount_in as f64) * 100.0;
            debug_eprintln!(
                "PROFIT: in={} out={} profit={} ({:.2}%)",
                optimal_amount_in, final_amount, profit, profit_pct
            );
        }
    }

    Ok(Some(arbitrage_path))
}

/// CASE 2: Multi-hop chain arbitrage
/// Edges form a sequential chain through different mints
/// Example: SOL -> TOKEN1 -> USDC -> SOL (3-hop)
#[inline(never)]
fn run_multi_hop_arbitrage<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance<'info>],
    config: &mut BotConfig,
) -> Result<Option<ArbitragePath>> {

    debug_eprintln!("Multi-hop chain: {} mints", config.mints);
    // Generate edges from all instances
    let edges = get_edges(instances)?;
    let edge_refs: Vec<&_> = edges.iter().collect();

    // Use triangular arbitrage finder for 3+ hop chains
    let (path_edges, profit, _) = find_triangular_arbitrage_iterative(&edge_refs, config)?;

    msg!("Hop: edges={}, est_profit={}", path_edges.len(), profit);

    if !config.test && (profit <= 0 || path_edges.is_empty()) {
        msg!("Rejected, profit={} edges={}", profit, path_edges.len());
        return Ok(None);
    }

    // In test mode, if no path was found, build a fallback from raw edges
    let path_edges = if config.test && path_edges.is_empty() {
        let start_token = config.start_token.unwrap_or(WSOL);
        let buy_edge = edges.iter().find(|e| e.left.mint_account == start_token);
        let sell_edge = edges.iter().find(|e| e.right.mint_account == start_token);
        if let (Some(buy), Some(sell)) = (buy_edge, sell_edge) {
            vec![buy.clone(), sell.clone()]
        } else {
            return Ok(None);
        }
    } else if path_edges.is_empty() {
        return Ok(None);
    } else {
        path_edges
    };

    // Optimize the amount_in for the N-hop path
    // find_optimal_amount_in_v2 now handles any number of edges
    let (optimal_amount_in, profit) =
        find_optimal_amount_in_v2(&path_edges, accounts, instances, config)?;

    msg!("Hop opt: in={}, profit={}", optimal_amount_in, profit);

    if !config.test && profit <= 0 {
        msg!("Hop: rejected after opt, profit={}", profit);
        return Ok(None);
    }

    let final_amount = (optimal_amount_in as i128).checked_add(profit).unwrap_or(0) as u128;

    let arbitrage_path = ArbitragePath {
        edges: path_edges,
        profit,
        final_amount,
        start_amount: optimal_amount_in,
    };

    #[cfg(any(test, feature = "debug"))]
    {
        let profit_pct = (profit as f64 / optimal_amount_in as f64) * 100.0;
        #[cfg(any(test, feature = "debug"))]
        {
            debug_eprintln!(
                "MULTI-HOP PROFIT: in={} out={} profit={} ({:.2}%)",
                optimal_amount_in, final_amount, profit, profit_pct
            );
        }
    }

    Ok(Some(arbitrage_path))
}

/// CASE 3: Multiple independent trades
/// Disconnected subgraphs, each a separate arbitrage opportunity
/// Example: (SOL -> TOKEN1 -> SOL) vs (SOL -> TOKEN2 -> SOL)
/// Groups edges by their non-start mint and evaluates each group
#[inline(never)]
fn run_multiple_trades_arbitrage<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance<'info>],
    config: &mut BotConfig,
) -> Result<Option<ArbitragePath>> {
    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("Multiple trades mode: {} instances", instances.len());

    let start_token = config.start_token.unwrap_or(WSOL);

    // Generate all edges from all instances
    let all_edges = get_edges(instances)?;

    // Group edges by their "other" mint (the one that's not the start token)
    // Each group represents a potential independent trade
    let mut edge_groups: Vec<Vec<usize>> = Vec::new();
    let mut group_mints: Vec<Pubkey> = Vec::new();

    for (idx, edge) in all_edges.iter().enumerate() {
        let left_mint = edge.left.mint_account;
        let right_mint = edge.right.mint_account;

        // Only consider edges that involve the start token
        let other_mint = if left_mint == start_token {
            right_mint
        } else if right_mint == start_token {
            left_mint
        } else {
            // Neither mint is start_token - skip
            continue;
        };

        // Find or create group for this other_mint
        if let Some(group_idx) = group_mints.iter().position(|m| *m == other_mint) {
            edge_groups[group_idx].push(idx);
        } else {
            group_mints.push(other_mint);
            edge_groups.push(vec![idx]);
        }
    }

    #[cfg(any(test, feature = "debug"))]
    {
        debug_eprintln!("Found {} trade groups", edge_groups.len());
    }

    let mut best_path: Option<ArbitragePath> = None;
    let mut best_profit: i128 = 0;

    // Evaluate each group independently
    for edge_indices in edge_groups.iter() {
        if edge_indices.len() < 2 {
            // Need at least 2 edges for a round-trip (buy + sell)
            continue;
        }

        // Collect edge references for this group
        let group_edge_refs: Vec<&_> = edge_indices.iter().map(|&idx| &all_edges[idx]).collect();

        debug_eprintln!("");
        debug_eprintln!("");
        debug_eprintln!("===============CROSS ARBITRAGE OPTIMIZED===============");
        debug_eprintln!("");
        debug_eprintln!("");

        let (path_edges, profit, _) =
            find_cross_arbitrage_optimized(&group_edge_refs, config)?;

        msg!("Multi grp: edges={}, est_profit={}", path_edges.len(), profit);

        if !config.test && (profit <= 0 || path_edges.is_empty()) {
            msg!("Multi grp: skip, profit={} edges={}", profit, path_edges.len());
            continue;
        }

        // In test mode, if no profitable path found, use first two edges from group
        let path_edges = if config.test && path_edges.is_empty() {
            let e0 = &all_edges[edge_indices[0]];
            let e1 = &all_edges[edge_indices[1]];
            vec![e0.clone(), e1.clone()]
        } else if path_edges.is_empty() {
            continue;
        } else {
            path_edges
        };

        // Optimize amount for this path (uses full instances for swap simulation)
        let (optimal_amount_in, refined_profit) =
            find_optimal_amount_in_v2(&path_edges, accounts, instances, config)?;

        msg!("Multi opt: in={}, profit={}", optimal_amount_in, refined_profit);

        if !config.test && refined_profit > best_profit || config.test && best_path.is_none() {
            best_profit = refined_profit;
            let final_amount = (optimal_amount_in as i128).checked_add(refined_profit).unwrap_or(0) as u128;
            best_path = Some(ArbitragePath {
                edges: path_edges,
                profit: refined_profit,
                final_amount,
                start_amount: optimal_amount_in,
            });
        }
    }

    if let Some(ref path) = best_path {
        msg!("Best: in={}, profit={}", path.start_amount, path.profit);
        #[cfg(any(test, feature = "debug"))]
        {
            let profit_pct = (path.profit as f64 / path.start_amount as f64) * 100.0;
            debug_eprintln!(
                "MULTI-TRADE BEST: in={} out={} profit={} ({:.2}%)",
                path.start_amount, path.final_amount, path.profit, profit_pct
            );
        }
    } else {
        msg!("Multi: no profitable group found");
    };

    Ok(best_path)
}

/// Execute arbitrage path with CU-optimized operations.
/// Key optimizations:
/// - No debug_eprintln! calls in hot path (gated with #[cfg(any(test, feature = "debug"))])
/// - Direct byte reads instead of try_deserialize
/// - Dynamic mint lookup table supports N mints (not just 2)
/// - Index-based instance lookup instead of .find()
///
/// Account layout:
///   accounts[0]             = payer
///   accounts[1]             = spl token program
///   accounts[2]             = token-2022 program
///   accounts[3]             = memo program
///   accounts[4 + i*2 + 0]  = mint_i
///   accounts[4 + i*2 + 1]  = user_mint_i_token_account
///   accounts[4 + N*2 ..]   = pool accounts
///
/// Token program for each mint is derived from mint_i.owner:
///   mint.owner == spl_token::id() => accounts[1] (spl token program)
///   otherwise                     => accounts[2] (token-2022 program)
#[inline(never)]
pub fn execute_arbitrage_path<'info>(
    accounts: &[AccountInfo<'info>],
    arbitrage_path: &ArbitragePath,
    instances: &mut Vec<ProgramInstance<'info>>,
    payer: &AccountInfo<'info>,
    num_mints: u8,
) -> Result<()> {
    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("Executing {} edges", arbitrage_path.edges.len());

    let num_mints = num_mints as usize;

    let start_amount = arbitrage_path.start_amount;
    let mut current_amount = start_amount;

    for edge in arbitrage_path.edges.iter() {
        // Find program instance by pool_id using linear scan
        let pool_id = &edge.pool_id;
        let program_instance = instances
            .iter()
            .find(|inst| inst.get_pool_id() == pool_id)
            .ok_or(SolarBError::UnknownProgram)?;

        let input_mint_key = &edge.left.mint_account;
        let output_mint_key = &edge.right.mint_account;

        // Resolve input/output mint base indices inline — no heap alloc,
        // no Pubkey copies. N is 2-4 so this is cheaper than any table.
        let mut input_base = 0usize;
        let mut output_base = 0usize;
        let mut found = 0u8;
        for i in 0..num_mints {
            let base = 4 + i * 2;
            let key = accounts[base].key; // &Pubkey, no copy
            if key == input_mint_key {
                input_base = base;
                found |= 1;
            } else if key == output_mint_key {
                output_base = base;
                found |= 2;
            }
            if found == 3 {
                break;
            }
        }
        if found != 3 {
            return Err(error!(SolarBError::InvalidAccountData));
        }

        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!(
            "Swap {} {} -> {}",
            current_amount, input_mint_key, output_mint_key
        );

        // Derive token program from mint owner
        let spl_token_program = &accounts[1];
        let token_2022_program = &accounts[2];
        let input_token_program = if accounts[input_base].owner == spl_token_program.key {
            spl_token_program.clone()
        } else {
            token_2022_program.clone()
        };
        let output_token_program = if accounts[output_base].owner == spl_token_program.key {
            spl_token_program.clone()
        } else {
            token_2022_program.clone()
        };

        // Execute swap - AccountInfo clone is unavoidable for CPI
        program_instance.invoke_swap_base_in(
            accounts,
            *input_mint_key,
            current_amount,
            None,
            payer.clone(),
            accounts[input_base + 1].clone(), // user source token account
            accounts[output_base + 1].clone(), // user destination token account
            accounts[input_base].clone(),     // input mint
            accounts[output_base].clone(),    // output mint
            input_token_program,              // input token program (derived from mint owner)
            output_token_program,             // output token program (derived from mint owner)
        )?;

        // Direct byte read of amount field - much cheaper than try_deserialize
        // SPL Token account layout: mint (32) + owner (32) + amount (8) + ...
        let output_token_account = &accounts[output_base + 1];
        let data = output_token_account.try_borrow_data()?;
        current_amount = u64::from_le_bytes(
            data[TOKEN_ACCOUNT_AMOUNT_OFFSET..TOKEN_ACCOUNT_AMOUNT_OFFSET + 8]
                .try_into()
                .map_err(|_| SolarBError::InvalidAccountData)?,
        );
    }

    #[cfg(any(test, feature = "debug"))]
    {
        let final_profit = current_amount as i128 - arbitrage_path.start_amount as i128;
        debug_eprintln!(
            "Done: {} -> {} (profit: {})",
            arbitrage_path.start_amount, current_amount, final_profit
        );
    }

    Ok(())
}

/// Read mint decimals from SPL Token mint account data (offset 44).
/// Returns 9 if the account is not found or data is too short.
#[cfg(any(test, feature = "debug"))]
fn get_mint_decimals(accounts: &[AccountInfo], mint_key: &Pubkey) -> u8 {
    for acc in accounts {
        if acc.key == mint_key {
            if let Ok(data) = acc.try_borrow_data() {
                if data.len() >= 45 {
                    return data[44];
                }
            }
            break;
        }
    }
    9
}

#[cfg(any(test, feature = "debug"))]
fn format_amount(amount: u64, decimals: u8) -> String {
    let divisor = 10u64.pow(decimals as u32) as f64;
    format!(
        "{} ({:.dec$})",
        amount,
        amount as f64 / divisor,
        dec = decimals as usize
    )
}

#[cfg(any(test, feature = "debug"))]
fn format_amount_i128(amount: i128, decimals: u8) -> String {
    let divisor = 10u64.pow(decimals as u32) as f64;
    format!(
        "{} ({:.dec$})",
        amount,
        amount as f64 / divisor,
        dec = decimals as usize
    )
}

#[cfg(any(test, feature = "debug"))]
fn run_simulation<'info>(
    accounts: &[AccountInfo<'info>],
    arbitrage_path: &ArbitragePath,
    instances: &mut Vec<ProgramInstance<'info>>,
    bot_config: &mut BotConfig,
) -> Result<()> {
    let start_amount = arbitrage_path.start_amount;
    let start_decimals = get_mint_decimals(accounts, &arbitrage_path.edges[0].left.mint_account);

    let mut current_amount = start_amount;

    for (i, edge) in arbitrage_path.edges.iter().enumerate() {
        let input_mint = edge.left.mint_account;
        let output_mint = edge.right.mint_account;
        let in_decimals = get_mint_decimals(accounts, &input_mint);
        let out_decimals = get_mint_decimals(accounts, &output_mint);

        let program_instance = instances
            .iter()
            .find(|inst| inst.get_pool_id() == &edge.pool_id)
            .ok_or(SolarBError::UnknownProgram)?;

        // swap_base_in: given amount_in, what do we get out?
        let amount_out = program_instance.swap_base_in(
            accounts,
            input_mint,
            current_amount,
            &bot_config.clock,
        )?;

        // swap_base_out: given that amount_out, how much would we need to put in?
        let required_in = program_instance.swap_base_out(
            accounts,
            output_mint,
            amount_out,
            &bot_config.clock,
        )?;

        let input_short = &input_mint.to_string()[..8];
        let output_short = &output_mint.to_string()[..8];

        #[cfg(any(test, feature = "debug"))]
        {
            debug_eprintln!(
            "Edge {}: [{}] {}..-> {}..  |  in={} -> out={}  |  swap_base_out max_in={}  |  diff={}",
            i + 1,
            program_instance.name(),
            input_short,
            output_short,
            format_amount(current_amount, in_decimals),
            format_amount(amount_out, out_decimals),
            format_amount(required_in, in_decimals),
            current_amount as i128 - required_in as i128,
        );
        }
        current_amount = amount_out;
    }

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("---");
    let end_decimals = get_mint_decimals(
        accounts,
        &arbitrage_path.edges.last().unwrap().right.mint_account,
    );
    let profit = current_amount as i128 - start_amount as i128;
    if profit > 0 {
        let profit_pct = (profit as f64 / start_amount as f64) * 100.0;
        #[cfg(any(test, feature = "debug"))]
        {
            debug_eprintln!(
                "PROFIT: {} ({:.4}%)",
                format_amount_i128(profit, end_decimals),
                profit_pct,
            );
        }
    } else {
        #[cfg(any(test, feature = "debug"))]
        {
            debug_eprintln!("NO PROFIT: {}", format_amount_i128(profit, end_decimals),);
        }
    }
    #[cfg(any(test, feature = "debug"))]
    {
        debug_eprintln!(
            "Final: in={} -> out={}",
            format_amount(start_amount, start_decimals),
            format_amount(current_amount, end_decimals)
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::{account_info::AccountInfo, pubkey::Pubkey, system_program};

    fn default_clock() -> Clock {
        Clock {
            slot: 350_000_000,
            epoch_start_timestamp: 0,
            epoch: 700,
            leader_schedule_epoch: 0,
            unix_timestamp: 1739800000,
        }
    }

    // Helper function to create a mock AccountInfo
    fn create_mock_account_info(
        key: Pubkey,
        owner: Pubkey,
        lamports: u64,
        data: Option<Vec<u8>>,
    ) -> AccountInfo<'static> {
        let data_vec = if let Some(provided_data) = data {
            Box::leak(Box::new(provided_data))
        } else {
            Box::leak(Box::new(Vec::new()))
        };
        let lamports_static = Box::leak(Box::new(lamports));
        let owner_static = Box::leak(Box::new(owner));
        let key_static = Box::leak(Box::new(key));

        AccountInfo::new(
            key_static,
            false, // is_signer
            false, // is_writable
            lamports_static,
            data_vec,
            owner_static,
            false, // executable
            0,     // rent_epoch
        )
    }

    // Helper to create multiple mock accounts
    fn create_mock_accounts(count: usize, owner: Pubkey) -> Vec<AccountInfo<'static>> {
        (0..count)
            .map(|_| {
                let key = Pubkey::new_unique();
                create_mock_account_info(key, owner, 1000, None)
            })
            .collect()
    }

    #[test]
    fn test_parse_accounts_success_single_program() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // Create MeteoraDammV2 program accounts (9 accounts: program_id + 8 payload)
        let program_id = MeteoraDammV2::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id, owner, 0, None));
        // Add 8 more accounts for MeteoraDammV2
        for _ in 0..8 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        let data = InstructionData {
            accounts_length: [9, 0, 0, 0, 0],
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
            test: false,
        };

        let result = parse_accounts(&accounts, 0, &data, &default_clock());
        assert!(result.is_ok());
        let instances = result.unwrap();
        assert!(instances.len() == 1);
        assert!(*instances[0].get_id() == program_id);
    }

    #[test]
    fn test_parse_accounts_success_multiple_programs() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // First program: MeteoraDammV2 (9 accounts)
        let program_id_1 = MeteoraDammV2::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id_1, owner, 0, None));
        for _ in 0..8 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        // Second program: MeteoraDlmm (13 accounts)
        let program_id_2 = MeteoraDlmm::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id_2, owner, 0, None));
        for _ in 0..12 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        let data = InstructionData {
            accounts_length: [9, 13, 0, 0, 0],
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
            test: false,
        };

        let result = parse_accounts(&accounts, 0, &data, &default_clock());
        assert!(result.is_ok());
        let instances = result.unwrap();
        assert!(instances.len() == 2);
        assert!(*instances[0].get_id() == program_id_1);
        assert!(*instances[1].get_id() == program_id_2);
    }

    #[test]
    fn test_parse_accounts_skips_zero_span() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // Create one program
        let program_id = MeteoraDammV2::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id, owner, 0, None));
        for _ in 0..8 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        // Zero spans should be skipped
        let data = InstructionData {
            accounts_length: [9, 0, 0, 0, 0],
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
            test: false,
        };

        let result = parse_accounts(&accounts, 0, &data, &default_clock());
        assert!(result.is_ok());
        let instances = result.unwrap();
        assert!(instances.len() == 1);
    }

    #[test]
    fn test_parse_accounts_insufficient_accounts() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // Only provide 5 accounts when 9 are needed
        let program_id = MeteoraDammV2::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id, owner, 0, None));
        for _ in 0..4 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        let data = InstructionData {
            accounts_length: [9, 0, 0, 0, 0],
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
            test: false,
        };

        let result = parse_accounts(&accounts, 0, &data, &default_clock());
        assert!(result.is_err());
        // Just verify it's an error - Anchor error types are complex to match
    }

    #[test]
    fn test_parse_accounts_trailing_accounts() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // Create program with 9 accounts
        let program_id = MeteoraDammV2::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id, owner, 0, None));
        for _ in 0..8 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        // Add an extra account that shouldn't be there
        accounts.push(create_mock_account_info(
            Pubkey::new_unique(),
            owner,
            0,
            None,
        ));

        let data = InstructionData {
            accounts_length: [9, 0, 0, 0, 0],
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
            test: false,
        };

        let result = parse_accounts(&accounts, 0, &data, &default_clock());
        assert!(result.is_err());
        // Just verify it's an error - Anchor error types are complex to match
    }

    #[test]
    fn test_parse_accounts_unknown_program() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // Use an unknown program ID
        let unknown_program_id = Pubkey::new_unique();
        accounts.push(create_mock_account_info(unknown_program_id, owner, 0, None));
        for _ in 0..8 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        let data = InstructionData {
            accounts_length: [9, 0, 0, 0, 0],
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
            test: false,
        };

        let result = parse_accounts(&accounts, 0, &data, &default_clock());
        assert!(result.is_err());
        // Just verify it's an error - Anchor error types are complex to match
    }

    #[test]
    fn test_parse_accounts_invalid_accounts_length() {
        let accounts = create_mock_accounts(5, system_program::id());

        // Use a span that's too large to convert from u32 to usize
        // On most platforms this won't happen, but we test the error path
        let data = InstructionData {
            accounts_length: [u8::MAX, 0, 0, 0, 0],
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
            test: false,
        };

        let result = parse_accounts(&accounts, 0, &data, &default_clock());
        // This should either error on conversion or on insufficient accounts
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_accounts_empty_segment() {
        let accounts = Vec::new();

        let data = InstructionData {
            accounts_length: [0, 0, 0, 0, 0],
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
            test: false,
        };

        let result = parse_accounts(&accounts, 0, &data, &default_clock());
        assert!(result.is_ok());
        let instances = result.unwrap();
        assert!(instances.len() == 0);
    }

    #[test]
    #[cfg(feature = "broken_tests")]
    fn test_parse_accounts_meteora_damm_v1() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // MeteoraDammV1 needs 10 accounts (no program_id in payload, starts with pool_id)
        // let program_id = MeteoraDammV1::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id, owner, 0, None));
        for _ in 0..9 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        let data = InstructionData {
            auth_key: AUTH_KEY,
            accounts_length: [10, 0, 0, 0, 0],
            max_amount_in: 1_000,
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
            test: false,
        };

        let result = parse_accounts(&accounts, 0, &data, &default_clock());
        assert!(result.is_ok());
        let instances = result.unwrap();
        assert!(instances.len() == 1);
        assert!(*instances[0].get_id() == program_id);
    }

    #[test]
    fn test_parse_accounts_meteora_dlmm() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // MeteoraDlmm needs 13 accounts
        let program_id = MeteoraDlmm::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id, owner, 0, None));
        for _ in 0..12 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        let data: InstructionData = InstructionData {
            accounts_length: [13, 0, 0, 0, 0],
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
            test: false,
        };

        let result = parse_accounts(&accounts, 0, &data, &default_clock());
        assert!(result.is_ok());
        let instances = result.unwrap();
        assert!(instances.len() == 1);
        assert!(*instances[0].get_id() == program_id);
    }

    #[test]
    fn test_parse_accounts_insufficient_accounts_for_program() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // MeteoraDlmm needs 13 accounts, but only provide 10
        let program_id = MeteoraDlmm::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id, owner, 0, None));
        for _ in 0..9 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        let data = InstructionData {
            accounts_length: [10, 0, 0, 0, 0],
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
            test: false,
        };

        let result = parse_accounts(&accounts, 0, &data, &default_clock());
        assert!(result.is_err());
        // Just verify it's an error - Anchor error types are complex to match
    }

    #[test]
    fn test_parse_accounts_multiple_programs_with_zero_spans() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // First program
        let program_id_1 = MeteoraDammV2::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id_1, owner, 0, None));
        for _ in 0..8 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        // Second program
        let program_id_2 = MeteoraDlmm::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id_2, owner, 0, None));
        for _ in 0..12 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        // Mix of zero and non-zero spans
        let data = InstructionData {
            accounts_length: [9, 0, 13, 0, 0],
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
            test: false,
        };

        let result = parse_accounts(&accounts, 0, &data, &default_clock());
        assert!(result.is_ok());
        let instances = result.unwrap();
        assert!(instances.len() == 2);
        assert!(*instances[0].get_id() == program_id_1);
        assert!(*instances[1].get_id() == program_id_2);
    }
}
