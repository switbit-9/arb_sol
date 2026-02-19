use anchor_lang::prelude::*;
use anchor_lang::solana_program::pubkey::Pubkey;

use crate::programs::programs::ProgramInstance;
use crate::arbitrage::algo_2::utils::*;

// =============================================================================
// AMM → AMM FORMULAS (PumpAmm ↔ MeteoraDammV2)
// =============================================================================
//
// These formulas handle arbitrage between two different AMM types without DLMM:
// 1. CP AMM → CP AMM: Two constant product pools
// 2. CP AMM → CL AMM: Constant product → Concentrated liquidity
// 3. CL AMM → CP AMM: Concentrated liquidity → Constant product
// 4. CL AMM → CL AMM: Two concentrated liquidity pools
// =============================================================================

/// Optimal amount for Constant Product AMM → Constant Product AMM arbitrage
///
/// # Path
/// input → [CP AMM 1] → middle → [CP AMM 2] → output
///
/// # Math Derivation
/// After CP1:  mid = y1 · input · f1 / (x1 + input · f1)
/// After CP2:  out = y2 · mid · f2 / (x2 + mid · f2)
/// Profit = out - input
///
/// This is a complex nested function. The optimal solution comes from:
/// d(profit)/d(input) = 0
///
/// After simplification:
/// optimal ≈ (sqrt(K) - x1) / f1
/// where K = x1 · x2 · y1 · y2 · f1 · f2
///
/// # Arguments
/// * `cp1_reserve_in` - CP1 reserve of input token (x1)
/// * `cp1_reserve_out` - CP1 reserve of middle token (y1)
/// * `cp1_fee` - CP1 fee factor
/// * `cp2_reserve_in` - CP2 reserve of middle token (x2)
/// * `cp2_reserve_out` - CP2 reserve of output token (y2)
/// * `cp2_fee` - CP2 fee factor
/// * `max_amount_in` - User's maximum input constraint
///
/// # Returns
/// Optimal input amount
pub fn find_optimal_amount_amm_to_amm<'info>(
    program_1: &Box<dyn ProgramMeta + 'info>,
    program_2: &Box<dyn ProgramMeta + 'info>,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    max_amount_in: u64,
) -> Result<u64> {

    let pump1 = extract_pump(program_1)?;
    let pump2 = extract_pump(program_2)?;

    let (cp1_in, cp1_out) = if input_mint == pump1.base_token_pk {
        (pump1.base_vault_amount, pump1.quote_vault_amount)
    } else {
        (pump1.quote_vault_amount, pump1.base_vault_amount)
    };

    let (cp2_in, cp2_out) = if middle_mint == pump2.base_token_pk {
        (pump2.base_vault_amount, pump2.quote_vault_amount)
    } else {
        (pump2.quote_vault_amount, pump2.base_vault_amount)
    };

    let fee1 = 1.0 - pump1.fee;
    let fee2 = 1.0 - pump2.fee;

    let x1 = cp1_in as f64;
    let y1 = cp1_out as f64;
    let f1 = fee1;
    let x2 = cp2_in as f64;
    let y2 = cp2_out as f64;
    let f2 = fee2;

    // Check for arbitrage opportunity
    // Effective rate through both pools
    let effective_rate = (y1 / x1) * f1 * (y2 / x2) * f2;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "CP1: x={}, y={}, fee={}; CP2: x={}, y={}, fee={}; rate={}",
        x1, y1, f1, x2, y2, f2, effective_rate
    );

    if effective_rate <= 1.0 {
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!("No arbitrage: effective_rate {} <= 1.0", effective_rate);
    }

    // Optimal formula (approximation for nested constant product)
    let k_term = x1 * x2 * y1 * y2 * f1 * f2;
    let approx_optimal = (k_term.sqrt() - x1) / f1;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("Approx optimal: {}", approx_optimal);

    let max_amount_in = max_amount_in;
    let final_amount = if approx_optimal > 0.0 {
        (approx_optimal as u64).min(max_amount_in)
    } else {
        max_amount_in / 2 // Fallback
    };

    Ok(final_amount)
}

