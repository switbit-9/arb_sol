use crate::programs::ProgramMeta;
use crate::utils::token::get_transfer_fee;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};
mod constants;
use crate::utils::utils::parse_token_account;
use std::marker::PhantomData;

pub fn get_prices(base_vault_amount: u64, quote_vault_amount: u64) -> Result<(f64, f64)> {
    // price : Base -> Quote
    // inverse_price : Quote -> Base
    let price = quote_vault_amount as f64 / base_vault_amount as f64;
    let inverse_price = 1.0 / price;
    Ok((price, inverse_price))
}

pub fn get_fees(price: f64, inverse_price: f64) -> Result<f64> {
    let sol_price = price.min(inverse_price);

    // price * 1 000 000 000 / 1 000 = market cap in SOL
    let market_cap_sol = sol_price * 1_000_000 as f64;
    // eprintln!("PUMP AMM MARKET CAP SOL {}: {}", sol_price, market_cap_sol);

    let total_fee = match market_cap_sol {
        m if m <= 420.0 => 0.0125,   // Total 1.25%
        m if m <= 1470.0 => 0.0120,  // Total 1.20%
        m if m <= 2460.0 => 0.0115,  // Total 1.15%
        m if m <= 3440.0 => 0.0110,  // Total 1.10%
        m if m <= 4420.0 => 0.0105,  // Total 1.05%
        m if m <= 9820.0 => 0.0100,  // Total 1.00%
        m if m <= 14740.0 => 0.0095, // Total 0.95%
        m if m <= 19650.0 => 0.0090, // Total 0.90%
        m if m <= 24560.0 => 0.0085, // Total 0.85%
        m if m <= 29470.0 => 0.0080, // Total 0.80%
        m if m <= 34380.0 => 0.0075, // Total 0.75%
        m if m <= 39300.0 => 0.0070, // Total 0.70%
        m if m <= 44210.0 => 0.0065, // Total 0.65%
        m if m <= 49120.0 => 0.0060, // Total 0.60%
        m if m <= 54030.0 => 0.0055, // Total 0.55%
        m if m <= 58940.0 => 0.0052, // Total 0.52% (27+5+20 = 52 basis points)
        m if m <= 63860.0 => 0.0050, // Total 0.50%
        m if m <= 68770.0 => 0.0047, // Total 0.47% (22+5+20 = 47 basis points)
        m if m <= 73681.0 => 0.0045, // Total 0.45%
        m if m <= 78590.0 => 0.0042, // Total 0.42% (17+5+20 = 42 basis points)
        m if m <= 83500.0 => 0.0040, // Total 0.40%
        m if m <= 88400.0 => 0.0037, // Total 0.37% (12+5+20 = 37 basis points)
        m if m <= 93330.0 => 0.0035, // Total 0.35%
        m if m <= 98240.0 => 0.0032, // Total 0.32% (7+5+20 = 32 basis points)
        _ => 0.0030,                 // Total 0.30% (> 98240 SOL)
    };
    Ok(total_fee)
}

pub struct PumpAmm<'info> {
    // pub program_id: AccountInfo<'info>,
    // pub pool_id: AccountInfo<'info>,
    // pub base_vault: AccountInfo<'info>,
    // pub quote_vault: AccountInfo<'info>,
    // pub base_token: AccountInfo<'info>,
    // pub quote_token: AccountInfo<'info>,
    // pub base_vault_account: TokenAccount,
    // pub quote_vault_account: TokenAccount,
    pub pool_id: Pubkey,
    pub base_token_pk: Pubkey,
    pub quote_token_pk: Pubkey,
    pub base_vault_amount: u64,
    pub quote_vault_amount: u64,
    pub price: f64,
    pub inverse_price: f64,
    pub fee_rate: f64,
    pub start_index: usize,
    pub end_index: usize,
    /// Cached max amounts from init
    pub buy_max_in: u64,
    pub buy_max_out: u64,
    pub sell_max_in: u64,
    pub sell_max_out: u64,
    _phantom: PhantomData<&'info ()>,
}

impl<'info> ProgramMeta for PumpAmm<'info> {
    fn get_id(&self) -> &Pubkey {
        &Self::PROGRAM_ID
    }

    fn get_pool_id(&self) -> &Pubkey {
        &self.pool_id
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (&self.base_token_pk, &self.quote_token_pk)
    }

    fn name(&self) -> &'static str { "PumpAmm" }

