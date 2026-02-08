// use anchor_lang::prelude::*;
// use anchor_lang::solana_program::pubkey::Pubkey;

// use crate::arbitrage::algo_2::arbitrage_path::ArbitragePath;
// use crate::programs::meteora_dlmm::MeteoraDlmm;
// use crate::programs::programs::{ProgramInstance, ProgramMeta};
// use crate::utils::bot_config::BotConfig;
// // #[cfg(test)]
// // use crate::utils::test_utils::write_results_to_file;
// use crate::utils::token::calculate_transfer_fee_included_amount;
// use crate::arbitrage::algo_2::utils::*;

// pub fn simulate_dlmm_to_amm<'info>(
//     program_1: &ProgramInstance<'info>,
//     program_2: &ProgramInstance<'info>,
//     input_mint: Pubkey,
//     middle_mint: Pubkey,
//     sol_in: u64,
//     accounts: &[AccountInfo<'info>],
//     config: &mut BotConfig,
// ) -> Result<i128> {
//     eprintln!("");
//     eprintln!("");
//     eprintln!("");


//     eprintln!("========== SIMULATE V1 DLMM -> AMM ==========");
//     let token_out_from_dlmm =
//         program_1.swap_base_in(accounts, input_mint, sol_in, config.clock.clone())?;

//     let sol_out_from_amm = program_2.swap_base_in(
//         accounts,
//         middle_mint,
//         token_out_from_dlmm,
//         config.clock.clone(),
//     )?;

//     eprintln!(
//         "DLMM: SOL {} -> TOKEN: {}",
//         sol_in as f64 / 1_000_000_000.0,
//         token_out_from_dlmm as f64 / 1_000_000.0
//     );

//     eprintln!(
//         "AMM: TOKEN {} -> SOL: {}",
//         token_out_from_dlmm as f64 / 1_000_000.0,
//         sol_out_from_amm as f64 / 1_000_000_000.0
//     );

//     let profit = sol_out_from_amm as i128 - sol_in as i128;

//     eprintln!(
//         "PROFIT: {} ({} %)",
//         profit as f64 / 1_000_000_000.0 as f64,
//         (profit as f64 / sol_in as f64 * 100.0) as f64
//     );

//     eprintln!("================================================");

//     Ok(profit)
// }

// pub fn simulate_amm_to_dlmm<'info>(
//     program_1: &ProgramInstance<'info>,
//     program_2: &ProgramInstance<'info>,
//     input_mint: Pubkey,
//     middle_mint: Pubkey,
//     sol_in: u64,
//     accounts: &[AccountInfo<'info>],
//     config: &mut BotConfig,
// ) -> Result<i128> {
//     eprintln!("");
//     eprintln!("========== SIMULATE AMM -> DLMM ==========");
//     let token_out = program_1.swap_base_in(accounts, input_mint, sol_in, config.clock.clone())?;

//     let sol_out = program_2.swap_base_in(accounts, middle_mint, token_out, config.clock.clone())?;

//     eprintln!(
//         "AMM: SOL {} -> TOKEN: {}",
//         sol_in as f64 / 1_000_000_000.0,
//         token_out as f64 / 1_000_000.0
//     );

//     eprintln!(
//         "DLMM: TOKEN {} -> SOL: {}",
//         token_out as f64 / 1_000_000.0,
//         sol_out as f64 / 1_000_000_000.0
//     );

//     let profit = sol_out as i128 - sol_in as i128;

//     eprintln!(
//         "PROFIT: {} ({} %)",
//         profit as f64 / 1_000_000_000.0 as f64,
//         (profit as f64 / sol_in as f64 * 100.0) as f64
//     );

//     Ok(profit)
// }