/// Optimal amount for Constant Product AMM → Concentrated Liquidity AMM arbitrage
///
/// # Path
/// input → [CP AMM] → middle → [CL AMM] → output
///
/// # Math
/// CP outputs middle token, CL uses virtual reserves for swap
///
/// # Arguments
/// * `cp_reserve_in` - CP AMM reserve of input token (x)
/// * `cp_reserve_out` - CP AMM reserve of middle token (y)
/// * `cp_fee` - CP AMM fee factor
/// * `cl_liquidity` - CL AMM liquidity (L)
/// * `cl_sqrt_price` - CL AMM sqrt price (√P), Q64.64 fixed point
/// * `cl_fee` - CL AMM fee factor
/// * `middle_is_base` - true if middle token is CL AMM base token
/// * `max_amount_in` - User's maximum input constraint
///
/// # Returns
/// Optimal input amount
pub fn find_optimal_amount_amm_to_damm2<'info>(
    program_1: &Box<dyn ProgramMeta + 'info>,
    program_2: &Box<dyn ProgramMeta + 'info>,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    max_amount_in: u64,
) -> Result<u64> {

    let pump = extract_pump(program_1)?;
    let damm = extract_damm2(program_2)?;

    let (cp_in, cp_out) = if input_mint == pump.base_token_pk {
        (pump.base_vault_amount, pump.quote_vault_amount)
    } else {
        (pump.quote_vault_amount, pump.base_vault_amount)
    };

    let liquidity = damm.pool.liquidity;
    let sqrt_price = damm.pool.sqrt_price;
    let middle_is_base = middle_mint == damm.base_token_pk;

    let fee_cp = 1.0 - pump.fee;
    let fee_cl = 0.9975; // MeteoraDammV2 typical fee

    // Convert sqrt_price from Q64.64 to f64
    let sqrt_price = sqrt_price as f64 / (1u128 << 64) as f64;
    let l = liquidity as f64;

    let x_cp = cp_in as f64;
    let y_cp = cp_out as f64;
    let f_cp = fee_cp;
    let f_cl = fee_cl;

    // Calculate CL virtual reserves
    let x_virtual = l / sqrt_price;
    let y_virtual = l * sqrt_price;

    let (cl_reserve_in, cl_reserve_out) = if middle_is_base {
        (x_virtual, y_virtual)
    } else {
        (y_virtual, x_virtual)
    };

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "CP: x={}, y={}, fee={}; CL: L={}, √P={}, virtual_in={}, virtual_out={}, fee={}",
        x_cp, y_cp, f_cp, l, sqrt_price, cl_reserve_in, cl_reserve_out, f_cl
    );

    // Effective rate
    let effective_rate = (y_cp / x_cp) * f_cp * (cl_reserve_out / cl_reserve_in) * f_cl;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("Effective rate: {}", effective_rate);

    if effective_rate <= 1.0 {
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!("No arbitrage: effective_rate {} <= 1.0", effective_rate);
    }

    // Use similar approach to CP → CP with virtual reserves
    let k_term = x_cp * cl_reserve_in * y_cp * cl_reserve_out * f_cp * f_cl;
    let approx_optimal = (k_term.sqrt() - x_cp) / f_cp;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("Approx optimal: {}", approx_optimal);

    let final_amount = if approx_optimal > 0.0 {
        (approx_optimal as u64).min(max_amount_in)
    } else {
        max_amount_in / 2
    };

    Ok(final_amount)
}

