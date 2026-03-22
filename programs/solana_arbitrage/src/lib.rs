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
pub mod pool_vec;
pub mod programs;
pub mod utils;

// Tests are now inline below - external test file moved to integration tests
// #[cfg(test)]
// #[path = "tests/lib_test.rs"]
// mod lib_test;

#[cfg(test)]
#[path = "tests/pubkey_test.rs"]
mod pubkey_test;

use anchor_spl::token::spl_token::native_mint::ID as WSOL;
use arbitrage::algo_2::ArbitragePath;
#[cfg(any(test, feature = "debug"))]
use arbitrage::algo_2::{
    optimal_amount_in_v2::find_optimal_amount_in_v2,
    find_cross_arbitrage_optimized,
    find_triangular_arbitrage_iterative, get_edges, EdgeArray,
};
use arbitrage::analytical_algo::{run_analytical_2hop, run_analytical_multihop};
use programs::{
    MeteoraDammV1, MeteoraDammV2, MeteoraDlmm, OrcaWhirlpool, ProgramInstance, ProgramMeta, PumpAmm,
    RaydiumAmm, RaydiumCLMM, RaydiumCPMM, SolarBError,
};
use pool_vec::PoolVec;
use utils::bot_config::BotConfig;
use utils::token::MintFee;
#[cfg(test)]
use utils::token::get_transfer_fees;

#[cfg(test)]
use crate::utils::test_utils::write_results_to_file;

/// DEX type IDs — must match client-side DEX_TYPE_ID mapping
pub mod dex_type {
    pub const METEORA_DAMM_V1: u8 = 1;
    pub const METEORA_DAMM_V2: u8 = 2;
    pub const METEORA_DLMM: u8 = 3;
    pub const WHIRLPOOL: u8 = 4;
    pub const RAYDIUM_AMM: u8 = 5;
    pub const RAYDIUM_CLMM: u8 = 6;
    pub const RAYDIUM_CPMM: u8 = 7;
    pub const METEORA_DBC: u8 = 8;
    pub const PUMP_AMM: u8 = 9;

    /// Number of fee slots consumed from pool_fees Vec per pool type.
    #[inline(always)]
    pub const fn fee_slot_count(pool_type: u8) -> usize {
        match pool_type {
            PUMP_AMM => 1,
            _ => 0,
        }
    }

    /// Fixed number of dynamic accounts per pool type.
    #[inline(always)]
    pub fn dynamic_account_count(pool_type: u8) -> usize {
        match pool_type {
            PUMP_AMM => crate::programs::pump_amm::DYNAMIC_ACCOUNTS,
            METEORA_DAMM_V1 | METEORA_DBC => crate::programs::meteora_damm_v1::MeteoraDammV1::DYNAMIC_ACCOUNTS,
            METEORA_DAMM_V2 => crate::programs::meteora_damm_v2::DYNAMIC_ACCOUNTS,
            METEORA_DLMM => crate::programs::meteora_dlmm::DYNAMIC_ACCOUNTS,
            WHIRLPOOL => crate::programs::orca::DYNAMIC_ACCOUNTS,
            RAYDIUM_AMM => crate::programs::raydium_amm::DYNAMIC_ACCOUNTS,
            RAYDIUM_CLMM => crate::programs::raydium_clmm::DYNAMIC_ACCOUNTS,
            RAYDIUM_CPMM => crate::programs::raydium_cpmm::DYNAMIC_ACCOUNTS,
            _ => 0,
        }
    }
}

// SPL Token account amount offset (after mint pubkey + owner pubkey)
const TOKEN_ACCOUNT_AMOUNT_OFFSET: usize = 64;

const MAX_POOLS: usize = 8;


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
    pub mints: u8,
    /// Total number of shared static accounts (all types combined)
    pub shared_statics_len: u8,
    /// DEX type ID per pool (see dex_type module)
    pub pool_types: [u8; 8],
    /// Offset into shared statics block where each pool's type statics begin
    pub type_static_offsets: [u8; 8],
    /// Arbitrage mode: 0=single pair multi-market, 1=multi-hop chain, 2=multiple trades
    pub mode: u8,
    /// Test mode: if true, skip profit checks and execute with tiny amount (100 lamports)
    pub test: bool,
    /// Number of pools per mint group (for MULTIPLE_TRADES lazy evaluation).
    /// e.g. [3, 2, 0, 0] = group 0 has 3 pools (pool_types[0..3]), group 1 has 2 pools (pool_types[3..5]).
    /// All zeros = fall back to parsing all pools at once.
    pub group_sizes: [u8; 4],
    /// Static pool fees per pool in millionths (denominator = 1_000_000).
    /// Variable-length: each pool consumes fee_slot_count(pool_type) slots in order.
    /// 0 = use on-chain fee.
    pub pool_fees: Vec<u32>,
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
    let shared_statics_start = 4 + num_mints * 2;
    let pool_start = shared_statics_start + data.shared_statics_len as usize;

    let user_token_account = &accounts[5];
    let max_amount_in = u64::from_le_bytes(
        user_token_account.try_borrow_data()?[64..72]
            .try_into()
            .unwrap(),
    );
    if max_amount_in == 0 {
        return Err(error!(SolarBError::InsufficientFunds));
    }

    #[cfg(test)]
    let max_amount_in = 3_000_000_000;
    debug_eprintln!("max_amount_in: {:?}", max_amount_in);

    let result = match data.mode {
        arb_mode::MULTI_HOP_CHAIN => {
            start_bot_multihop(accounts, &data, clock, payer, shared_statics_start, pool_start, max_amount_in)?
        }
        _ => {
            // SINGLE_PAIR (1 group) and MULTIPLE_TRADES (N groups) share the same loop
            start_bot_grouped(accounts, &data, clock, payer, shared_statics_start, pool_start, max_amount_in)?
        }
    };

    if result.is_none() && !data.test {
        return Err(error!(SolarBError::NoProfitFound));
    }

    Ok(result)
}

