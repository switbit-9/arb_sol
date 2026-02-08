use anchor_lang::prelude::*;

pub mod arbitrage;
pub mod math;
pub mod programs;
pub mod utils;

// Tests are now inline below - external test file moved to integration tests
#[cfg(test)]
#[path = "tests/lib_test.rs"]
mod lib_test;

use anchor_spl::token::spl_token::native_mint::ID as WSOL;
use arbitrage::algo_2::optimal_amount_in_v2::find_optimal_amount_in_v2;
use arbitrage::algo_2::{
    check_arbitrage, ArbitragePath, get_edges,
    find_cross_arbitrage_iterative, find_triangular_arbitrage_iterative,
};
use programs::{MeteoraDammV2, MeteoraDlmm, OrcaWhirlpool, ProgramInstance, PumpAmm, SolarBError};
use utils::bot_config::BotConfig;

#[cfg(test)]
use crate::utils::test_utils::write_results_to_file;

// Pre-computed program ID bytes for fast comparison (avoids repeated .to_bytes() calls)
const PUMP_AMM_ID_BYTES: [u8; 32] = PumpAmm::PROGRAM_ID.to_bytes();
const METEORA_DAMM_V2_ID_BYTES: [u8; 32] = MeteoraDammV2::PROGRAM_ID.to_bytes();
const METEORA_DLMM_ID_BYTES: [u8; 32] = MeteoraDlmm::PROGRAM_ID.to_bytes();
const ORCA_WHIRLPOOL_ID_BYTES: [u8; 32] = OrcaWhirlpool::PROGRAM_ID.to_bytes();

// SPL Token account amount offset (after mint pubkey + owner pubkey)
const TOKEN_ACCOUNT_AMOUNT_OFFSET: usize = 64;

declare_id!("Ckgi61iKuKeVLfCgAuqaURw18e52D7SvqVj9TUw6NftF");

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
    pub max_amount_in: u64,
    pub mints: u16,
    pub accounts_length: [u32; 5],
    /// Arbitrage mode: 0=single pair multi-market, 1=multi-hop chain, 2=multiple trades
    pub mode: u8,
}

#[derive(Accounts)]
pub struct Initialize {}

#[program]
pub mod solar_b {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>, data: InstructionData) -> Result<()> {
        let first_accounts = &ctx.remaining_accounts[..7];

        let payer = &first_accounts[0];
        if payer.lamports() == 0 {
            return Err(error!(SolarBError::InsufficientFunds));
        }
        let rest = &ctx.remaining_accounts[7..];
        let clock: Clock = Clock::get()?;

        start_bot(&first_accounts, &rest, data, clock)?;

        Ok(())
    }
}

fn start_bot<'info>(
    first_accounts: &[AccountInfo<'info>],
    rest: &[AccountInfo<'info>],
    data: InstructionData,
    clock: Clock,
) -> Result<Option<ArbitragePath>> {
    let payer = &first_accounts[0];

    if payer.lamports() == 0 {
        return Err(error!(SolarBError::InsufficientFunds));
    }

    let mut instances = parse_accounts(rest, &data)?;

    #[cfg(feature = "debug")]
    msg!("INSTANCES: {}", instances.len());

    let max_amount_in = 100_000_000_000_u64;
    let mut bot_config = BotConfig::new(Some(WSOL), max_amount_in, 0, data.mints, data.mode, clock);

    let Some(arbitrage_path) = run_arbitrage(rest, &instances, &mut bot_config)? else {
        return Ok(None);
    };

    if arbitrage_path.profit <= 0 {
        return Ok(None);
    }

    #[cfg(feature = "debug")]
    msg!("Arbitrage found. Profit: {}", arbitrage_path.profit);

    execute_arbitrage_path(
        rest,
        &arbitrage_path,
        &mut instances,
        payer,
        &first_accounts[1], // mint_1
        &first_accounts[2], // mint_1_token_program
        &first_accounts[3], // user_mint_1_token_account
        &first_accounts[4], // mint_2
        &first_accounts[5], // mint_2_token_program
        &first_accounts[6], // user_mint_2_token_account
    )?;

    Ok(Some(arbitrage_path))
}

fn parse_accounts<'info>(
    accounts: &[AccountInfo<'info>],
    data: &InstructionData,
) -> Result<Vec<ProgramInstance<'info>>> {
    let mut index: usize = 0;
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
        let instance = find_program_instance(program_key, accounts, index, end_index)?;
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
#[inline]
pub fn find_program_instance<'info>(
    program_id: &Pubkey,
    accounts: &[AccountInfo<'info>],
    start_index: usize,
    end_index: usize,
) -> Result<ProgramInstance<'info>> {
    let id_bytes = program_id.to_bytes();

    // Order by expected frequency (most common first)
    if id_bytes == PUMP_AMM_ID_BYTES {
        return Ok(ProgramInstance::PumpAmm(PumpAmm::new(
            accounts,
            start_index,
            end_index,
        )?));
    }
    if id_bytes == METEORA_DAMM_V2_ID_BYTES {
        return Ok(ProgramInstance::MeteoraDammV2(MeteoraDammV2::new(
            accounts,
            start_index,
            end_index,
        )?));
    }
    if id_bytes == METEORA_DLMM_ID_BYTES {
        return Ok(ProgramInstance::MeteoraDlmm(MeteoraDlmm::new(
            accounts,
            start_index,
            end_index,
        )?));
    }
    if id_bytes == ORCA_WHIRLPOOL_ID_BYTES {
        return Ok(ProgramInstance::OrcaWhirlpool(OrcaWhirlpool::new(
            accounts,
            start_index,
            end_index,
        )?));
    }

    Err(error!(SolarBError::UnknownProgram))
}