/// Optimal amount for Concentrated Liquidity AMM → Constant Product AMM arbitrage
///
/// # Path
/// input → [CL AMM] → middle → [CP AMM] → output
///
/// # Arguments
/// * `cl_liquidity` - CL AMM liquidity (L)
/// * `cl_sqrt_price` - CL AMM sqrt price (√P), Q64.64 fixed point
/// * `cl_fee` - CL AMM fee factor
/// * `input_is_base` - true if input is CL AMM base token
/// * `cp_reserve_in` - CP AMM reserve of middle token (x)
/// * `cp_reserve_out` - CP AMM reserve of output token (y)
/// * `cp_fee` - CP AMM fee factor
/// * `max_amount_in` - User's maximum input constraint
///
/// # Returns
/// Optimal input amount
pub fn find_optimal_amount_damm2_to_amm<'info>(
    program_1: &Box<dyn ProgramMeta + 'info>,
    program_2: &Box<dyn ProgramMeta + 'info>,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    max_amount_in: u64,
) -> Result<u64> {

    let pump = extract_pump(program_1)?;
    let damm = extract_damm2(program_2)?;

    let liquidity = damm.pool.liquidity;
    let sqrt_price = damm.pool.sqrt_price;
    let input_is_base = input_mint == damm.base_token_pk;

    let (cp_in, cp_out) = if middle_mint == pump.base_token_pk {
        (pump.base_vault_amount, pump.quote_vault_amount)
    } else {
        (pump.quote_vault_amount, pump.base_vault_amount)
    };

    let fee_cl = 0.9975;
    let fee_cp = 1.0 - pump.fee;

    // Convert sqrt_price from Q64.64 to f64
    let sqrt_price = sqrt_price as f64 / (1u128 << 64) as f64;
    let l = liquidity as f64;
    let f_cl = fee_cl;

    let x_cp = cp_in as f64;
    let y_cp = cp_out as f64;
    let f_cp = fee_cp;

    // Calculate CL virtual reserves
    let x_virtual = l / sqrt_price;
    let y_virtual = l * sqrt_price;

    let (cl_reserve_in, cl_reserve_out) = if input_is_base {
        (x_virtual, y_virtual)
    } else {
        (y_virtual, x_virtual)
    };

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "CL: L={}, √P={}, virtual_in={}, virtual_out={}, fee={}; CP: x={}, y={}, fee={}",
        l, sqrt_price, cl_reserve_in, cl_reserve_out, f_cl, x_cp, y_cp, f_cp
    );

    // Effective rate
    let effective_rate = (cl_reserve_out / cl_reserve_in) * f_cl * (y_cp / x_cp) * f_cp;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("Effective rate: {}", effective_rate);

    if effective_rate <= 1.0 {
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!("No arbitrage: effective_rate {} <= 1.0", effective_rate);
    }

    // Approximation using virtual reserves
    let k_term = cl_reserve_in * x_cp * cl_reserve_out * y_cp * f_cl * f_cp;
    let approx_optimal = (k_term.sqrt() - cl_reserve_in) / f_cl;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("Approx optimal: {}", approx_optimal);

    let final_amount = if approx_optimal > 0.0 {
        (approx_optimal as u64).min(max_amount_in)
    } else {
        max_amount_in / 2
    };

    Ok(final_amount)
}