// /// Find optimal amount in for DLMM -> AMM arbitrage path
// ///
// /// Uses price comparison to determine optimal trade size.
// /// For DLMM -> AMM: Buy from DLMM (at bin_price) and sell to AMM
// /// Generalized to work with any AMM type (PumpAmm, MeteoraDammV2, etc.)
// pub fn find_optimal_amount_dlmm_to_amm<'info>(
//     program_1: &ProgramInstance<'info>,
//     program_2: &ProgramInstance<'info>,
//     input_mint: Pubkey,
//     middle_mint: Pubkey,
//     accounts: &[AccountInfo<'info>],
//     config: &mut BotConfig,
// ) -> Result<u64> {
//     // Direction DLMM -> AMM
//     // Leg 1: input_mint -> middle_mint (via DLMM)
//     // Leg 2: middle_mint -> input_mint (via AMM)
//     eprintln!("");
//     eprintln!("");
//     eprintln!("");
//     eprintln!("========== SIMULATE V1 DLMM -> AMM ==========");
//     let dlmm = extract_dlmm(program_1)?;
//     let amm = program_2;
//     let amm_base = amm.get_base_token()?;

//     // let (input_mint_account, middle_mint_account) = if input_mint == *config.mint_1.mint.key {
//     //     (&config.mint_1.mint, &config.mint_2.mint)
//     // } else {
//     //     (&config.mint_2.mint, &config.mint_1.mint)
//     // };

//     // Get prices from DLMM
//     // price = quote_token per base_token (base -> quote)
//     // inverse_price = base_token per quote_token (quote -> base)
//     let (dlmm_price, dlmm_inverse_price) = dlmm.get_prices()?;

//     // Determine DLMM's exchange rate for input_mint -> middle_mint
//     // This is how much middle_mint we get per input_mint from DLMM
//     let dlmm_rate = if input_mint == dlmm.base_token_pk {
//         // DLMM swap: base -> quote (input_mint is base, middle_mint is quote)
//         // rate = price (quote per base)
//         dlmm_price
//     } else {
//         // DLMM swap: quote -> base (input_mint is quote, middle_mint is base)
//         // rate = inverse_price (base per quote)
//         dlmm_inverse_price
//     };

//     // Calculate target price for AMM (always in base->quote terms)
//     // Optimal condition: DLMM_rate × AMM_rate = 1 (at margin)
//     // AMM receives middle_mint and outputs input_mint
//     // So: AMM_rate (input_mint per middle_mint) = 1 / DLMM_rate
//     let target_price = if middle_mint == amm_base {
//         // middle_mint is AMM base, input_mint is AMM quote
//         // AMM price = quote/base = input_mint/middle_mint
//         // We want input_mint/middle_mint = 1/dlmm_rate
//         // So target_price = 1/dlmm_rate
//         1.0 / dlmm_rate
//     } else {
//         // middle_mint is AMM quote, input_mint is AMM base
//         // AMM price = quote/base = middle_mint/input_mint
//         // AMM rate (input_mint per middle_mint) = 1/price
//         // We want 1/price = 1/dlmm_rate
//         // So target_price = dlmm_rate
//         dlmm_rate
//     };

//     eprintln!(
//         "DLMM rate (input->middle): {}, AMM target price (base->quote): {}",
//         dlmm_rate, target_price
//     );

//     // Optimal token input to AMM (middle_mint amount)
//     let optimal_token_in_amm = amm.calculate_optimal_amount_in(middle_mint, target_price)?;

//     simulate_with_prices(
//         amm,
//         dlmm,
//         input_mint,
//         middle_mint,
//         optimal_token_in_amm,
//         false,
//     )?;

//     eprintln!(
//         "AMM: OPTIMAL TOKEN IN: {}",
//         optimal_token_in_amm as f64 / 1_000_000.0
//     );

//     if optimal_token_in_amm <= 0 {
//         // return Err(error!(SolarBError::NoProfitFound));
//     }

//     let transfer_fee_token =
//         calculate_transfer_fee_included_amount(middle_mint_account, optimal_token_in_amm)?;
//     let optimal_amount_in_amm_with_fee = transfer_fee_token.amount;

//     eprintln!(
//         "AMM: OPTIMAL TOKEN IN WITH FEES: {}",
//         optimal_amount_in_amm_with_fee as f64 / 1_000_000.0
//     );

