use crate::programs::SolarBError;
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};
use pinocchio::sysvars::{clock::Clock, Sysvar};
use spl_token_2022::{
    extension::{self, BaseStateWithExtensions, StateWithExtensions},
    extension::transfer_fee::{TransferFee, TransferFeeConfig, MAX_FEE_BASIS_POINTS},
};

// Standard SPL Token program ID (not Token-2022).
// Mints owned by this program never have transfer fee extensions.
const SPL_TOKEN_PROGRAM_ID: Pubkey =
    five8_const::decode_32_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

/// Mint transfer fee parameters passed via instruction data (no on-chain parsing needed).
#[derive(Clone, Copy, Debug)]
pub struct MintFee {
    pub bps: u16,
    pub max: u64,
}

impl MintFee {
    pub const ZERO: MintFee = MintFee { bps: 0, max: 0 };
}

#[derive(Debug)]
pub struct TransferFeeIncludedAmount {
    pub amount: u64,
    pub transfer_fee: u64,
}

#[derive(Debug)]
pub struct TransferFeeExcludedAmount {
    pub amount: u64,
    pub transfer_fee: u64,
}

pub fn calculate_transfer_fee_excluded_amount(
    token_mint: &AccountInfo,
    transfer_fee_included_amount: u64,
) -> Result<TransferFeeExcludedAmount, ProgramError> {
    if let Some(epoch_transfer_fee) = get_epoch_transfer_fee(token_mint)? {
        let transfer_fee = epoch_transfer_fee
            .calculate_fee(transfer_fee_included_amount)
            .unwrap();
        let transfer_fee_excluded_amount = transfer_fee_included_amount
            .checked_sub(transfer_fee)
            .unwrap();
        return Ok(TransferFeeExcludedAmount {
            amount: transfer_fee_excluded_amount,
            transfer_fee,
        });
    }
    Ok(TransferFeeExcludedAmount {
        amount: transfer_fee_included_amount,
        transfer_fee: 0,
    })
}

pub fn calculate_transfer_fee_included_amount(
    token_mint: &AccountInfo,
    transfer_fee_excluded_amount: u64,
) -> Result<TransferFeeIncludedAmount, ProgramError> {
    if transfer_fee_excluded_amount == 0 {
        return Ok(TransferFeeIncludedAmount { amount: 0, transfer_fee: 0 });
    }
    if let Some(epoch_transfer_fee) = get_epoch_transfer_fee(token_mint)? {
        let transfer_fee: u64 =
            if u16::from(epoch_transfer_fee.transfer_fee_basis_points) == MAX_FEE_BASIS_POINTS {
                u64::from(epoch_transfer_fee.maximum_fee)
            } else {
                epoch_transfer_fee
                    .calculate_inverse_fee(transfer_fee_excluded_amount)
                    .ok_or(ProgramError::from(SolarBError::TransferFeeCalculationError))?
            };
        let transfer_fee_included_amount = transfer_fee_excluded_amount
            .checked_add(transfer_fee)
            .ok_or(ProgramError::from(SolarBError::TransferFeeCalculationError))?;
        let transfer_fee_verification = epoch_transfer_fee
            .calculate_fee(transfer_fee_included_amount)
            .unwrap();
        if transfer_fee != transfer_fee_verification {
            return Err(SolarBError::TransferFeeCalculationError.into());
        }
        return Ok(TransferFeeIncludedAmount {
            amount: transfer_fee_included_amount,
            transfer_fee,
        });
    }
    Ok(TransferFeeIncludedAmount {
        amount: transfer_fee_excluded_amount,
        transfer_fee: 0,
    })
}

pub fn get_epoch_transfer_fee(token_mint: &AccountInfo) -> Result<Option<TransferFee>, ProgramError> {
    if token_mint.owner() == &SPL_TOKEN_PROGRAM_ID {
        return Ok(None);
    }
    let token_mint_data = unsafe { token_mint.borrow_data_unchecked() };
    let token_mint_unpacked =
        StateWithExtensions::<spl_token_2022::state::Mint>::unpack(token_mint_data)
            .map_err(|_| ProgramError::InvalidAccountData)?;
    if let Ok(transfer_fee_config) =
        token_mint_unpacked.get_extension::<extension::transfer_fee::TransferFeeConfig>()
    {
        let epoch = Clock::get()?.epoch;
        return Ok(Some(*transfer_fee_config.get_epoch_fee(epoch)));
    }
    Ok(None)
}

/// Extract transfer fee parameters from a mint account.
/// Returns MintFee::ZERO for standard SPL Token mints or mints without the extension.
pub fn extract_mint_fee(token_mint: &AccountInfo, epoch: u64) -> Result<MintFee, ProgramError> {
    if token_mint.owner() == &SPL_TOKEN_PROGRAM_ID {
        return Ok(MintFee::ZERO);
    }
    let token_mint_data = unsafe { token_mint.borrow_data_unchecked() };
    let token_mint_unpacked =
        StateWithExtensions::<spl_token_2022::state::Mint>::unpack(token_mint_data)
            .map_err(|_| ProgramError::InvalidAccountData)?;
    if let Ok(transfer_fee_config) =
        token_mint_unpacked.get_extension::<extension::transfer_fee::TransferFeeConfig>()
    {
        let fee = transfer_fee_config.get_epoch_fee(epoch);
        let bps = u16::from(fee.transfer_fee_basis_points);
        let max = u64::from(fee.maximum_fee);
        return Ok(MintFee { bps, max });
    }
    Ok(MintFee::ZERO)
}

