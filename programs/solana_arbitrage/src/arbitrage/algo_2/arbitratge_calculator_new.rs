use anchor_lang::prelude::*;
use anchor_lang::solana_program::pubkey::Pubkey;

use crate::arbitrage::algo_2::utils::*;
use crate::programs::programs::ProgramMeta;
/// Optimal amount for Constant Product AMM → DLMM arbitrage
///
/// # Path
/// input → [CP AMM] → middle → [DLMM] → output
///
/// # Math Derivation
/// After CP:   mid = y · input · f_cp / (x + input · f_cp)
/// After DLMM: out = mid · P · f_dlmm
/// Profit = out - input
///
/// Setting d(profit)/d(input) = 0:
/// optimal = (sqrt(y · P · f_dlmm · f_cp · x) - x) / f_cp
///
/// # Arguments
/// * `cp_reserve_in` - CP AMM reserve of input token (x)
/// * `cp_reserve_out` - CP AMM reserve of middle token (y)
/// * `cp_fee` - CP AMM fee factor (e.g., 0.9875 for 1.25% fee)
/// * `dlmm_price` - DLMM bin price (middle token → output token)
/// * `dlmm_fee` - DLMM fee factor (e.g., 0.997)
/// * `dlmm_max_input` - Max tokens DLMM can accept in current bin
/// * `max_amount_in` - User's maximum input constraint
///
/// # Returns
/// Optimal input amount, or error if no arbitrage opportunity
pub fn find_optimal_amount_amm_to_dlmm_v2<'info>(
    program_1: &Box<dyn ProgramMeta + 'info>,
    program_2: &Box<dyn ProgramMeta + 'info>,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    max_amount_in: u64,
) -> Result<u64> {
    let pump = extract_pump(program_1)?;
    let dlmm = extract_dlmm(program_2)?;
    // Constant Product AMM - use reserves-based formula
    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("AMM Type: Constant Product (PumpAmm)");
    // Get DLMM price for input_mint → middle_mint direction
    let (dlmm_price, dlmm_inverse_price) = dlmm.get_prices()?;

    // DLMM rate: how much middle_mint we get per input_mint
    let dlmm_price_for_input = if input_mint == dlmm.base_token_pk {
        // input_mint is DLMM base, we're swapping base → quote
        dlmm_price
    } else {
        // input_mint is DLMM quote, we're swapping quote → base
        dlmm_inverse_price
    };

    let dlmm_fee = 1.0 - dlmm.fee_rate;
    let dlmm_max_output = dlmm.get_max_amount_out(input_mint)?;
    let dlmm_max_input = dlmm.get_max_amount_in(middle_mint)?;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "DLMM: price={}, fee_factor={}, max_output={}",
        dlmm_price_for_input, dlmm_fee, dlmm_max_output
    );

    let (base_vault_amount, quote_vault_amount) = pump.get_vault_amounts()?;
    let (cp_reserve_in, cp_reserve_out) = if input_mint == pump.base_token_pk {
        (base_vault_amount, quote_vault_amount)
    } else {
        (quote_vault_amount, base_vault_amount)
    };
    let cp_fee = 1.0 - pump.fee;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "CP AMM reserves: in={}, out={}, fee_factor={}",
        cp_reserve_in, cp_reserve_out, cp_fee
    );
    let x = cp_reserve_in as f64;
    let y = cp_reserve_out as f64;
    let f_cp = cp_fee;
    let p = dlmm_price_for_input;
    let f_dlmm = dlmm_fee;

    // Combined effective rate through both pools
    // Rate = (y/x) * f_cp * P * f_dlmm
    // Arbitrage exists when rate > 1
    let effective_rate = (y / x) * f_cp * p * f_dlmm;

    #[cfg(any(test, feature = "debug"))]
    {
        debug_eprintln!(
            "CP reserves: x={}, y={}, fee={}, DLMM price={}, fee={}, effective_rate={}",
            x, y, f_cp, p, f_dlmm, effective_rate
        );

        if effective_rate <= 1.0 {
            debug_eprintln!("No arbitrage: effective_rate {} <= 1.0", effective_rate);
        }
    }

    if effective_rate <= 1.0 {
        // return Err(error!(SolarBError::NoProfitFound));
    }

    // Optimal input: (sqrt(y · P · f_dlmm · f_cp · x) - x) / f_cp
    let sqrt_term = (y * p * f_dlmm * f_cp * x).sqrt();
    let optimal = (sqrt_term - x) / f_cp;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "Optimal calculation: sqrt_term={}, optimal={}",
        sqrt_term, optimal
    );

    if optimal <= 0.0 {
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!("No profit: optimal {} <= 0", optimal);
        // return Err(error!(SolarBError::NoProfitFound));
    }

    // Check DLMM constraint: CP output must not exceed DLMM capacity
    // CP output formula: y * input * f_cp / (x + input * f_cp)
    let cp_output_at_optimal = y * optimal * f_cp / (x + optimal * f_cp);

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "CP output at optimal: {}, DLMM max input: {}",
        cp_output_at_optimal, dlmm_max_input
    );

    let constrained_optimal = if cp_output_at_optimal > dlmm_max_input as f64 {
        // Reduce input so CP output equals DLMM max input
        // Solve: dlmm_max_input = y * input * f_cp / (x + input * f_cp)
        // => dlmm_max_input * x + dlmm_max_input * input * f_cp = y * input * f_cp
        // => dlmm_max_input * x = input * f_cp * (y - dlmm_max_input)
        // => input = dlmm_max_input * x / (f_cp * (y - dlmm_max_input))
        let max_mid = dlmm_max_input as f64;
        if y <= max_mid {
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!("No profit: y {} <= max_mid {}", y, max_mid);
            // return Err(error!(SolarBError::NoProfitFound));
        }
        let constrained = max_mid * x / (f_cp * (y - max_mid));
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!(
            "Constrained by DLMM capacity: {} -> {}",
            optimal, constrained
        );
        constrained
    } else {
        optimal
    };

    let final_amount = (constrained_optimal as u64).min(max_amount_in);

    Ok(final_amount)
}