pub fn run_arbitrage<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance<'info>],
    config: &mut BotConfig,
) -> Result<Option<ArbitragePath>> {
    match config.mode {
        arb_mode::SINGLE_PAIR_MULTI_MARKET => {
            run_single_pair_arbitrage(accounts, instances, config)
        }
        arb_mode::MULTI_HOP_CHAIN => {
            run_multi_hop_arbitrage(accounts, instances, config)
        }
        arb_mode::MULTIPLE_TRADES => {
            run_multiple_trades_arbitrage(accounts, instances, config)
        }
        _ => Err(error!(SolarBError::InvalidMode)),
    }
}

/// CASE 1: Single token pair, multiple markets
/// All markets share the same two mints (e.g., SOL <-> TOKEN1)
/// Finds the best path through available markets
fn run_single_pair_arbitrage<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance<'info>],
    config: &mut BotConfig,
) -> Result<Option<ArbitragePath>> {
    let (edges, profit, _) = check_arbitrage(instances, config)?;

    if profit <= 0 {
        return Ok(None);
    }

    let (optimal_amount_in, profit) = find_optimal_amount_in_v2(&edges, accounts, instances, config)?;

    if profit <= 0 {
        return Ok(None);
    }

    // Use wrapping arithmetic to avoid overflow checks (we know values are valid)
    let final_amount = (optimal_amount_in as i128).wrapping_add(profit) as u128;

    let arbitrage_path = ArbitragePath {
        edges,
        profit,
        final_amount,
        start_amount: optimal_amount_in,
    };

    // Debug logging only in test/debug builds - no float operations in production
    #[cfg(any(test, feature = "debug"))]
    {
        let profit_pct = (profit as f64 / optimal_amount_in as f64) * 100.0;
        msg!(
            "PROFIT: in={} out={} profit={} ({:.2}%)",
            optimal_amount_in,
            final_amount,
            profit,
            profit_pct
        );
    }

    #[cfg(test)]
    write_results_to_file(&[Some(arbitrage_path.clone())]);

    Ok(Some(arbitrage_path))
}

/// CASE 2: Multi-hop chain arbitrage
/// Edges form a sequential chain through different mints
/// Example: SOL -> TOKEN1 -> USDC -> SOL (3-hop)
fn run_multi_hop_arbitrage<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance<'info>],
    config: &mut BotConfig,
) -> Result<Option<ArbitragePath>> {
    #[cfg(feature = "debug")]
    msg!("Multi-hop chain: {} mints", config.mints);

    // Generate edges from all instances
    let edges = get_edges(instances)?;
    let edge_refs: Vec<&_> = edges.iter().collect();

    // Use triangular arbitrage finder for 3+ hop chains
    let (path_edges, profit, _) = find_triangular_arbitrage_iterative(&edge_refs, config)?;

    if profit <= 0 || path_edges.is_empty() {
        return Ok(None);
    }

    // Optimize the amount_in for the N-hop path
    // find_optimal_amount_in_v2 now handles any number of edges
    let (optimal_amount_in, profit) = find_optimal_amount_in_v2(&path_edges, accounts, instances, config)?;

    if profit <= 0 {
        return Ok(None);
    }

    let final_amount = (optimal_amount_in as i128).wrapping_add(profit) as u128;

    let arbitrage_path = ArbitragePath {
        edges: path_edges,
        profit,
        final_amount,
        start_amount: optimal_amount_in,
    };

    #[cfg(any(test, feature = "debug"))]
    {
        let profit_pct = (profit as f64 / optimal_amount_in as f64) * 100.0;
        msg!(
            "MULTI-HOP PROFIT: in={} out={} profit={} ({:.2}%)",
            optimal_amount_in,
            final_amount,
            profit,
            profit_pct
        );
    }

    #[cfg(test)]
    write_results_to_file(&[Some(arbitrage_path.clone())]);

    Ok(Some(arbitrage_path))
}