/// Compare the analytical optimal against a high-precision golden section search
/// that uses real `swap_base_in` calls — the ground truth.
#[cfg(test)]
fn compare_golden_vs_analytical<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance],
    config: &BotConfig,
    mint_fees: &[(Pubkey, MintFee)],
    actual_path: &Option<ArbitragePath>,
) {

    debug_eprintln!("==============================================");
    use arbitrage::golden_search::golden_search_2hop;
    
    let golden_result = golden_search_2hop(accounts, instances, config, mint_fees);

    let (golden_amount, golden_profit) = match &golden_result {
        Some(golden) => (golden.optimal_amount, golden.profit),
        None => (0, 0),
    };

    let (actual_amount, actual_profit) = match actual_path {
        Some(actual) => (actual.start_amount, actual.profit),
        None => (0, 0),
    };

    let profit_gap_pct = if golden_profit > 0 {
        ((golden_profit - actual_profit) as f64 / golden_profit as f64).abs() * 100.0
    } else { 0.0 };

    let amount_diff_pct = if golden_amount > 0 {
        ((actual_amount as f64 - golden_amount as f64) / golden_amount as f64).abs() * 100.0
    } else { 0.0 };

    eprintln!("\n╔══════════════════════════════════════════════════════════════════╗");
    eprintln!("║  GOLDEN (swap_base_in) vs ACTUAL (run_analytical_2hop)          ║");
    eprintln!("╠══════════════════════════════════════════════════════════════════╣");
    eprintln!("║  Golden:      amount={:>15}     profit={:>12.6} SOL{}",
        golden_amount, golden_profit as f64 / 1e9,
        if golden_result.is_none() { "  (no path)" } else { "" });
    eprintln!("║  Actual(2hop): amount={:>15}     profit={:>12.6} SOL{}",
        actual_amount, actual_profit as f64 / 1e9,
        if actual_path.is_none() { "  (no path)" } else { "" });
    eprintln!("╠══════════════════════════════════════════════════════════════════╣");
    eprintln!("║  Amount diff:  {:.4}%", amount_diff_pct);
    eprintln!("║  Profit gap:   {:.4}%", profit_gap_pct);
    eprintln!("║  Same path:    {}", match (&golden_result, actual_path) {
        (Some(g), Some(a)) if a.edges.len() >= 2 =>
            if *instances[g.buy_idx].get_pool_id() == a.edges[0].pool_id
                && *instances[g.sell_idx].get_pool_id() == a.edges[1].pool_id { "YES" } else { "NO" },
        _ => "N/A",
    });
    eprintln!("║  Result:       {}", if profit_gap_pct < 5.0 { "PASS" } else { "CHECK" });
    eprintln!("╠══════════════════════════════════════════════════════════════════╣");
    eprintln!("║  GOLDEN PATH:");
    if let Some(g) = &golden_result {
        let buy = &instances[g.buy_idx];
        let sell = &instances[g.sell_idx];
        let start_token = config.start_token.unwrap_or(WSOL);
        let (base_mint, quote_mint) = buy.get_mints();
        let middle_mint = if *base_mint == start_token { *quote_mint } else { *base_mint };
        eprintln!("║    buy:  [{}] {} -> {} (pool {})",
            buy.name(), start_token, middle_mint, buy.get_pool_id());
        eprintln!("║    sell: [{}] {} -> {} (pool {})",
            sell.name(), middle_mint, start_token, sell.get_pool_id());
    } else {
        eprintln!("║    (none)");
    }
    eprintln!("║  ACTUAL PATH:");
    if let Some(actual) = actual_path {
        for (i, edge) in actual.edges.iter().enumerate() {
            let label = if i == 0 { "buy" } else { "sell" };
            eprintln!("║    {}:  {} -> {} (pool {})",
                label, edge.left.mint_account, edge.right.mint_account, edge.pool_id);
        }
    } else {
        eprintln!("║    (none)");
    }
    eprintln!("╚══════════════════════════════════════════════════════════════════╝\n");
}

