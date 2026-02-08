use crate::programs::ProgramMeta;
use crate::utils::token::get_transfer_fee;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use bytemuck;
pub mod damm_v2;

/// Precomputed Q64 scale factor (2^64) for sqrt_price calculations
/// Avoids recomputing `Q64_SCALE` on every call
const Q64_SCALE: f64 = 18446744073709551616.0; // Q64_SCALE
pub use damm_v2::curve::{get_spot_price_a_to_b, get_spot_price_b_to_a};
pub use crate::utils::utils::parse_token_account;
pub use damm_v2::{ActivationType, FeeMode, Pool, TradeDirection};
use std::marker::PhantomData;

pub fn get_current_point(
    activation_type: u8,
    current_slot: u64,
    current_timestamp: u64,
) -> Result<u64> {
    use anchor_lang::prelude::*;
    use damm_v2::ActivationType;

    let activation_type =
        ActivationType::try_from(activation_type).map_err(|_| ProgramError::InvalidAccountData)?;

    let current_point = match activation_type {
        ActivationType::Slot => current_slot,
        ActivationType::Timestamp => current_timestamp,
    };

    Ok(current_point)
}

pub fn get_prices(pool: Pool) -> Result<(f64, f64)> {
    // price : token_A -> token_B (A -> B)
    // inverse_price : token_B -> token_A (B -> A)
    let actual_sqrt_price = pool.sqrt_price as f64 / Q64_SCALE;
    let price_b_to_a_base = actual_sqrt_price * actual_sqrt_price; // token_b / token_a in base units
    let price = 1.0 / price_b_to_a_base; // token_a / token_b in base units
    Ok((price_b_to_a_base as f64, price as f64))
}

#[derive(Clone)]
pub struct MeteoraDammV2<'info> {
    // pub program_id: AccountInfo<'info>,
    // pub pool_id: AccountInfo<'info>,
    // pub base_vault: AccountInfo<'info>,
    // pub quote_vault: AccountInfo<'info>,
    pub pool_id: Pubkey,
    pub base_token_pk: Pubkey,
    pub quote_token_pk: Pubkey,
    // pub pool_authority: AccountInfo<'info>,
    // pub event_authority: AccountInfo<'info>,
    // pub referral_token_account: AccountInfo<'info>,
    pub pool: Pool,
    pub base_vault_amount: u64,
    pub quote_vault_amount: u64,
    pub start_index: usize,
    pub end_index: usize,
    pub price: f64,
    pub inverse_price: f64,
    pub phantom: PhantomData<&'info ()>,
}

impl<'info> ProgramMeta for MeteoraDammV2<'info> {
    fn get_id(&self) -> &Pubkey {
        &Self::PROGRAM_ID
    }

    fn get_pool_id(&self) -> &Pubkey {
        &self.pool_id
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        // price : token_A -> token_B (A -> B)
        // inverse_price : token_B -> token_A (B -> A)
        let actual_sqrt_price = self.pool.sqrt_price as f64 / Q64_SCALE;
        let price_b_to_a_base = actual_sqrt_price * actual_sqrt_price; // token_b / token_a in base units
        let price = 1.0 / price_b_to_a_base; // token_a / token_b in base units
        Ok((price_b_to_a_base as f64, price as f64))
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (&self.base_token_pk, &self.quote_token_pk)
    }

    fn get_max_amounts_in_out(&self, input_mint: Pubkey) -> Result<(u64, u64)> {
        if input_mint == self.base_token_pk {
            Ok((self.base_vault_amount, self.quote_vault_amount))
        } else {
            Ok((self.quote_vault_amount, self.base_vault_amount))
        }
    }

    //     /// Returns (max_in, max_out) for the given input mint using virtual reserves
    // /// and a 99% target of the opposite reserve to avoid the asymptote.
    // fn get_max_amounts_in_out(&self, input_mint: Pubkey) -> Result<(u64, u64)> {
    //     let fee_factor = 0.9975_f64; // adjust if fee differs
    //     let sqrt_price = self.pool.sqrt_price as f64 / Q64_SCALE;

    //     let l = self.pool.liquidity as f64;
    //     let (base_reserve, quote_reserve) = (l / sqrt_price, l * sqrt_price); // virtual reserves

    //     // choose in/out sides based on input mint
    //     let (in_reserve, out_reserve) = if input_mint == self.base_token_pk {
    //         (base_reserve, quote_reserve) // input is base, output is quote
    //     } else {
    //         (quote_reserve, base_reserve) // input is quote, output is base
    //     };

    //     // target 99% of the output reserve to avoid asymptote
    //     let target_out = out_reserve * 0.99;
    //     if target_out <= 0.0 {
    //         return Ok((0, 0));
    //     }

    //     // Solve for dx: dx = (x/f) * ((y/(y - dy)) - 1)
    //     let max_in = (in_reserve / fee_factor) * ((out_reserve / (out_reserve - target_out)) - 1.0);
    //     let max_in = max_in.max(0.0).min(u64::MAX as f64) as u64;
    //     let max_out = target_out as u64; // Fixed: return 99%, not 100%

    //     Ok((max_in, max_out))
    // }

