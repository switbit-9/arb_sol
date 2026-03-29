use anchor_lang::prelude::*;

#[macro_use]
pub mod utils;

pub mod arbitrage;
pub mod arbitrage_checker;
pub mod pool_vec;
pub mod programs;

// Tests are now inline below - external test file moved to integration tests
// #[cfg(test)]
// #[path = "tests/lib_test.rs"]
// mod lib_test;

#[path = "tests/test_fixtures.rs"]
pub mod test_fixtures;

#[cfg(test)]
#[path = "tests/pubkey_test.rs"]
mod pubkey_test;

use anchor_spl::token::spl_token::native_mint::ID as WSOL;
use arbitrage_checker::ArbitrageResult;
use pool_vec::PoolVec;
use programs::{
    MeteoraDammV1, MeteoraDammV2, MeteoraDlmm, OrcaWhirlpool, ProgramInstance, ProgramMeta,
    PumpAmm, RaydiumAmm, RaydiumCLMM, RaydiumCPMM, SolarBError,
};
pub use utils::arb_mode;
use utils::bot_config::BotConfig;
pub use utils::dex_type;
use utils::token::get_transfer_fees;
use utils::token::MintFee;

#[cfg(test)]
use crate::utils::test_utils::write_results_to_file;

// SPL Token account amount offset (after mint pubkey + owner pubkey)
const TOKEN_ACCOUNT_AMOUNT_OFFSET: usize = 64;

#[cfg(all(feature = "benchmark", target_os = "solana"))]
pub fn sol_remaining_cu() -> u64 {
    extern "C" {
        fn sol_remaining_compute_units() -> u64;
    }
    unsafe { sol_remaining_compute_units() }
}
#[cfg(not(all(feature = "benchmark", target_os = "solana")))]
pub fn sol_remaining_cu() -> u64 {
    0
}

#[cfg(feature = "benchmark")]
fn sol_log_cu() {
    debug_msg!("CU remaining: {}", sol_remaining_cu());
}

pub const MAX_POOLS: usize = 8;

declare_id!("BJREZ2NxHAqSf4jeaogmdoyF2nhexVpeewokt5iqqCMt");

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
) -> Result<Option<ArbitrageResult>> {
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

    #[cfg(any(test, feature = "benchmark"))]
    let max_amount_in = 3_000_000_000;
    debug_eprintln!("max_amount_in: {:?}", max_amount_in);

    let result = match data.mode {
        arb_mode::MULTI_HOP_CHAIN => start_bot_multihop(
            accounts,
            &data,
            clock,
            payer,
            shared_statics_start,
            pool_start,
            max_amount_in,
        )?,
        _ => {
            // SINGLE_PAIR (1 group) and MULTIPLE_TRADES (N groups) share the same loop
            start_bot_grouped(
                accounts,
                &data,
                clock,
                payer,
                shared_statics_start,
                pool_start,
                max_amount_in,
            )?
        }
    };

    if result.is_none() && !data.test {
        return Err(error!(SolarBError::NoProfitFound));
    }

    Ok(result)
}