/// Optimal amount for DLMM → Constant Product AMM arbitrage
///
/// # Path
/// input → [DLMM] → middle → [CP AMM] → output
///
/// # Math Derivation
/// After DLMM: mid = input · P · f_dlmm
/// After CP:   out = y · mid · f_cp / (x + mid · f_cp)
/// Profit = out - input
///
/// Setting d(profit)/d(input) = 0:
/// Let M = P · f_dlmm (DLMM multiplier)
/// optimal_mid = (sqrt(y · f_cp · x) - x) / f_cp
/// optimal_input = optimal_mid / M
///
/// # Arguments
/// * `dlmm_price` - DLMM bin price (input token → middle token)
/// * `dlmm_fee` - DLMM fee factor
/// * `dlmm_max_output` - Max tokens DLMM can output in current bin
/// * `cp_reserve_in` - CP AMM reserve of middle token (x)
/// * `cp_reserve_out` - CP AMM reserve of output token (y)
/// * `cp_fee` - CP AMM fee factor
/// * `max_amount_in` - User's maximum input constraint
///
/// # Returns
/// Optimal input amount, or error if no arbitrage opportunity
pub fn find_optimal_amount_dlmm_to_amm_v2<'info>(
    program_1: &Box<dyn ProgramMeta + 'info>,
    program_2: &Box<dyn ProgramMeta + 'info>,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    max_amount_in: u64,
) -> Result<u64> {
    let dlmm = extract_dlmm(program_1)?;
    let pump = extract_pump(program_2)?;

    // Get DLMM price for input_mint → middle_mint direction
    let (dlmm_price, dlmm_inverse_price) = dlmm.get_prices()?;

    // DLMM rate: how much middle_mint we get per input_mint
    let dlmm_price_for_input = if input_mint == dlmm.base_token_pk {
        // input_mint is DLMM base, we're swapping base → quote
        dlmm_price
    } else {
        // input_mint is DLMM quote, we're swapping quote → base
        dlmm_inverse_price
    };

    let dlmm_fee = 1.0 - dlmm.fee_rate;
    let dlmm_max_output = dlmm.get_max_amount_out(input_mint)?;
    let dlmm_max_input = dlmm.get_max_amount_in(input_mint)?;

    #[cfg(any(test, feature = "debug"))]
    {
        debug_eprintln!(
            "DLMM: SOL IN ={}, TOKEN OUT={}",
            dlmm_max_input as f64 / 1_000_000_000.0, dlmm_max_output as f64 / 1_000_000.0
        );
        debug_eprintln!(
            "DLMM: price={}, fee_factor={}, max_output={}",
            dlmm_price_for_input, dlmm_fee, dlmm_max_output
        );
    }

    let (base_vault_amount, quote_vault_amount) = pump.get_vault_amounts()?;
    let (cp_reserve_in, cp_reserve_out) = if middle_mint == pump.base_token_pk {
        (base_vault_amount, quote_vault_amount)
    } else {
        (quote_vault_amount, base_vault_amount)
    };
    let cp_fee = 1.0 - pump.fee;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "CP AMM reserves: in={}, out={}, fee_factor={}",
        cp_reserve_in, cp_reserve_out, cp_fee
    );

    let p = dlmm_price_for_input;
    let f_dlmm = dlmm_fee;
    let x = cp_reserve_in as f64;
    let y = cp_reserve_out as f64;
    let f_cp = cp_fee;

    // DLMM multiplier: how much middle token per input token
    let m = p * f_dlmm;

    // Combined effective rate: M * (y/x) * f_cp
    // Arbitrage exists when rate > 1
    let effective_rate = m * (y / x) * f_cp;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "DLMM price={}, fee={}, CP reserves: x={}, y={}, fee={}, effective_rate={}",
        p, f_dlmm, x, y, f_cp, effective_rate
    );

    if effective_rate <= 1.0 {
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!("No arbitrage: effective_rate {} <= 1.0", effective_rate);
        // return Err(error!(SolarBError::NoProfitFound));
    }

    // Optimal middle token input to CP: (sqrt(y · f_cp · x · M) - x) / f_cp
    // But we need to account for the effective rate through DLMM
    // Actually, let's derive properly:
    // Profit = y * mid * f_cp / (x + mid * f_cp) - input
    // Where mid = input * M
    // Profit = y * input * M * f_cp / (x + input * M * f_cp) - input
    // d(Profit)/d(input) = y * M * f_cp * x / (x + input * M * f_cp)^2 - 1 = 0
    // (x + input * M * f_cp)^2 = y * M * f_cp * x
    // x + input * M * f_cp = sqrt(y * M * f_cp * x)
    // input = (sqrt(y * M * f_cp * x) - x) / (M * f_cp)

    let sqrt_term = (y * m * f_cp * x).sqrt();
    let optimal_input = (sqrt_term - x) / (m * f_cp);

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "Optimal calculation: sqrt_term={}, optimal_input={}",
        sqrt_term, optimal_input
    );

    if optimal_input <= 0.0 {
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!("No profit: optimal_input {} <= 0", optimal_input);
        // return Err(error!(SolarBError::NoProfitFound));
    }

    // Check DLMM constraint: DLMM output must not exceed max
    let dlmm_output_at_optimal = optimal_input * m;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "DLMM output at optimal: {}, DLMM max output: {}",
        dlmm_output_at_optimal, dlmm_max_output
    );

    let constrained_optimal = if dlmm_output_at_optimal > dlmm_max_output as f64 {
        // Reduce input so DLMM output equals max
        let constrained = dlmm_max_output as f64 / m;
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!(
            "Constrained by DLMM capacity: {} -> {}",
            optimal_input, constrained
        );
        constrained
    } else {
        optimal_input
    };

    let final_amount = (constrained_optimal as u64).min(max_amount_in);
    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("Final optimal amount: {}", final_amount);

    Ok(final_amount)
}