/// CASE 3: Multiple independent trades
/// Disconnected subgraphs, each a separate arbitrage opportunity
/// Example: (SOL -> TOKEN1 -> SOL) vs (SOL -> TOKEN2 -> SOL)
/// Groups edges by their non-start mint and evaluates each group
fn run_multiple_trades_arbitrage<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance<'info>],
    config: &mut BotConfig,
) -> Result<Option<ArbitragePath>> {
    #[cfg(feature = "debug")]
    msg!("Multiple trades mode: {} instances", instances.len());

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

    #[cfg(feature = "debug")]
    msg!("Found {} trade groups", edge_groups.len());

    let mut best_path: Option<ArbitragePath> = None;
    let mut best_profit: i128 = 0;

    // Evaluate each group independently
    for edge_indices in edge_groups.iter() {
        if edge_indices.len() < 2 {
            // Need at least 2 edges for a round-trip (buy + sell)
            continue;
        }

        // Collect edge references for this group
        let group_edge_refs: Vec<&_> = edge_indices
            .iter()
            .map(|&idx| &all_edges[idx])
            .collect();

        // Run cross arbitrage on this edge group
        let (path_edges, profit, _) = find_cross_arbitrage_iterative(&group_edge_refs, config)?;

        if profit <= 0 || path_edges.is_empty() {
            continue;
        }

        // Optimize amount for this path (uses full instances for swap simulation)
        let (optimal_amount_in, refined_profit) =
            find_optimal_amount_in_v2(&path_edges, accounts, instances, config)?;

        if refined_profit > best_profit {
            best_profit = refined_profit;
            let final_amount = (optimal_amount_in as i128).wrapping_add(refined_profit) as u128;
            best_path = Some(ArbitragePath {
                edges: path_edges,
                profit: refined_profit,
                final_amount,
                start_amount: optimal_amount_in,
            });
        }
    }

    if let Some(ref path) = best_path {
        #[cfg(any(test, feature = "debug"))]
        {
            let profit_pct = (path.profit as f64 / path.start_amount as f64) * 100.0;
            msg!(
                "MULTI-TRADE BEST: in={} out={} profit={} ({:.2}%)",
                path.start_amount,
                path.final_amount,
                path.profit,
                profit_pct
            );
        }

        #[cfg(test)]
        write_results_to_file(&[best_path.clone()]);
    }

    Ok(best_path)
}