/// Grouped flow for SINGLE_PAIR and MULTIPLE_TRADES modes.
/// Iterates mint groups lazily: parse + evaluate group 0 first, only parse group 1 if no profit.
/// SINGLE_PAIR is just 1 iteration (all pools in one group).
#[inline(never)]
fn start_bot_grouped<'info>(
    accounts: &[AccountInfo<'info>],
    data: &InstructionData,
    clock: Clock,
    payer: &AccountInfo<'info>,
    shared_statics_start: usize,
    pool_start: usize,
    max_amount_in: u64,
) -> Result<Option<ArbitragePath>> {
    let test_mode = data.test;

    // Determine groups: for MULTIPLE_TRADES use group_sizes, for SINGLE_PAIR = 1 group with all pools
    let effective_groups: [u8; 4] = if data.mode == arb_mode::MULTIPLE_TRADES && data.group_sizes[0] > 0 {
        data.group_sizes
    } else {
        let total_pools = data.pool_types.iter().filter(|&&t| dex_type::dynamic_account_count(t) > 0).count() as u8;
        [total_pools, 0, 0, 0]
    };

    #[cfg(test)]
    let mint_fees: Vec<(Pubkey, MintFee)> = vec![];

    let mut pool_idx_offset = 0usize;
    for g in 0..4usize {
        let group_size = effective_groups[g] as usize;
        if group_size == 0 {
            break;
        }

        let pool_idx_start = pool_idx_offset;
        let pool_idx_end = pool_idx_offset + group_size;
        pool_idx_offset = pool_idx_end;

        let mut instances = parse_accounts(
            accounts, shared_statics_start, pool_start, data, &clock,
            pool_idx_start, pool_idx_end,
        )?;

        let mut group_config = BotConfig::new(
            Some(WSOL),
            max_amount_in,
            5_500,
            2,
            arb_mode::SINGLE_PAIR_MULTI_MARKET,
            clock.clone(),
            test_mode,
        );

        #[cfg(test)]
        let mut comparison_instances = instances.clone();
        #[cfg(test)]
        let mut simulation_instances = instances.clone();

        let arbitrage_path = run_analytical_2hop(
            accounts, &mut instances, &mut group_config,
        )?;

        #[cfg(test)]
        compare_golden_vs_analytical(accounts, &mut comparison_instances, &group_config, &mint_fees, &arbitrage_path);

        let Some(mut arb_path) = arbitrage_path else {
            continue;
        };

        #[cfg(test)]
        {
            let sim_profit = run_simulation(
                accounts, &arb_path, &mut simulation_instances, &mut group_config, &mint_fees,
            )?;

            if sim_profit <= group_config.min_profit
                || (!test_mode && arb_path.profit <= group_config.min_profit)
            {
                debug_eprintln!("No Profit");
                continue;
            }
        }

        if test_mode {
            arb_path.start_amount = 1_000_000;
        }

        #[cfg(test)]
        continue;

        #[cfg(not(test))]
        {
            execute_arbitrage_path(accounts, &arb_path, &mut instances, payer, data.mints)?;

            // Re-read start token balance after swaps and abort if not profitable
            let balance_after = u64::from_le_bytes(
                accounts[5].try_borrow_data()?[TOKEN_ACCOUNT_AMOUNT_OFFSET..TOKEN_ACCOUNT_AMOUNT_OFFSET + 8]
                    .try_into()
                    .map_err(|_| SolarBError::InvalidAccountData)?,
            );
            if balance_after < max_amount_in {
                return Err(error!(SolarBError::NoProfitFound));
            }

            return Ok(Some(arb_path));
        }

    }
    Ok(None)
}

