use anchor_lang::prelude::*;
use anchor_lang::solana_program::pubkey::Pubkey;
use crate::utils::token::MintFee;

use super::meteora_damm_v1::MeteoraDammV1;
use super::meteora_damm_v2::MeteoraDammV2;
use super::meteora_dlmm::MeteoraDlmm;
use super::orca::OrcaWhirlpool;
use super::pump_amm::PumpAmm;
use super::raydium_amm::RaydiumAmm;
use super::raydium_clmm::RaydiumCLMM;
use super::raydium_cpmm::RaydiumCPMM;

/// Enum dispatch replaces Box<dyn ProgramMeta> — eliminates heap allocation + vtable overhead.
pub enum ProgramInstance<'info> {
    PumpAmm(PumpAmm<'info>),
    RaydiumAmm(RaydiumAmm<'info>),
    RaydiumCLMM(RaydiumCLMM<'info>),
    RaydiumCPMM(RaydiumCPMM<'info>),
    MeteoraDammV1(MeteoraDammV1<'info>),
    MeteoraDammV2(MeteoraDammV2<'info>),
    OrcaWhirlpool(OrcaWhirlpool<'info>),
    MeteoraDlmm(MeteoraDlmm),
}

/// Dispatch a &self method call to the inner type.
macro_rules! dispatch {
    ($self:expr, $method:ident ( $($arg:expr),* )) => {
        match $self {
            ProgramInstance::PumpAmm(p) => p.$method($($arg),*),
            ProgramInstance::RaydiumAmm(p) => p.$method($($arg),*),
            ProgramInstance::RaydiumCLMM(p) => p.$method($($arg),*),
            ProgramInstance::RaydiumCPMM(p) => p.$method($($arg),*),
            ProgramInstance::MeteoraDammV1(p) => p.$method($($arg),*),
            ProgramInstance::MeteoraDammV2(p) => p.$method($($arg),*),
            ProgramInstance::OrcaWhirlpool(p) => p.$method($($arg),*),
            ProgramInstance::MeteoraDlmm(p) => p.$method($($arg),*),
        }
    };
}

/// Dispatch a &mut self method call to the inner type.
macro_rules! dispatch_mut {
    ($self:expr, $method:ident ( $($arg:expr),* )) => {
        match $self {
            ProgramInstance::PumpAmm(p) => p.$method($($arg),*),
            ProgramInstance::RaydiumAmm(p) => p.$method($($arg),*),
            ProgramInstance::RaydiumCLMM(p) => p.$method($($arg),*),
            ProgramInstance::RaydiumCPMM(p) => p.$method($($arg),*),
            ProgramInstance::MeteoraDammV1(p) => p.$method($($arg),*),
            ProgramInstance::MeteoraDammV2(p) => p.$method($($arg),*),
            ProgramInstance::OrcaWhirlpool(p) => p.$method($($arg),*),
            ProgramInstance::MeteoraDlmm(p) => p.$method($($arg),*),
        }
    };
}

impl<'info> ProgramMeta for ProgramInstance<'info> {
    fn get_id(&self) -> &Pubkey {
        dispatch!(self, get_id())
    }

    fn get_pool_id(&self) -> &Pubkey {
        dispatch!(self, get_pool_id())
    }

    fn name(&self) -> &'static str {
        dispatch!(self, name())
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        dispatch!(self, get_mints())
    }

    fn get_base_token(&self) -> Pubkey {
        dispatch!(self, get_base_token())
    }

    fn swap_base_in<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        clock: &Clock,
    ) -> Result<u64> {
        dispatch_mut!(self, swap_base_in(accounts, input_mint, amount_in, input_transfer_fee, output_transfer_fee, clock))
    }

    fn swap_base_out<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        clock: &Clock,
    ) -> Result<u64> {
        dispatch_mut!(self, swap_base_out(accounts, output_mint, amount_out, input_transfer_fee, output_transfer_fee, clock))
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        dispatch!(self, get_prices())
    }

    fn invoke_swap_base_in<'a>(
        &mut self,
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
        dispatch_mut!(self, invoke_swap_base_in(
            accounts, input_mint, max_amount_in, amount_out,
            payer, user_mint_1_token_account, user_mint_2_token_account,
            mint_1_account, mint_2_account, mint_1_token_program, mint_2_token_program
        ))
    }

    fn invoke_swap_base_out<'a>(
        &mut self,
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
        dispatch_mut!(self, invoke_swap_base_out(
            accounts, input_mint, amount_in, min_amount_out,
            payer, user_mint_1_token_account, user_mint_2_token_account,
            mint_1_account, mint_2_account, mint_1_token_program, mint_2_token_program
        ))
    }

    #[cfg(any(test, feature = "debug"))]
    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        dispatch!(self, log_accounts(accounts))
    }

    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        dispatch!(self, get_vault_amounts())
    }

    fn get_fee_factor(&self) -> Result<(f64, f64)> {
        dispatch!(self, get_fee_factor())
    }

    fn is_fee_on_input(&self, input_mint: Pubkey) -> bool {
        dispatch!(self, is_fee_on_input(input_mint))
    }

    fn get_max_amount_in<'a>(&self, accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        dispatch!(self, get_max_amount_in(accounts, mint))
    }

    fn get_max_amount_out<'a>(&self, accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        dispatch!(self, get_max_amount_out(accounts, mint))
    }

    fn get_active_bin_max_in(&self, input_mint: Pubkey) -> Result<u64> {
        dispatch!(self, get_active_bin_max_in(input_mint))
    }

    fn get_cached_max_amounts(&self, input_mint: Pubkey) -> (u64, u64) {
        dispatch!(self, get_cached_max_amounts(input_mint))
    }

    fn has_output_liquidity(&self, input_mint: Pubkey) -> bool {
        dispatch!(self, has_output_liquidity(input_mint))
    }

    fn get_bin_segment<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        bin_offset: i32,
    ) -> Result<Option<(f64, u64)>> {
        dispatch!(self, get_bin_segment(accounts, input_mint, bin_offset))
    }

    fn get_bin_step_frac(&self) -> f64 {
        dispatch!(self, get_bin_step_frac())
    }

    fn fast_quote(&mut self, input_mint: Pubkey, amount_in: u64, profit_pct: f64) -> Result<(u64, u64)> {
        dispatch_mut!(self, fast_quote(input_mint, amount_in, profit_pct))
    }

    fn prepare_for_execution<'a>(&mut self, accounts: &[AccountInfo<'a>]) {
        // Transmute lifetime to match inner type — safe because accounts outlive the call
        let accounts: &[AccountInfo<'info>] = unsafe { std::mem::transmute(accounts) };
        match self {
            ProgramInstance::PumpAmm(p) => p.prepare_for_execution(accounts),
            ProgramInstance::RaydiumAmm(p) => p.prepare_for_execution(accounts),
            ProgramInstance::RaydiumCLMM(p) => p.prepare_for_execution(accounts),
            ProgramInstance::RaydiumCPMM(p) => p.prepare_for_execution(accounts),
            ProgramInstance::MeteoraDammV1(p) => p.prepare_for_execution(accounts),
            ProgramInstance::MeteoraDammV2(p) => p.prepare_for_execution(accounts),
            ProgramInstance::OrcaWhirlpool(p) => p.prepare_for_execution(accounts),
            ProgramInstance::MeteoraDlmm(_) => {} // DLMM computes everything in new()
        }
    }
}