// =============================================================================
// CONCENTRATED LIQUIDITY AMM (MeteoraDammV2) FORMULAS
// =============================================================================
//
// MeteoraDammV2 uses Uniswap v3 style concentrated liquidity:
// - Pool state: liquidity (L) and sqrt_price (√P)
// - Virtual reserves: x_virtual = L/√P, y_virtual = L·√P
// - Swap math: Δy = L·(√P_old - √P_new) for X→Y swap
//
// The virtual reserves behave like constant product within the active range,
// so we can use the CP formula with virtual reserves for optimization.
// =============================================================================

/// Optimal amount for Concentrated Liquidity AMM → DLMM arbitrage
///
/// # Path
/// input → [CL AMM] → middle → [DLMM] → output
///
/// # Math
/// CL AMM uses virtual reserves: x = L/√P, y = L·√P
/// The swap follows constant product within the active range.
///
/// # Arguments
/// * `liquidity` - CL AMM liquidity (L)
/// * `sqrt_price` - CL AMM sqrt price (√P), Q64.64 fixed point
/// * `cl_fee` - CL AMM fee factor (e.g., 0.9975 for 0.25% fee)
/// * `input_is_base` - true if input is base token (token A)
/// * `dlmm_price` - DLMM bin price (middle token → output token)
/// * `dlmm_fee` - DLMM fee factor
/// * `dlmm_max_input` - Max tokens DLMM can accept in current bin
/// * `max_amount_in` - User's maximum input constraint
///
/// # Returns
/// Optimal input amount, or error if no arbitrage opportunity
pub fn find_optimal_amount_damm2_to_dlmm_v2<'info>(
    program_1: &Box<dyn ProgramMeta + 'info>,
    program_2: &Box<dyn ProgramMeta + 'info>,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    max_amount_in: u64,
) -> Result<u64> {
    let dlmm = extract_dlmm(program_1)?;
    let damm = extract_damm2(program_2)?;
    // Get DLMM price for middle_mint → input_mint direction
    let (dlmm_price, dlmm_inverse_price) = dlmm.get_prices()?;

    // DLMM rate: how much input_mint we get per middle_mint
    let dlmm_price_for_middle = if middle_mint == dlmm.base_token_pk {
        // middle_mint is DLMM base, we're swapping base → quote
        dlmm_price
    } else {
        // middle_mint is DLMM quote, we're swapping quote → base
        dlmm_inverse_price
    };

    let dlmm_fee = 1.0 - dlmm.fee_rate;
    let dlmm_max_input: u64 = dlmm.get_max_amount_in(middle_mint)?;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "DLMM: price={}, fee_factor={}, max_input={}",
        dlmm_price_for_middle, dlmm_fee, dlmm_max_input
    );
    // Concentrated Liquidity AMM - use liquidity/sqrt_price formula
    let liquidity = damm.pool.liquidity;
    let sqrt_price = damm.pool.sqrt_price;
    let cl_fee = 0.9975; // MeteoraDammV2 uses 0.25% fee typically
    let input_is_base = input_mint == damm.base_token_pk;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "CL AMM: L={}, √P={}, input_is_base={}",
        liquidity, sqrt_price, input_is_base
    );

    // Convert sqrt_price from Q64.64 fixed point to f64
    let sqrt_price = damm.pool.sqrt_price as f64 / (1u128 << 64) as f64;
    let l = liquidity as f64;
    let f_cl = cl_fee;
    let p = dlmm_price;
    let f_dlmm = dlmm_fee;

    // Calculate virtual reserves
    // x_virtual = L / √P (base token)
    // y_virtual = L · √P (quote token)
    let x_virtual = l / sqrt_price;
    let y_virtual = l * sqrt_price;

    #[cfg(any(test, feature = "debug"))]
    {
        debug_eprintln!(
            "CL AMM: L={}, √P={}, virtual_x={}, virtual_y={}, fee={}",
            l, sqrt_price, x_virtual, y_virtual, f_cl
        );
        debug_eprintln!(
            "DLMM: price={}, fee={}, max_input={}",
            p, f_dlmm, dlmm_max_input
        );
    }

    // Based on input direction, determine which virtual reserve is in/out
    let (cp_reserve_in, cp_reserve_out) = if input_is_base {
        // Input is base (X), output is quote (Y)
        (x_virtual, y_virtual)
    } else {
        // Input is quote (Y), output is base (X)
        (y_virtual, x_virtual)
    };

    // Use CP formula with virtual reserves
    // effective_rate = (y/x) * f_cl * P * f_dlmm
    let effective_rate = (cp_reserve_out / cp_reserve_in) * f_cl * p * f_dlmm;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "CL virtual reserves: in={}, out={}, effective_rate={}",
        cp_reserve_in, cp_reserve_out, effective_rate
    );

    if effective_rate <= 1.0 {
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!("No arbitrage: effective_rate {} <= 1.0", effective_rate);
    }

    // Optimal input: (sqrt(y · P · f_dlmm · f_cl · x) - x) / f_cl
    let sqrt_term = (cp_reserve_out * p * f_dlmm * f_cl * cp_reserve_in).sqrt();
    let optimal = (sqrt_term - cp_reserve_in) / f_cl;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "Optimal calculation: sqrt_term={}, optimal={}",
        sqrt_term, optimal
    );

    if optimal <= 0.0 {
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!("No profit: optimal {} <= 0", optimal);
    }

    // Check DLMM constraint
    let cl_output_at_optimal = cp_reserve_out * optimal * f_cl / (cp_reserve_in + optimal * f_cl);

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "CL output at optimal: {}, DLMM max input: {}",
        cl_output_at_optimal, dlmm_max_input
    );

    let constrained_optimal = if cl_output_at_optimal > dlmm_max_input as f64 {
        let max_mid = dlmm_max_input as f64;
        if cp_reserve_out <= max_mid {
            #[cfg(any(test, feature = "debug"))]
            debug_eprintln!(
                "No profit: reserve_out {} <= max_mid {}",
                cp_reserve_out, max_mid
            );
        }
        let constrained = max_mid * cp_reserve_in / (f_cl * (cp_reserve_out - max_mid));
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!(
            "Constrained by DLMM capacity: {} -> {}",
            optimal, constrained
        );
        constrained
    } else {
        optimal
    };

    let final_amount = (constrained_optimal as u64).min(max_amount_in);
    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("Final optimal amount (CL→DLMM): {}", final_amount);

    Ok(final_amount)
}