/// Multi-hop flow: all pools parsed at once, needs all mints for the chain.
#[inline(never)]
fn start_bot_multihop<'info>(
    accounts: &[AccountInfo<'info>],
    data: &InstructionData,
    clock: Clock,
    payer: &AccountInfo<'info>,
    shared_statics_start: usize,
    pool_start: usize,
    max_amount_in: u64,
) -> Result<Option<ArbitragePath>> {
    let test_mode = data.test;

    let mint_fees: Vec<(Pubkey, MintFee)> = vec![];

    let mut bot_config = BotConfig::new(
        Some(WSOL), max_amount_in, 5_500, data.mints, data.mode, clock, test_mode,
    );

    let mut instances = parse_accounts(
        accounts, shared_statics_start, pool_start, data, &bot_config.clock, 0, 8,
    )?;

    let arbitrage_path = run_analytical_multihop(accounts, &mut instances, &mut bot_config, &mint_fees)?;

    // Log comparison
    #[cfg(any(test, feature = "debug"))]
    {
        let helper_result = run_arbitrage(accounts, &mut instances, &mut bot_config)?;
        let grid_profit = helper_result.as_ref().map_or(0, |r| r.profit);
        let ana_profit = arbitrage_path.as_ref().map_or(0, |r| r.profit);
        let grid_amount_in = helper_result.as_ref().map_or(0, |r| r.start_amount);
        let ana_amount_in = arbitrage_path.as_ref().map_or(0, |r| r.start_amount);

        eprintln!("");
        eprintln!(
            "RESULTS _____________________ : profit={} in={} | Ana: profit={} in={} | winner={}",
            grid_profit as f64 / 1_000_000_000.0,
            grid_amount_in as f64 / 1_000_000_000.0,
            ana_profit as f64 / 1_000_000_000.0,
            ana_amount_in as f64 / 1_000_000_000.0,
            if ana_profit > grid_profit { "Ana" } else { "Grid" }
        );
        eprintln!("");
    }

    let Some(mut arbitrage_path) = arbitrage_path else {
        return Ok(None);
    };

    for edge in arbitrage_path.edges.iter() {
        if let Some(inst) = instances.iter_mut().find(|i| i.get_pool_id() == &edge.pool_id) {
            if !inst.prepare_for_execution(accounts, &bot_config.clock) {
                debug_eprintln!("Skipping arb path: pool preparation failed");
                return Ok(None);
            }
        }
    }

    #[cfg(test)]
    {
        let sim_profit = run_simulation(accounts, &arbitrage_path, &mut instances, &mut bot_config, &mint_fees)?;

        if sim_profit <= bot_config.min_profit
            || (!test_mode && arbitrage_path.profit <= bot_config.min_profit)
        {
            debug_eprintln!(
                "Not found: sp={} ap={} mp={}",
                sim_profit, arbitrage_path.profit, bot_config.min_profit
            );
            return Ok(None);
        }
    }

    #[cfg(not(test))]
    {
        if arbitrage_path.profit <= bot_config.min_profit {
            debug_eprintln!(
                "Not found: ap={} mp={}",
                arbitrage_path.profit, bot_config.min_profit
            );
            return Ok(None);
        }
    }

    if test_mode {
        arbitrage_path.start_amount = 1_000_000;
    }

    #[cfg(test)]
    {
        if arbitrage_path.profit > 0 {
            write_results_to_file(&[Some(arbitrage_path.clone())]);
        }
    }

    execute_arbitrage_path(accounts, &arbitrage_path, &mut instances, payer, data.mints)?;

    // Re-read start token balance after swaps and abort if not profitable
    let balance_after = u64::from_le_bytes(
        accounts[5].try_borrow_data()?[TOKEN_ACCOUNT_AMOUNT_OFFSET..TOKEN_ACCOUNT_AMOUNT_OFFSET + 8]
            .try_into()
            .map_err(|_| SolarBError::InvalidAccountData)?,
    );
    if balance_after < max_amount_in {
        return Err(error!(SolarBError::NoProfitFound));
    }

    Ok(Some(arbitrage_path))
}