    fn get_fee_factor(&self) -> Result<(f64, f64)> { let f = 1.0 - self.fee_rate; Ok((f, f)) }

    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        Ok((
            self.base_vault_amount as u64,
            self.quote_vault_amount as u64,
        ))
    }

    fn swap_base_in<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        _clock: &Clock,
    ) -> Result<u64> {
        // if input_mint == self.base_token_pk {
        //     return self.swap_base_out_impl(accounts, input_mint, amount_in);
        // }

        // Use u128 math to avoid overflow on large vaults
        let base_reserve = self.base_vault_amount as u128;
        let quote_reserve = self.quote_vault_amount as u128;

        let output_reserve = if input_mint == self.base_token_pk {
            self.quote_vault_amount
        } else {
            self.base_vault_amount
        };

        let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
        let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];

        let (token_in_mint, token_out_mint) = if input_mint == self.base_token_pk {
            (base_token, quote_token)
        } else {
            (quote_token, base_token)
        };

        let amount_out_after_fee = if input_mint == self.quote_token_pk {
            let transfer_fee = get_transfer_fee(token_in_mint, amount_in)?;
            let amount_in_after_fee = amount_in.checked_sub(transfer_fee).unwrap();

            let amount_in_after_fees = (amount_in_after_fee as f64 * (1.0 - self.fee_rate)) as u128;

            let amount_out: u64 =
                self.calculate_buy_amount_out(base_reserve, quote_reserve, amount_in_after_fees)?;

            let transfer_fee_out = get_transfer_fee(token_out_mint, amount_out)?;
            let amount_out_after_fee = amount_out.checked_sub(transfer_fee_out).unwrap();
            amount_out_after_fee.min(output_reserve)
        } else {
            // Selling base for quote: fee is applied on quote OUTPUT (not base input)
            let transfer_fee = get_transfer_fee(token_in_mint, amount_in)?;
            let amount_in_after_fee = amount_in.checked_sub(transfer_fee).unwrap();

            // No pool fee on base input
            let amount_out = self.calculate_sell_amount_out(base_reserve, quote_reserve, amount_in_after_fee as u128)?;

            // Apply pool fee on quote output
            let amount_out_after_pool_fee = (amount_out as f64 * (1.0 - self.fee_rate)) as u64;

            let transfer_fee_out = get_transfer_fee(token_out_mint, amount_out_after_pool_fee)?;
            let amount_out_after_fee = amount_out_after_pool_fee.checked_sub(transfer_fee_out).unwrap();
            amount_out_after_fee.min(output_reserve)
        };
        Ok(amount_out_after_fee)
    }

    fn get_max_amount_in<'a>(&self, _accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_in) } else { Ok(self.sell_max_in) }
    }

    fn get_max_amount_out<'a>(&self, _accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_out) } else { Ok(self.sell_max_out) }
    }

    fn get_cached_max_amounts(&self, input_mint: Pubkey) -> (u64, u64) {
        if input_mint == self.base_token_pk { (self.buy_max_in, self.buy_max_out) } else { (self.sell_max_in, self.sell_max_out) }
    }

    fn has_output_liquidity(&self, input_mint: Pubkey) -> bool {
        if input_mint == self.base_token_pk {
            self.buy_max_out > 0
        } else {
            self.sell_max_out > 0
        }
    }



    fn swap_base_out<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        _clock: &Clock,
    ) -> Result<u64> {
        return self.swap_base_out_impl(accounts, output_mint, amount_out);
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        // price : Base -> Quote
        // inverse_price : Quote -> Base
        // Calculate prices dynamically from current vault amounts
        Ok((self.price, self.inverse_price))
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
        if input_mint == self.base_token_pk {
            return self.invoke_swap_base_out(
                accounts,
                input_mint,
                max_amount_in,
                amount_out,
                payer,
                user_mint_1_token_account,
                user_mint_2_token_account,
                mint_1_account,
                mint_2_account,
                mint_1_token_program,
                mint_2_token_program,
            );
        }

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

        let amount_out_value = self.swap_base_in(accounts, input_mint, max_amount_in, &Clock::default())?;
        // Get stored accounts from self.get_accounts() - these are the accounts stored in the struct
        let program_id_stored = &accounts[self.start_index + Self::PROGRAM_ID_IDX];
        let pool_id = &accounts[self.start_index + Self::POOL_ID_IDX];
        let base_vault = &accounts[self.start_index + Self::BASE_VAULT_IDX];
        let quote_vault = &accounts[self.start_index + Self::QUOTE_VAULT_IDX];
        let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
        let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];
        let protocol_fee_recipient = &accounts[self.start_index + Self::PROTOCOL_FEE_RECIPIENT_IDX];
        let protocol_fee_token_account =
            &accounts[self.start_index + Self::PROTOCOL_FEE_TOKEN_ACCOUNT_IDX];
        let event_authority = &accounts[self.start_index + Self::EVENT_AUTHORITY_IDX];
        let fee_config = &accounts[self.start_index + Self::FEE_CONFIG_IDX];
        let fee_program = &accounts[self.start_index + Self::FEE_PROGRAM_IDX];
        let user_volume_accumulator =
            &accounts[self.start_index + Self::USER_VOLUME_ACCUMULATOR_IDX];
        let pump_amm_global = &accounts[self.start_index + Self::PUMP_AMM_GLOBAL_IDX];
        let system_program = &accounts[self.start_index + Self::SYSTEM_PROGRAM_IDX];
        let associated_token_instruction_program =
            &accounts[self.start_index + Self::ASSOCIATED_TOKEN_INSTRUCTION_PROGRAM_IDX];
        let global_vol_accumulator = &accounts[self.start_index + Self::GLOBAL_VOL_ACCUMULATOR_IDX];
        let cashback_vault = &accounts[self.start_index + Self::CASHBACK_VAULT_IDX];
        
        // Extract optional vault_ata and vault_authority if present
        let (vault_ata, vault_authority) = if accounts.len() >= self.start_index + 18 {
            (
                Some(&accounts[self.start_index + 16]),
                Some(&accounts[self.start_index + 17]),
            )
        } else {
            (None, None)
        };

        let mut metas = Vec::with_capacity(23);
        metas.push(AccountMeta::new(*pool_id.key, false));
        metas.push(AccountMeta::new(*payer.key, true));
        metas.push(AccountMeta::new_readonly(*pump_amm_global.key, false));
        metas.push(AccountMeta::new_readonly(*base_token.key, false));
        metas.push(AccountMeta::new_readonly(*quote_token.key, false));
        metas.push(AccountMeta::new(*user_base_token_account.key, false));
        metas.push(AccountMeta::new(*user_quote_token_account.key, false));
        metas.push(AccountMeta::new(*base_vault.key, false));
        metas.push(AccountMeta::new(*quote_vault.key, false));
        metas.push(AccountMeta::new_readonly(*protocol_fee_recipient.key, false));
        metas.push(AccountMeta::new(*protocol_fee_token_account.key, false));
        metas.push(AccountMeta::new_readonly(*base_token_program.key, false));
        metas.push(AccountMeta::new_readonly(*quote_token_program.key, false));
        metas.push(AccountMeta::new_readonly(*system_program.key, false));
        metas.push(AccountMeta::new_readonly(*associated_token_instruction_program.key, false));
        metas.push(AccountMeta::new_readonly(*event_authority.key, false));
        metas.push(AccountMeta::new_readonly(Self::PROGRAM_ID, false));
        if let (Some(vault_ata_acc), Some(vault_authority_acc)) = (vault_ata, vault_authority) {
            metas.push(AccountMeta::new(*vault_ata_acc.key, false));
            metas.push(AccountMeta::new_readonly(*vault_authority_acc.key, false));
        }
        metas.push(AccountMeta::new_readonly(*global_vol_accumulator.key, false));
        metas.push(AccountMeta::new(*user_volume_accumulator.key, false));
        metas.push(AccountMeta::new_readonly(*fee_config.key, false));
        metas.push(AccountMeta::new_readonly(*fee_program.key, false));
        metas.push(AccountMeta::new_readonly(*cashback_vault.key, false));

        let mut data = vec![0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea];
        data.extend_from_slice(&amount_out_value.to_le_bytes());
        data.extend_from_slice(&max_amount_in.to_le_bytes());

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data,
        };
        // Pre-allocated accounts - avoids reallocation
        let mut accounts_vec: Vec<AccountInfo<'a>> = Vec::with_capacity(23);
        accounts_vec.push(pool_id.clone());
        accounts_vec.push(unsafe { std::mem::transmute(payer.to_account_info()) });
        accounts_vec.push(pump_amm_global.clone());
        accounts_vec.push(base_token.clone());
        accounts_vec.push(quote_token.clone());
        accounts_vec.push(unsafe { std::mem::transmute(user_base_token_account.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(user_quote_token_account.to_account_info()) });
        accounts_vec.push(base_vault.clone());
        accounts_vec.push(quote_vault.clone());
        accounts_vec.push(protocol_fee_recipient.clone());
        accounts_vec.push(protocol_fee_token_account.clone());
        accounts_vec.push(unsafe { std::mem::transmute(base_token_program.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(quote_token_program.to_account_info()) });
        accounts_vec.push(system_program.clone());
        accounts_vec.push(associated_token_instruction_program.clone());
        accounts_vec.push(event_authority.clone());
        accounts_vec.push(program_id_stored.clone());
        accounts_vec.push(cashback_vault.clone());

        if let (Some(vault_ata_acc), Some(vault_authority_acc)) = (vault_ata, vault_authority) {
            accounts_vec.push(vault_ata_acc.clone());
            accounts_vec.push(vault_authority_acc.clone());
        }

        accounts_vec.push(global_vol_accumulator.clone());
        accounts_vec.push(user_volume_accumulator.clone());
        accounts_vec.push(fee_config.clone());
        accounts_vec.push(fee_program.clone());

        unsafe {
            let accounts_slice: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts_slice)?;
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
        if input_mint == self.quote_token_pk {
            return self.invoke_swap_base_in(
                accounts,
                input_mint,
                amount_in,
                min_amount_out,
                payer,
                user_mint_1_token_account,
                user_mint_2_token_account,
                mint_1_account,
                mint_2_account,
                mint_1_token_program,
                mint_2_token_program,
            );
        }
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

        // Get stored accounts from self.get_accounts() - these are the accounts stored in the struct
        let program_id = &accounts[self.start_index + Self::PROGRAM_ID_IDX];
        let pool_id = &accounts[self.start_index + Self::POOL_ID_IDX];
        let base_vault = &accounts[self.start_index + Self::BASE_VAULT_IDX];
        let quote_vault = &accounts[self.start_index + Self::QUOTE_VAULT_IDX];
        let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
        let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];
        let protocol_fee_recipient = &accounts[self.start_index + Self::PROTOCOL_FEE_RECIPIENT_IDX];
        let protocol_fee_token_account =
            &accounts[self.start_index + Self::PROTOCOL_FEE_TOKEN_ACCOUNT_IDX];
        let event_authority = &accounts[self.start_index + Self::EVENT_AUTHORITY_IDX];
        let fee_config = &accounts[self.start_index + Self::FEE_CONFIG_IDX];
        let fee_program = &accounts[self.start_index + Self::FEE_PROGRAM_IDX];
        let user_volume_accumulator =
            &accounts[self.start_index + Self::USER_VOLUME_ACCUMULATOR_IDX];
        let pump_amm_global = &accounts[self.start_index + Self::PUMP_AMM_GLOBAL_IDX];
        let system_program = &accounts[self.start_index + Self::SYSTEM_PROGRAM_IDX];
        let associated_token_instruction_program =
            &accounts[self.start_index + Self::ASSOCIATED_TOKEN_INSTRUCTION_PROGRAM_IDX];
        let global_vol_accumulator = &accounts[self.start_index + Self::GLOBAL_VOL_ACCUMULATOR_IDX];
        let cashback_vault = &accounts[self.start_index + Self::CASHBACK_VAULT_IDX];

        // Extract optional vault_ata and vault_authority if present
        let (vault_ata, vault_authority) = if accounts.len() >= self.start_index + Self::VAULT_AUTHORITY_IDX + 1 {
            (
                Some(&accounts[self.start_index + Self::VAULT_ATA_IDX]),
                Some(&accounts[self.start_index + Self::VAULT_AUTHORITY_IDX]),
            )
        } else {
            (None, None)
        };

        // Note: payer, user_base_token_account, user_quote_token_account, base_token_program, quote_token_program
        // are function parameters (already available from lines 442-463)

        let min_amount_out_value = min_amount_out.unwrap_or(0);
        let mut metas = Vec::with_capacity(21);
        metas.push(AccountMeta::new(*pool_id.key, false));
        metas.push(AccountMeta::new(*payer.key, true));
        metas.push(AccountMeta::new_readonly(*pump_amm_global.key, false));
        metas.push(AccountMeta::new_readonly(*base_token.key, false));
        metas.push(AccountMeta::new_readonly(*quote_token.key, false));
        metas.push(AccountMeta::new(*user_base_token_account.key, false));
        metas.push(AccountMeta::new(*user_quote_token_account.key, false));
        metas.push(AccountMeta::new(*base_vault.key, false));
        metas.push(AccountMeta::new(*quote_vault.key, false));
        metas.push(AccountMeta::new_readonly(*protocol_fee_recipient.key, false));
        metas.push(AccountMeta::new(*protocol_fee_token_account.key, false));
        metas.push(AccountMeta::new_readonly(*base_token_program.key, false));
        metas.push(AccountMeta::new_readonly(*quote_token_program.key, false));
        metas.push(AccountMeta::new_readonly(*system_program.key, false));
        metas.push(AccountMeta::new_readonly(*associated_token_instruction_program.key, false));
        metas.push(AccountMeta::new_readonly(*event_authority.key, false));
        metas.push(AccountMeta::new_readonly(*program_id.key, false));
        if let (Some(vault_ata_acc), Some(vault_authority_acc)) = (vault_ata, vault_authority) {
            metas.push(AccountMeta::new(*vault_ata_acc.key, false));
            metas.push(AccountMeta::new_readonly(*vault_authority_acc.key, false));
        }
        metas.push(AccountMeta::new_readonly(*fee_config.key, false));
        metas.push(AccountMeta::new_readonly(*fee_program.key, false));
        metas.push(AccountMeta::new_readonly(*cashback_vault.key, false));

        let mut data = vec![0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad];
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out_value.to_le_bytes());

        let swap_ix = Instruction {
            program_id: *program_id.key,
            accounts: metas,
            data,
        };

        // Pre-allocated accounts - avoids reallocation
        let mut accounts_vec: Vec<AccountInfo<'a>> = Vec::with_capacity(23);
        accounts_vec.push(pool_id.clone());
        accounts_vec.push(unsafe { std::mem::transmute(payer.to_account_info()) });
        accounts_vec.push(pump_amm_global.clone());
        accounts_vec.push(base_token.clone());
        accounts_vec.push(quote_token.clone());
        accounts_vec.push(unsafe { std::mem::transmute(user_base_token_account.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(user_quote_token_account.to_account_info()) });
        accounts_vec.push(base_vault.clone());
        accounts_vec.push(quote_vault.clone());
        accounts_vec.push(protocol_fee_recipient.clone());
        accounts_vec.push(protocol_fee_token_account.clone());
        accounts_vec.push(unsafe { std::mem::transmute(base_token_program.to_account_info()) });
        accounts_vec.push(unsafe { std::mem::transmute(quote_token_program.to_account_info()) });
        accounts_vec.push(system_program.clone());
        accounts_vec.push(associated_token_instruction_program.clone());
        accounts_vec.push(event_authority.clone());
        accounts_vec.push(program_id.clone());

        if let (Some(vault_ata_acc), Some(vault_authority_acc)) = (vault_ata, vault_authority) {
            accounts_vec.push(vault_ata_acc.clone());
            accounts_vec.push(vault_authority_acc.clone());
        }
        accounts_vec.push(global_vol_accumulator.clone());
        accounts_vec.push(user_volume_accumulator.clone());
        accounts_vec.push(fee_config.clone());
        accounts_vec.push(fee_program.clone());
        accounts_vec.push(cashback_vault.clone());
        
        unsafe {
            let accounts_slice: &[AccountInfo<'a>] = std::mem::transmute(accounts_vec.as_slice());
            invoke(&swap_ix, accounts_slice)?;
        }
        Ok(())
    }
    

    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        msg!("=== Pump AMM ===");
        msg!("0 program_id: {}", accounts[self.start_index + Self::PROGRAM_ID_IDX].key);
        msg!("1 pool_id: {}", accounts[self.start_index + Self::POOL_ID_IDX].key);
        msg!("2 base_vault: {}", accounts[self.start_index + Self::BASE_VAULT_IDX].key);
        msg!("3 quote_vault: {}", accounts[self.start_index + Self::QUOTE_VAULT_IDX].key);
        msg!("4 base_token: {}", accounts[self.start_index + Self::BASE_TOKEN_IDX].key);
        msg!("5 quote_token: {}", accounts[self.start_index + Self::QUOTE_TOKEN_IDX].key);
        msg!("6 protocol_fee_recipient: {}", accounts[self.start_index + Self::PROTOCOL_FEE_RECIPIENT_IDX].key);
        msg!("7 protocol_fee_token_account: {}", accounts[self.start_index + Self::PROTOCOL_FEE_TOKEN_ACCOUNT_IDX].key);
        msg!("8 event_authority: {}", accounts[self.start_index + Self::EVENT_AUTHORITY_IDX].key);
        msg!("9 fee_config: {}", accounts[self.start_index + Self::FEE_CONFIG_IDX].key);
        msg!("10 fee_program: {}", accounts[self.start_index + Self::FEE_PROGRAM_IDX].key);
        msg!("11 user_volume_accumulator: {}", accounts[self.start_index + Self::USER_VOLUME_ACCUMULATOR_IDX].key);
        msg!("12 pump_amm_global: {}", accounts[self.start_index + Self::PUMP_AMM_GLOBAL_IDX].key);
        msg!("13 system_program: {}", accounts[self.start_index + Self::SYSTEM_PROGRAM_IDX].key);
        msg!("14 associated_token_program: {}", accounts[self.start_index + Self::ASSOCIATED_TOKEN_INSTRUCTION_PROGRAM_IDX].key);
        msg!("15 global_vol_accumulator: {}", accounts[self.start_index + Self::GLOBAL_VOL_ACCUMULATOR_IDX].key);
        msg!("16 cashback_vault: {}", accounts[self.start_index + Self::CASHBACK_VAULT_IDX].key);
        msg!("17 vault_ata: {}", accounts[self.start_index + Self::VAULT_ATA_IDX].key);
        msg!("18 vault_authority: {}", accounts[self.start_index + Self::VAULT_AUTHORITY_IDX].key);
        Ok(())
    }

}