/// Optimal amount for DLMM → Concentrated Liquidity AMM arbitrage
///
/// # Path
/// input → [DLMM] → middle → [CL AMM] → output
///
/// # Math
/// DLMM output: mid = input · P · f_dlmm
/// CL AMM uses virtual reserves for constant product math
///
/// # Arguments
/// * `dlmm_price` - DLMM bin price (input token → middle token)
/// * `dlmm_fee` - DLMM fee factor
/// * `dlmm_max_output` - Max tokens DLMM can output in current bin
/// * `liquidity` - CL AMM liquidity (L)
/// * `sqrt_price` - CL AMM sqrt price (√P), Q64.64 fixed point
/// * `cl_fee` - CL AMM fee factor
/// * `middle_is_base` - true if middle token is CL AMM base token
/// * `max_amount_in` - User's maximum input constraint
///
/// # Returns
/// Optimal input amount, or error if no arbitrage opportunity
pub fn find_optimal_amount_dlmm_to_damm2_v2<'info>(
    program_1: &Box<dyn ProgramMeta + 'info>,
    program_2: &Box<dyn ProgramMeta + 'info>,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    max_amount_in: u64,
) -> Result<u64> {
    let dlmm = extract_dlmm(program_1)?;
    let damm = extract_damm2(program_2)?;
    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("AMM Type: Concentrated Liquidity (MeteoraDammV2)");
    let liquidity = damm.pool.liquidity;
    let sqrt_price = damm.pool.sqrt_price;
    let cl_fee = 0.9975; // MeteoraDammV2 uses 0.25% fee typically
    let middle_is_base = middle_mint == damm.base_token_pk;
    // Get DLMM price for input_mint → middle_mint direction
    let (dlmm_price, dlmm_inverse_price) = dlmm.get_prices()?;

    // DLMM rate: how much middle_mint we get per input_mint
    let dlmm_price_for_input = if input_mint == dlmm.base_token_pk {
        // input_mint is DLMM base, we're swapping base → quote
        dlmm_price
    } else {
        // input_mint is DLMM quote, we're swapping quote → base
        dlmm_inverse_price
    };

    let dlmm_fee = 1.0 - dlmm.fee_rate;
    let dlmm_max_output = dlmm.get_max_amount_out(input_mint)?;

    // Convert sqrt_price from Q64.64 fixed point to f64
    let sqrt_price = sqrt_price as f64 / (1u128 << 64) as f64;
    let l = liquidity as f64;
    let p = dlmm_price;
    let f_dlmm = dlmm_fee;
    let f_cl = cl_fee;

    // DLMM multiplier: how much middle token per input token
    let m = p * f_dlmm;

    // Calculate virtual reserves
    let x_virtual = l / sqrt_price;
    let y_virtual = l * sqrt_price;

    #[cfg(any(test, feature = "debug"))]
    {
        debug_eprintln!(
            "DLMM: price={}, fee={}, max_output={}, multiplier={}",
            p, f_dlmm, dlmm_max_output, m
        );
        debug_eprintln!(
            "CL AMM: L={}, √P={}, virtual_x={}, virtual_y={}, fee={}",
            l, sqrt_price, x_virtual, y_virtual, f_cl
        );
    }

    // Based on middle token direction in CL AMM
    let (cp_reserve_in, cp_reserve_out) = if middle_is_base {
        // Middle token is base (X), output is quote (Y)
        (x_virtual, y_virtual)
    } else {
        // Middle token is quote (Y), output is base (X)
        (y_virtual, x_virtual)
    };

    // Combined effective rate: M * (y/x) * f_cl
    let effective_rate = m * (cp_reserve_out / cp_reserve_in) * f_cl;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "CL virtual reserves: in={}, out={}, effective_rate={}",
        cp_reserve_in, cp_reserve_out, effective_rate
    );

    if effective_rate <= 1.0 {
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!("No arbitrage: effective_rate {} <= 1.0", effective_rate);
    }

    // Optimal input derivation (same as DLMM→CP but with virtual reserves):
    // optimal_input = (sqrt(y * M * f_cl * x) - x) / (M * f_cl)
    let sqrt_term = (cp_reserve_out * m * f_cl * cp_reserve_in).sqrt();
    let optimal_input = (sqrt_term - cp_reserve_in) / (m * f_cl);

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "Optimal calculation: sqrt_term={}, optimal_input={}",
        sqrt_term, optimal_input
    );

    if optimal_input <= 0.0 {
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!("No profit: optimal_input {} <= 0", optimal_input);
    }

    // Check DLMM constraint: DLMM output must not exceed max
    let dlmm_output_at_optimal = optimal_input * m;

    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!(
        "DLMM output at optimal: {}, DLMM max output: {}",
        dlmm_output_at_optimal, dlmm_max_output
    );

    let constrained_optimal = if dlmm_output_at_optimal > dlmm_max_output as f64 {
        let constrained = dlmm_max_output as f64 / m;
        #[cfg(any(test, feature = "debug"))]
        debug_eprintln!(
            "Constrained by DLMM capacity: {} -> {}",
            optimal_input, constrained
        );
        constrained
    } else {
        optimal_input
    };

    let final_amount = (constrained_optimal as u64).min(max_amount_in);
    #[cfg(any(test, feature = "debug"))]
    debug_eprintln!("Final optimal amount (DLMM→CL): {}", final_amount);

    Ok(final_amount)
}

