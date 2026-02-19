use anchor_lang::prelude::*;

use crate::arbitrage::base::Edge;
use crate::arbitrage::utils::find_instance_by_pool_id;
use crate::programs::meteora_damm_v2::MeteoraDammV2;
use crate::programs::meteora_dlmm::MeteoraDlmm;
use crate::programs::programs::ProgramInstance;
use crate::programs::pump_amm::PumpAmm;
use crate::programs::SolarBError;
use crate::utils::bot_config::BotConfig;

use crate::arbitrage::algo_2::arbitrage_path::ArbitragePath;

use crate::arbitrage::algo_2::amm_to_amm_formulas::{
    find_optimal_amount_amm_to_amm, find_optimal_amount_amm_to_damm2,
    find_optimal_amount_damm2_to_amm, find_optimal_amount_damm2_to_damm2,
};
use crate::arbitrage::algo_2::arbitratge_calculator_new::{
    find_optimal_amount_amm_to_dlmm_v2, find_optimal_amount_damm2_to_dlmm_v2,
    find_optimal_amount_dlmm_to_amm_v2, find_optimal_amount_dlmm_to_damm2_v2,
    find_optimal_amount_in_dlmm_to_dlmm,
};

// #[cfg(test)]
// use crate::utils::test_utils::write_results_to_file;

// All immutable borrows of arbitrage_path are now dropped
// Now we can mutably borrow it in the match arms
// Require each side to be exactly one of DLMM / AMM / DAMM2
#[derive(Debug, Clone, Copy)]
enum MarketKind {
    Dlmm,
    Amm,
    Damm2,
}