impl<'info> PumpAmm<'info> {
    pub const PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
    pub const PROGRAM_ID_IDX: usize = 0;
    pub const POOL_ID_IDX: usize = 1;
    pub const BASE_VAULT_IDX: usize = 2;
    pub const QUOTE_VAULT_IDX: usize = 3;
    pub const BASE_TOKEN_IDX: usize = 4;
    pub const QUOTE_TOKEN_IDX: usize = 5;
    pub const PROTOCOL_FEE_RECIPIENT_IDX: usize = 6;
    pub const PROTOCOL_FEE_TOKEN_ACCOUNT_IDX: usize = 7;
    pub const EVENT_AUTHORITY_IDX: usize = 8;
    pub const FEE_CONFIG_IDX: usize = 9;
    pub const FEE_PROGRAM_IDX: usize = 10;
    pub const USER_VOLUME_ACCUMULATOR_IDX: usize = 11;
    pub const PUMP_AMM_GLOBAL_IDX: usize = 12;
    pub const SYSTEM_PROGRAM_IDX: usize = 13;
    pub const ASSOCIATED_TOKEN_INSTRUCTION_PROGRAM_IDX: usize = 14;
    pub const GLOBAL_VOL_ACCUMULATOR_IDX: usize = 15;
    pub const CASHBACK_VAULT_IDX: usize = 16;
    pub const VAULT_ATA_IDX: usize = 17;
    pub const VAULT_AUTHORITY_IDX: usize = 18;