/// Optimal amount for Concentrated Liquidity AMM → Concentrated Liquidity AMM arbitrage
///
/// # Path
/// input → [CL AMM 1] → middle → [CL AMM 2] → output
///
/// # Arguments
/// * `cl1_liquidity` - CL1 liquidity (L1)
/// * `cl1_sqrt_price` - CL1 sqrt price (√P1), Q64.64 fixed point
/// * `cl1_fee` - CL1 fee factor
/// * `input_is_base` - true if input is CL1 base token
/// * `cl2_liquidity` - CL2 liquidity (L2)
/// * `cl2_sqrt_price` - CL2 sqrt price (√P2), Q64.64 fixed point
/// * `cl2_fee` - CL2 fee factor
/// * `middle_is_base` - true if middle token is CL2 base token
/// * `max_amount_in` - User's maximum input constraint
///
/// # Returns
/// Optimal input amount
pub fn find_optimal_amount_damm2_to_damm2<'info>(
    program_1: &Box<dyn ProgramMeta + 'info>,
    program_2: &Box<dyn ProgramMeta + 'info>,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    max_amount_in: u64,
) -> Result<u64> {

    let damm1 = extract_damm2(program_1)?;
    let damm2 = extract_damm2(program_2)?;

    let l1 = damm1.pool.liquidity;
    let sqrt_price1 = damm1.pool.sqrt_price;
    let input_is_base = input_mint == damm1.base_token_pk;

    let l2 = damm2.pool.liquidity;
    let sqrt_price2 = damm2.pool.sqrt_price;
    let middle_is_base = middle_mint == damm2.base_token_pk;

    let fee1 = 0.9975;
    let fee2 = 0.9975;

    // Convert sqrt prices from Q64.64 to f64
    let sqrt_price1 = sqrt_price1 as f64 / (1u128 << 64) as f64;
    let sqrt_price2 = sqrt_price2 as f64 / (1u128 << 64) as f64;
    let l1 = l1 as f64;
    let l2 = l2 as f64;
    let f1 = fee1;
    let f2 = fee2;

    // Calculate CL1 virtual reserves
    let x1_virtual = l1 / sqrt_price1;
    let y1_virtual = l1 * sqrt_price1;

    let (cl1_reserve_in, cl1_reserve_out) = if input_is_base {
        (x1_virtual, y1_virtual)
    } else {
        (y1_virtual, x1_virtual)
    };

    // Calculate CL2 virtual reserves
    let x2_virtual = l2 / sqrt_price2;
    let y2_virtual = l2 * sqrt_price2;

    let (cl2_reserve_in, cl2_reserve_out) = if middle_is_base {
        (x2_virtual, y2_virtual)
    } else {
        (y2_virtual, x2_virtual)
    };

    #[cfg(any(test, feature = "debug"))]
    {
        debug_eprintln!(
            "CL1: L={}, √P={}, virtual_in={}, virtual_out={}, fee={}",
            l1, sqrt_price1, cl1_reserve_in, cl1_reserve_out, f1
        );
        debug_eprintln!(
            "CL2: L={}, √P={}, virtual_in={}, virtual_out={}, fee={}",
            l2, sqrt_price2, cl2_reserve_in, cl2_reserve_out, f2
        );
    }

    // Effective rate
    let effective_rate =
        (cl1_reserve_out / cl1_reserve_in) * f1 * (cl2_reserve_out / cl2_reserve_in) * f2;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("Effective rate: {}", effective_rate);

    if effective_rate <= 1.0 {
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!("No arbitrage: effective_rate {} <= 1.0", effective_rate);
    }

    // Approximation using both virtual reserves
    let k_term = cl1_reserve_in * cl2_reserve_in * cl1_reserve_out * cl2_reserve_out * f1 * f2;
    let approx_optimal = (k_term.sqrt() - cl1_reserve_in) / f1;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("Approx optimal: {}", approx_optimal);

    let final_amount = if approx_optimal > 0.0 {
        (approx_optimal as u64).min(max_amount_in)
    } else {
        max_amount_in / 2
    };

    Ok(final_amount)
}

// /// Find optimal amount for AMM → AMM arbitrage path (PumpAmm ↔ MeteoraDammV2)
// /// Supports all combinations: CP→CP, CP→CL, CL→CP, CL→CL
// pub fn find_optimal_amount_amm_to_amm<'info>(
//     arbitrage_path: &mut ArbitragePath,
//     amm1: &ProgramInstance<'info>,
//     amm2: &ProgramInstance<'info>,
//     input_mint: Pubkey,
//     accounts: &[AccountInfo<'info>],
//     config: &mut BotConfig<'info>,
// ) -> Result<u64> {
//     debug_eprintln!("");
//     debug_eprintln!("");
//     debug_eprintln!("========== ANALYTICAL AMM -> AMM ==========");