//     // How much middle_mint can DLMM give us at most?
//     let max_sol_in_dlmm = dlmm.get_max_amount_in(input_mint)?;
//     let max_token_out_dlmm = dlmm.get_max_amount_out(input_mint)?;
//     eprintln!(
//         "DLMM: MAX TOKEN IN {} -> MAX TOKEN OUT: {}",
//         max_sol_in_dlmm as f64 / 1_000_000_000.0,
//         max_token_out_dlmm as f64 / 1_000_000.0
//     );

//     // The actual amount we'll put into AMM is the min of both constraints
//     let actual_amount_into_amm: u64 =
//         (optimal_amount_in_amm_with_fee as u64).min(max_token_out_dlmm);
//     eprintln!(
//         "AMM: ACTUAL TOKEN IN: {}",
//         actual_amount_into_amm as f64 / 1_000_000.0
//     );

//     // Now calculate: how much input_mint do we need to give DLMM
//     // to get `actual_amount_into_amm` of middle_mint?
//     let optimal_sol_in = dlmm.swap_base_out(
//         accounts,
//         middle_mint,
//         actual_amount_into_amm,
//         config.clock.clone(),
//     )? as u64;

//     eprintln!(
//         "DLMM: OPTIMAL SOL IN: {}",
//         optimal_sol_in as f64 / 1_000_000_000.0
//     );

//     let transfer_fee_sol =
//         calculate_transfer_fee_included_amount(input_mint_account, optimal_sol_in)?;
//     let optimal_sol_in_with_fees = transfer_fee_sol.amount;

//     // let input_mint_transfer_fee = transfer_fee_config_input.calculate_epoch_fee(clock.epoch, optimal_amount_in);

//     let optimal_amount_in = optimal_sol_in_with_fees.min(config.max_amount_in);

//     let _profit = simulate_dlmm_to_amm(
//         program_1,
//         program_2,
//         input_mint,
//         middle_mint,
//         optimal_amount_in,
//         accounts,
//         config,
//     )?;

//     eprintln!("========== SIMULATION DLMM -> AMM ==========");
//     let token_out_from_dlmm = dlmm.swap_base_in(
//         accounts,
//         input_mint,
//         optimal_amount_in,
//         config.clock.clone(),
//     )?;

//     let sol_out_from_amm = amm.swap_base_in(
//         accounts,
//         middle_mint,
//         token_out_from_dlmm,
//         config.clock.clone(),
//     )?;

//     eprintln!(
//         "DLMM: SOL {} -> TOKEN: {}",
//         optimal_amount_in as f64 / 1_000_000_000.0,
//         token_out_from_dlmm as f64 / 1_000_000.0
//     );

//     eprintln!(
//         "AMM: TOKEN {} -> SOL: {}",
//         token_out_from_dlmm as f64 / 1_000_000.0,
//         sol_out_from_amm as f64 / 1_000_000_000.0
//     );

//     let profit = sol_out_from_amm as i128 - optimal_amount_in as i128;

//     eprintln!(
//         "PROFIT: {} ({} %)",
//         profit as f64 / 1_000_000_000.0 as f64,
//         (profit as f64 / optimal_amount_in as f64 * 100.0) as f64
//     );

//     Ok(optimal_amount_in)
// }

// fn simulate_with_prices<'info>(
//     amm: &ProgramInstance<'info>,
//     dlmm: &MeteoraDlmm<'info>,
//     input_mint: Pubkey,
//     middle_mint: Pubkey,
//     amount_in: u64,
//     amm_to_dlmm: bool,
// ) -> Result<i128> {
//     // Get on-chain prices (base -> quote and quote -> base)
//     let (price_amm, inverse_price_amm) = amm.get_prices()?;
//     let (price_dlmm, inverse_price_dlmm) = dlmm.get_prices()?;