    pub fn new(
        accounts: &[AccountInfo<'info>],
        start_index: usize,
        end_index: usize,
    ) -> Result<Self> {
        // let mut iter = accounts.iter();
        // let program_id = accounts[0].clone(); // 0
        // let pool_id = accounts[1].clone(); // 1
        let pool_id = accounts[start_index + Self::POOL_ID_IDX].clone(); // 1
        let base_vault = accounts[start_index + Self::BASE_VAULT_IDX].clone(); // 2
        let quote_vault = accounts[start_index + Self::QUOTE_VAULT_IDX].clone(); // 3
        let base_token = accounts[start_index + Self::BASE_TOKEN_IDX].clone(); // 4
        let quote_token = accounts[start_index + Self::QUOTE_TOKEN_IDX].clone(); // 5

        let base_vault_amount = parse_token_account(&base_vault)?.amount;
        let quote_vault_amount = parse_token_account(&quote_vault)?.amount;

        // eprintln!("base_vault_amount: {:?}", base_vault_amount / 1_000_000_000);
        // eprintln!("quote_vault_amount: {:?}", quote_vault_amount / 1_000_000);
        // TODO: maket to run in test
        // let base_vault_amount: u64 = (base_vault_amount as f64 * 0.85) as u64;
        // let quote_vault_amount: u64 = quote_vault_amount;

        // eprintln!("base_vault_amount: {:?}", base_vault_amount);
        // eprintln!("quote_vault_amount: {:?}", quote_vault_amount);

        let (price, inverse_price) = get_prices(base_vault_amount, quote_vault_amount)?;
        let fee_rate = get_fees(price, inverse_price)?;

        // Cache max amounts for both directions
        let fee_factor = 1.0 - fee_rate;
        let (buy_max_in, buy_max_out) = {
            // base→quote: fee on output
            let (x, y) = (base_vault_amount as f64, quote_vault_amount as f64);
            let eff = y * fee_factor;
            let target = eff * 0.99;
            if target <= 0.0 || fee_factor * y <= target {
                (0, y as u64)
            } else {
                let dx = (x * target) / (fee_factor * y - target);
                (dx.max(0.0).min(u64::MAX as f64) as u64, y as u64)
            }
        };
        let (sell_max_in, sell_max_out) = {
            // quote→base: fee on input
            let (x, y) = (quote_vault_amount as f64, base_vault_amount as f64);
            let target = y * 0.99;
            if target <= 0.0 || y <= target {
                (0, y as u64)
            } else {
                let dx = (x * target) / (fee_factor * (y - target));
                (dx.max(0.0).min(u64::MAX as f64) as u64, y as u64)
            }
        };

        let instance = PumpAmm {
            price,
            inverse_price,
            fee_rate,
            pool_id: *pool_id.key,
            base_token_pk: *base_token.key,
            quote_token_pk: *quote_token.key,
            base_vault_amount,
            quote_vault_amount,
            start_index,
            end_index,
            buy_max_in,
            buy_max_out,
            sell_max_in,
            sell_max_out,
            _phantom: PhantomData,
        };
        // instance.log_accounts(accounts)?;
        Ok(instance)
    }

    pub fn calculate_buy_amount_out(
        &self,
        base_reserve: u128,
        quote_reserve: u128,
        amount_in: u128,
    ) -> Result<u64> {
        // Constant Product Formula: y_out = y - (x * y) / (x + x_in)
        let numerator = base_reserve
            .checked_mul(quote_reserve)
            .ok_or(ProgramError::InvalidArgument)?;

        let denominator = quote_reserve
            .checked_add(amount_in)
            .ok_or(ProgramError::InvalidArgument)?;

        let quotient = numerator
            .checked_div(denominator)
            .ok_or(ProgramError::InvalidArgument)?;

        let amount_out = base_reserve
            .checked_sub(quotient)
            .ok_or(ProgramError::InvalidArgument)?;

        let amount_out_u64 =
            u64::try_from(amount_out).map_err(|_| ProgramError::InvalidArgument)?;

        Ok(amount_out_u64)
    }

    pub fn calculate_sell_amount_out(
        &self,
        base_reserve: u128,
        quote_reserve: u128,
        amount_in: u128,
    ) -> Result<u64> {
        let numerator = base_reserve
            .checked_mul(quote_reserve)
            .ok_or(ProgramError::InvalidArgument)?;
        let denominator = base_reserve
            .checked_add(amount_in)
            .ok_or(ProgramError::InvalidArgument)?;
        let quotient = numerator
            .checked_div(denominator)
            .ok_or(ProgramError::InvalidArgument)?;
        let quote_amount_out = quote_reserve
            .checked_sub(quotient)
            .ok_or(ProgramError::InvalidArgument)?;

        let amount_out_u64 =
            u64::try_from(quote_amount_out).map_err(|_| ProgramError::InvalidArgument)?;
        Ok(amount_out_u64)
    }
    /// Calculate base output amount for a given quote input amount
    /// Formula: base_amount_out = base_reserve - (base_reserve * quote_reserve) / (quote_reserve + quote_amount_in)
    /// Then applies 0.02% fee (multiply by 0.9998)
    pub fn swap_base_in_impl<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
    ) -> Result<u64> {
        // if input_mint == self.base_token_pk {
        //     return self.swap_base_out_impl(accounts, input_mint, amount_in);
        // }

        // Use u128 math to avoid overflow on large vaults
        let base_reserve = self.base_vault_amount as u128;
        let quote_reserve = self.quote_vault_amount as u128;

        let output_reserve = if input_mint == self.base_token_pk {
            self.quote_vault_amount
        } else {
            self.base_vault_amount
        };

        let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
        let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];