pub trait ProgramMeta {
    fn get_id(&self) -> &Pubkey;

    /// Get the unique pool ID for this instance
    fn get_pool_id(&self) -> &Pubkey;

    /// Human-readable program name (e.g. "PumpAmm", "MeteoraDLMM")
    fn name(&self) -> &'static str;

    /// Get base and quote token mints
    fn get_mints(&self) -> (&Pubkey, &Pubkey);

    /// Get base token pubkey
    fn get_base_token(&self) -> Pubkey {
        *self.get_mints().0
    }

    /// Calculate output amount for swap base in (base -> quote)
    fn swap_base_in<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        clock: &Clock,
    ) -> Result<u64>;

    /// Calculate input amount needed to receive a specific output amount
    /// Given output_mint and amount_out, returns the required amount_in
    fn swap_base_out<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        clock: &Clock,
    ) -> Result<u64>;

    /// Get prices for swap base in (base -> quote) and swap base out (quote -> base)
    fn get_prices(&self) -> Result<(f64, f64)>;

    /// Invoke swap base in (base -> quote)
    fn invoke_swap_base_in<'a>(
        &mut self,
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
        &mut self,
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
    #[cfg(any(test, feature = "debug"))]
    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()>;

    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        Err(error!(crate::programs::SolarBError::InvalidProgramType))
    }

    /// Get directional fee factors: (fee_factor_a_to_b, fee_factor_b_to_a)
    /// Each is (1 - fee_rate) for that direction, e.g. 0.9975 for 0.25% fee.
    /// For AMMs with symmetric fees, both values are identical.
    fn get_fee_factor(&self) -> Result<(f64, f64)> {
        Err(error!(crate::programs::SolarBError::InvalidProgramType))
    }

    /// Whether the pool's fee is deducted from the input amount (before swap).
    /// Default true (most AMMs). Override for pools with fee-on-output (e.g. DAMM V2).
    fn is_fee_on_input(&self, _input_mint: Pubkey) -> bool {
        true
    }

    /// Get max amount that can be input for a given mint direction
    fn get_max_amount_in<'a>(&self, _accounts: &[AccountInfo<'a>], _mint: Pubkey) -> Result<u64> {
        Err(error!(crate::programs::SolarBError::InvalidProgramType))
    }

    /// Get max amount that can be output for a given input mint direction
    fn get_max_amount_out<'a>(&self, _accounts: &[AccountInfo<'a>], _mint: Pubkey) -> Result<u64> {
        Err(error!(crate::programs::SolarBError::InvalidProgramType))
    }

    /// Max input the active bin can absorb before crossing to the next bin (DLMM only).
    /// Returns the amount BEFORE fees are added.
    fn get_active_bin_max_in(&self, _input_mint: Pubkey) -> Result<u64> {
        Err(error!(crate::programs::SolarBError::InvalidProgramType))
    }

    /// Get cached max amounts for a given input direction. Returns (max_in, max_out).
    /// Uses pre-computed values from initialization — no accounts needed.
    fn get_cached_max_amounts(&self, _input_mint: Pubkey) -> (u64, u64) {
        (u64::MAX, u64::MAX)
    }

    /// Whether the pool has output liquidity for this input direction.
    /// Cached from initialization — zero cost to call.
    /// Default true for AMMs (vault amounts always imply liquidity).
    fn has_output_liquidity(&self, _input_mint: Pubkey) -> bool {
        true
    }

    /// Get the slope (price * fee_factor) and capacity (max_amount_in) for a DLMM bin
    /// at `bin_offset` from the active bin in the swap direction.
    /// offset=0 is active bin, offset=1 is next bin, etc.
    /// Returns Ok(None) for non-DLMM pools or if bin data is not available.
    fn get_bin_segment<'a>(
        &self,
        _accounts: &[AccountInfo<'a>],
        _input_mint: Pubkey,
        _bin_offset: i32,
    ) -> Result<Option<(f64, u64)>> {
        Ok(None)
    }

    /// Bin step as a fraction (e.g. 0.008 for 80bps). Returns 0.0 for non-DLMM pools.
    fn get_bin_step_frac(&self) -> f64 {
        0.0
    }

    /// Compute deferred fields (max amounts, transfer fees, etc.) needed for
    /// simulation and execution. Only called for instances in a profitable path.
    /// Default no-op for programs that compute everything in new().
    fn prepare_for_execution<'a>(&mut self, _accounts: &[AccountInfo<'a>]) {}

    /// Simplified swap estimate from cached state only (no account reads).
    /// Skips transfer fees. Used for candidate ranking, not execution.
    /// Each program overrides this with its own swap math.
    /// Returns (actual_amount_in, amount_out) — programs clamp to their max amounts.
    /// profit_pct is the arb cycle's profit as a fraction (e.g. 0.02 = 2%).
    /// DLMM uses this to decide whether crossing into the next bin is worthwhile.
    /// Default: linear model from cached price * fee (ignores profit_pct).
    fn fast_quote(&mut self, input_mint: Pubkey, amount_in: u64, _profit_pct: f64) -> Result<(u64, u64)> {
        let (price, inverse_price) = self.get_prices()?;
        let (fee_a_to_b, fee_b_to_a) = self.get_fee_factor().unwrap_or((1.0, 1.0));
        let (base_mint, _) = self.get_mints();
        let (p, f) = if input_mint == *base_mint {
            (price, fee_a_to_b)
        } else {
            (inverse_price, fee_b_to_a)
        };
        let (max_in, max_out) = self.get_cached_max_amounts(input_mint);
        let clamped_in = amount_in.min(max_in);
        let out = (clamped_in as f64 * p * f) as u64;
        Ok((clamped_in, out.min(max_out)))
    }
}
