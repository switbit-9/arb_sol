use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};
use pinocchio::sysvars::clock::Clock;
use crate::utils::token::MintFee;

use super::meteora_damm_v1::MeteoraDammV1;
use super::meteora_damm_v2::MeteoraDammV2;
use super::meteora_dlmm::MeteoraDlmm;
use super::orca::OrcaWhirlpool;
use super::pump_amm::PumpAmm;
use super::raydium_amm::RaydiumAmm;
use super::raydium_clmm::RaydiumCLMM;
use super::raydium_cpmm::RaydiumCPMM;

pub type Result<T> = core::result::Result<T, ProgramError>;

/// Numeric pool type discriminant — single u8 comparison instead of string matching.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolKind {
    PumpAmm = 0,
    RaydiumAmm = 1,
    RaydiumCLMM = 2,
    RaydiumCPMM = 3,
    MeteoraDammV1 = 4,
    MeteoraDammV2 = 5,
    OrcaWhirlpool = 6,
    MeteoraDlmm = 7,
}

/// Enum dispatch replaces Box<dyn ProgramMeta> — eliminates heap allocation + vtable overhead.
#[derive(Clone)]
pub enum ProgramInstance {
    PumpAmm(PumpAmm),
    RaydiumAmm(RaydiumAmm),
    RaydiumCLMM(RaydiumCLMM),
    RaydiumCPMM(RaydiumCPMM),
    MeteoraDammV1(MeteoraDammV1),
    MeteoraDammV2(MeteoraDammV2),
    OrcaWhirlpool(OrcaWhirlpool),
    MeteoraDlmm(MeteoraDlmm),
}

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

impl ProgramMeta for ProgramInstance {
    fn get_id(&self) -> &Pubkey { dispatch!(self, get_id()) }
    fn get_pool_id(&self) -> &Pubkey { dispatch!(self, get_pool_id()) }
    fn name(&self) -> &'static str { dispatch!(self, name()) }

    fn pool_kind(&self) -> PoolKind {
        match self {
            ProgramInstance::PumpAmm(_) => PoolKind::PumpAmm,
            ProgramInstance::RaydiumAmm(_) => PoolKind::RaydiumAmm,
            ProgramInstance::RaydiumCLMM(_) => PoolKind::RaydiumCLMM,
            ProgramInstance::RaydiumCPMM(_) => PoolKind::RaydiumCPMM,
            ProgramInstance::MeteoraDammV1(_) => PoolKind::MeteoraDammV1,
            ProgramInstance::MeteoraDammV2(_) => PoolKind::MeteoraDammV2,
            ProgramInstance::OrcaWhirlpool(_) => PoolKind::OrcaWhirlpool,
            ProgramInstance::MeteoraDlmm(_) => PoolKind::MeteoraDlmm,
        }
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) { dispatch!(self, get_mints()) }

    fn swap_base_in(
        &mut self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        amount_in: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        clock: &Clock,
    ) -> Result<u64> {
        dispatch_mut!(self, swap_base_in(accounts, input_mint, amount_in, input_transfer_fee, output_transfer_fee, clock))
    }

    fn swap_base_out(
        &mut self,
        accounts: &[AccountInfo],
        output_mint: Pubkey,
        amount_out: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        clock: &Clock,
    ) -> Result<u64> {
        dispatch_mut!(self, swap_base_out(accounts, output_mint, amount_out, input_transfer_fee, output_transfer_fee, clock))
    }

    fn get_prices(&self) -> Result<(f64, f64)> { dispatch!(self, get_prices()) }

    fn invoke_swap_base_in(
        &mut self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        max_amount_in: u64,
        amount_out: Option<u64>,
        payer: &AccountInfo,
        user_mint_1_token_account: &AccountInfo,
        user_mint_2_token_account: &AccountInfo,
        mint_1_account: &AccountInfo,
        mint_2_account: &AccountInfo,
        mint_1_token_program: &AccountInfo,
        mint_2_token_program: &AccountInfo,
    ) -> Result<()> {
        dispatch_mut!(self, invoke_swap_base_in(
            accounts, input_mint, max_amount_in, amount_out,
            payer, user_mint_1_token_account, user_mint_2_token_account,
            mint_1_account, mint_2_account, mint_1_token_program, mint_2_token_program
        ))
    }