        let (token_in_mint, token_out_mint) = if input_mint == self.base_token_pk {
            (base_token, quote_token)
        } else {
            (quote_token, base_token)
        };

        let amount_out_after_fee = if input_mint == self.quote_token_pk {
            let transfer_fee = get_transfer_fee(token_in_mint, amount_in)?;
            let amount_in_after_fee = amount_in.checked_sub(transfer_fee).unwrap();

            let amount_in_after_fees = (amount_in_after_fee as f64 * (1.0 - self.fee_rate)) as u128;

            let amount_out: u64 =
                self.calculate_buy_amount_out(base_reserve, quote_reserve, amount_in_after_fees)?;

            let transfer_fee_out = get_transfer_fee(token_out_mint, amount_out)?;
            let amount_out_after_fee = amount_out.checked_sub(transfer_fee_out).unwrap();
            amount_out_after_fee.min(output_reserve)
        } else {
            // Selling base for quote: fee is applied on quote OUTPUT (not base input)
            let transfer_fee = get_transfer_fee(token_in_mint, amount_in)?;
            let amount_in_after_fee = amount_in.checked_sub(transfer_fee).unwrap();

            // No pool fee on base input
            let amount_out = self.calculate_sell_amount_out(base_reserve, quote_reserve, amount_in_after_fee as u128)?;

            // Apply pool fee on quote output
            let amount_out_after_pool_fee = (amount_out as f64 * (1.0 - self.fee_rate)) as u64;

            let transfer_fee_out = get_transfer_fee(token_out_mint, amount_out_after_pool_fee)?;
            let amount_out_after_fee = amount_out_after_pool_fee.checked_sub(transfer_fee_out).unwrap();
            amount_out_after_fee.min(output_reserve)
        };
        Ok(amount_out_after_fee)
    }

    /// Calculate the maximum input amount required to receive a specific output amount
    /// This is the inverse of swap_base_in_impl
    /// Given output_mint and amount_out, returns the required amount_in
    /// Note: Pool fee is only applied on the QUOTE side (not base)
    pub fn swap_base_out_impl<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
    ) -> Result<u64> {
        let base_reserve = self.base_vault_amount as u128;
        let quote_reserve = self.quote_vault_amount as u128;

        // Cap amount_out to output reserve
        let output_reserve = if output_mint == self.base_token_pk {
            self.base_vault_amount
        } else {
            self.quote_vault_amount
        };
        let amount_out = amount_out.min(output_reserve.saturating_sub(1));

        let base_token = &accounts[self.start_index + Self::BASE_TOKEN_IDX];
        let quote_token = &accounts[self.start_index + Self::QUOTE_TOKEN_IDX];

        let (token_in_mint, token_out_mint) = if output_mint == self.base_token_pk {
            // Output is base, input is quote
            (quote_token, base_token)
        } else {
            // Output is quote, input is base
            (base_token, quote_token)
        };

        let max_amount_in = if output_mint == self.base_token_pk {
            // Buying base with quote: calculate quote_in for desired base_out
            // Fee is applied on quote (input side)

            // Add transfer fee to desired output
            let transfer_fee_out = get_transfer_fee(token_out_mint, amount_out)?;
            let amount_out_before_transfer_fee = (amount_out as u128)
                .checked_add(transfer_fee_out as u128)
                .ok_or(ProgramError::InvalidArgument)?;

            // Inverse formula: quote_in = (quote_reserve * base_out) / (base_reserve - base_out)
            let numerator = quote_reserve
                .checked_mul(amount_out_before_transfer_fee)
                .ok_or(ProgramError::InvalidArgument)?;
            let denominator = base_reserve
                .checked_sub(amount_out_before_transfer_fee)
                .ok_or(ProgramError::InvalidArgument)?;
            let amount_in_after_fee = numerator
                .checked_div(denominator)
                .ok_or(ProgramError::InvalidArgument)?
                .checked_add(1) // Round up
                .ok_or(ProgramError::InvalidArgument)?;

            // Add back pool fee: amount_in_before_fee = amount_in_after_fee / (1 - fee)
            let amount_in_before_fee = (amount_in_after_fee as f64 / (1.0 - self.fee_rate)) as u128;

            // Add transfer fee on input
            let amount_in_u64 = u64::try_from(amount_in_before_fee)
                .map_err(|_| ProgramError::InvalidArgument)?;
            let transfer_fee_in = get_transfer_fee(token_in_mint, amount_in_u64)?;
            amount_in_u64
                .checked_add(transfer_fee_in)
                .ok_or(ProgramError::InvalidArgument)?
        } else {
            // Selling base for quote: calculate base_in for desired quote_out
            // Fee is applied on quote (output side), NOT on base (input side)

            // Add transfer fee to desired output
            let transfer_fee_out = get_transfer_fee(token_out_mint, amount_out)?;
            let amount_out_before_transfer_fee = (amount_out as u128)
                .checked_add(transfer_fee_out as u128)
                .ok_or(ProgramError::InvalidArgument)?;

            // Add back pool fee on quote output: quote_before_fee = quote_after_fee / (1 - fee)
            let quote_out_before_fee = (amount_out_before_transfer_fee as f64 / (1.0 - self.fee_rate)) as u128;

            // Inverse formula: base_in = (base_reserve * quote_out) / (quote_reserve - quote_out)
            let numerator = base_reserve
                .checked_mul(quote_out_before_fee)
                .ok_or(ProgramError::InvalidArgument)?;
            let denominator = quote_reserve
                .checked_sub(quote_out_before_fee)
                .ok_or(ProgramError::InvalidArgument)?;
            let base_in = numerator
                .checked_div(denominator)
                .ok_or(ProgramError::InvalidArgument)?
                .checked_add(1) // Round up
                .ok_or(ProgramError::InvalidArgument)?;

            // Add transfer fee on input (no pool fee on base)
            let amount_in_u64 = u64::try_from(base_in)
                .map_err(|_| ProgramError::InvalidArgument)?;
            let transfer_fee_in = get_transfer_fee(token_in_mint, amount_in_u64)?;
            amount_in_u64
                .checked_add(transfer_fee_in)
                .ok_or(ProgramError::InvalidArgument)?
        };

        Ok(max_amount_in)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::{account_info::AccountInfo, pubkey::Pubkey, system_program};

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

    // Helper function to create a minimal mock AccountInfo
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

    async fn build_test_scenario() -> (PumpAmm<'static>, Vec<AccountInfo<'static>>) {
        use anchor_client::Cluster;
        use solana_client::nonblocking::rpc_client::RpcClient;

        // RPC client pointing to mainnet
        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        // Pool ID from mainnet
        let base_vault_key = Pubkey::from_str_const("48Y6uKg1kqyACUMCpGgbfQhaiKBkjov89JLGCkgC1ZSz");
        let quote_vault_key =
            Pubkey::from_str_const("9NKqscTAEJ3c6EkVN2imhhS6wE9gBxgtfxybUXrLB9jR");
        let base_token_key = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let quote_token_key =
            Pubkey::from_str_const("6u2fHHSU75GjarRQdCgGqAbkSD9Bnas7te8HeCY3pump");

        let base_vault_account = fetch_account_info_from_rpc(&rpc_client, base_vault_key).await;
        let quote_vault_account = fetch_account_info_from_rpc(&rpc_client, quote_vault_key).await;
        let base_token_account = fetch_account_info_from_rpc(&rpc_client, base_token_key).await;
        let quote_token_account = fetch_account_info_from_rpc(&rpc_client, quote_token_key).await;

        let program_id = create_mock_account_info(PumpAmm::PROGRAM_ID, system_program::id(), None);
        let pool_id = create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let protocol_fee_recipient =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let protocol_fee_token_account =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let event_authority =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let fee_config = create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let fee_program =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let user_volume_accumulator =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let pump_amm_global =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let system_program_account =
            create_mock_account_info(system_program::id(), system_program::id(), None);
        let associated_token_instruction_program =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let global_vol_accumulator =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let vault_ata = create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);
        let vault_authority =
            create_mock_account_info(Pubkey::new_unique(), system_program::id(), None);

        eprintln!("Base vault account: {:?}", base_vault_key);
        eprintln!("Quote vault account: {:?}", quote_vault_key);
        eprintln!("Base token account: {:?}", base_token_key);
        eprintln!("Quote token account: {:?}", quote_token_key);

        let accounts = vec![
            program_id,                           // 0
            pool_id,                              // 1
            base_vault_account.clone(),           // 2
            quote_vault_account.clone(),          // 3
            base_token_account.clone(),           // 4
            quote_token_account.clone(),          // 5
            protocol_fee_recipient,               // 6
            protocol_fee_token_account,           // 7
            event_authority,                      // 8
            fee_config,                           // 9
            fee_program,                          // 10
            user_volume_accumulator,              // 11
            pump_amm_global,                      // 12
            system_program_account,               // 13
            associated_token_instruction_program, // 14
            global_vol_accumulator,               // 15
            vault_ata,                            // 16
            vault_authority,                      // 17
        ];

        let pump_amm = PumpAmm::new(accounts.as_slice(), 0, accounts.len()).unwrap();

        (pump_amm, accounts)
    }

    // Helper function to create a mock AccountInfo with TokenAccount data
    #[tokio::test]
    async fn test_pump_amm_swap() {
        let (pump_amm, accounts) = build_test_scenario().await;

        let prices = pump_amm.get_prices().unwrap();
        let price = prices.0;
        let inverse_price = prices.1;
        eprintln!("Price: {:?}", price);
        eprintln!("Inverse price: {:?}", inverse_price);

        // Test with quote_amount_in = 10_000_000
        let quote_amount_in: u64 = 1_000_000_000;
        let clock = Clock::default();
        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112"); // Use quote_mint directly since quote_token was moved into accounts
        eprintln!("================================================");
        let amount_out_1 = pump_amm
            .swap_base_in(accounts.as_slice(), sol_mint, quote_amount_in, &clock)
            .unwrap();
        let (sol_price, inverse_sol_price) = if pump_amm.base_token_pk == sol_mint {
            (price, inverse_price)
        } else {
            (inverse_price, price)
        };
        let amount_out_1_v2 = quote_amount_in as f64 * sol_price;
        eprintln!(
            "SOL {:?} -> TOKEN {:?} TOKEN V2: {:?}",
            quote_amount_in as f64 / 1_000_000_000.0,
            amount_out_1 as f64 / 1_000_000.0,
            amount_out_1_v2 as f64 / 1_000_000.0
        );

        eprintln!("================================================");
        let token_mint = if pump_amm.base_token_pk == sol_mint {
            pump_amm.quote_token_pk
        } else {
            pump_amm.base_token_pk
        };
        let amount_out_2 = pump_amm
            .swap_base_out_impl(
                accounts.as_slice(),
                token_mint,
                amount_out_1_v2 as u64,
            )
            .unwrap();
        let amount_received_v2_2 = amount_out_1_v2 as f64 * inverse_sol_price;
        eprintln!(
            "TOKEN {:?} -> SOL {:?} SOL V2: {:?}",
            amount_out_1 as f64 / 1_000_000.0,
            amount_out_2 as f64 / 1_000_000_000.0,
            amount_received_v2_2 as f64 / 1_000_000_000.0
        );
    }
}