    fn log_accounts<'a>(&self, _accounts: &[AccountInfo<'a>]) -> Result<()> {
        // Note: This method would need accounts parameter to log, but it's only for debugging
        // For now, just log the stored token keys
        msg!(
            "Meteora DAMM v2: base_token={}, quote_token={}",
            self.base_token_pk,
            self.quote_token_pk,
            // self.referral_token_account.key,
        );
        Ok(())
    }

    fn swap_base_in<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        clock: Clock,
    ) -> Result<u64> {
        // Determine trade direction based on input_mint
        let trade_direction = if input_mint == self.base_token_pk {
            TradeDirection::AtoB
        } else {
            TradeDirection::BtoA
        };
        eprintln!("trade_direction: {:?}", trade_direction);
        let current_timestamp = clock.unix_timestamp as u64;
        let current_slot = clock.slot as u64;
    
        let current_point =
            get_current_point(self.pool.activation_type, current_slot, current_timestamp)?;



        let referral_token_account = accounts[self.start_index + Self::REFERRAL_TOKEN_ACCOUNT_IDX];
        let has_referral = !referral_token_account.key.eq(&Pubkey::default()); // TODO: check if this is correct
        let fee_mode: FeeMode =
            FeeMode::get_fee_mode(self.pool.collect_fee_mode, trade_direction, has_referral)?;

        let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
        let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];

        let (token_in_mint, token_out_mint) = if input_mint == self.base_token_pk {
            (base_token, quote_token)
        } else {
            (quote_token, base_token)
        };

        let transfer_fee = get_transfer_fee(token_in_mint, amount_in)?;
        let amount_in_after_fee = amount_in.checked_sub(transfer_fee).unwrap();

        let results = self.pool.get_swap_result_from_exact_input(
            amount_in_after_fee,
            &fee_mode,
            trade_direction,
            current_point,
        )?;

        let transfer_fee_out = get_transfer_fee(token_out_mint, results.output_amount)?;
        let amount_out_after_fee = results.output_amount.checked_sub(transfer_fee_out).unwrap();

        Ok(amount_out_after_fee)
    }

    fn swap_base_out<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        clock: Clock,
    ) -> Result<u64> {
        // Determine trade direction based on output_mint
        // If output is quote (B), direction is A→B (base to quote)
        // If output is base (A), direction is B→A (quote to base)
        let trade_direction = if output_mint == self.base_token_pk {
            TradeDirection::BtoA // Output is base, input is quote
        } else {
            TradeDirection::AtoB // Output is quote, input is base
        };
        eprintln!("trade_direction: {:?}", trade_direction);
        let current_timestamp = clock.unix_timestamp as u64;
        let current_slot = clock.slot as u64;

        let current_point =
            get_current_point(self.pool.activation_type, current_slot, current_timestamp)?;

        let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
        let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];

        // Determine input/output token accounts based on output_mint
        let (token_in_mint, token_out_mint) = if output_mint == self.base_token_pk {
            (quote_token, base_token) // Output is base, input is quote
        } else {
            (base_token, quote_token) // Output is quote, input is base
        };

        let transfer_fee = get_transfer_fee(token_out_mint, amount_out)?;
        let amount_out_with_fees = amount_out.checked_add(transfer_fee).unwrap();

        let referral_token_account = accounts[self.start_index + Self::REFERRAL_TOKEN_ACCOUNT_IDX];
        let has_referral = !referral_token_account.key.eq(&Pubkey::default()); // TODO: check if this is correct
        let fee_mode =
            FeeMode::get_fee_mode(self.pool.collect_fee_mode, trade_direction, has_referral)?;
        let results = self.pool.get_swap_result_from_exact_output(
            amount_out_with_fees,
            &fee_mode,
            trade_direction,
            current_point,
        )?;

        let transfer_fee_in = get_transfer_fee(token_in_mint, results.included_fee_input_amount)?;
        let amount_in_with_fees = results
            .included_fee_input_amount
            .checked_add(transfer_fee_in)
            .unwrap();

        // Return the input amount needed to get the desired output
        Ok(amount_in_with_fees)
    }



    fn calculate_optimal_amount_in(&self, input_mint: Pubkey, target_price: f64) -> Result<u64> {
        let liquidity = self.pool.liquidity as f64;
        let f_amm = 0.9975;
        let sqrt_price = self.pool.sqrt_price as f64 / Q64_SCALE;
        let target_sqrt_price = target_price.sqrt();
        let current_sqrt_price = sqrt_price;
        let optimal_amount_in = if self.base_token_pk == input_mint {
            // Input X to DAMMV2
            (liquidity / f_amm) * (1.0 / target_sqrt_price - 1.0 / current_sqrt_price)
        } else {
            // Input Y to DAMMV2
            (liquidity / f_amm) * (target_sqrt_price - current_sqrt_price)
        };
        return Ok(optimal_amount_in as u64);
    }





    fn invoke_swap_base_in<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        max_amount_in: u64,
        amount_out: Option<u64>,
        payer: AccountInfo<'a>,
        user_mint_1_token_account: AccountInfo<'a>,
        user_mint_2_token_account: AccountInfo<'a>,
        mint_1_account: AccountInfo<'a>,
        mint_2_account: AccountInfo<'a>,
        mint_1_token_program: AccountInfo<'a>,
        mint_2_token_program: AccountInfo<'a>,
    ) -> Result<()> {
        let (
            base_token_program,
            quote_token_program,
            user_base_token_account,
            user_quote_token_account,
        ) = if input_mint == *mint_1_account.key {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
            )
        } else {
            (
                mint_2_token_program,
                mint_1_token_program,
                user_mint_2_token_account,
                user_mint_1_token_account,
            )
        };

        let program_id = &accounts[self.start_index + Self::PROGRAM_ID_IDX];
        let pool_id = &accounts[self.start_index + Self::POOL_ID_IDX];
        let base_vault = &accounts[self.start_index + Self::BASE_VAULT_IDX];
        let quote_vault = &accounts[self.start_index + Self::QUOTE_VAULT_IDX];
        let pool_authority = &accounts[self.start_index + Self::POOL_AUTHORITY_IDX];
        let event_authority = &accounts[self.start_index + Self::EVENT_AUTHORITY_IDX];
        let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
        let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];
        let referral_token_account = &accounts[self.start_index + Self::REFERRAL_TOKEN_ACCOUNT_IDX];

        let amount_out_value = amount_out.unwrap_or(0);
        let metas = vec![
            AccountMeta::new_readonly(*pool_authority.key, false),
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new(*user_quote_token_account.key, false),
            AccountMeta::new(*user_base_token_account.key, false),
            AccountMeta::new(*base_vault.key, false),
            AccountMeta::new(*quote_vault.key, false),
            AccountMeta::new_readonly(self.base_token_pk, false),
            AccountMeta::new_readonly(self.quote_token_pk, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*base_token_program.key, false),
            AccountMeta::new_readonly(*quote_token_program.key, false),
            AccountMeta::new_readonly(*referral_token_account.key, false),
            AccountMeta::new_readonly(*event_authority.key, false),
            AccountMeta::new_readonly(*program_id.key, false),
        ];

        let mut data = vec![0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];
        data.extend_from_slice(&max_amount_in.to_le_bytes());
        data.extend_from_slice(&amount_out_value.to_le_bytes());

        let swap_ix = Instruction {
            program_id: *program_id.key,
            accounts: metas,
            data,
        };

        // Collect AccountInfo into a vector and use unsafe to cast lifetimes
        // This is safe because 'a outlives 'info in practice when called from execute_arbitrage_path
        let mut accounts_vec: Vec<AccountInfo<'a>> = vec![
            pool_authority.clone(),                              // pool_authority
            pool_id.clone(),                                     // pool_id
            base_vault.clone(),                                  // base_vault
            quote_vault.clone(),                                 // quote_vault
            unsafe { std::mem::transmute(base_token.clone()) },  // base_token
            unsafe { std::mem::transmute(quote_token.clone()) }, // quote_token
            unsafe { std::mem::transmute(referral_token_account.clone()) }, // referral_token_accountclea
            event_authority.clone(),                                             // event_authority
            program_id.clone(),                                                  // program_id
        ];
        // Cast parameter AccountInfo<'a> to AccountInfo<'info> to add to vector
        accounts_vec
            .push(unsafe { std::mem::transmute(user_quote_token_account.to_account_info()) });
        accounts_vec
            .push(unsafe { std::mem::transmute(user_base_token_account.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(payer.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(base_token_program.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(quote_token_program.to_account_info()) });

        // Cast entire vector to AccountInfo<'a> for invoke
        unsafe {
            let accounts: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts)?;
        }

        Ok(())
    }

    fn invoke_swap_base_out<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        min_amount_out: Option<u64>,
        payer: AccountInfo<'a>,
        user_mint_1_token_account: AccountInfo<'a>,
        user_mint_2_token_account: AccountInfo<'a>,
        mint_1_account: AccountInfo<'a>,
        mint_2_account: AccountInfo<'a>,
        mint_1_token_program: AccountInfo<'a>,
        mint_2_token_program: AccountInfo<'a>,
    ) -> Result<()> {
        let (
            base_token_program,
            quote_token_program,
            user_base_token_account,
            user_quote_token_account,
        ) = if mint_1_account.key == &self.base_token_pk {
            (
                mint_1_token_program,
                mint_2_token_program,
                user_mint_1_token_account,
                user_mint_2_token_account,
            )
        } else if mint_2_account.key == &self.base_token_pk {
            (
                mint_2_token_program,
                mint_1_token_program,
                user_mint_2_token_account,
                user_mint_1_token_account,
            )
        } else {
            return Err(ProgramError::InvalidAccountData.into());
        };

        let program_id = &accounts[self.start_index + Self::PROGRAM_ID_IDX];
        let pool_id = &accounts[self.start_index + Self::POOL_ID_IDX];
        let base_vault = &accounts[self.start_index + Self::BASE_VAULT_IDX];
        let quote_vault = &accounts[self.start_index + Self::QUOTE_VAULT_IDX];
        let pool_authority = &accounts[self.start_index + Self::POOL_AUTHORITY_IDX];
        let event_authority = &accounts[self.start_index + Self::EVENT_AUTHORITY_IDX];
        let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
        let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];
        let referral_token_account = &accounts[self.start_index + Self::REFERRAL_TOKEN_ACCOUNT_IDX];

        let min_amount_out_value = min_amount_out.unwrap_or(0);
        let metas = vec![
            AccountMeta::new_readonly(*pool_authority.key, false), // pool_authority
            AccountMeta::new(*pool_id.key, false),                 // pool_id
            AccountMeta::new(*user_base_token_account.key, false),
            AccountMeta::new(*user_quote_token_account.key, false),
            AccountMeta::new(*base_vault.key, false), // base_vault
            AccountMeta::new(*quote_vault.key, false), // quote_vault
            AccountMeta::new_readonly(self.base_token_pk, false),
            AccountMeta::new_readonly(self.quote_token_pk, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(*base_token_program.key, false),
            AccountMeta::new_readonly(*quote_token_program.key, false),
            AccountMeta::new_readonly(*referral_token_account.key, false),
            AccountMeta::new_readonly(*event_authority.key, false), // event_authority
            AccountMeta::new_readonly(*program_id.key, false),      // program_id
        ];
        let mut data = vec![0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out_value.to_le_bytes());

        let swap_ix = Instruction {
            program_id: *accounts[self.start_index + 0].key, // program_id
            accounts: metas,
            data,
        };

        // Collect AccountInfo into a vector
        let mut accounts_vec: Vec<AccountInfo<'a>> = vec![
            accounts[self.start_index + Self::POOL_AUTHORITY_IDX].clone(), // pool_authority
            accounts[self.start_index + Self::POOL_ID_IDX].clone(),        // pool_id
            accounts[self.start_index + Self::BASE_VAULT_IDX].clone(),     // base_vault
            accounts[self.start_index + Self::QUOTE_VAULT_IDX].clone(),    // quote_vault
            unsafe { std::mem::transmute(base_token.clone()) },            // base_token
            unsafe { std::mem::transmute(quote_token.clone()) },           // quote_token
            unsafe { std::mem::transmute(referral_token_account.to_account_info()) }, // referral_token_account
            accounts[self.start_index + Self::EVENT_AUTHORITY_IDX].clone(),      // event_authority
            accounts[self.start_index + Self::PROGRAM_ID_IDX].clone(),           // program_id
        ];
        accounts_vec
            .push(unsafe { std::mem::transmute(user_base_token_account.to_account_info()) });
        accounts_vec
            .push(unsafe { std::mem::transmute(user_quote_token_account.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(payer.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(base_token_program.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(quote_token_program.to_account_info()) });

        unsafe {
            let accounts: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts)?;
        }
        Ok(())
    }
}