// Find optimal amount for AMM → DLMM arbitrage path (legacy, kept for reference)
// Uses profit maximization formula instead of price matching
// Supports both Constant Product (PumpAmm) and Concentrated Liquidity (MeteoraDammV2) AMMs
// (This block is intentionally commented out; kept for documentation only.)
//
// /// Same signature as the original find_optimal_amount_in_amm_to_dlmm
// pub fn find_optimal_amount_in_amm_to_dlmm<'info>(
//     arbitrage_path: &mut ArbitragePath,
//     amm: &ProgramInstance<'info>,
//     dlmm: &MeteoraDlmm<'info>,
//     input_mint: Pubkey,
//     accounts: &[AccountInfo<'info>],
//     config: &mut BotConfig<'info>,
// ) -> Result<u64> {
//     debug_eprintln!("");
//     debug_eprintln!("");
//     debug_eprintln!("");
//     debug_eprintln!("========== SIMULATE NEW AMM -> DLMM ==========");

//     let middle_mint: Pubkey = {
//         let edges = &arbitrage_path.edges;
//         edges[0].right.mint_account
//     };
//     let clock = config.clock.clone();

//     // Get DLMM price for middle_mint → input_mint direction
//     let (dlmm_price, dlmm_inverse_price) = dlmm.get_prices()?;