/// Look up a cached mint fee by pubkey. Returns MintFee::ZERO if not found.
#[inline(always)]
pub fn lookup_fee_rate(mint_fees: &[(Pubkey, MintFee)], mint: &Pubkey) -> MintFee {
    mint_fees.iter().find(|(k, _)| k == mint).map(|(_, f)| *f).unwrap_or(MintFee::ZERO)
}

/// Get (input_transfer_fee, output_transfer_fee) for a given input mint.
#[inline(always)]
pub fn get_transfer_fees(
    input_mint: Pubkey,
    base_token_pk: &Pubkey,
    quote_token_pk: &Pubkey,
    mint_fees: &[(Pubkey, MintFee)],
) -> (MintFee, MintFee) {
    let base_fee = lookup_fee_rate(mint_fees, base_token_pk);
    let quote_fee = lookup_fee_rate(mint_fees, quote_token_pk);
    if input_mint == *base_token_pk {
        (base_fee, quote_fee)
    } else {
        (quote_fee, base_fee)
    }
}

pub fn get_transfer_inverse_fee(mint_info: &AccountInfo, post_fee_amount: u64) -> Result<u64, ProgramError> {
    if mint_info.owner() == &SPL_TOKEN_PROGRAM_ID {
        return Ok(0);
    }
    if post_fee_amount == 0 {
        return Ok(0);
    }
    let mint_data = unsafe { mint_info.borrow_data_unchecked() };
    let mint = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(mint_data)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    let fee = if let Ok(transfer_fee_config) = mint.get_extension::<TransferFeeConfig>() {
        let epoch = Clock::get()?.epoch;
        let transfer_fee = transfer_fee_config.get_epoch_fee(epoch);
        if u16::from(transfer_fee.transfer_fee_basis_points) == MAX_FEE_BASIS_POINTS {
            u64::from(transfer_fee.maximum_fee)
        } else {
            let transfer_fee = transfer_fee_config
                .calculate_inverse_epoch_fee(epoch, post_fee_amount)
                .unwrap();
            let check = transfer_fee_config
                .calculate_epoch_fee(epoch, post_fee_amount.checked_add(transfer_fee).unwrap())
                .unwrap();
            if transfer_fee != check {
                return Err(SolarBError::TransferFeeCalculationError.into());
            }
            transfer_fee
        }
    } else {
        0
    };
    Ok(fee)
}

pub fn get_transfer_fee(mint_info: &AccountInfo, pre_fee_amount: u64) -> Result<u64, ProgramError> {
    if mint_info.owner() == &SPL_TOKEN_PROGRAM_ID {
        return Ok(0);
    }
    let mint_data = unsafe { mint_info.borrow_data_unchecked() };
    let mint = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(mint_data)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    let fee = if let Ok(transfer_fee_config) = mint.get_extension::<TransferFeeConfig>() {
        transfer_fee_config
            .calculate_epoch_fee(Clock::get()?.epoch, pre_fee_amount)
            .unwrap()
    } else {
        0
    };
    Ok(fee)
}

pub fn get_transfer_fee_config(mint_info: &AccountInfo) -> Result<Option<TransferFeeConfig>, ProgramError> {
    if mint_info.owner() == &SPL_TOKEN_PROGRAM_ID {
        return Ok(None);
    }
    let mint_data = unsafe { mint_info.borrow_data_unchecked() };
    let mint = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(mint_data)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    let fee = if let Ok(transfer_fee_config) = mint.get_extension::<TransferFeeConfig>() {
        Some(*transfer_fee_config)
    } else {
        None
    };
    Ok(fee)
}

/// Apply forward transfer fee using MintFee (bps + max cap).
/// Returns the fee amount to subtract from `pre_fee_amount`.
#[inline(always)]
pub fn apply_transfer_fee(pre_fee_amount: u64, fee: MintFee) -> u64 {
    if fee.bps == 0 {
        return 0;
    }
    let calculated = ((pre_fee_amount as u128) * (fee.bps as u128) / 10_000) as u64;
    if fee.max > 0 && calculated > fee.max { fee.max } else { calculated }
}

/// Apply inverse transfer fee using MintFee (bps + max cap).
/// Returns the fee to add to `post_fee_amount` to get the pre-fee amount.
#[inline(always)]
pub fn apply_transfer_inverse_fee(post_fee_amount: u64, fee: MintFee) -> u64 {
    if fee.bps == 0 || post_fee_amount == 0 {
        return 0;
    }
    let denom = 10_000u64.saturating_sub(fee.bps as u64);
    if denom == 0 {
        return post_fee_amount;
    }
    let numer = (post_fee_amount as u128) * (fee.bps as u128);
    let calculated = ((numer + denom as u128 - 1) / denom as u128) as u64;
    if fee.max > 0 && calculated > fee.max { fee.max } else { calculated }
}