#[inline(never)]
fn parse_accounts<'info>(
    accounts: &[AccountInfo<'info>],
    shared_statics_start: usize,
    pool_start: usize,
    data: &InstructionData,
    clock: &Clock,
    pool_idx_start: usize,
    pool_idx_end: usize,
) -> Result<PoolVec> {
    // Compute dynamic account offset by skipping pools before pool_idx_start
    let mut index: usize = pool_start;
    for i in 0..pool_idx_start {
        index += dex_type::dynamic_account_count(data.pool_types[i]);
    }
    let accounts_len = accounts.len();

    let mut instances = PoolVec::new();

    // Compute fee offset by skipping fee slots for pools before pool_idx_start
    let mut fee_offset: usize = 0;
    for i in 0..pool_idx_start {
        fee_offset += dex_type::fee_slot_count(data.pool_types[i]);
    }

    for i in pool_idx_start..pool_idx_end {
        let dex = data.pool_types[i];
        let span = dex_type::dynamic_account_count(dex);

        if span == 0 {
            continue;
        }

        let n_fees = dex_type::fee_slot_count(dex);
        let pool_fee = if n_fees > 0 { data.pool_fees[fee_offset] } else { 0 };
        fee_offset += n_fees;

        let end_index = index + span;
        if accounts_len < end_index {
            return Err(error!(SolarBError::InsufficientAccounts));
        }

        let static_base = shared_statics_start + data.type_static_offsets[i] as usize;

        let instance = match dex {
            dex_type::PUMP_AMM => {
                debug_eprintln!("PumpAmm");
                create_pump_amm(accounts, static_base, index, end_index, pool_fee)?
            }
            dex_type::METEORA_DAMM_V1 | dex_type::METEORA_DBC => {
                debug_eprintln!("MeteoraDammV1");
                create_meteora_damm_v1(accounts, static_base, index, end_index, clock)?
            }
            dex_type::METEORA_DAMM_V2 => {
                debug_eprintln!("MeteoraDammV2");
                create_meteora_damm_v2(accounts, static_base, index, end_index, clock)?
            }
            dex_type::METEORA_DLMM => {
                debug_eprintln!("MeteoraDlmm");
                create_meteora_dlmm(accounts, static_base, index, end_index)?
            }
            dex_type::WHIRLPOOL => {
                debug_eprintln!("OrcaWhirlpool");
                create_orca_whirlpool(accounts, static_base, index, end_index)?
            }
            dex_type::RAYDIUM_AMM => {
                debug_eprintln!("RaydiumAmm");
                create_raydium_amm(accounts, static_base, index, end_index)?
            }
            dex_type::RAYDIUM_CLMM => {
                debug_eprintln!("RaydiumCLMM");
                create_raydium_clmm(accounts, static_base, index, end_index)?
            }
            dex_type::RAYDIUM_CPMM => {
                debug_eprintln!("RaydiumCPMM");
                create_raydium_cpmm(accounts, static_base, index, end_index)?
            }
            _ => return Err(error!(SolarBError::UnknownProgram)),
        };

        #[cfg(any(test, feature = "debug"))]
        {
            instance.log_accounts(accounts)?;
        }

        instances.push(instance);
        index = end_index;
    }

    Ok(instances)
}
/// Each constructor is in its own #[inline(never)] fn to get a separate stack frame,
/// avoiding the compiler reserving stack for all program structs in one frame.


#[inline(never)]
fn create_pump_amm<'info>(
    accounts: &[AccountInfo<'info>],
    static_base: usize,
    dyn_start: usize,
    dyn_end: usize,
    pool_fee: u32,
) -> Result<ProgramInstance> {
    Ok(ProgramInstance::PumpAmm(PumpAmm::new(accounts, static_base, dyn_start, dyn_end, pool_fee)?))
}

#[inline(never)]
fn create_meteora_damm_v1<'info>(
    accounts: &[AccountInfo<'info>],
    static_base: usize,
    dyn_start: usize,
    dyn_end: usize,
    clock: &Clock,
) -> Result<ProgramInstance> {
    Ok(ProgramInstance::MeteoraDammV1(MeteoraDammV1::new(accounts, static_base, dyn_start, dyn_end, clock)?))
}

#[inline(never)]
fn create_meteora_damm_v2<'info>(
    accounts: &[AccountInfo<'info>],
    static_base: usize,
    dyn_start: usize,
    dyn_end: usize,
    clock: &Clock,
) -> Result<ProgramInstance> {
    Ok(ProgramInstance::MeteoraDammV2(MeteoraDammV2::new(accounts, static_base, dyn_start, dyn_end, clock)?))
}

#[inline(never)]
fn create_meteora_dlmm<'info>(
    accounts: &[AccountInfo<'info>],
    static_base: usize,
    dyn_start: usize,
    dyn_end: usize,
) -> Result<ProgramInstance> {
    Ok(ProgramInstance::MeteoraDlmm(MeteoraDlmm::new(accounts, static_base, dyn_start, dyn_end)?))
}

#[inline(never)]
fn create_orca_whirlpool<'info>(
    accounts: &[AccountInfo<'info>],
    static_base: usize,
    dyn_start: usize,
    dyn_end: usize,
) -> Result<ProgramInstance> {
    Ok(ProgramInstance::OrcaWhirlpool(OrcaWhirlpool::new(accounts, static_base, dyn_start, dyn_end)?))
}

#[inline(never)]
fn create_raydium_amm<'info>(
    accounts: &[AccountInfo<'info>],
    static_base: usize,
    dyn_start: usize,
    dyn_end: usize,
) -> Result<ProgramInstance> {
    Ok(ProgramInstance::RaydiumAmm(RaydiumAmm::new(accounts, static_base, dyn_start, dyn_end)?))
}

#[inline(never)]
fn create_raydium_clmm<'info>(
    accounts: &[AccountInfo<'info>],
    static_base: usize,
    dyn_start: usize,
    dyn_end: usize,
) -> Result<ProgramInstance> {
    Ok(ProgramInstance::RaydiumCLMM(RaydiumCLMM::new(accounts, static_base, dyn_start, dyn_end)?))
}

#[inline(never)]
fn create_raydium_cpmm<'info>(
    accounts: &[AccountInfo<'info>],
    static_base: usize,
    dyn_start: usize,
    dyn_end: usize,
) -> Result<ProgramInstance> {
    Ok(ProgramInstance::RaydiumCPMM(RaydiumCPMM::new(accounts, static_base, dyn_start, dyn_end)?))
}