//     // DLMM rate: how much input_mint we get per middle_mint
//     let dlmm_price_for_middle = if middle_mint == dlmm.base_token_pk {
//         // middle_mint is DLMM base, we're swapping base → quote
//         dlmm_price
//     } else {
//         // middle_mint is DLMM quote, we're swapping quote → base
//         dlmm_inverse_price
//     };

//     let dlmm_fee = 1.0 - dlmm.fee_rate;
//     let dlmm_max_input = dlmm.get_max_amount_in(middle_mint)?;

//     debug_eprintln!(
//         "DLMM: price={}, fee_factor={}, max_input={}",
//         dlmm_price_for_middle, dlmm_fee, dlmm_max_input
//     );

//     // Detect AMM type and calculate optimal amount accordingly
//     let optimal_sol_in = match amm {
//         ProgramInstance::PumpAmm(pump) => {
//             // Constant Product AMM - use reserves-based formula
//             debug_eprintln!("AMM Type: Constant Product (PumpAmm)");
//             let (base_vault_amount, quote_vault_amount) = pump.get_vault_amounts()?;
//             let (cp_reserve_in, cp_reserve_out) = if input_mint == pump.base_token_pk {
//                 (base_vault_amount, quote_vault_amount)
//             } else {
//                 (quote_vault_amount, base_vault_amount)
//             };
//             let cp_fee = 1.0 - pump.fee;

//             debug_eprintln!(
//                 "CP AMM reserves: in={}, out={}, fee_factor={}",
//                 cp_reserve_in, cp_reserve_out, cp_fee
//             );

//             find_optimal_amount_amm_to_dlmm(
//                 cp_reserve_in,
//                 cp_reserve_out,
//                 cp_fee,
//                 dlmm_price_for_middle,
//                 dlmm_fee,
//                 dlmm_max_input,
//                 config.max_amount_in,
//             )?
//         }
//         ProgramInstance::MeteoraDammV2(damm) => {
//             // Concentrated Liquidity AMM - use liquidity/sqrt_price formula
//             debug_eprintln!("AMM Type: Concentrated Liquidity (MeteoraDammV2)");
//             let liquidity = damm.pool.liquidity;
//             let sqrt_price = damm.pool.sqrt_price;
//             let cl_fee = 0.9975; // MeteoraDammV2 uses 0.25% fee typically
//             let input_is_base = input_mint == damm.base_token_pk;

//             debug_eprintln!(
//                 "CL AMM: L={}, √P={}, input_is_base={}",
//                 liquidity, sqrt_price, input_is_base
//             );

//             find_optimal_amount_damm2_to_dlmm(
//                 damm,
//                 dlmm,
//                 input_mint,
//                 middle_mint,
//                 config.max_amount_in,
//             )?
//         }
//         ProgramInstance::MeteoraDlmm(_) => {
//             // DLMM → DLMM not handled here
//             return Err(error!(SolarBError::InvalidProgramType));
//         }
//     };