//     let profit = if amm_to_dlmm {
//         // AMM leg uses input_mint
//         let price_amm_dir = if input_mint == amm.get_base_token()? {
//             price_amm
//         } else {
//             inverse_price_amm
//         };
//         // DLMM leg uses middle_mint (output of AMM)
//         let price_dlmm_dir = if middle_mint == dlmm.base_token_pk {
//             price_dlmm
//         } else {
//             inverse_price_dlmm
//         };
//         let token_out_amm = amount_in as f64 * price_amm_dir;
//         let sol_back_from_dlmm = token_out_amm * price_dlmm_dir;
//         (sol_back_from_dlmm - amount_in as f64) as i128
//     } else {
//         // DLMM leg uses input_mint
//         let price_dlmm_dir = if input_mint == dlmm.base_token_pk {
//             price_dlmm
//         } else {
//             inverse_price_dlmm
//         };
//         // AMM leg uses middle_mint (output of DLMM)
//         let price_amm_dir = if middle_mint == amm.get_base_token()? {
//             price_amm
//         } else {
//             inverse_price_amm
//         };
//         // amount_in here is the token amount fed into AMM (middle_mint),
//         // which is the output of the DLMM leg. Compute how much base
//         // the DLMM had to spend to produce that token_out, then the base
//         // we get back from AMM, and take the difference.
//         let token_in_amm = amount_in as f64;
//         // Base spent in DLMM to get token_in_amm
//         let base_spent_dlmm = token_in_amm / price_dlmm_dir;
//         // Base received from AMM when selling those tokens
//         let base_back_from_amm = token_in_amm * price_amm_dir;
//         (base_back_from_amm - base_spent_dlmm) as i128
//     };

//     eprintln!("STRAIGHT PROFIT: {}", profit as f64 / 1_000_000_000.0);

//     Ok(profit)
// }
// /// Find optimal amount in for AMM -> DLMM arbitrage path
// ///
// /// Uses price comparison to determine optimal trade size.
// /// For AMM -> DLMM: Buy from AMM and sell to DLMM (at bin_price)
// /// Generalized to work with any AMM type (PumpAmm, MeteoraDammV2, etc.)
// pub fn find_optimal_amount_in_amm_to_dlmm<'info>(
//     arbitrage_path: &mut ArbitragePath,
//     program_1: &ProgramInstance<'info>,
//     program_2: &ProgramInstance<'info>,
//     input_mint: Pubkey,
//     middle_mint: Pubkey,
//     accounts: &[AccountInfo<'info>],
//     config: &mut BotConfig,
// ) -> Result<u64> {
//     // Direction AMM -> DLMM
//     // Leg 1: input_mint -> middle_mint (via AMM)
//     // Leg 2: middle_mint -> input_mint (via DLMM)
//     eprintln!("========== AMM -> DLMM ==========");
//     let dlmm = extract_dlmm(program_2)?;    
//     let amm = program_1;
//     let amm_base = amm.get_base_token()?;

//     // Get prices from DLMM
//     // price = quote_token per base_token (base -> quote)
//     // inverse_price = base_token per quote_token (quote -> base)
//     let (dlmm_price, dlmm_inverse_price) = dlmm.get_prices()?;

//     // Determine DLMM's exchange rate for middle_mint -> input_mint
//     // This is how much input_mint we get per middle_mint from DLMM
//     let dlmm_rate = if middle_mint == dlmm.base_token_pk {
//         // DLMM swap: base -> quote, rate = price (quote per base)
//         dlmm_price
//     } else {
//         // DLMM swap: quote -> base, rate = inverse_price (base per quote)
//         dlmm_inverse_price
//     };

//     // Calculate target price for AMM (always in base->quote terms for calculate_optimal_amount_in)
//     // Optimal condition: AMM_output_rate × DLMM_rate = 1
//     // So: AMM_output_rate = 1 / DLMM_rate
//     let target_price = if input_mint == amm_base {
//         // Input is AMM base, output is AMM quote (= middle_mint)
//         // AMM_output_rate = AMM_price (quote per base)
//         // Target: AMM_price = 1 / dlmm_rate
//         1.0 / dlmm_rate
//     } else {
//         // Input is AMM quote, output is AMM base (= middle_mint)
//         // AMM_output_rate = 1 / AMM_price (base per quote)
//         // Target: 1 / AMM_price = 1 / dlmm_rate
//         // So: AMM_price = dlmm_rate
//         dlmm_rate
//     };

//     eprintln!(
//         "DLMM rate (middle->input): {}, AMM target price (base->quote): {}",
//         dlmm_rate, target_price
//     );