//     let middle_mint: Pubkey = {
//         let edges = &arbitrage_path.edges;
//         edges[0].right.mint_account
//     };
//     let clock = config.clock.clone();

//     // Determine AMM types and calculate optimal amount
//     let optimal_amount_in = match (amm1, amm2) {
//         // CP AMM → CP AMM (PumpAmm → PumpAmm)
//         (ProgramInstance::PumpAmm(pump1), ProgramInstance::PumpAmm(pump2)) => {
//             debug_eprintln!("Path: CP AMM → CP AMM (PumpAmm → PumpAmm)");
//             optimal_amount_cp_to_cp(arbitrage_path, amm1, amm1, input_mint, middle_mint, accounts, config)?
//         }

//         // CP AMM → CL AMM (PumpAmm → MeteoraDammV2)
//         (ProgramInstance::PumpAmm(pump), ProgramInstance::MeteoraDammV2(damm)) => {
//             debug_eprintln!("Path: CP AMM → CL AMM (PumpAmm → MeteoraDammV2)");

//             // let (base, quote) = pump.get_vault_amounts()?;
//             // let (cp_in, cp_out) = if input_mint == pump.base_token_pk {
//             //     (base, quote)
//             // } else {
//             //     (quote, base)
//             // };

//             // let liquidity = damm.pool.liquidity;
//             // let sqrt_price = damm.pool.sqrt_price;
//             // let middle_is_base = middle_mint == damm.base_token_pk;

//             // let fee_cp = 1.0 - pump.fee;
//             // let fee_cl = 0.9975; // MeteoraDammV2 typical fee
//             find_optimal_amount_amm_to_damm2(arbitrage_path, amm1, amm2, input_mint, middle_mint, accounts, config)?;
//         }

//         // CL AMM → CP AMM (MeteoraDammV2 → PumpAmm)
//         (ProgramInstance::MeteoraDammV2(damm), ProgramInstance::PumpAmm(pump)) => {
//             debug_eprintln!("Path: CL AMM → CP AMM (MeteoraDammV2 → PumpAmm)");

//             // let liquidity = damm.pool.liquidity;
//             // let sqrt_price = damm.pool.sqrt_price;
//             // let input_is_base = input_mint == damm.base_token_pk;

//             // let (base, quote) = pump.get_vault_amounts()?;
//             // let (cp_in, cp_out) = if middle_mint == pump.base_token_pk {
//             //     (base, quote)
//             // } else {
//             //     (quote, base)
//             // };

//             // let fee_cl = 0.9975;
//             // let fee_cp = 1.0 - pump.fee;

//             find_optimal_amount_damm2_to_amm(
//                 arbitrage_path, amm1, amm2, input_mint, middle_mint, accounts, config)?;
//         }

//         // CL AMM → CL AMM (MeteoraDammV2 → MeteoraDammV2)
//         (ProgramInstance::MeteoraDammV2(damm1), ProgramInstance::MeteoraDammV2(damm2)) => {
//             debug_eprintln!("Path: CL AMM → CL AMM (MeteoraDammV2 → MeteoraDammV2)");

//             // let l1 = damm1.pool.liquidity;
//             // let sqrt_price1 = damm1.pool.sqrt_price;
//             // let input_is_base = input_mint == damm1.base_token_pk;

//             // let l2 = damm2.pool.liquidity;
//             // let sqrt_price2 = damm2.pool.sqrt_price;
//             // let middle_is_base = middle_mint == damm2.base_token_pk;

//             // let fee1 = 0.9975;
//             // let fee2 = 0.9975;

//             find_optimal_amount_damm2_to_damm2(
//                 arbitrage_path, damm1, damm2, input_mint, middle_mint, accounts, config)?;
//         }

//         _ => {
//             debug_eprintln!("Unsupported AMM combination");
//             return Err(error!(SolarBError::InvalidProgramType));
//         }
//     };

// }