//     // Simulate the actual swap to get token amounts
//     let token_out_amm = amm.swap_base_in(accounts, input_mint, optimal_sol_in, clock.clone())?;

//     debug_eprintln!(
//         "AMM: {} input -> {} middle",
//         optimal_sol_in as f64 / 1_000_000_000.0,
//         token_out_amm as f64 / 1_000_000.0
//     );

//     // Simulate DLMM swap to calculate profit
//     let final_output = dlmm.swap_base_in(accounts, middle_mint, token_out_amm, clock.clone())?;
//     let profit = final_output as i128 - optimal_sol_in as i128;

//     debug_eprintln!(
//         "DLMM: {} middle -> {} output",
//         token_out_amm as f64 / 1_000_000.0,
//         final_output as f64 / 1_000_000_000.0
//     );

//     if profit > 0 {
//         let profit_sol = profit as f64 / 1_000_000_000.0;
//         let pct = (profit as f64 / optimal_sol_in as f64) * 100.0;
//         debug_eprintln!("✅ PROFIT {profit} ({profit_sol} SOL) {pct:.4}%");
//         #[cfg(test)]
//         write_results_to_file(&[Some(arbitrage_path.clone())]);
//     } else {
//         debug_eprintln!("❌ NO PROFIT");
//     }

//     // Update arbitrage path
//     arbitrage_path.profit = profit;
//     arbitrage_path.start_amount = optimal_sol_in;

//     // Update edges with actual amounts
//     {
//         let edges = &mut arbitrage_path.edges;
//         edges[0].amount_in = optimal_sol_in;
//         edges[0].amount_out = 0;
//         edges[1].amount_in = token_out_amm;
//         edges[1].amount_out = 0;
//     }

//     debug_eprintln!("================================================");

//     Ok(optimal_sol_in)
// }

// Find optimal amount for DLMM → AMM arbitrage path (legacy, kept for reference)
// Uses profit maximization formula instead of price matching
// Supports both Constant Product (PumpAmm) and Concentrated Liquidity (MeteoraDammV2) AMMs
// (This block is intentionally commented out; kept for documentation only.)
//
// /// Same signature as the original find_optimal_amount_dlmm_to_amm
// pub fn find_optimal_amount_dlmm_to_amm2_v2<'info>(
//     arbitrage_path: &mut ArbitragePath,
//     dlmm: &MeteoraDlmm<'info>,
//     amm: &ProgramInstance<'info>,
//     input_mint: Pubkey,
//     accounts: &[AccountInfo<'info>],
//     config: &mut BotConfig<'info>,
// ) -> Result<u64> {
//     debug_eprintln!("");
//     debug_eprintln!("");
//     debug_eprintln!("");
//     debug_eprintln!("========== SIMULATE NEW DLMM -> AMM ==========");

//     let middle_mint: Pubkey = {
//         let edges = &arbitrage_path.edges;
//         edges[0].right.mint_account
//     };
//     let clock = config.clock.clone();

//     // Get DLMM price for input_mint → middle_mint direction
//     let (dlmm_price, dlmm_inverse_price) = dlmm.get_prices()?;

//     // DLMM rate: how much middle_mint we get per input_mint
//     let dlmm_price_for_input = if input_mint == dlmm.base_token_pk {
//         // input_mint is DLMM base, we're swapping base → quote
//         dlmm_price
//     } else {
//         // input_mint is DLMM quote, we're swapping quote → base
//         dlmm_inverse_price
//     };

//     let dlmm_fee = 1.0 - dlmm.fee_rate;
//     let dlmm_max_output = dlmm.get_max_amount_out(input_mint)?;

//     debug_eprintln!(
//         "DLMM: price={}, fee_factor={}, max_output={}",
//         dlmm_price_for_input, dlmm_fee, dlmm_max_output
//     );

//     // Detect AMM type and calculate optimal amount accordingly
//     let optimal_sol_in = match amm {
//         ProgramInstance::PumpAmm(pump) => {
//             // Constant Product AMM - use reserves-based formula
//             debug_eprintln!("AMM Type: Constant Product (PumpAmm)");
//             let (base_vault_amount, quote_vault_amount) = pump.get_vault_amounts()?;
//             let (cp_reserve_in, cp_reserve_out) = if middle_mint == pump.base_token_pk {
//                 (base_vault_amount, quote_vault_amount)
//             } else {
//                 (quote_vault_amount, base_vault_amount)
//             };
//             let cp_fee = 1.0 - pump.fee;

//             debug_eprintln!(
//                 "CP AMM reserves: in={}, out={}, fee_factor={}",
//                 cp_reserve_in, cp_reserve_out, cp_fee
//             );