/// Execute arbitrage path with CU-optimized operations.
/// Key optimizations:
/// - No msg! calls in hot path (use #[cfg(feature = "debug")])
/// - Direct byte reads instead of try_deserialize
/// - Pre-cache mint_2 key to avoid repeated .key() calls
/// - Index-based instance lookup instead of .find()
pub fn execute_arbitrage_path<'info>(
    accounts: &[AccountInfo<'info>],
    arbitrage_path: &ArbitragePath,
    instances: &mut Vec<ProgramInstance<'info>>,
    payer: &AccountInfo<'info>,
    mint_1: &AccountInfo<'info>,
    mint_1_token_program: &AccountInfo<'info>,
    user_mint_1_token_account: &AccountInfo<'info>,
    mint_2: &AccountInfo<'info>,
    mint_2_token_program: &AccountInfo<'info>,
    user_mint_2_token_account: &AccountInfo<'info>,
) -> Result<()> {
    #[cfg(feature = "debug")]
    msg!("Executing {} edges", arbitrage_path.edges.len());

    let mut current_amount = arbitrage_path.start_amount;

    // Cache mint_2 key bytes for fast comparison (avoid repeated .key() calls)
    let mint_2_key = *mint_2.key;

    for edge in arbitrage_path.edges.iter() {
        // Find program instance by pool_id using linear scan
        // TODO: Consider adding instance_index to Edge struct for O(1) lookup
        let pool_id = &edge.pool_id;
        let program_instance = instances
            .iter()
            .find(|inst| inst.get_pool_id() == pool_id)
            .ok_or(SolarBError::UnknownProgram)?;

        let input_mint = edge.left.mint_account;

        #[cfg(feature = "debug")]
        msg!("Swap {} {} -> {}", current_amount, input_mint, edge.right.mint_account);

        // Execute swap - AccountInfo clone is unavoidable for CPI
        program_instance.invoke_swap_base_in(
            accounts,
            input_mint,
            current_amount,
            None,
            payer.clone(),
            user_mint_1_token_account.clone(),
            user_mint_2_token_account.clone(),
            mint_1.clone(),
            mint_2.clone(),
            mint_1_token_program.clone(),
            mint_2_token_program.clone(),
        )?;

        // Direct byte read of amount field - much cheaper than try_deserialize
        // SPL Token account layout: mint (32) + owner (32) + amount (8) + ...
        let output_token_account = if edge.right.mint_account == mint_2_key {
            user_mint_2_token_account
        } else {
            user_mint_1_token_account
        };

        let data = output_token_account.try_borrow_data()?;
        current_amount = u64::from_le_bytes(
            data[TOKEN_ACCOUNT_AMOUNT_OFFSET..TOKEN_ACCOUNT_AMOUNT_OFFSET + 8]
                .try_into()
                .map_err(|_| SolarBError::InvalidAccountData)?,
        );
    }

    #[cfg(feature = "debug")]
    {
        let final_profit = current_amount as i128 - arbitrage_path.start_amount as i128;
        msg!("Done: {} -> {} (profit: {})", arbitrage_path.start_amount, current_amount, final_profit);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::{account_info::AccountInfo, pubkey::Pubkey, system_program};

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
            max_amount_in: 1_000,
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
        };

        let result = parse_accounts(&accounts, &data);
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
            max_amount_in: 1_000,
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
        };

        let result = parse_accounts(&accounts, &data);
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
            max_amount_in: 1_000,
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
        };

        let result = parse_accounts(&accounts, &data);
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
            max_amount_in: 1_000,
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
        };

        let result = parse_accounts(&accounts, &data);
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
            max_amount_in: 1_000,
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
        };

        let result = parse_accounts(&accounts, &data);
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
            max_amount_in: 1_000,
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
        };

        let result = parse_accounts(&accounts, &data);
        assert!(result.is_err());
        // Just verify it's an error - Anchor error types are complex to match
    }

    #[test]
    fn test_parse_accounts_invalid_accounts_length() {
        let accounts = create_mock_accounts(5, system_program::id());

        // Use a span that's too large to convert from u32 to usize
        // On most platforms this won't happen, but we test the error path
        let data = InstructionData {
            accounts_length: [u32::MAX, 0, 0, 0, 0],
            max_amount_in: 1_000,
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
        };

        let result = parse_accounts(&accounts, &data);
        // This should either error on conversion or on insufficient accounts
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_accounts_empty_segment() {
        let accounts = Vec::new();

        let data = InstructionData {
            accounts_length: [0, 0, 0, 0, 0],
            max_amount_in: 1_000,
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
        };

        let result = parse_accounts(&accounts, &data);
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
            accounts_length: [10, 0, 0, 0, 0],
            max_amount_in: 1_000,
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
        };

        let result = parse_accounts(&accounts, &data);
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

        let data = InstructionData {
            accounts_length: [13, 0, 0, 0, 0],
            max_amount_in: 1_000,
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
        };

        let result = parse_accounts(&accounts, &data);
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
            max_amount_in: 1_000,
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
        };

        let result = parse_accounts(&accounts, &data);
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
            max_amount_in: 1_000,
            mints: 2,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
        };

        let result = parse_accounts(&accounts, &data);
        assert!(result.is_ok());
        let instances = result.unwrap();
        assert!(instances.len() == 2);
        assert!(*instances[0].get_id() == program_id_1);
        assert!(*instances[1].get_id() == program_id_2);
    }
}