impl<'info> MeteoraDammV2<'info> {
    pub const PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");
    pub const PROGRAM_ID_IDX: usize = 0;
    pub const POOL_ID_IDX: usize = 1;
    pub const BASE_VAULT_IDX: usize = 2;
    pub const QUOTE_VAULT_IDX: usize = 3;
    pub const BASE_TOKEN_IDX: usize = 4;
    pub const QUOTE_TOKEN_IDX: usize = 5;
    pub const POOL_AUTHORITY_IDX: usize = 6;
    pub const EVENT_AUTHORITY_IDX: usize = 7;
    pub const REFERRAL_TOKEN_ACCOUNT_IDX: usize = 8;
    pub fn new(
        accounts: &[AccountInfo<'info>],
        start_index: usize,
        end_index: usize,
    ) -> Result<Self> {
        // Access accounts by indices (relative to start_index)
        let pool_id = accounts[start_index + Self::POOL_ID_IDX].clone(); // 1
        let pool_data = pool_id.try_borrow_data()?;
        let pool: Pool = bytemuck::pod_read_unaligned(&pool_data[8..]);
        let base_token = accounts[start_index + Self::BASE_TOKEN_IDX].clone(); // 4
        let quote_token = accounts[start_index + Self::QUOTE_TOKEN_IDX].clone(); // 5
        // let referral_token_account =
        //     accounts[start_index + Self::REFERRAL_TOKEN_ACCOUNT_IDX].clone(); // 8
        let (price, inverse_price) = get_prices(pool)?;
        let base_vault = accounts[start_index + Self::BASE_VAULT_IDX].clone(); // 2
        let quote_vault = accounts[start_index + Self::QUOTE_VAULT_IDX].clone(); // 3
        let base_vault_amount = parse_token_account(&base_vault)?.amount;
        let quote_vault_amount = parse_token_account(&quote_vault)?.amount;
        eprintln!("base_vault_amount: {:?}", base_vault_amount);
        eprintln!("quote_vault_amount: {:?}", quote_vault_amount);

        Ok(MeteoraDammV2 {
            base_token_pk: *base_token.key,
            quote_token_pk: *quote_token.key,
            // referral_token_account: referral_token_account.clone(),
            pool: pool.clone(),
            pool_id: *pool_id.key,
            price: price,
            inverse_price: inverse_price,
            base_vault_amount: base_vault_amount,
            quote_vault_amount: quote_vault_amount,
            start_index,
            end_index,
            phantom: PhantomData,
        })
    }

    // pub fn calculate_optimal_amount_in_impl(&self, input_mint: Pubkey, target_price: f64) -> Result<u64> {
    //     let liquidity = self.pool.liquidity as f64;
    //     let f_amm = 0.9975;
    //     let sqrt_price = self.pool.sqrt_price as f64 / Q64_SCALE;
    //     let target_sqrt_price = target_price.sqrt();
    //     let current_sqrt_price = sqrt_price;
    //     let optimal_amount_in = if self.base_token_pk == input_mint {
    //         // Input X to DAMMV2
    //         (liquidity / f_amm) * (1.0 / target_sqrt_price - 1.0 / current_sqrt_price)
    //     } else {
    //         // Input Y to DAMMV2
    //         (liquidity / f_amm) * (target_sqrt_price - current_sqrt_price)
    //     };
    //     return Ok(optimal_amount_in as u64);
    // }

    // pub fn swap_base_in_impl<'a>(
    //     &self,
    //     accounts: &[AccountInfo<'a>],
    //     input_mint: Pubkey,
    //     amount_in: u64,
    //     clock: Clock,
    // ) -> Result<u64> {
    //     // Determine trade direction based on input_mint
    //     let trade_direction = if input_mint == self.base_token_pk {
    //         TradeDirection::AtoB
    //     } else {
    //         TradeDirection::BtoA
    //     };
    //     let current_timestamp = clock.unix_timestamp as u64;
    //     let current_slot = clock.slot as u64;

    //     let current_point =
    //         get_current_point(self.pool.activation_type, current_slot, current_timestamp)?;

    //     let has_referral = !self.referral_token_account.key.eq(&Pubkey::default());
    //     let fee_mode: FeeMode =
    //         FeeMode::get_fee_mode(self.pool.collect_fee_mode, trade_direction, has_referral)?;

    //     let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
    //     let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];

    //     let (token_in_mint, token_out_mint) = if input_mint == self.base_token_pk {
    //         (base_token, quote_token)
    //     } else {
    //         (quote_token, base_token)
    //     };

    //     let transfer_fee = get_transfer_fee(token_in_mint, amount_in)?;
    //     let amount_in_after_fee = amount_in.checked_sub(transfer_fee).unwrap();

    //     let results = self.pool.get_swap_result_from_exact_input(
    //         amount_in_after_fee,
    //         &fee_mode,
    //         trade_direction,
    //         current_point,
    //     )?;

    //     let transfer_fee_out = get_transfer_fee(token_out_mint, results.output_amount)?;
    //     let amount_out_after_fee = results.output_amount.checked_sub(transfer_fee_out).unwrap();

    //     Ok(amount_out_after_fee)
    // }

    // pub fn swap_base_out_impl<'a>(
    //     &self,
    //     accounts: &[AccountInfo<'a>],
    //     input_mint: Pubkey,
    //     amount_out: u64,
    //     clock: Clock,
    // ) -> Result<u64> {
    //     // Determine trade direction based on input_mint
    //     let trade_direction = if input_mint == self.base_token_pk {
    //         TradeDirection::AtoB
    //     } else {
    //         TradeDirection::BtoA
    //     };
    //     let current_timestamp = clock.unix_timestamp as u64;
    //     let current_slot = clock.slot as u64;

    //     let current_point =
    //         get_current_point(self.pool.activation_type, current_slot, current_timestamp)?;

    //     let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
    //     let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];

    //     let (token_in_mint, token_out_mint) = if input_mint == self.base_token_pk {
    //         (quote_token, base_token)
    //     } else {
    //         (base_token, quote_token)
    //     };

    //     let transfer_fee = get_transfer_fee(token_out_mint, amount_out)?;
    //     let amount_out_with_fees = amount_out.checked_add(transfer_fee).unwrap();

    //     let has_referral = !self.referral_token_account.key.eq(&Pubkey::default());
    //     let fee_mode =
    //         FeeMode::get_fee_mode(self.pool.collect_fee_mode, trade_direction, has_referral)?;
    //     let results = self.pool.get_swap_result_from_exact_output(
    //         amount_out_with_fees,
    //         &fee_mode,
    //         trade_direction,
    //         current_point,
    //     )?;

    //     let transfer_fee = get_transfer_fee(token_in_mint, results.excluded_fee_input_amount)?;
    //     let amount_in_with_fees = results.excluded_fee_input_amount.checked_add(transfer_fee).unwrap();

    //     eprintln!("results: {:?}", results);

    //     // Return the input amount needed to get the desired output
    //     Ok(amount_in_with_fees)
    // }

    // pub fn invoke_swap_base_in_impl<'a>(
    //     &self,
    //     accounts: &[AccountInfo<'a>],
    //     _input_mint: Pubkey,
    //     max_amount_in: u64,
    //     amount_out: Option<u64>,
    //     payer: AccountInfo<'a>,
    //     user_mint_1_token_account: AccountInfo<'a>,
    //     user_mint_2_token_account: AccountInfo<'a>,
    //     mint_1_account: AccountInfo<'a>,
    //     mint_2_account: AccountInfo<'a>,
    //     mint_1_token_program: AccountInfo<'a>,
    //     mint_2_token_program: AccountInfo<'a>,
    // ) -> Result<()> {

    //     let (
    //         base_token_program,
    //         quote_token_program,
    //         user_base_token_account,
    //         user_quote_token_account,
    //     ) = if mint_1_account.key == &self.base_token_pk {
    //         (
    //             mint_1_token_program,
    //             mint_2_token_program,
    //             user_mint_1_token_account,
    //             user_mint_2_token_account,
    //         )
    //     } else if mint_2_account.key == &self.base_token_pk {
    //         (
    //             mint_2_token_program,
    //             mint_1_token_program,
    //             user_mint_2_token_account,
    //             user_mint_1_token_account,
    //         )
    //     } else {
    //         return Err(ProgramError::InvalidAccountData.into());
    //     };

    //     let program_id = &accounts[self.start_index + Self::PROGRAM_ID_IDX];
    //     let pool_id = &accounts[self.start_index + Self::POOL_ID_IDX];
    //     let base_vault = &accounts[self.start_index + Self::BASE_VAULT_IDX];
    //     let quote_vault = &accounts[self.start_index + Self::QUOTE_VAULT_IDX];
    //     let pool_authority = &accounts[self.start_index + Self::POOL_AUTHORITY_IDX];
    //     let event_authority = &accounts[self.start_index + Self::EVENT_AUTHORITY_IDX];
    //     let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
    //     let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];

    //     let amount_out_value = amount_out.unwrap_or(0);
    //     let metas = vec![
    //         AccountMeta::new_readonly(*pool_authority.key, false),
    //         AccountMeta::new(*pool_id.key, false),
    //         AccountMeta::new(*user_quote_token_account.key, false),
    //         AccountMeta::new(*user_base_token_account.key, false),
    //         AccountMeta::new(*base_vault.key, false),
    //         AccountMeta::new(*quote_vault.key, false),
    //         AccountMeta::new_readonly(self.base_token_pk, false),
    //         AccountMeta::new_readonly(self.quote_token_pk, false),
    //         AccountMeta::new(*payer.key, true),
    //         AccountMeta::new_readonly(*base_token_program.key, false),
    //         AccountMeta::new_readonly(*quote_token_program.key, false),
    //         AccountMeta::new_readonly(*self.referral_token_account.key, false),
    //         AccountMeta::new_readonly(*event_authority.key, false),
    //         AccountMeta::new_readonly(*program_id.key, false),
    //     ];

    //     let mut data = vec![0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];
    //     data.extend_from_slice(&max_amount_in.to_le_bytes());
    //     data.extend_from_slice(&amount_out_value.to_le_bytes());

    //     let swap_ix = Instruction {
    //         program_id: *program_id.key,
    //         accounts: metas,
    //         data,
    //     };

    //     // Collect AccountInfo into a vector and use unsafe to cast lifetimes
    //     // This is safe because 'a outlives 'info in practice when called from execute_arbitrage_path
    //     let mut accounts_vec: Vec<AccountInfo<'a>> = vec![
    //         pool_authority.clone(),                                   // pool_authority
    //         pool_id.clone(),                                          // pool_id
    //         base_vault.clone(),                                       // base_vault
    //         quote_vault.clone(),                                      // quote_vault
    //         unsafe { std::mem::transmute(base_token.clone()) },  // base_token
    //         unsafe { std::mem::transmute(quote_token.clone()) }, // quote_token
    //         unsafe { std::mem::transmute(self.referral_token_account.clone()) }, // referral_token_accountclea
    //         event_authority.clone(),                                             // event_authority
    //         program_id.clone(),                                                  // program_id
    //     ];
    //     // Cast parameter AccountInfo<'a> to AccountInfo<'info> to add to vector
    //     accounts_vec
    //         .push(unsafe { std::mem::transmute(user_quote_token_account.to_account_info()) });
    //     accounts_vec
    //         .push(unsafe { std::mem::transmute(user_base_token_account.to_account_info()) });
    //     accounts_vec.push(unsafe { std::mem::transmute(payer.to_account_info()) });
    //     accounts_vec.push(unsafe { std::mem::transmute(base_token_program.to_account_info()) });
    //     accounts_vec.push(unsafe { std::mem::transmute(quote_token_program.to_account_info()) });

    //     // Cast entire vector to AccountInfo<'a> for invoke
    //     unsafe {
    //         let accounts: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
    //         invoke(&swap_ix, accounts)?;
    //     }

    //     Ok(())
    // }

    // pub fn invoke_swap_base_out_impl<'a>(
    //     &self,
    //     accounts: &[AccountInfo<'a>],
    //     _input_mint: Pubkey,
    //     amount_in: u64,
    //     min_amount_out: Option<u64>,
    //     payer: AccountInfo<'a>,
    //     user_mint_1_token_account: AccountInfo<'a>,
    //     user_mint_2_token_account: AccountInfo<'a>,
    //     mint_1_account: AccountInfo<'a>,
    //     mint_2_account: AccountInfo<'a>,
    //     mint_1_token_program: AccountInfo<'a>,
    //     mint_2_token_program: AccountInfo<'a>,
    // ) -> Result<()> {
    //     use anchor_lang::solana_program::{
    //         instruction::{AccountMeta, Instruction},
    //         program::invoke,
    //     };

    //     let (
    //         base_token_program,
    //         quote_token_program,
    //         user_base_token_account,
    //         user_quote_token_account,
    //     ) = if mint_1_account.key == &self.base_token_pk {
    //         (
    //             mint_1_token_program,
    //             mint_2_token_program,
    //             user_mint_1_token_account,
    //             user_mint_2_token_account,
    //         )
    //     } else if mint_2_account.key == &self.base_token_pk {
    //         (
    //             mint_2_token_program,
    //             mint_1_token_program,
    //             user_mint_2_token_account,
    //             user_mint_1_token_account,
    //         )
    //     } else {
    //         return Err(ProgramError::InvalidAccountData.into());
    //     };

    //     let program_id = &accounts[self.start_index + Self::PROGRAM_ID_IDX];
    //     let pool_id = &accounts[self.start_index + Self::POOL_ID_IDX];
    //     let base_vault = &accounts[self.start_index + Self::BASE_VAULT_IDX];
    //     let quote_vault = &accounts[self.start_index + Self::QUOTE_VAULT_IDX];
    //     let pool_authority = &accounts[self.start_index + Self::POOL_AUTHORITY_IDX];
    //     let event_authority = &accounts[self.start_index + Self::EVENT_AUTHORITY_IDX];
    //     let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
    //     let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];

    //     let min_amount_out_value = min_amount_out.unwrap_or(0);
    //     let metas = vec![
    //         AccountMeta::new_readonly(*pool_authority.key, false), // pool_authority
    //         AccountMeta::new(*pool_id.key, false),                 // pool_id
    //         AccountMeta::new(*user_base_token_account.key, false),
    //         AccountMeta::new(*user_quote_token_account.key, false),
    //         AccountMeta::new(*base_vault.key, false), // base_vault
    //         AccountMeta::new(*quote_vault.key, false), // quote_vault
    //         AccountMeta::new_readonly(self.base_token_pk, false),
    //         AccountMeta::new_readonly(self.quote_token_pk, false),
    //         AccountMeta::new(*payer.key, true),
    //         AccountMeta::new_readonly(*base_token_program.key, false),
    //         AccountMeta::new_readonly(*quote_token_program.key, false),
    //         AccountMeta::new_readonly(*self.referral_token_account.key, false),
    //         AccountMeta::new_readonly(*event_authority.key, false), // event_authority
    //         AccountMeta::new_readonly(*program_id.key, false),      // program_id
    //     ];
    //     let mut data = vec![0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];
    //     data.extend_from_slice(&amount_in.to_le_bytes());
    //     data.extend_from_slice(&min_amount_out_value.to_le_bytes());

    //     let swap_ix = Instruction {
    //         program_id: *accounts[self.start_index + 0].key, // program_id
    //         accounts: metas,
    //         data,
    //     };

    //     // Collect AccountInfo into a vector
    //     let mut accounts_vec: Vec<AccountInfo<'a>> = vec![
    //         accounts[self.start_index + Self::POOL_AUTHORITY_IDX].clone(), // pool_authority
    //         accounts[self.start_index + Self::POOL_ID_IDX].clone(),        // pool_id
    //         accounts[self.start_index + Self::BASE_VAULT_IDX].clone(),     // base_vault
    //         accounts[self.start_index + Self::QUOTE_VAULT_IDX].clone(),    // quote_vault
    //         unsafe { std::mem::transmute(base_token.clone()) },       // base_token
    //         unsafe { std::mem::transmute(quote_token.clone()) },      // quote_token
    //         unsafe { std::mem::transmute(self.referral_token_account.clone()) }, // referral_token_account
    //         accounts[self.start_index + Self::EVENT_AUTHORITY_IDX].clone(),      // event_authority
    //         accounts[self.start_index + Self::PROGRAM_ID_IDX].clone(),           // program_id
    //     ];
    //     accounts_vec
    //         .push(unsafe { std::mem::transmute(user_base_token_account.to_account_info()) });
    //     accounts_vec
    //         .push(unsafe { std::mem::transmute(user_quote_token_account.to_account_info()) });
    //     accounts_vec.push(unsafe { std::mem::transmute(payer.to_account_info()) });
    //     accounts_vec.push(unsafe { std::mem::transmute(base_token_program.to_account_info()) });
    //     accounts_vec.push(unsafe { std::mem::transmute(quote_token_program.to_account_info()) });

    //     unsafe {
    //         let accounts: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
    //         invoke(&swap_ix, accounts)?;
    //     }
    //     Ok(())
    // }
}

#[cfg(test)]
mod tests {