//             find_optimal_amount_dlmm_to_amm(
//                 pump,
//                 dlmm,
//                 input_mint,
//                 middle_mint,
//                 config.max_amount_in,
//             )?
//         }
//         ProgramInstance::MeteoraDammV2(damm) => {
//             // Concentrated Liquidity AMM - use liquidity/sqrt_price formula
//             debug_eprintln!("AMM Type: Concentrated Liquidity (MeteoraDammV2)");
//             let liquidity = damm.pool.liquidity;
//             let sqrt_price = damm.pool.sqrt_price;
//             let cl_fee = 0.9975; // MeteoraDammV2 uses 0.25% fee typically
//             let middle_is_base = middle_mint == damm.base_token_pk;

//             debug_eprintln!(
//                 "CL AMM: L={}, √P={}, middle_is_base={}",
//                 liquidity, sqrt_price, middle_is_base
//             );

//             find_optimal_amount_dlmm_to_damm2(
//                 dlmm,
//                 damm,
//                 input_mint,
//                 middle_mint,
//                 config.max_amount_in,
//             )?
//         }
//         ProgramInstance::MeteoraDlmm(_) => {
//             // DLMM → DLMM not handled here
//             return Err(error!(SolarBError::InvalidProgramType));
//         }
//     };

//     // Simulate the actual swaps
//     let token_out_dlmm = dlmm.swap_base_in(accounts, input_mint, optimal_sol_in, clock.clone())?;

//     debug_eprintln!(
//         "DLMM: {} input -> {} middle",
//         optimal_sol_in as f64 / 1_000_000_000.0,
//         token_out_dlmm as f64 / 1_000_000.0
//     );

//     let final_output = amm.swap_base_in(accounts, middle_mint, token_out_dlmm, clock.clone())?;
//     let profit = final_output as i128 - optimal_sol_in as i128;

//     debug_eprintln!(
//         "AMM: {} middle -> {} output",
//         token_out_dlmm as f64 / 1_000_000.0,
//         final_output as f64 / 1_000_000_000.0
//     );

//     if profit > 0 {
//         let profit_sol = profit as f64 / 1_000_000_000.0;
//         let pct = (profit as f64 / optimal_sol_in as f64) * 100.0;
//         debug_eprintln!("✅ PROFIT {profit} ({profit_sol} SOL) {pct:.4}%");
//         #[cfg(test)]
//         write_results_to_file(&[Some(arbitrage_path.clone())]);
//     } else {
//         debug_eprintln!("❌ NO PROFIT");
//     }

//     // Update arbitrage path
//     arbitrage_path.profit = profit;
//     arbitrage_path.start_amount = optimal_sol_in;

//     // Update edges with actual amounts
//     {
//         let edges = &mut arbitrage_path.edges;
//         edges[0].amount_in = optimal_sol_in;
//         edges[0].amount_out = 0;
//         edges[1].amount_in = token_out_dlmm;
//         edges[1].amount_out = 0;
//     }

//     debug_eprintln!("================================================");

//     Ok(optimal_sol_in)
// }

// /// Find optimal amount in for DLMM -> DLMM arbitrage path
// ///
// /// For two DLMMs, trade based on price difference
pub fn find_optimal_amount_in_dlmm_to_dlmm<'info>(
    program_1: &ProgramInstance<'info>,
    program_2: &ProgramInstance<'info>,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    max_amount_in: u64,
) -> Result<u64> {


    let dlmm1 = extract_dlmm(program_1)?;
    let dlmm2 = extract_dlmm(program_2)?;

    // Get prices from both DLMMs
    let (p1, _inv_p1) = dlmm1.get_prices()?;
    let (_p2, inv_p2) = dlmm2.get_prices()?;

    // Check if arbitrage is profitable
    // Buy from dlmm1 at p1, sell to dlmm2 at inv_p2
    if p1 >= inv_p2 {
        // return Err(error!(SolarBError::NoProfitFound));
    }

    let max_amount_in = max_amount_in;

    // Get max amounts from both pools
    let max_out_dlmm1 = dlmm1.get_max_amount_out(input_mint)?;
    let max_in_dlmm1 = dlmm1.get_max_amount_in(input_mint)?;
    let max_in_dlmm2 = dlmm2.get_max_amount_in(middle_mint)?;

    // Use the smallest limit
    let optimal_amount = max_in_dlmm1.min(max_out_dlmm1).min(max_in_dlmm2);
    let final_amount_in = optimal_amount.min(max_amount_in);

    if final_amount_in == 0 {
        // return Err(error!(SolarBError::NoProfitFound));
    }

    Ok(final_amount_in)
}