#[cfg(any(test, feature = "debug"))]
#[inline(never)]
pub fn run_arbitrage<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance],
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
#[cfg(any(test, feature = "debug"))]
#[inline(never)]
fn run_single_pair_arbitrage<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance],
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

    let candidate = find_cross_arbitrage_optimized(accounts, &edge_refs, instances, config)?;

    // msg!("candidate={}", candidate.is_some());

    // In test mode, if no candidate found, build a fallback path
    if config.test && candidate.is_none() {
        let all_edges = get_edges(instances)?;
        if all_edges.len() >= 2 {
            let start_token = config.start_token.unwrap_or(WSOL);
            let buy_edge = all_edges
                .iter()
                .find(|e| e.left.mint_account == start_token);
            if let Some(buy) = buy_edge {
                let sell_edge = all_edges
                    .iter()
                    .find(|e| e.right.mint_account == start_token && e.pool_id != buy.pool_id);
                if let Some(sell) = sell_edge {
                    let edges = EdgeArray::from_2(buy.clone(), sell.clone());
                    let (optimal_amount_in, profit) =
                        find_optimal_amount_in_v2(&edges, accounts, instances, config)?;
                    let final_amount =
                        (optimal_amount_in as i128).checked_add(profit).unwrap_or(0) as u128;
                    return Ok(Some(ArbitragePath {
                        edges,
                        profit,
                        final_amount,
                        start_amount: optimal_amount_in,
                    }));
                }
            }
        }
        return Ok(None);
    }

    let Some((edge_vec, est_profit, _)) = candidate else {
        if !config.test {
            // msg!("Single: Rejected, no profitable candidates");
        }
        return Ok(None);
    };

    // msg!("Evaluating candidate: est_profit={}", est_profit);
    let (optimal_amount_in, profit) =
        find_optimal_amount_in_v2(&edge_vec, accounts, instances, config)?;

    // msg!("  opt: in={}, p={}", optimal_amount_in, profit);

    if !config.test && (profit <= 0 || optimal_amount_in == 0) {
        // msg!("Single: rejected after opt, no profitable candidate");
        return Ok(None);
    }

    let final_amount = (optimal_amount_in as i128).checked_add(profit).unwrap_or(0) as u128;
    let best_result = ArbitragePath {
        edges: EdgeArray::from(edge_vec),
        profit,
        final_amount,
        start_amount: optimal_amount_in,
    };

    #[cfg(any(test, feature = "debug"))]
    {
        if best_result.start_amount > 0 {
            let profit_pct = (best_result.profit as f64 / best_result.start_amount as f64) * 100.0;
            debug_eprintln!(
                "PROFIT: in={} out={} profit={} ({:.2}%)",
                best_result.start_amount,
                best_result.final_amount,
                best_result.profit,
                profit_pct
            );
        }
    }

    Ok(Some(best_result))
}

/// CASE 2: Multi-hop chain arbitrage
/// Edges form a sequential chain through different mints
/// Example: SOL -> TOKEN1 -> USDC -> SOL (3-hop)
#[cfg(any(test, feature = "debug"))]
#[inline(never)]
fn run_multi_hop_arbitrage<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance],
    config: &mut BotConfig,
) -> Result<Option<ArbitragePath>> {
    debug_eprintln!("Multi-hop chain: {} mints", config.mints);
    // Generate edges from all instances
    let edges = get_edges(instances)?;
    let edge_refs: Vec<&_> = edges.iter().collect();

    // Use triangular arbitrage finder for 3+ hop chains
    let (path_edges, profit, _) =
        find_triangular_arbitrage_iterative(accounts, &edge_refs, instances, config)?;

    debug_eprintln!("Hop: edges={}, est_profit={}", path_edges.len(), profit);

    if !config.test && (profit <= 0 || path_edges.is_empty()) {
        debug_eprintln!("Rejected, profit={} edges={}", profit, path_edges.len());
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

    // msg!("Hop opt: in={}, profit={}", optimal_amount_in, profit);

    if !config.test && profit <= 0 {
        // msg!("Hop: rejected after opt, profit={}", profit);
        return Ok(None);
    }

    let final_amount = (optimal_amount_in as i128).checked_add(profit).unwrap_or(0) as u128;

    let arbitrage_path = ArbitragePath {
        edges: EdgeArray::from(path_edges),
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
                optimal_amount_in,
                final_amount,
                profit,
                profit_pct
            );
        }
    }

    Ok(Some(arbitrage_path))
}

