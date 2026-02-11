use anchor_lang::prelude::*;
use anchor_lang::solana_program::pubkey::Pubkey;

// Import all program types (uncomment as needed)
// use crate::programs::meteora_damm_v1::MeteoraDammV1;
use crate::programs::meteora_damm_v2::MeteoraDammV2;
use crate::programs::meteora_dlmm::MeteoraDlmm;
use crate::programs::orca::OrcaWhirlpool;
use crate::programs::pump_amm::PumpAmm;
use crate::programs::raydium_cpmm::RaydiumCPMM;
use crate::programs::raydium_amm::RaydiumAmm;
use crate::programs::raydium_clmm::RaydiumCLMM;

/// Enum to hold different program instances - eliminates lifetime complexity
pub enum ProgramInstance<'info> {
    MeteoraDlmm(MeteoraDlmm<'info>),
    // MeteoraDammV1(MeteoraDammV1<'info>),
    MeteoraDammV2(MeteoraDammV2<'info>),
    OrcaWhirlpool(OrcaWhirlpool<'info>),
    PumpAmm(PumpAmm<'info>),
    RaydiumAmm(RaydiumAmm<'info>),
    RaydiumCPMM(RaydiumCPMM<'info>),
    RaydiumCLMM(RaydiumCLMM<'info>),
}

impl<'info> ProgramInstance<'info> {
    pub fn get_id(&self) -> &Pubkey {
        match self {
            ProgramInstance::MeteoraDlmm(p) => p.get_id(),
            ProgramInstance::MeteoraDammV2(p) => p.get_id(),
            ProgramInstance::OrcaWhirlpool(p) => p.get_id(),
            ProgramInstance::PumpAmm(p) => p.get_id(),
            ProgramInstance::RaydiumAmm(p) => p.get_id(),
            ProgramInstance::RaydiumCPMM(p) => p.get_id(),
            ProgramInstance::RaydiumCLMM(p) => p.get_id(),
        }
    }

    pub fn get_pool_id(&self) -> &Pubkey {
        match self {
            ProgramInstance::MeteoraDlmm(p) => p.get_pool_id(),
            ProgramInstance::MeteoraDammV2(p) => p.get_pool_id(),
            ProgramInstance::OrcaWhirlpool(p) => p.get_pool_id(),
            ProgramInstance::PumpAmm(p) => p.get_pool_id(),
            ProgramInstance::RaydiumAmm(p) => p.get_pool_id(),
            ProgramInstance::RaydiumCLMM(p) => p.get_pool_id(),
            ProgramInstance::RaydiumCPMM(p) => p.get_pool_id(),
        }
    }

    pub fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        match self {
            ProgramInstance::MeteoraDlmm(p) => p.get_mints(),
            ProgramInstance::MeteoraDammV2(p) => p.get_mints(),
            ProgramInstance::OrcaWhirlpool(p) => p.get_mints(),
            ProgramInstance::PumpAmm(p) => p.get_mints(),
            ProgramInstance::RaydiumAmm(p) => p.get_mints(),
            ProgramInstance::RaydiumCLMM(p) => p.get_mints(),
            ProgramInstance::RaydiumCPMM(p) => p.get_mints(),
        }
    }

    pub fn get_prices(&self) -> Result<(f64, f64)> {
        match self {
            ProgramInstance::MeteoraDlmm(p) => p.get_prices(),
            ProgramInstance::MeteoraDammV2(p) => p.get_prices(),
            ProgramInstance::OrcaWhirlpool(p) => p.get_prices(),
            ProgramInstance::PumpAmm(p) => p.get_prices(),
            ProgramInstance::RaydiumAmm(p) => p.get_prices(),
            ProgramInstance::RaydiumCLMM(p) => p.get_prices(),
            ProgramInstance::RaydiumCPMM(p) => p.get_prices(),
        }
    }

    /// Calculate optimal amount in for AMM types (PumpAmm, MeteoraDammV2)
    pub fn calculate_optimal_amount_in(
        &self,
        input_mint: Pubkey,
        target_price: f64,
    ) -> Result<u64> {
        match self {
            ProgramInstance::MeteoraDlmm(_) | ProgramInstance::OrcaWhirlpool(_) => {
                // Concentrated liquidity pools don't implement calculate_optimal_amount_in
                Err(error!(crate::programs::SolarBError::InvalidProgramType))
            }
            ProgramInstance::PumpAmm(p) => p.calculate_optimal_amount_in(input_mint, target_price),
            ProgramInstance::MeteoraDammV2(p) => {
                p.calculate_optimal_amount_in(input_mint, target_price)
            }
            ProgramInstance::RaydiumAmm(p) => p.calculate_optimal_amount_in(input_mint, target_price),
            ProgramInstance::RaydiumCLMM(p) => p.calculate_optimal_amount_in(input_mint, target_price),
            ProgramInstance::RaydiumCPMM(p) => p.calculate_optimal_amount_in(input_mint, target_price),
        }
    }

    /// Get base token pubkey (for AMM types)
    pub fn get_base_token(&self) -> Result<Pubkey> {
        match self {
            ProgramInstance::MeteoraDlmm(p) => Ok(p.base_token_pk),
            ProgramInstance::MeteoraDammV2(p) => Ok(p.base_token_pk),
            ProgramInstance::OrcaWhirlpool(p) => Ok(p.base_token_pk),
            ProgramInstance::PumpAmm(p) => Ok(p.base_token_pk),
            ProgramInstance::RaydiumAmm(p) => Ok(p.base_token_pk),
            ProgramInstance::RaydiumCLMM(p) => Ok(p.base_token_pk),
            ProgramInstance::RaydiumCPMM(p) => Ok(p.base_token_pk),
        }
    }

    /// Get quote token pubkey
    pub fn get_quote_token(&self) -> Result<Pubkey> {
        match self {
            ProgramInstance::MeteoraDlmm(p) => Ok(p.quote_token_pk),
            ProgramInstance::MeteoraDammV2(p) => Ok(p.quote_token_pk),
            ProgramInstance::OrcaWhirlpool(p) => Ok(p.quote_token_pk),
            ProgramInstance::PumpAmm(p) => Ok(p.quote_token_pk),
            ProgramInstance::RaydiumAmm(p) => Ok(p.quote_token_pk),
            ProgramInstance::RaydiumCLMM(p) => Ok(p.quote_token_pk),
            ProgramInstance::RaydiumCPMM(p) => Ok(p.quote_token_pk),
        }
    }

    /// Get reserves for input token direction
    /// Returns (reserve_in, reserve_out) based on input_mint
    pub fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        match self {
            ProgramInstance::MeteoraDlmm(_) | ProgramInstance::OrcaWhirlpool(_) | ProgramInstance::MeteoraDammV2(_) => {
                // Concentrated liquidity pools don't have traditional reserves
                Err(error!(crate::programs::SolarBError::InvalidProgramType))
            }
            ProgramInstance::PumpAmm(p) => p.get_vault_amounts(),
            ProgramInstance::RaydiumAmm(p) => p.get_vault_amounts(),
            ProgramInstance::RaydiumCLMM(p) => p.get_vault_amounts(),
            ProgramInstance::RaydiumCPMM(p) => p.get_vault_amounts(),
        }
    }

    pub fn get_max_amounts_in_out(&self, input_mint: Pubkey) -> Result<(u64, u64)> {
        match self {
            ProgramInstance::MeteoraDlmm(p) => p.get_max_amounts_in_out(input_mint),
            ProgramInstance::MeteoraDammV2(p) => p.get_max_amounts_in_out(input_mint),
            ProgramInstance::OrcaWhirlpool(p) => p.get_max_amounts_in_out(input_mint),
            ProgramInstance::PumpAmm(p) => p.get_max_amounts_in_out(input_mint),
            ProgramInstance::RaydiumAmm(p) => p.get_max_amounts_in_out(input_mint),
            ProgramInstance::RaydiumCLMM(p) => p.get_max_amounts_in_out(input_mint),
            ProgramInstance::RaydiumCPMM(p) => p.get_max_amounts_in_out(input_mint),
        }
    }

    /// Get fee factor (1 - fee_rate) for the AMM
    /// e.g., 0.9875 for 1.25% fee
    pub fn get_fee_factor(&self) -> Result<f64> {
        match self {
            ProgramInstance::MeteoraDlmm(p) => Ok(1.0 - p.fee_rate),
            ProgramInstance::MeteoraDammV2(p) => Ok(1.0 - p.fee_rate),
            ProgramInstance::OrcaWhirlpool(p) => {
                // Orca fee_rate is in hundredths of basis point (1_000_000 = 100%)
                Ok(1.0 - (p.fee_rate as f64 / 1_000_000.0))
            }
            ProgramInstance::PumpAmm(p) => Ok(1.0 - p.fee_rate),
            ProgramInstance::RaydiumAmm(p) => Ok(1.0 - p.fee_rate),
            ProgramInstance::RaydiumCLMM(p) => Ok(1.0 - p.fee_rate),
            ProgramInstance::RaydiumCPMM(p) => Ok(1.0 - p.fee_rate),
        }
    }

    pub fn swap_base_in<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        clock: Clock,
    ) -> Result<u64> {
        match self {
            ProgramInstance::MeteoraDlmm(p) => {
                p.swap_base_in(accounts, input_mint, amount_in, clock)
            }
            ProgramInstance::MeteoraDammV2(p) => {
                p.swap_base_in(accounts, input_mint, amount_in, clock)
            }
            ProgramInstance::OrcaWhirlpool(p) => {
                p.swap_base_in(accounts, input_mint, amount_in, clock)
            }
            ProgramInstance::PumpAmm(p) => p.swap_base_in(accounts, input_mint, amount_in, clock),
            ProgramInstance::RaydiumAmm(p) => p.swap_base_in(accounts, input_mint, amount_in, clock),
            ProgramInstance::RaydiumCLMM(p) => p.swap_base_in(accounts, input_mint, amount_in, clock),
            ProgramInstance::RaydiumCPMM(p) => p.swap_base_in(accounts, input_mint, amount_in, clock),
        }
    }

    pub fn swap_base_out<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        clock: Clock,
    ) -> Result<u64> {
        match self {
            ProgramInstance::MeteoraDlmm(p) => {
                p.swap_base_out(accounts, output_mint, amount_out, clock)
            }
            ProgramInstance::MeteoraDammV2(p) => {
                p.swap_base_out(accounts, output_mint, amount_out, clock)
            }
            ProgramInstance::OrcaWhirlpool(p) => {
                p.swap_base_out(accounts, output_mint, amount_out, clock)
            }
            ProgramInstance::PumpAmm(p) => p.swap_base_out(accounts, output_mint, amount_out, clock),
            ProgramInstance::RaydiumAmm(p) => p.swap_base_out(accounts, output_mint, amount_out, clock),
            ProgramInstance::RaydiumCLMM(p) => p.swap_base_out(accounts, output_mint, amount_out, clock),
            ProgramInstance::RaydiumCPMM(p) => p.swap_base_out(accounts, output_mint, amount_out, clock),
        }
    }

    pub fn invoke_swap_base_in<'a>(
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
        match self {
            ProgramInstance::MeteoraDlmm(p) => p.invoke_swap_base_in(
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
            ),
            ProgramInstance::MeteoraDammV2(p) => p.invoke_swap_base_in(
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
            ),
            ProgramInstance::OrcaWhirlpool(p) => p.invoke_swap_base_in(
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
            ),
            ProgramInstance::PumpAmm(p) => p.invoke_swap_base_in(
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
            ),
            ProgramInstance::RaydiumAmm(p) => p.invoke_swap_base_in(
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
            ),
            ProgramInstance::RaydiumCLMM(p) => p.invoke_swap_base_in(
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
            ),
            ProgramInstance::RaydiumCPMM(p) => p.invoke_swap_base_in(
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
            ),
        }
    }

    pub fn invoke_swap_base_out<'a>(
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
        match self {
            ProgramInstance::MeteoraDlmm(p) => p.invoke_swap_base_out(
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
            ),
            ProgramInstance::MeteoraDammV2(p) => p.invoke_swap_base_out(
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
            ),
            ProgramInstance::OrcaWhirlpool(p) => p.invoke_swap_base_out(
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
            ),
            ProgramInstance::PumpAmm(p) => p.invoke_swap_base_out(
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
            ),
            ProgramInstance::RaydiumAmm(p) => p.invoke_swap_base_out(
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
            ),
            ProgramInstance::RaydiumCLMM(p) => p.invoke_swap_base_out(
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
            ),
            ProgramInstance::RaydiumCPMM(p) => p.invoke_swap_base_out(
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
            ),
        }
    }

    pub fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        match self {
            ProgramInstance::MeteoraDlmm(p) => p.log_accounts(accounts),
            ProgramInstance::MeteoraDammV2(p) => p.log_accounts(accounts),
            ProgramInstance::OrcaWhirlpool(p) => p.log_accounts(accounts),
            ProgramInstance::PumpAmm(p) => p.log_accounts(accounts),
            ProgramInstance::RaydiumAmm(p) => p.log_accounts(accounts),
            ProgramInstance::RaydiumCLMM(p) => p.log_accounts(accounts),
            ProgramInstance::RaydiumCPMM(p) => p.log_accounts(accounts),
        }
    }
}

