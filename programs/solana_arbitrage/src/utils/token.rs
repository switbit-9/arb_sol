use crate::programs::SolarBError;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::pubkey::Pubkey;
use anchor_spl::token_2022::spl_token_2022::extension::transfer_fee::{
    TransferFee, TransferFeeConfig, MAX_FEE_BASIS_POINTS,
};
use anchor_spl::token_interface::spl_token_2022::extension::BaseStateWithExtensions;

use anchor_spl::token::Token;
use anchor_spl::token_2022::spl_token_2022::{
    self,
    extension::{self, StateWithExtensions},
};

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
) -> Result<TransferFeeExcludedAmount> {
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
) -> Result<TransferFeeIncludedAmount> {
    if transfer_fee_excluded_amount == 0 {
        return Ok(TransferFeeIncludedAmount {
            amount: 0,
            transfer_fee: 0,
        });
    }

    // now transfer_fee_excluded_amount > 0

    if let Some(epoch_transfer_fee) = get_epoch_transfer_fee(token_mint)? {
        let transfer_fee: u64 =
            if u16::from(epoch_transfer_fee.transfer_fee_basis_points) == MAX_FEE_BASIS_POINTS {
                // edge-case: if transfer fee rate is 100%, current SPL implementation returns 0 as inverse fee.
                // https://github.com/solana-labs/solana-program-library/blob/fe1ac9a2c4e5d85962b78c3fc6aaf028461e9026/token/program-2022/src/extension/transfer_fee/mod.rs#L95

                // But even if transfer fee is 100%, we can use maximum_fee as transfer fee.
                // if transfer_fee_excluded_amount + maximum_fee > u64 max, the following checked_add should fail.
                u64::from(epoch_transfer_fee.maximum_fee)
            } else {
                epoch_transfer_fee
                    .calculate_inverse_fee(transfer_fee_excluded_amount)
                    .ok_or(SolarBError::TransferFeeCalculationError)?
            };

        let transfer_fee_included_amount = transfer_fee_excluded_amount
            .checked_add(transfer_fee)
            .ok_or(SolarBError::TransferFeeCalculationError)?;

        // verify transfer fee calculation for safety
        let transfer_fee_verification = epoch_transfer_fee
            .calculate_fee(transfer_fee_included_amount)
            .unwrap();
        if transfer_fee != transfer_fee_verification {
            // We believe this should never happen
            return Err(error!(SolarBError::TransferFeeCalculationError));
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

pub fn get_epoch_transfer_fee(token_mint: &AccountInfo) -> Result<Option<TransferFee>> {
    if *token_mint.owner == Token::id() {
        return Ok(None);
    }

    let token_mint_data = token_mint.try_borrow_data()?;
    let token_mint_unpacked =
        StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&token_mint_data)?;
    if let Ok(transfer_fee_config) =
        token_mint_unpacked.get_extension::<extension::transfer_fee::TransferFeeConfig>()
    {
        let epoch = Clock::get()?.epoch;
        return Ok(Some(*transfer_fee_config.get_epoch_fee(epoch)));
    }

    Ok(None)
}

/// Extract fee rate from a mint account as a f64 (basis_points / 10_000).
/// Returns 0.0 for standard SPL Token mints or mints without transfer fee extension.
pub fn extract_fee_rate(token_mint: &AccountInfo, epoch: u64) -> Result<f64> {
    if *token_mint.owner == Token::id() {
        return Ok(0.0);
    }

    let token_mint_data = token_mint.try_borrow_data()?;
    let token_mint_unpacked =
        StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&token_mint_data)?;
    if let Ok(transfer_fee_config) =
        token_mint_unpacked.get_extension::<extension::transfer_fee::TransferFeeConfig>()
    {
        let fee = transfer_fee_config.get_epoch_fee(epoch);
        let basis_points = u16::from(fee.transfer_fee_basis_points);
        return Ok(basis_points as f64 / 10_000.0);
    }

    Ok(0.0)
}

/// Look up a cached fee rate by mint pubkey. Returns 0.0 if not found.
#[inline(always)]
pub fn lookup_fee_rate(mint_fees: &[(Pubkey, f64)], mint: &Pubkey) -> f64 {
    mint_fees.iter().find(|(k, _)| k == mint).map(|(_, f)| *f).unwrap_or(0.0)
}

/// Calculate the fee for output amount
pub fn get_transfer_inverse_fee(mint_info: &AccountInfo, post_fee_amount: u64) -> Result<u64> {
    if *mint_info.owner == Token::id() {
        return Ok(0);
    }
    if post_fee_amount == 0 {
        return Ok(0);
    }
    let mint_data = mint_info.try_borrow_data()?;
    let mint = StateWithExtensions::<anchor_spl::token_2022::spl_token_2022::state::Mint>::unpack(
        &mint_data,
    )?;

    let fee = if let Ok(transfer_fee_config) = mint.get_extension::<TransferFeeConfig>() {
        let epoch = Clock::get()?.epoch;

        let transfer_fee = transfer_fee_config.get_epoch_fee(epoch);
        if u16::from(transfer_fee.transfer_fee_basis_points) == MAX_FEE_BASIS_POINTS {
            u64::from(transfer_fee.maximum_fee)
        } else {
            let transfer_fee = transfer_fee_config
                .calculate_inverse_epoch_fee(epoch, post_fee_amount)
                .unwrap();
            let transfer_fee_for_check = transfer_fee_config
                .calculate_epoch_fee(epoch, post_fee_amount.checked_add(transfer_fee).unwrap())
                .unwrap();
            if transfer_fee != transfer_fee_for_check {
                return Err(error!(SolarBError::TransferFeeCalculationError));
            }
            transfer_fee
        }
    } else {
        0
    };
    Ok(fee)
}

/// Calculate the fee for input amount
pub fn get_transfer_fee(mint_info: &AccountInfo, pre_fee_amount: u64) -> Result<u64> {
    if *mint_info.owner == Token::id() {
        return Ok(0);
    }
    let mint_data = mint_info.try_borrow_data()?;
    let mint = StateWithExtensions::<anchor_spl::token_2022::spl_token_2022::state::Mint>::unpack(
        &mint_data,
    )?;

    let fee = if let Ok(transfer_fee_config) = mint.get_extension::<TransferFeeConfig>() {
        transfer_fee_config
            .calculate_epoch_fee(Clock::get()?.epoch, pre_fee_amount)
            .unwrap()
    } else {
        0
    };
    Ok(fee)
}

/// Apply forward transfer fee using a pre-computed fee rate.
/// Returns the fee amount to subtract from `pre_fee_amount`.
#[inline(always)]
pub fn apply_transfer_fee(pre_fee_amount: u64, fee_rate: f64) -> u64 {
    if fee_rate == 0.0 {
        return 0;
    }
    (pre_fee_amount as f64 * fee_rate) as u64
}

/// Apply inverse transfer fee using a pre-computed fee rate.
/// Returns the fee amount to add to `post_fee_amount` to get the pre-fee amount.
#[inline(always)]
pub fn apply_transfer_inverse_fee(post_fee_amount: u64, fee_rate: f64) -> u64 {
    if fee_rate == 0.0 || post_fee_amount == 0 {
        return 0;
    }
    (post_fee_amount as f64 * fee_rate / (1.0 - fee_rate)).ceil() as u64
}

pub fn get_transfer_fee_config(mint_info: &AccountInfo) -> Result<Option<TransferFeeConfig>> {
    if *mint_info.owner == Token::id() {
        return Ok(None);
    }
    let mint_data = mint_info.try_borrow_data()?;
    let mint = StateWithExtensions::<anchor_spl::token_2022::spl_token_2022::state::Mint>::unpack(
        &mint_data,
    )?;
    let fee = if let Ok(transfer_fee_config) = mint.get_extension::<TransferFeeConfig>() {
        Some(*transfer_fee_config)
    } else {
        None
    };
    Ok(fee)
}