/// Main entry point to find optimal amount in for any arbitrage path
pub fn find_optimal_amount_in<'info>(
    edges: &[Edge],
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance<'info>],
    config: &mut BotConfig,
) -> Result<u64> {
    if edges.len() < 2 {
        return Err(error!(SolarBError::InsufficientAccounts));
    }

    // Extract values before mutably borrowing arbitrage_path
    let first_pool_id = edges[0].pool_id;
    let second_pool_id = edges[1].pool_id;
    let first_program = edges[0].program;
    let second_program = edges[1].program;
    let input_mint = edges[0].left.mint_account;
    let middle_mint = edges[0].right.mint_account;

    // Find instances by matching pool_id from edges
    let first_instance = find_instance_by_pool_id(instances, &first_pool_id)?;
    let second_instance = find_instance_by_pool_id(instances, &second_pool_id)?;

    let is_first_dlmm = first_program == MeteoraDlmm::PROGRAM_ID;
    let is_second_dlmm = second_program == MeteoraDlmm::PROGRAM_ID;

    let is_first_amm = first_program == PumpAmm::PROGRAM_ID;
    let is_second_amm = second_program == PumpAmm::PROGRAM_ID;

    let is_first_damm2 = first_program == MeteoraDammV2::PROGRAM_ID;
    let is_second_damm2 = second_program == MeteoraDammV2::PROGRAM_ID;

    let max_amount_in = config.max_amount_in;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("PATH: {:?} -> {:?}", first_program, second_program);

    let first_kind = match (is_first_dlmm, is_first_amm, is_first_damm2) {
        (true, false, false) => MarketKind::Dlmm,
        (false, true, false) => MarketKind::Amm,
        (false, false, true) => MarketKind::Damm2,
        _ => return Err(error!(SolarBError::InvalidPathType)),
    };

    let second_kind = match (is_second_dlmm, is_second_amm, is_second_damm2) {
        (true, false, false) => MarketKind::Dlmm,
        (false, true, false) => MarketKind::Amm,
        (false, false, true) => MarketKind::Damm2,
        _ => return Err(error!(SolarBError::InvalidPathType)),
    };

    let optimal_amount_in: u64 = match (first_kind, second_kind) {
        (MarketKind::Dlmm, MarketKind::Dlmm) => {
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!("PATH: DLMM -> DLMM");
            // Run twice (per user request) and return the second result
            find_optimal_amount_in_dlmm_to_dlmm(
                first_instance,
                second_instance,
                input_mint,
                middle_mint,
                max_amount_in,
            )?
        }
        (MarketKind::Dlmm, MarketKind::Amm) => {
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!("PATH: DLMM -> AMM");
            find_optimal_amount_dlmm_to_amm_v2(
                first_instance,
                second_instance,
                input_mint,
                middle_mint,
                max_amount_in,
            )?
        }
        (MarketKind::Dlmm, MarketKind::Damm2) => {
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!("PATH: DLMM -> DAMM V2");
            find_optimal_amount_dlmm_to_damm2_v2(
                first_instance,
                second_instance,
                input_mint,
                middle_mint,
                max_amount_in,
            )?
        }
        (MarketKind::Amm, MarketKind::Dlmm) => {
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!("PATH: AMM -> DLMM");
            find_optimal_amount_amm_to_dlmm_v2(
                first_instance,
                second_instance,
                input_mint,
                middle_mint,
                max_amount_in,
            )?
        }
        (MarketKind::Amm, MarketKind::Amm) => {
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!("PATH: AMM -> AMM");
            find_optimal_amount_amm_to_amm(
                first_instance,
                second_instance,
                input_mint,
                middle_mint,
                max_amount_in,
            )?
        }
        (MarketKind::Amm, MarketKind::Damm2) => {
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!("PATH: AMM -> DAMM V2");
            find_optimal_amount_amm_to_damm2(
                first_instance,
                second_instance,
                input_mint,
                middle_mint,
                max_amount_in,
            )?
        }
        (MarketKind::Damm2, MarketKind::Dlmm) => {
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!("PATH: DAMM V2 -> DLMM");
            find_optimal_amount_damm2_to_dlmm_v2(
                first_instance,
                second_instance,
                input_mint,
                middle_mint,
                max_amount_in,
            )?
        }
        (MarketKind::Damm2, MarketKind::Amm) => {
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!("PATH: DAMM V2 -> AMM");
            find_optimal_amount_damm2_to_amm(
                first_instance,
                second_instance,
                input_mint,
                middle_mint,
                max_amount_in,
            )?
        }
        (MarketKind::Damm2, MarketKind::Damm2) => {
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!("PATH: DAMM V2 -> DAMM V2");
            find_optimal_amount_damm2_to_damm2(
                first_instance,
                second_instance,
                input_mint,
                middle_mint,
                max_amount_in,
            )?
        }
    };

    let clock: Clock = config.clock.clone();
    let token_out =
        first_instance.swap_base_in(accounts, input_mint, optimal_amount_in, clock.clone())?;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        ": {} input -> {} middle",
        optimal_amount_in as f64 / 1_000_000_000.0,
        token_out as f64 / 1_000_000.0
    );

    let amount_out =
        second_instance.swap_base_in(accounts, middle_mint, token_out, clock.clone())?;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "AMM: {} middle -> {} output",
        token_out as f64 / 1_000_000.0,
        amount_out as f64 / 1_000_000_000.0
    );

    let profit: i64 = amount_out as i64 - optimal_amount_in as i64;

    if profit > 0 {
        #[cfg(any(test, feature = "debug"))]
        {
            let _profit = profit as f64 / 1_000_000_000.0;
            let _optimal_amount_in = optimal_amount_in as f64 / 1_000_000_000.0;
            let pct: f64 = (_profit as f64 / _optimal_amount_in as f64) * 100.0;
            debug_eprintln!("✅ PROFIT {_optimal_amount_in} SOL -> {amount_out} SOL => {profit} ({_profit} SOL) {pct:.4}%");
        }
        let arbitrage_path = ArbitragePath {
            edges: edges.to_vec(),
            profit: profit as i128,
            final_amount: amount_out as u128,
            start_amount: optimal_amount_in,
        };
        // #[cfg(test)]
        // write_results_to_file(&[Some(arbitrage_path.clone())]);
    } else {
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!("❌ NO PROFIT");
    }

    Ok(optimal_amount_in)
}