/// CASE 3: Multiple independent trades
/// Disconnected subgraphs, each a separate arbitrage opportunity
/// Example: (SOL -> TOKEN1 -> SOL) vs (SOL -> TOKEN2 -> SOL)
/// Groups edges by their non-start mint and evaluates each group
#[cfg(any(test, feature = "debug"))]
#[inline(never)]
fn run_multiple_trades_arbitrage<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance],
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

        let candidate = find_cross_arbitrage_optimized(accounts, &group_edge_refs, instances, config)?;

        // msg!("Multi grp: candidate={}", candidate.is_some());

        let Some((path_edges, est_profit, _)) = candidate else {
            if config.test {
                // In test mode, if no candidate found, use first two edges from group
                let e0 = &all_edges[edge_indices[0]];
                let e1 = &all_edges[edge_indices[1]];
                let fallback_edges = EdgeArray::from_2(e0.clone(), e1.clone());
                let (optimal_amount_in, refined_profit) =
                    find_optimal_amount_in_v2(&fallback_edges, accounts, instances, config)?;
                if best_path.is_none() {
                    let final_amount = (optimal_amount_in as i128)
                        .checked_add(refined_profit)
                        .unwrap_or(0) as u128;
                    best_path = Some(ArbitragePath {
                        edges: fallback_edges,
                        profit: refined_profit,
                        final_amount,
                        start_amount: optimal_amount_in,
                    });
                }
            } else {
                // msg!("Multi grp: skip, no candidate");
            }
            continue;
        };

        // msg!("  Evaluating candidate: est_profit={}", est_profit);
        let (optimal_amount_in, refined_profit) =
            find_optimal_amount_in_v2(&path_edges, accounts, instances, config)?;

        // msg!("  Multi opt: in={}, profit={}", optimal_amount_in, refined_profit);

        if !config.test && (refined_profit <= 0 || optimal_amount_in == 0) {
            continue;
        }

        if refined_profit > best_profit || (config.test && best_path.is_none()) {
            best_profit = refined_profit;
            let final_amount = (optimal_amount_in as i128)
                .checked_add(refined_profit)
                .unwrap_or(0) as u128;
            best_path = Some(ArbitragePath {
                edges: EdgeArray::from(path_edges),
                profit: refined_profit,
                final_amount,
                start_amount: optimal_amount_in,
            });
        }
    }

    if let Some(ref path) = best_path {
        // msg!("Best: in={}, profit={}", path.start_amount, path.profit);
        #[cfg(any(test, feature = "debug"))]
        {
            let profit_pct = (path.profit as f64 / path.start_amount as f64) * 100.0;
            debug_eprintln!(
                "MULTI-TRADE BEST: in={} out={} profit={} ({:.2}%)",
                path.start_amount,
                path.final_amount,
                path.profit,
                profit_pct
            );
        }
    } else {
        // msg!("Multi: no profitable group found");
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
    instances: &mut [ProgramInstance],
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
            .iter_mut()
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
            current_amount,
            input_mint_key,
            output_mint_key
        );

        // Derive token program from mint owner
        let spl_token_program = &accounts[1];
        let token_2022_program = &accounts[2];
        let input_token_program = if accounts[input_base].owner == spl_token_program.key {
            spl_token_program
        } else {
            token_2022_program
        };
        let output_token_program = if accounts[output_base].owner == spl_token_program.key {
            spl_token_program
        } else {
            token_2022_program
        };

        // Execute swap — no AccountInfo clones needed, passing references
        program_instance.invoke_swap_base_in(
            accounts,
            *input_mint_key,
            current_amount,
            None,
            payer,
            &accounts[input_base + 1],  // user source token account
            &accounts[output_base + 1], // user destination token account
            &accounts[input_base],      // input mint
            &accounts[output_base],     // output mint
            input_token_program,        // input token program (derived from mint owner)
            output_token_program,       // output token program (derived from mint owner)
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
            arbitrage_path.start_amount,
            current_amount,
            final_profit
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

#[cfg(test)]
fn run_simulation<'info>(
    accounts: &[AccountInfo<'info>],
    arbitrage_path: &ArbitragePath,
    instances: &mut [ProgramInstance],
    bot_config: &mut BotConfig,
    mint_fees: &[(Pubkey, MintFee)],
) -> Result<i128> {
    let start_amount = arbitrage_path.start_amount;

    let mut current_amount = start_amount;

    for (i, edge) in arbitrage_path.edges.iter().enumerate() {
        let input_mint = edge.left.mint_account;

        let program_instance = instances
            .iter_mut()
            .find(|inst| inst.get_pool_id() == &edge.pool_id)
            .ok_or(SolarBError::UnknownProgram)?;

        let (base_pk, quote_pk) = program_instance.get_mints();
        let (in_fee, out_fee) = get_transfer_fees(input_mint, base_pk, quote_pk, mint_fees);

        let amount_out = program_instance.swap_base_in(
            accounts,
            input_mint,
            current_amount,
            in_fee,
            out_fee,
            &bot_config.clock,
        )?;

        #[cfg(any(test, feature = "debug"))]
        {
            let output_mint = edge.right.mint_account;
            // swap_base_out only needed for debug logging — skip in production to save CU
            let required_in =
                program_instance.swap_base_out(accounts, output_mint, amount_out, in_fee, out_fee, &bot_config.clock)?;
            let input_short = &input_mint.to_string()[..8];
            let output_short = &output_mint.to_string()[..8];
            let in_decimals = get_mint_decimals(accounts, &input_mint);
            let out_decimals = get_mint_decimals(accounts, &output_mint);
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

    let profit = current_amount as i128 - start_amount as i128;

    #[cfg(any(test, feature = "debug"))]
    {
        debug_eprintln!("---");
        let end_decimals = get_mint_decimals(
            accounts,
            &arbitrage_path.edges.last().unwrap().right.mint_account,
        );
        if profit > 0 {
            let profit_pct = (profit as f64 / start_amount as f64) * 100.0;
            debug_eprintln!(
                "PROFIT: {} ({:.4}%)",
                format_amount_i128(profit, end_decimals),
                profit_pct,
            );
        } else {
            debug_eprintln!("NO PROFIT: {}", format_amount_i128(profit, end_decimals),);
        }
        let start_decimals =
            get_mint_decimals(accounts, &arbitrage_path.edges[0].left.mint_account);
        debug_eprintln!(
            "Final: in={} -> out={}",
            format_amount(start_amount, start_decimals),
            format_amount(current_amount, end_decimals)
        );
    }

    Ok(profit)
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
            false,
            false,
            lamports_static,
            data_vec,
            owner_static,
            false,
            0,
        )
    }

    fn create_mock_accounts(count: usize, owner: Pubkey) -> Vec<AccountInfo<'static>> {
        (0..count)
            .map(|_| {
                let key = Pubkey::new_unique();
                create_mock_account_info(key, owner, 1000, None)
            })
            .collect()
    }

    /// Helper: build InstructionData with the new fields.
    /// pool_types, type_static_offsets are padded to [_;8].
    fn make_instruction_data(
        pool_types: &[u8],
        type_static_offsets: &[u8],
        shared_statics_len: u8,
    ) -> InstructionData {
        let mut pt = [0u8; 8];
        let mut to = [0u8; 8];
        for (i, &v) in pool_types.iter().enumerate() { pt[i] = v; }
        for (i, &v) in type_static_offsets.iter().enumerate() { to[i] = v; }
        InstructionData {
            mints: 2,
            shared_statics_len,
            pool_types: pt,
            type_static_offsets: to,
            mode: arb_mode::SINGLE_PAIR_MULTI_MARKET,
            test: false,
            group_sizes: [0; 4],
            pool_fees: vec![],
        }
    }

    // New layout: [shared_statics...][dynamic_pool_1...][dynamic_pool_2...]
    // shared_statics_start = 0 in these unit tests (no payer/mint prefix)
    // pool_start = shared_statics_start + shared_statics_len

    #[test]
    fn test_parse_accounts_skips_zero_span() {
        // All pool_types are 0 → no pools parsed
        let accounts: Vec<AccountInfo<'static>> = Vec::new();
        let data = make_instruction_data(&[], &[], 0);
        let result = parse_accounts(&accounts, 0, 0, &data, &default_clock(), 0, 8);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_parse_accounts_empty_segment() {
        let accounts: Vec<AccountInfo<'static>> = Vec::new();
        let data = make_instruction_data(&[], &[], 0);
        let result = parse_accounts(&accounts, 0, 0, &data, &default_clock(), 0, 8);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_parse_accounts_insufficient_accounts() {
        // Request 3 dynamic accounts for METEORA_DAMM_V2 but provide only 1 dynamic + 4 static
        let owner = system_program::id();
        let accounts = create_mock_accounts(5, owner);
        // shared_statics_start=0, shared_statics_len=4 (DAMM_V2 has 4 statics), pool_start=4
        // DAMM_V2 needs 3 dynamic accounts but only 1 available after index 4
        let data = make_instruction_data(
            &[dex_type::METEORA_DAMM_V2],
            &[0],
            4,
        );
        let result = parse_accounts(&accounts, 0, 4, &data, &default_clock(), 0, 8);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_accounts_insufficient_dynamic_accounts() {
        let accounts = create_mock_accounts(5, system_program::id());
        let data = make_instruction_data(
            &[dex_type::RAYDIUM_CPMM],
            &[0],
            2,
        );
        let result = parse_accounts(&accounts, 0, 2, &data, &default_clock(), 0, 8);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_accounts_unknown_dex_type_skipped() {
        let owner = system_program::id();
        let accounts = create_mock_accounts(10, owner);
        // dex_type 99 is unknown → dynamic_account_count=0 → skipped
        let data = make_instruction_data(
            &[99],
            &[0],
            0,
        );
        let result = parse_accounts(&accounts, 0, 0, &data, &default_clock(), 0, 8);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