    fn invoke_swap_base_out(
        &mut self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        amount_in: u64,
        min_amount_out: Option<u64>,
        payer: &AccountInfo,
        user_mint_1_token_account: &AccountInfo,
        user_mint_2_token_account: &AccountInfo,
        mint_1_account: &AccountInfo,
        mint_2_account: &AccountInfo,
        mint_1_token_program: &AccountInfo,
        mint_2_token_program: &AccountInfo,
    ) -> Result<()> {
        dispatch_mut!(self, invoke_swap_base_out(
            accounts, input_mint, amount_in, min_amount_out,
            payer, user_mint_1_token_account, user_mint_2_token_account,
            mint_1_account, mint_2_account, mint_1_token_program, mint_2_token_program
        ))
    }

    #[cfg(any(test, feature = "debug"))]
    fn log_accounts(&self, accounts: &[AccountInfo]) -> Result<()> {
        dispatch!(self, log_accounts(accounts))
    }

    fn get_vault_amounts(&self) -> Result<(u64, u64)> { dispatch!(self, get_vault_amounts()) }
    fn get_fee_factor(&self) -> Result<(f64, f64)> { dispatch!(self, get_fee_factor()) }
    fn is_fee_on_input(&self, input_mint: Pubkey) -> bool { dispatch!(self, is_fee_on_input(input_mint)) }
    fn get_max_amount_in(&self, accounts: &[AccountInfo], mint: Pubkey) -> Result<u64> { dispatch!(self, get_max_amount_in(accounts, mint)) }
    fn get_max_amount_out(&self, accounts: &[AccountInfo], mint: Pubkey) -> Result<u64> { dispatch!(self, get_max_amount_out(accounts, mint)) }
    fn get_active_bin_max_in(&self, input_mint: Pubkey) -> Result<u64> { dispatch!(self, get_active_bin_max_in(input_mint)) }
    fn get_cached_max_amounts(&self, input_mint: Pubkey) -> (u64, u64) { dispatch!(self, get_cached_max_amounts(input_mint)) }

    fn get_bin_segment(
        &self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        bin_offset: i32,
    ) -> Result<Option<(f64, u64, f64)>> {
        dispatch!(self, get_bin_segment(accounts, input_mint, bin_offset))
    }

    fn get_bin_step_frac(&self) -> f64 { dispatch!(self, get_bin_step_frac()) }

    fn get_clmm_virtual_reserves(&self, input_mint: Pubkey) -> Option<(f64, f64)> {
        dispatch!(self, get_clmm_virtual_reserves(input_mint))
    }

    fn get_clmm_segment(
        &self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        bin_offset: i32,
    ) -> Result<Option<(f64, f64, u64, f64)>> {
        dispatch!(self, get_clmm_segment(accounts, input_mint, bin_offset))
    }

    fn fast_quote(&mut self, accounts: &[AccountInfo], input_mint: Pubkey, amount_in: u64, profit_pct: f64) -> Result<(u64, u64)> {
        dispatch_mut!(self, fast_quote(accounts, input_mint, amount_in, profit_pct))
    }

    fn prepare_for_execution(&mut self, accounts: &[AccountInfo], clock: &Clock) -> bool {
        match self {
            ProgramInstance::PumpAmm(p) => { p.prepare_for_execution(accounts); true }
            ProgramInstance::RaydiumAmm(p) => { p.prepare_for_execution(accounts); true }
            ProgramInstance::RaydiumCLMM(p) => { p.prepare_for_execution(accounts); true }
            ProgramInstance::RaydiumCPMM(p) => { p.prepare_for_execution(accounts); true }
            ProgramInstance::MeteoraDammV1(p) => { p.prepare_for_execution(accounts); true }
            ProgramInstance::MeteoraDammV2(p) => { p.prepare_for_execution(accounts); true }
            ProgramInstance::OrcaWhirlpool(p) => { p.prepare_for_execution(accounts); true }
            ProgramInstance::MeteoraDlmm(p) => p.prepare_for_execution(accounts, clock).unwrap_or(false)
        }
    }
}

pub trait ProgramMeta {
    fn get_id(&self) -> &Pubkey;
    fn get_pool_id(&self) -> &Pubkey;
    fn name(&self) -> &'static str;
    fn pool_kind(&self) -> PoolKind;
    fn get_mints(&self) -> (&Pubkey, &Pubkey);