// Keep the trait for backward compatibility with existing implementations
pub trait ProgramMeta {
    fn get_id(&self) -> &Pubkey;

    /// Get the unique pool ID for this instance
    fn get_pool_id(&self) -> &Pubkey;

    /// Compute price for swap base in (base -> quote)
    fn compute_price_swap_base_in(&self, base_amount: u128, quote_amount: u128) -> Result<f64> {
        if base_amount > 0 {
            Ok(quote_amount as f64 / base_amount as f64)
        } else {
            Ok(0.0)
        }
    }

    /// Compute price for swap base out (quote -> base)
    fn compute_price_swap_base_out(&self, base_amount: u128, quote_amount: u128) -> Result<f64> {
        if quote_amount > 0 {
            Ok(base_amount as f64 / quote_amount as f64)
        } else {
            Ok(0.0)
        }
    }

    /// Get base and quote token mints
    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        panic!("get_mints not implemented for this program");
    }

    /// Calculate output amount for swap base in (base -> quote)
    fn swap_base_in<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        clock: Clock,
    ) -> Result<u64>;

    /// Calculate input amount needed to receive a specific output amount
    /// Given output_mint and amount_out, returns the required amount_in
    fn swap_base_out<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        clock: Clock,
    ) -> Result<u64>;

    /// Get prices for swap base in (base -> quote) and swap base out (quote -> base)
    fn get_prices(&self) -> Result<(f64, f64)>;

    /// Invoke swap base in (base -> quote)
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
    ) -> Result<()>;

    /// Invoke swap base out (quote -> base)
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
    ) -> Result<()>;

    /// Log account information for debugging
    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()>;

    /// Calculate optimal amount in for AMM types
    fn calculate_optimal_amount_in(&self, input_mint: Pubkey, target_price: f64) -> Result<u64> {
        panic!("calculate_optimal_amount_in not implemented for this program");
    }

    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        panic!("get_vault_amounts not implemented for this program");
    }

    fn get_fee_factor(&self) -> Result<f64> {
        panic!("get_fee_factor not implemented for this program");
    }

    fn get_max_amounts_in_out(&self, input_mint: Pubkey) -> Result<(u64, u64)> {
        panic!("get_max_amounts_in_out not implemented for this program");
    }
}