/// Compare the checker result against a high-precision golden section search
/// that uses real `swap_base_in` calls — the ground truth.
#[cfg(test)]
fn compare_checker_vs_golden<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &mut [ProgramInstance],
    config: &BotConfig,
    mint_fees: &[(Pubkey, MintFee)],
    checker_result: &arbitrage_checker::ArbitrageResult,
) {
    debug_eprintln!("==============================================");
    use arbitrage::golden_search::golden_search_2hop;

    let golden_result = golden_search_2hop(accounts, instances, config, mint_fees);

    let (golden_amount, golden_profit) = match &golden_result {
        Some(golden) => (golden.optimal_amount, golden.profit),
        None => (0, 0),
    };

    let checker_amount = checker_result.amount_in;
    let checker_profit = checker_result.profit;

    let profit_gap_pct = if golden_profit > 0 {
        let checker_profit_i128 = checker_profit as i128;
        ((golden_profit - checker_profit_i128) as f64 / golden_profit as f64).abs() * 100.0
    } else {
        0.0
    };

    let amount_diff_pct = if golden_amount > 0 {
        ((checker_amount as f64 - golden_amount as f64) / golden_amount as f64).abs() * 100.0
    } else {
        0.0
    };

    let (checker_buy_idx, checker_sell_idx) =
        if checker_result.profit > 0 && checker_result.hop_count >= 2 {
            (
                Some(checker_result.hops[0].instance_idx as usize),
                Some(checker_result.hops[1].instance_idx as usize),
            )
        } else {
            (None, None)
        };

    debug_eprintln!(
        "\n╔══════════════════════════════════════════════════════════════════════════════╗"
    );
    debug_eprintln!(
        "║  CHECKER vs GOLDEN comparison                                               ║"
    );
    debug_eprintln!(
        "╠══════════════════════════════════════════════════════════════════════════════╣"
    );
    debug_eprintln!(
        "║  Golden:       amount={:>15}     profit={:>15.9} SOL{}",
        golden_amount,
        golden_profit as f64 / 1e9,
        if golden_result.is_none() {
            "  (no path)"
        } else {
            ""
        }
    );
    debug_eprintln!(
        "║  Checker:      amount={:>15}     profit={:>15.9} SOL{}",
        checker_amount,
        checker_profit as f64 / 1e9,
        if checker_profit <= 0 || checker_result.hop_count < 2 {
            "  (no path)"
        } else {
            ""
        }
    );
    debug_eprintln!(
        "╠══════════════════════════════════════════════════════════════════════════════╣"
    );
    debug_eprintln!(
        "║  Checker vs Golden:  amount_diff={:.4}%   profit_gap={:.4}%",
        amount_diff_pct,
        profit_gap_pct
    );
    debug_eprintln!(
        "║  Same path (golden vs checker): {}",
        match &golden_result {
            Some(g) => match (checker_buy_idx, checker_sell_idx) {
                (Some(buy_idx), Some(sell_idx))
                    if g.buy_idx == buy_idx && g.sell_idx == sell_idx =>
                    "YES",
                (Some(_), Some(_)) => "NO",
                _ => "N/A",
            },
            _ => "N/A",
        }
    );
    debug_eprintln!(
        "║  Result: {}",
        if profit_gap_pct < 5.0 {
            "PASS"
        } else {
            "CHECK"
        }
    );
    debug_eprintln!(
        "╠══════════════════════════════════════════════════════════════════════════════╣"
    );
    debug_eprintln!("║  GOLDEN PATH:");
    if let Some(g) = &golden_result {
        let buy = &instances[g.buy_idx];
        let sell = &instances[g.sell_idx];
        let start_token = config.start_token.unwrap_or(WSOL);
        let (base_mint, quote_mint) = buy.get_mints();
        let middle_mint = if *base_mint == start_token {
            *quote_mint
        } else {
            *base_mint
        };
        debug_eprintln!(
            "║    buy:  [{}] {} -> {} (pool {})",
            buy.name(),
            start_token,
            middle_mint,
            buy.get_pool_id()
        );
        debug_eprintln!(
            "║    sell: [{}] {} -> {} (pool {})",
            sell.name(),
            middle_mint,
            start_token,
            sell.get_pool_id()
        );
    } else {
        debug_eprintln!("║    (none)");
    }
    debug_eprintln!("║  CHECKER PATH:");
    if let (Some(buy_idx), Some(sell_idx)) = (checker_buy_idx, checker_sell_idx) {
        let (Some(buy), Some(sell)) = (instances.get(buy_idx), instances.get(sell_idx)) else {
            debug_eprintln!(
                "║    (invalid hop indices: buy_idx={:?}, sell_idx={:?})",
                checker_buy_idx,
                checker_sell_idx
            );
            debug_eprintln!("╚══════════════════════════════════════════════════════════════════════════════╝\n");
            return;
        };
        let start_token = config.start_token.unwrap_or(WSOL);
        let (base_mint, quote_mint) = buy.get_mints();
        let middle_mint = if *base_mint == start_token {
            *quote_mint
        } else {
            *base_mint
        };
        debug_eprintln!(
            "║    buy:  [{}] {} -> {} (pool {})",
            buy.name(),
            start_token,
            middle_mint,
            buy.get_pool_id()
        );
        debug_eprintln!(
            "║    sell: [{}] {} -> {} (pool {})",
            sell.name(),
            middle_mint,
            start_token,
            sell.get_pool_id()
        );
    } else {
        debug_eprintln!("║    (none)");
    }
    debug_eprintln!(
        "╚══════════════════════════════════════════════════════════════════════════════╝\n"
    );
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
) -> Result<Option<ArbitrageResult>> {
    let test_mode = data.test;

    // Determine groups: for MULTIPLE_TRADES use group_sizes, for SINGLE_PAIR = 1 group with all pools
    let effective_groups: [u8; 4] =
        if data.mode == arb_mode::MULTIPLE_TRADES && data.group_sizes[0] > 0 {
            data.group_sizes
        } else {
            let total_pools = data
                .pool_types
                .iter()
                .filter(|&&t| dex_type::dynamic_account_count(t) > 0)
                .count() as u8;
            [total_pools, 0, 0, 0]
        };

    let mut pool_idx_offset = 0usize;
    for g in 0..4usize {
        let group_size = effective_groups[g] as usize;
        if group_size == 0 {
            break;
        }

        let pool_idx_start = pool_idx_offset;
        let pool_idx_end = pool_idx_offset + group_size;
        pool_idx_offset = pool_idx_end;

        #[cfg(feature = "benchmark")]
        {
            debug_msg!(">> before parse_accounts");
            sol_log_cu();
        }
        let mut instances = parse_accounts(
            accounts,
            shared_statics_start,
            pool_start,
            data,
            &clock,
            pool_idx_start,
            pool_idx_end,
        )?;
        #[cfg(feature = "benchmark")]
        {
            debug_msg!(">> after parse_accounts");
            sol_log_cu();
        }

        let mut group_config = BotConfig::new(
            Some(WSOL),
            max_amount_in,
            5_500,
            2,
            arb_mode::SINGLE_PAIR_MULTI_MARKET,
            clock.clone(),
            test_mode,
        );

        #[cfg(feature = "benchmark")]
        {
            debug_msg!(">> before run_arb_checker");
            sol_log_cu();
        }
        let mut arb_result =
            match arbitrage_checker::runner::run_arb_checker(accounts, &instances, &group_config) {
                Some(r) => r,
                None => continue,
            };
        #[cfg(feature = "benchmark")]
        {
            debug_msg!(">> after run_arb_checker");
            sol_log_cu();
        }

        #[cfg(test)]
        {
            let mint_fees: Vec<(Pubkey, MintFee)> = vec![];
            #[cfg(feature = "benchmark")]
            {
                debug_msg!(">> before compare_checker_vs_golden");
                sol_log_cu();
            }
            let mut comparison_instances = instances.clone();
            compare_checker_vs_golden(
                accounts,
                &mut comparison_instances,
                &group_config,
                &mint_fees,
                &arb_result,
            );
            #[cfg(feature = "benchmark")]
            {
                debug_msg!(">> after compare_checker_vs_golden");
                sol_log_cu();
            }
            let mut simulation_instances = instances.clone();
            #[cfg(feature = "benchmark")]
            {
                debug_msg!(">> before run_simulation");
                sol_log_cu();
            }
            let sim_profit = run_simulation(
                accounts,
                &arb_result,
                &mut simulation_instances,
                &mut group_config,
                &mint_fees,
            )?;
            #[cfg(feature = "benchmark")]
            {
                debug_msg!(">> after run_simulation");
                sol_log_cu();
            }
        }

        debug_msg!("PROFIT {}", arb_result.profit);
        if arb_result.profit <= 0 {
            continue;
        }

        if test_mode {
            arb_result.amount_in = 1_000_000;
        }

        // #[cfg(test)]
        // return Ok(Some(arb_result));

        execute_arbitrage_path(accounts, &arb_result, &mut instances, payer, data.mints)?;

        // Re-read start token balance after swaps and abort if not profitable
        let balance_after = u64::from_le_bytes(
            accounts[5].try_borrow_data()?
                [TOKEN_ACCOUNT_AMOUNT_OFFSET..TOKEN_ACCOUNT_AMOUNT_OFFSET + 8]
                .try_into()
                .map_err(|_| SolarBError::InvalidAccountData)?,
        );
        if balance_after < max_amount_in {
            return Err(error!(SolarBError::NoProfitFound2));
        }

        return Ok(Some(arb_result));
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
) -> Result<Option<ArbitrageResult>> {
    Ok(None)
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
        let pool_fee = if n_fees > 0 {
            data.pool_fees[fee_offset]
        } else {
            0
        };
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
                create_meteora_dlmm(accounts, static_base, index, end_index, clock)?
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
    Ok(ProgramInstance::PumpAmm(PumpAmm::new(
        accounts,
        static_base,
        dyn_start,
        dyn_end,
        pool_fee,
    )?))
}

#[inline(never)]
fn create_meteora_damm_v1<'info>(
    accounts: &[AccountInfo<'info>],
    static_base: usize,
    dyn_start: usize,
    dyn_end: usize,
    clock: &Clock,
) -> Result<ProgramInstance> {
    Ok(ProgramInstance::MeteoraDammV1(MeteoraDammV1::new(
        accounts,
        static_base,
        dyn_start,
        dyn_end,
        clock,
    )?))
}

#[inline(never)]
fn create_meteora_damm_v2<'info>(
    accounts: &[AccountInfo<'info>],
    static_base: usize,
    dyn_start: usize,
    dyn_end: usize,
    clock: &Clock,
) -> Result<ProgramInstance> {
    Ok(ProgramInstance::MeteoraDammV2(MeteoraDammV2::new(
        accounts,
        static_base,
        dyn_start,
        dyn_end,
        clock,
    )?))
}

#[inline(never)]
fn create_meteora_dlmm<'info>(
    accounts: &[AccountInfo<'info>],
    static_base: usize,
    dyn_start: usize,
    dyn_end: usize,
    clock: &Clock,
) -> Result<ProgramInstance> {
    Ok(ProgramInstance::MeteoraDlmm(MeteoraDlmm::new(
        accounts,
        static_base,
        dyn_start,
        dyn_end,
        clock,
    )?))
}

#[inline(never)]
fn create_orca_whirlpool<'info>(
    accounts: &[AccountInfo<'info>],
    static_base: usize,
    dyn_start: usize,
    dyn_end: usize,
) -> Result<ProgramInstance> {
    Ok(ProgramInstance::OrcaWhirlpool(OrcaWhirlpool::new(
        accounts,
        static_base,
        dyn_start,
        dyn_end,
    )?))
}

#[inline(never)]
fn create_raydium_amm<'info>(
    accounts: &[AccountInfo<'info>],
    static_base: usize,
    dyn_start: usize,
    dyn_end: usize,
) -> Result<ProgramInstance> {
    Ok(ProgramInstance::RaydiumAmm(RaydiumAmm::new(
        accounts,
        static_base,
        dyn_start,
        dyn_end,
    )?))
}

#[inline(never)]
fn create_raydium_clmm<'info>(
    accounts: &[AccountInfo<'info>],
    static_base: usize,
    dyn_start: usize,
    dyn_end: usize,
) -> Result<ProgramInstance> {
    Ok(ProgramInstance::RaydiumCLMM(RaydiumCLMM::new(
        accounts,
        static_base,
        dyn_start,
        dyn_end,
    )?))
}

#[inline(never)]
fn create_raydium_cpmm<'info>(
    accounts: &[AccountInfo<'info>],
    static_base: usize,
    dyn_start: usize,
    dyn_end: usize,
) -> Result<ProgramInstance> {
    Ok(ProgramInstance::RaydiumCPMM(RaydiumCPMM::new(
        accounts,
        static_base,
        dyn_start,
        dyn_end,
    )?))
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
    arb: &ArbitrageResult,
    instances: &mut [ProgramInstance],
    payer: &AccountInfo<'info>,
    num_mints: u8,
) -> Result<()> {
    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("Executing {} hops", arb.hop_count);

    let num_mints = num_mints as usize;

    let start_amount = arb.amount_in;
    let mut current_amount = start_amount;

    for hop in &arb.hops[..arb.hop_count as usize] {
        let program_instance = &mut instances[hop.instance_idx as usize];
        let (base_mint, quote_mint) = program_instance.get_mints();
        let (input_mint_key, output_mint_key) = if hop.left_to_right {
            (base_mint, quote_mint)
        } else {
            (quote_mint, base_mint)
        };

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
        let final_profit = current_amount as i128 - arb.amount_in as i128;
        debug_eprintln!(
            "Done: {} -> {} (profit: {})",
            arb.amount_in,
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
    arb: &ArbitrageResult,
    instances: &mut [ProgramInstance],
    bot_config: &mut BotConfig,
    mint_fees: &[(Pubkey, MintFee)],
) -> Result<i128> {
    let start_amount = arb.amount_in;

    let mut current_amount = start_amount;

    for (i, hop) in arb.hops[..arb.hop_count as usize].iter().enumerate() {
        let program_instance = &mut instances[hop.instance_idx as usize];
        let (base_mint, quote_mint) = program_instance.get_mints();
        let (input_mint, output_mint) = if hop.left_to_right {
            (*base_mint, *quote_mint)
        } else {
            (*quote_mint, *base_mint)
        };

        let (in_fee, out_fee) = get_transfer_fees(input_mint, base_mint, quote_mint, mint_fees);

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
            let required_in = program_instance.swap_base_out(
                accounts,
                output_mint,
                amount_out,
                in_fee,
                out_fee,
                &bot_config.clock,
            )?;
            let input_short = &input_mint.to_string()[..8];
            let output_short = &output_mint.to_string()[..8];
            let in_decimals = get_mint_decimals(accounts, &input_mint);
            let out_decimals = get_mint_decimals(accounts, &output_mint);
            debug_eprintln!(
            "Hop {}: [{}] {}..-> {}..  |  in={} -> out={}  |  swap_base_out max_in={}  |  diff={}",
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
    if arb.hop_count > 0 {
        debug_eprintln!("---");
        let last_hop = &arb.hops[arb.hop_count as usize - 1];
        let last_inst = &instances[last_hop.instance_idx as usize];
        let (last_base, last_quote) = last_inst.get_mints();
        let end_mint = if last_hop.left_to_right {
            last_quote
        } else {
            last_base
        };
        let end_decimals = get_mint_decimals(accounts, end_mint);
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
        let first_hop = &arb.hops[0];
        let first_inst = &instances[first_hop.instance_idx as usize];
        let (first_base, first_quote) = first_inst.get_mints();
        let start_mint = if first_hop.left_to_right {
            first_base
        } else {
            first_quote
        };
        let start_decimals = get_mint_decimals(accounts, start_mint);
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
        for (i, &v) in pool_types.iter().enumerate() {
            pt[i] = v;
        }
        for (i, &v) in type_static_offsets.iter().enumerate() {
            to[i] = v;
        }
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
        let data = make_instruction_data(&[dex_type::METEORA_DAMM_V2], &[0], 4);
        let result = parse_accounts(&accounts, 0, 4, &data, &default_clock(), 0, 8);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_accounts_insufficient_dynamic_accounts() {
        let accounts = create_mock_accounts(5, system_program::id());
        let data = make_instruction_data(&[dex_type::RAYDIUM_CPMM], &[0], 2);
        let result = parse_accounts(&accounts, 0, 2, &data, &default_clock(), 0, 8);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_accounts_unknown_dex_type_skipped() {
        let owner = system_program::id();
        let accounts = create_mock_accounts(10, owner);
        // dex_type 99 is unknown → dynamic_account_count=0 → skipped
        let data = make_instruction_data(&[99], &[0], 0);
        let result = parse_accounts(&accounts, 0, 0, &data, &default_clock(), 0, 8);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