//     // Calculate optimal input to AMM to move its price to target_price
//     let optimal_sol_in_without_fee = amm.calculate_optimal_amount_in(input_mint, target_price)?;
//     simulate_with_prices(
//         amm,
//         dlmm,
//         input_mint,
//         middle_mint,
//         optimal_sol_in_without_fee,
//         true,
//     )?;

//     // Check MAX SOL IN
//     let optimal_sol_in = optimal_sol_in_without_fee.min(config.max_amount_in);
//     let clock = config.clock.clone();

//     // Simulate: how much middle_mint would we get from AMM?
//     let token_out_amm = amm.swap_base_in(accounts, input_mint, optimal_sol_in, clock.clone())?;

//     eprintln!(
//         "AMM: SOL {} -> TOKEN: {}",
//         optimal_sol_in as f64 / 1_000_000_000.0,
//         token_out_amm as f64 / 1_000_000.0
//     );

//     // How much middle_mint can DLMM accept?
//     // Use dlmm.price (raw u128) instead of normalized f64 price
//     let max_token_in_dlmm = dlmm.get_max_amount_in(middle_mint)?;
//     eprintln!(
//         "DLMM: MAX TOKEN IN: {}",
//         max_token_in_dlmm as f64 / 1_000_000.0
//     );

//     // If AMM output exceeds DLMM capacity, reduce AMM input accordingly
//     let optimal_sol_in = if token_out_amm > max_token_in_dlmm {
//         // Recalculate: how much input_mint gives us exactly max_amount_in_dlmm of middle_mint?
//         let adjusted_sol_in =
//             amm.swap_base_out(accounts, middle_mint, max_token_in_dlmm, clock.clone())?;
//         eprintln!(
//             "DLMM: ADJUSTED SOL IN: {}",
//             adjusted_sol_in as f64 / 1_000_000_000.0
//         );
//         optimal_sol_in
//             .min(adjusted_sol_in)
//             .min(config.max_amount_in) as u64
//     } else {
//         // AMM output fits within DLMM capacity, use optimal amount
//         optimal_sol_in as u64
//     };

//     let max_sol_out_dlmm = dlmm.get_max_amount_out(middle_mint)?;
//     let max_token_out_dlmm = dlmm.get_max_amount_in(middle_mint)?;
//     eprintln!(
//         "DLMM: MAX TOKEN IN: {} -> MAX TOKEN OUT: {}",
//         max_sol_out_dlmm as f64 / 1_000_000_000.0,
//         max_token_out_dlmm as f64 / 1_000_000.0
//     );

//     // Recalculate actual token_out_amm with final optimal_sol_in
//     let final_token_out_amm =
//         amm.swap_base_in(accounts, input_mint, optimal_sol_in, clock.clone())?;

//     let profit = simulate_amm_to_dlmm(
//         program_1,
//         program_2,
//         input_mint,
//         middle_mint,
//         optimal_sol_in,
//         accounts,
//         config,
//     )?;

//     if profit > 0 {
//         let profit_sol = profit as f64 / 1_000_000_000.0;
//         let pct = (profit as f64 / optimal_sol_in as f64) * 100.0;
//         let amount_in_sol = optimal_sol_in as f64 / 1_000_000_000.0;
//         eprintln!("✅ PROFIT {amount_in_sol} SOL -> {profit} ({profit_sol} SOL) {pct:.4}%");
//         // #[cfg(test)]
//         // write_results_to_file(&[Some(arbitrage_path.clone())]);
//     } else {
//         eprintln!("❌ NO PROFIT");
//     }

//     // Update arbitrage path and edges
//     arbitrage_path.profit = profit;
//     arbitrage_path.start_amount = optimal_sol_in;

//     // Update edges with actual amounts (similar to find_optimal_amount_dlmm_to_amm)
//     {
//         let edges = &mut arbitrage_path.edges;
//         edges[0].amount_in = optimal_sol_in;
//         edges[0].amount_out = 0;
//         edges[1].amount_in = final_token_out_amm;
//         edges[1].amount_out = 0;
//     }

//     eprintln!("================================================");

//     Ok(optimal_sol_in)
// }