    use super::*;
    use anchor_lang::solana_program::{
        account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, system_program,
    };
    use bytemuck;
    use damm_v2::state::pool::Pool;

    // Helper to convert solana_sdk::account::Account to AccountInfo
    fn account_to_account_info(
        key: Pubkey,
        account: solana_sdk::account::Account,
    ) -> AccountInfo<'static> {
        let data = Box::leak(Box::new(account.data));
        let lamports = Box::leak(Box::new(account.lamports));
        let owner_bytes: [u8; 32] = account.owner.to_bytes();
        let owner = Pubkey::try_from(owner_bytes.as_ref()).unwrap();
        let owner_static = Box::leak(Box::new(owner));
        let key_static = Box::leak(Box::new(key));
        AccountInfo::new(
            key_static,
            false, // is_signer
            false, // is_writable
            lamports,
            data,
            owner_static,
            account.executable,
            account.rent_epoch,
        )
    }

    // Helper function to fetch account from RPC and convert to AccountInfo
    async fn fetch_account_info_from_rpc(
        rpc_client: &solana_client::nonblocking::rpc_client::RpcClient,
        key: Pubkey,
    ) -> AccountInfo<'static> {
        use solana_sdk::pubkey::Pubkey as SdkPubkey;

        let sdk_pubkey = SdkPubkey::try_from(key.to_bytes().as_ref())
            .expect("Failed to convert Pubkey to SdkPubkey");
        let account = rpc_client
            .get_account(&sdk_pubkey)
            .await
            .expect(&format!("Failed to fetch account {}", key));
        account_to_account_info(key, account)
    }

    /// Get on chain clock from RPC
    async fn get_clock(
        rpc_client: &solana_client::nonblocking::rpc_client::RpcClient,
    ) -> anyhow::Result<Clock> {
        use anchor_client::solana_sdk::sysvar;

        let clock_account = rpc_client.get_account(&sysvar::clock::ID).await?;

        // Clock from Solana is borsh-serialized with these fields in order:
        // slot: u64 (8 bytes)
        // epoch_start_timestamp: i64 (8 bytes)
        // epoch: u64 (8 bytes)
        // leader_schedule_epoch: u64 (8 bytes)
        // unix_timestamp: i64 (8 bytes)
        // Total: 40 bytes
        if clock_account.data.len() < 40 {
            return Err(anyhow::anyhow!(
                "Clock account data too short: {} bytes",
                clock_account.data.len()
            ));
        }

        let data = &clock_account.data;
        let slot = u64::from_le_bytes(
            data[0..8]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to parse slot"))?,
        );
        let epoch_start_timestamp = i64::from_le_bytes(
            data[8..16]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to parse epoch_start_timestamp"))?,
        );
        let epoch = u64::from_le_bytes(
            data[16..24]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to parse epoch"))?,
        );
        let leader_schedule_epoch = u64::from_le_bytes(
            data[24..32]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to parse leader_schedule_epoch"))?,
        );
        let unix_timestamp = i64::from_le_bytes(
            data[32..40]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to parse unix_timestamp"))?,
        );

        Ok(Clock {
            slot,
            epoch_start_timestamp,
            epoch,
            leader_schedule_epoch,
            unix_timestamp,
        })
    }

    // Helper function to create a mock AccountInfo with provided data
    fn create_mock_account_info_with_data(
        key: Pubkey,
        owner: Pubkey,
        data: Option<Vec<u8>>,
    ) -> AccountInfo<'static> {
        let data_vec = data.unwrap_or_else(|| vec![0u8; 8]);
        let data_vec = Box::leak(Box::new(data_vec));
        let lamports = Box::leak(Box::new(0u64));
        let owner_static = Box::leak(Box::new(owner));
        let key_static = Box::leak(Box::new(key));

        AccountInfo::new(
            key_static,
            false,
            true,
            lamports,
            data_vec,
            owner_static,
            false,
            0,
        )
    }

    // Helper function to create a mock AccountInfo
    fn create_mock_account_info(
        key: Pubkey,
        owner: Pubkey,
        account_data: Option<Vec<u8>>,
    ) -> AccountInfo<'static> {
        let data = if let Some(provided_data) = account_data {
            Box::leak(Box::new(provided_data))
        } else {
            Box::leak(Box::new(Vec::new()))
        };
        let lamports = Box::leak(Box::new(0u64));
        let owner_static = Box::leak(Box::new(owner));
        let key_static = Box::leak(Box::new(key));

        AccountInfo::new(
            key_static,
            false,
            false,
            lamports,
            data,
            owner_static,
            false,
            0,
        )
    }

    // Helper function to create a Pool from actual pool data
    // Pool data from pool_data.txt (Python bytes literal converted to Rust)
    fn create_test_pool() -> Pool {
        let pool_id = Pubkey::new_unique();
        let pool = create_mock_account_info(pool_id, system_program::id(), None);
        let pool_data = pool.try_borrow_data().unwrap();
        let pool: Pool = bytemuck::pod_read_unaligned(&pool_data[8..]);
        pool
    }

    #[test]
    fn test_get_current_point_slot() {
        let activation_type = 0u8; // Slot
        let current_slot = 1000u64;
        let current_timestamp = 1234567890u64;

        let result = get_current_point(activation_type, current_slot, current_timestamp).unwrap();
        assert_eq!(result, current_slot);
    }

    #[test]
    fn test_get_current_point_timestamp() {
        let activation_type = 1u8; // Timestamp
        let current_slot = 1000u64;
        let current_timestamp = 1234567890u64;

        let result = get_current_point(activation_type, current_slot, current_timestamp).unwrap();
        assert_eq!(result, current_timestamp);
    }

    #[test]
    fn test_get_current_point_invalid_type() {
        let activation_type = 255u8; // Invalid
        let current_slot = 1000u64;
        let current_timestamp = 1234567890u64;

        let result = get_current_point(activation_type, current_slot, current_timestamp);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), ProgramError::InvalidAccountData.into());
    }

    #[test]
    fn test_meteora_damm_v2_program_id() {
        let expected_bytes = [
            202, 173, 213, 232, 67, 75, 181, 53, 88, 180, 220, 112, 105, 107, 171, 119, 215, 173,
            214, 67, 75, 181, 53, 88, 180, 220, 112, 105, 107, 171, 119, 215,
        ];
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&expected_bytes);
        let expected_id = Pubkey::new_from_array(arr);
        assert_eq!(MeteoraDammV2::PROGRAM_ID, expected_id);
    }

    #[test]
    fn test_meteora_damm_v2_new_insufficient_accounts() {
        let accounts = vec![];
        let result = MeteoraDammV2::new(accounts.as_slice(), 0, accounts.len());
        assert!(result.is_err());
    }

    #[test]
    fn test_meteora_damm_v2_new_sufficient_accounts() {
        let program_id = Pubkey::new_unique();
        let pool_id = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let base_token = Pubkey::new_unique();
        let quote_token = Pubkey::new_unique();
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        let referral_token_account = Pubkey::new_unique();

        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),
            create_mock_account_info(pool_id, system_program::id(), None),
            create_mock_account_info(base_vault, system_program::id(), None),
            create_mock_account_info(quote_vault, system_program::id(), None),
            create_mock_account_info(base_token, system_program::id(), None),
            create_mock_account_info(quote_token, system_program::id(), None),
            create_mock_account_info(pool_authority, system_program::id(), None),
            create_mock_account_info(event_authority, system_program::id(), None),
            create_mock_account_info(referral_token_account, system_program::id(), None),
        ];

        let result = MeteoraDammV2::new(accounts.as_slice(), 0, accounts.len());
        assert!(result.is_ok());

        let meteora = result.unwrap();
        // assert_eq!(*meteora.accounts[0].key, program_id);
        // assert_eq!(*meteora.accounts[1].key, pool_id);
        // assert_eq!(*meteora.accounts[2].key, base_vault);
        // assert_eq!(*meteora.accounts[3].key, quote_vault);
        assert_eq!(meteora.base_token_pk, base_token);
        assert_eq!(meteora.quote_token_pk, quote_token);
        assert_eq!(meteora.referral_token_account.key(), referral_token_account);
    }

    #[test]
    fn test_swap_base_in_basic() {
        let pool = create_test_pool();
        let pool_bytes = bytemuck::bytes_of(&pool);

        // Create pool account with 8-byte discriminator + pool data
        let mut pool_data = vec![0u8; 8];
        pool_data.extend_from_slice(pool_bytes);

        let pool_id = Pubkey::new_unique();
        let pool_account = create_mock_account_info(pool_id, system_program::id(), Some(pool_data));

        let program_id = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let base_token = Pubkey::new_unique();
        let quote_token = Pubkey::new_unique();
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        let referral_token_account = Pubkey::new_unique();

        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),
            pool_account.clone(),
            create_mock_account_info(base_vault, system_program::id(), None),
            create_mock_account_info(quote_vault, system_program::id(), None),
            create_mock_account_info(base_token, system_program::id(), None),
            create_mock_account_info(quote_token, system_program::id(), None),
            create_mock_account_info(pool_authority, system_program::id(), None),
            create_mock_account_info(event_authority, system_program::id(), None),
            create_mock_account_info(referral_token_account, system_program::id(), None),
        ];

        let meteora = MeteoraDammV2::new(accounts.as_slice(), 0, accounts.len()).unwrap();
        let data = accounts[1].try_borrow_data().unwrap();
        let pool: Pool = bytemuck::pod_read_unaligned(&data[8..]);

        eprintln!("pool: {:?}", pool.token_a_mint);
        eprintln!("pool: {:?}", pool.token_b_mint);
        eprintln!("pool: {:?}", pool.token_a_vault);
        eprintln!("pool: {:?}", pool.token_b_vault);
        eprintln!("pool activation_point: {}", pool.activation_point);
        eprintln!("pool activation_type: {}", pool.activation_type);
        eprintln!("pool liquidity: {}", pool.liquidity);
        eprintln!("pool pool_status: {}", pool.pool_status);
        eprintln!("pool sqrt_price: {}", pool.sqrt_price);

        // Use actual addresses from pool data for important accounts
        let program_id = MeteoraDammV2::PROGRAM_ID;
        let base_vault = pool.token_a_vault;
        let quote_vault = pool.token_b_vault;
        let base_token = pool.token_a_mint;
        let quote_token = pool.token_b_mint;
        let pool_authority = Pubkey::new_unique(); // This might need to be calculated properly
        let event_authority = Pubkey::new_unique();
        let referral_token_account = Pubkey::default(); // Use default for no referral

        let correct_accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),
            pool_account.clone(),
            create_mock_account_info(base_vault, system_program::id(), None),
            create_mock_account_info(quote_vault, system_program::id(), None),
            create_mock_account_info(base_token, system_program::id(), None),
            create_mock_account_info(quote_token, system_program::id(), None),
            create_mock_account_info(pool_authority, system_program::id(), None),
            create_mock_account_info(event_authority, system_program::id(), None),
            create_mock_account_info(referral_token_account, system_program::id(), None),
        ];

        let meteora_correct =
            MeteoraDammV2::new(correct_accounts.as_slice(), 0, correct_accounts.len()).unwrap();

        let clock = Clock {
            slot: 200000000, // High slot number to ensure activation
            epoch_start_timestamp: 0,
            epoch: 500, // High epoch
            leader_schedule_epoch: 0,
            unix_timestamp: 1700000000, // Recent timestamp (2023)
        };

        // Test with a much smaller amount first
        let amount_in = 1000000; // 0.001 tokens (assuming 9 decimals)
        let input_mint = base_token; // Swap base token in
        let result =
            meteora_correct.swap_base_in(correct_accounts.as_slice(), input_mint, amount_in, clock);
        eprintln!("result: {:?}", result);
        if let Err(ref e) = result {
            eprintln!("Error: {:?}", e);
        }
        // Should succeed and return some output amount
        assert!(result.is_ok());
        let output_amount = result.unwrap();
        assert!(output_amount > 0);
        eprintln!("Result {:?}", output_amount);
    }

    #[test]
    fn test_swap_base_out_basic() {
        let pool = create_test_pool();
        let pool_bytes = bytemuck::bytes_of(&pool);

        let mut pool_data = vec![0u8; 8];
        pool_data.extend_from_slice(pool_bytes);

        let pool_id = Pubkey::new_unique();
        let pool_account = create_mock_account_info(pool_id, system_program::id(), Some(pool_data));

        let program_id = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let base_token = Pubkey::new_unique();
        let quote_token = Pubkey::new_unique();
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        let referral_token_account = Pubkey::new_unique();

        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),
            pool_account.clone(),
            create_mock_account_info(base_vault, system_program::id(), None),
            create_mock_account_info(quote_vault, system_program::id(), None),
            create_mock_account_info(base_token, system_program::id(), None),
            create_mock_account_info(quote_token, system_program::id(), None),
            create_mock_account_info(pool_authority, system_program::id(), None),
            create_mock_account_info(event_authority, system_program::id(), None),
            create_mock_account_info(referral_token_account, system_program::id(), None),
        ];

        let meteora = MeteoraDammV2::new(accounts.as_slice(), 0, accounts.len()).unwrap();
        let data = accounts[1].try_borrow_data().unwrap();
        let pool: Pool = bytemuck::pod_read_unaligned(&data[8..]);

        eprintln!("pool: {:?}", pool.token_a_mint);
        eprintln!("pool: {:?}", pool.token_b_mint);

        let clock = Clock {
            slot: 1000,
            epoch_start_timestamp: 0,
            epoch: 0,
            leader_schedule_epoch: 0,
            unix_timestamp: 1234567890,
        };

        // Test with a small amount (desired output amount)
        let amount_out = 1_000_000_000; // Desired output amount
        let input_mint = quote_token; // For swap_base_out, input is quote_token to get base_token out
        let result = meteora.swap_base_out(accounts.as_slice(), input_mint, amount_out, clock);

        // Should succeed and return some output amount
        assert!(result.is_ok());
        let output_amount = result.unwrap();
        assert!(output_amount > 0);
        eprintln!("Result {:?}", output_amount);
    }

    #[test]
    fn test_swap_base_in_with_referral() {
        let pool = create_test_pool();
        let pool_bytes = bytemuck::bytes_of(&pool);

        let mut pool_data = vec![0u8; 8];
        pool_data.extend_from_slice(pool_bytes);

        let pool_id = Pubkey::new_unique();
        let pool_account = create_mock_account_info(pool_id, system_program::id(), Some(pool_data));

        let program_id = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let base_token = Pubkey::new_unique();
        let quote_token = Pubkey::new_unique();
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        // Use a non-default referral token account
        let referral_token_account = Pubkey::new_unique();

        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),
            pool_account.clone(),
            create_mock_account_info(base_vault, system_program::id(), None),
            create_mock_account_info(quote_vault, system_program::id(), None),
            create_mock_account_info(base_token, system_program::id(), None),
            create_mock_account_info(quote_token, system_program::id(), None),
            create_mock_account_info(pool_authority, system_program::id(), None),
            create_mock_account_info(event_authority, system_program::id(), None),
            create_mock_account_info(referral_token_account, system_program::id(), None),
        ];

        let meteora = MeteoraDammV2::new(accounts.as_slice(), 0, accounts.len()).unwrap();

        let clock = Clock {
            slot: 1000,
            epoch_start_timestamp: 0,
            epoch: 0,
            leader_schedule_epoch: 0,
            unix_timestamp: 1234567890,
        };

        let amount_in = 1_000_000;
        let input_mint = base_token; // Swap base token in
        let result = meteora.swap_base_in(accounts.as_slice(), input_mint, amount_in, clock);

        // Should succeed even with referral
        assert!(result.is_ok());
    }

    #[test]
    fn test_swap_base_in_with_default_referral() {
        let pool = create_test_pool();
        let pool_bytes = bytemuck::bytes_of(&pool);

        let mut pool_data = vec![0u8; 8];
        pool_data.extend_from_slice(pool_bytes);

        let pool_id = Pubkey::new_unique();
        let pool_account = create_mock_account_info(pool_id, system_program::id(), Some(pool_data));

        let program_id = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let base_token = Pubkey::new_unique();
        let quote_token = Pubkey::new_unique();
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        // Use default (zero) referral token account
        let referral_token_account = Pubkey::default();

        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),
            pool_account.clone(),
            create_mock_account_info(base_vault, system_program::id(), None),
            create_mock_account_info(quote_vault, system_program::id(), None),
            create_mock_account_info(base_token, system_program::id(), None),
            create_mock_account_info(quote_token, system_program::id(), None),
            create_mock_account_info(pool_authority, system_program::id(), None),
            create_mock_account_info(event_authority, system_program::id(), None),
            create_mock_account_info(referral_token_account, system_program::id(), None),
        ];

        let meteora = MeteoraDammV2::new(accounts.as_slice(), 0, accounts.len()).unwrap();

        let clock = Clock {
            slot: 1000,
            epoch_start_timestamp: 0,
            epoch: 0,
            leader_schedule_epoch: 0,
            unix_timestamp: 1234567890,
        };

        let amount_in = 1_000_000;
        let input_mint = base_token; // Swap base token in
        let result = meteora.swap_base_in(accounts.as_slice(), input_mint, amount_in, clock);

        // Should succeed without referral
        assert!(result.is_ok());
    }

    #[test]
    fn test_program_meta_implementation() {
        let program_id = MeteoraDammV2::PROGRAM_ID;
        let pool_id = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let base_token = Pubkey::new_unique();
        let quote_token = Pubkey::new_unique();
        let pool_authority = Pubkey::new_unique();
        let event_authority = Pubkey::new_unique();
        let referral_token_account = Pubkey::new_unique();

        let accounts = vec![
            create_mock_account_info(program_id, system_program::id(), None),
            create_mock_account_info(pool_id, system_program::id(), None),
            create_mock_account_info(base_vault, system_program::id(), None),
            create_mock_account_info(quote_vault, system_program::id(), None),
            create_mock_account_info(base_token, system_program::id(), None),
            create_mock_account_info(quote_token, system_program::id(), None),
            create_mock_account_info(pool_authority, system_program::id(), None),
            create_mock_account_info(event_authority, system_program::id(), None),
            create_mock_account_info(referral_token_account, system_program::id(), None),
        ];

        let meteora = MeteoraDammV2::new(accounts.as_slice(), 0, accounts.len()).unwrap();

        // Test ProgramMeta trait implementation
        let id = meteora.get_id();
        assert_eq!(*id, MeteoraDammV2::PROGRAM_ID);
    }

    #[tokio::test]
    async fn test_damm_v2_swap() {
        use anchor_client::Cluster;
        use solana_client::nonblocking::rpc_client::RpcClient;

        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        let pool_id = Pubkey::from_str_const("D5JnazxpKDqWtUwKHnyzHvEasxTous7PDapzeMGxQuaW");
        let pool_account_info = fetch_account_info_from_rpc(&rpc_client, pool_id).await;

        // Read pool data from AccountInfo in a separate scope to drop the borrow
        let (token_a_mint, token_b_mint, token_a_vault, token_b_vault) = {
            let pool_data: std::cell::Ref<'_, &mut [u8]> =
                pool_account_info.try_borrow_data().unwrap();
            let pool: Pool = bytemuck::pod_read_unaligned(&pool_data[8..]);

            eprintln!("Mint A: {:?}", pool.token_a_mint);
            eprintln!("Mint B: {:?}", pool.token_b_mint);
            eprintln!("Pool A Vault: {:?}", pool.token_a_vault);
            eprintln!("Pool B Vault: {:?}", pool.token_b_vault);
            eprintln!("pool activation_point: {}", pool.activation_point);
            eprintln!("pool activation_type: {}", pool.activation_type);
            eprintln!("pool liquidity: {}", pool.liquidity);
            eprintln!("pool pool_status: {}", pool.pool_status);
            eprintln!("pool sqrt_price: {}", pool.sqrt_price);

            (
                pool.token_a_mint,
                pool.token_b_mint,
                pool.token_a_vault,
                pool.token_b_vault,
            )
        };

        // Create program_id account
        let program_id_account = create_mock_account_info_with_data(
            MeteoraDammV2::PROGRAM_ID,
            system_program::id(),
            None,
        );
        let base_vault_account = fetch_account_info_from_rpc(&rpc_client, token_a_vault).await;
        let quote_vault_account = fetch_account_info_from_rpc(&rpc_client, token_b_vault).await;
        let base_token_account = fetch_account_info_from_rpc(&rpc_client, token_a_mint).await;
        let quote_token_account = fetch_account_info_from_rpc(&rpc_client, token_b_mint).await;

        // Create pool authority and event authority accounts
        let pool_authority = create_mock_account_info_with_data(
            MeteoraDammV2::PROGRAM_ID,
            system_program::id(),
            None,
        );
        let event_authority = create_mock_account_info_with_data(
            MeteoraDammV2::PROGRAM_ID,
            system_program::id(),
            None,
        );
        let referral_token_account = create_mock_account_info_with_data(
            MeteoraDammV2::PROGRAM_ID,
            system_program::id(),
            None,
        );

        let accounts = vec![
            program_id_account,             // 0: program_id
            pool_account_info.clone(),      // 1: pool_id
            base_vault_account.clone(),     // 2: base_vault
            quote_vault_account.clone(),    // 3: quote_vault
            base_token_account.clone(),     // 4: base_token
            quote_token_account.clone(),    // 5: quote_token
            pool_authority.clone(),         // 6: pool_authority
            event_authority.clone(),        // 7: event_authority
            referral_token_account.clone(), // 8: referral_token_account
        ];

        let clock1 = get_clock(&rpc_client).await.unwrap();
        let meteora_damm_v2 = MeteoraDammV2::new(accounts.as_slice(), 0, accounts.len()).unwrap();

        let prices = meteora_damm_v2.get_prices().unwrap();
        let price = prices.0;
        let inverse_price = prices.1;
        eprintln!("price: {:?}", price);
        eprintln!("inverse_price: {:?}", inverse_price);
        eprintln!("================================================");

        let in_sol_amount = 1_000;
        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let token_mint = if token_a_mint == sol_mint {
            token_b_mint
        } else {
            token_a_mint
        };

        let (sol_price, token_price) = if token_a_mint == sol_mint {
            (price, inverse_price)
        } else {
            (inverse_price, price)
        };

        let (max_sol_in, max_token_out) = meteora_damm_v2.get_max_amounts_in_out(sol_mint).unwrap();
        eprintln!("Max SOL IN: {:?} -> MAX TOKEN OUT: {:?}", max_sol_in as f64 / 1_000_000_000.0, max_token_out as f64 / 1_000_000.0);

        eprintln!("Sol price: {:?}", sol_price);
        eprintln!("Token price: {:?}", token_price);
        let token_out: u64 = meteora_damm_v2
            .swap_base_in(accounts.as_slice(), sol_mint, in_sol_amount, clock1.clone())
            .unwrap();

        // Expected using oracle price (for debug only)
        let expected_token_out = (in_sol_amount as f64 * sol_price) as u64;

        eprintln!(
            "Step 1 (swap_base_in): {} SOL -> {} TOKEN / {}",
            in_sol_amount as f64 / 1_000_000_000.0,
            token_out as f64 / 1_000_000.0,
            expected_token_out as f64 / 1_000_000.0,
        );

        let max_sol_in = meteora_damm_v2
            .swap_base_out(accounts.as_slice(), token_mint, token_out, clock1.clone())
            .unwrap();
        eprintln!(
            "Step 1 (swap_base_out): {} MAX SOL IN -> {} TOKEN OUT",
            max_sol_in as f64 / 1_000_000_000.0,
            token_out as f64 / 1_000_000.0
        );
        eprintln!("================================================");

        let sol_out = meteora_damm_v2
            .swap_base_in(accounts.as_slice(), token_mint, token_out, clock1.clone())
            .unwrap();
        let expected_sol_out = (token_out as f64 * token_price) as u64;

        eprintln!(
            "Step 2 (swap_base_in): {} TOKEN -> {} SOL / {}",
            token_out as f64 / 1_000_000.0,
            sol_out as f64 / 1_000_000_000.0,
            expected_sol_out as f64 / 1_000_000_000.0,
        );
        let max_token_in = meteora_damm_v2
            .swap_base_out(accounts.as_slice(), sol_mint, sol_out, clock1.clone())
            .unwrap();
        eprintln!(
            "Step 2 (swap_base_out): {} MAX TOKEN IN -> {} SOL OUT",
            max_token_in as f64 / 1_000_000.0,
            sol_out as f64 / 1_000_000_000.0,
        );
        eprintln!("================================================");
    }
}