    fn get_base_token(&self) -> Pubkey {
        *self.get_mints().0
    }

    fn swap_base_in(
        &mut self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        amount_in: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        clock: &Clock,
    ) -> Result<u64>;

    fn swap_base_out(
        &mut self,
        accounts: &[AccountInfo],
        output_mint: Pubkey,
        amount_out: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        clock: &Clock,
    ) -> Result<u64>;

    fn get_prices(&self) -> Result<(f64, f64)>;

    fn invoke_swap_base_in(
        &mut self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        max_amount_in: u64,
        amount_out: Option<u64>,
        payer: &AccountInfo,
        user_mint_1_token_account: &AccountInfo,
        user_mint_2_token_account: &AccountInfo,
        mint_1_account: &AccountInfo,
        mint_2_account: &AccountInfo,
        mint_1_token_program: &AccountInfo,
        mint_2_token_program: &AccountInfo,
    ) -> Result<()>;

    fn invoke_swap_base_out(
        &mut self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        amount_in: u64,
        min_amount_out: Option<u64>,
        payer: &AccountInfo,
        user_mint_1_token_account: &AccountInfo,
        user_mint_2_token_account: &AccountInfo,
        mint_1_account: &AccountInfo,
        mint_2_account: &AccountInfo,
        mint_1_token_program: &AccountInfo,
        mint_2_token_program: &AccountInfo,
    ) -> Result<()>;

    #[cfg(any(test, feature = "debug"))]
    fn log_accounts(&self, accounts: &[AccountInfo]) -> Result<()>;

    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        Err(crate::programs::SolarBError::InvalidProgramType.into())
    }

    fn get_fee_factor(&self) -> Result<(f64, f64)> {
        Err(crate::programs::SolarBError::InvalidProgramType.into())
    }

    fn is_fee_on_input(&self, _input_mint: Pubkey) -> bool { true }

    fn get_max_amount_in(&self, _accounts: &[AccountInfo], _mint: Pubkey) -> Result<u64> {
        Err(crate::programs::SolarBError::InvalidProgramType.into())
    }

    fn get_max_amount_out(&self, _accounts: &[AccountInfo], _mint: Pubkey) -> Result<u64> {
        Err(crate::programs::SolarBError::InvalidProgramType.into())
    }

    fn get_active_bin_max_in(&self, _input_mint: Pubkey) -> Result<u64> {
        Err(crate::programs::SolarBError::InvalidProgramType.into())
    }

    fn get_cached_max_amounts(&self, _input_mint: Pubkey) -> (u64, u64) { (u64::MAX, u64::MAX) }

    fn get_bin_segment(
        &self,
        _accounts: &[AccountInfo],
        _input_mint: Pubkey,
        _bin_offset: i32,
    ) -> Result<Option<(f64, u64, f64)>> {
        Ok(None)
    }

    fn get_bin_step_frac(&self) -> f64 { 0.0 }

    fn get_clmm_virtual_reserves(&self, _input_mint: Pubkey) -> Option<(f64, f64)> { None }

    fn get_clmm_segment(
        &self,
        _accounts: &[AccountInfo],
        _input_mint: Pubkey,
        _bin_offset: i32,
    ) -> Result<Option<(f64, f64, u64, f64)>> {
        Ok(None)
    }

    fn prepare_for_execution(&mut self, _accounts: &[AccountInfo], _clock: &Clock) -> bool { true }

    fn fast_quote(&mut self, _accounts: &[AccountInfo], input_mint: Pubkey, amount_in: u64, _profit_pct: f64) -> Result<(u64, u64)> {
        let (price, inverse_price) = self.get_prices()?;
        let (fee_a_to_b, fee_b_to_a) = self.get_fee_factor().unwrap_or((1.0, 1.0));
        let (base_mint, _) = self.get_mints();
        let (p, f) = if input_mint == *base_mint { (price, fee_a_to_b) } else { (inverse_price, fee_b_to_a) };
        let (max_in, max_out) = self.get_cached_max_amounts(input_mint);
        let clamped_in = amount_in.min(max_in);
        let out = (clamped_in as f64 * p * f) as u64;
        Ok((clamped_in, out.min(max_out)))
    }
}
