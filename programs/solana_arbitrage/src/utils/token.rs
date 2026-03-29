use crate::compat::*;
use crate::programs::SolarBError;
use spl_token_2022::extension::transfer_fee::{
    TransferFee, TransferFeeConfig, MAX_FEE_BASIS_POINTS,
};
use spl_token_2022::extension::BaseStateWithExtensions;
use spl_token_2022::{
    self,
    extension::{self, StateWithExtensions},
};

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
            return Err(solar_error!(SolarBError::TransferFeeCalculationError));
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
    if *token_mint.owner == SPL_TOKEN_ID {
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

/// Extract transfer fee parameters from a mint account, returning a MintFee.
/// Returns MintFee::ZERO for standard SPL Token mints or mints without transfer fee extension.
pub fn extract_mint_fee(token_mint: &AccountInfo, epoch: u64) -> Result<MintFee> {
    if *token_mint.owner == SPL_TOKEN_ID {
        return Ok(MintFee::ZERO);
    }

    let token_mint_data = token_mint.try_borrow_data()?;
    let token_mint_unpacked =
        StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&token_mint_data)?;
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

/// Get (input_transfer_fee, output_transfer_fee) for a given input mint,
/// looking up fees from the cached mint_fees slice.
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

/// Calculate the fee for output amount
pub fn get_transfer_inverse_fee(mint_info: &AccountInfo, post_fee_amount: u64) -> Result<u64> {
    if *mint_info.owner == SPL_TOKEN_ID {
        return Ok(0);
    }
    if post_fee_amount == 0 {
        return Ok(0);
    }
    let mint_data = mint_info.try_borrow_data()?;
    let mint = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(
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
                return Err(solar_error!(SolarBError::TransferFeeCalculationError));
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
    if *mint_info.owner == SPL_TOKEN_ID {
        return Ok(0);
    }
    let mint_data = mint_info.try_borrow_data()?;
    let mint = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(
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

/// Apply forward transfer fee using MintFee (bps + max cap).
/// Returns the fee amount to subtract from `pre_fee_amount`.
#[inline(always)]
pub fn apply_transfer_fee(pre_fee_amount: u64, fee: MintFee) -> u64 {
    if fee.bps == 0 {
        return 0;
    }
    let calculated = ((pre_fee_amount as u128) * (fee.bps as u128) / 10_000) as u64;
    if fee.max > 0 && calculated > fee.max {
        fee.max
    } else {
        calculated
    }
}

/// Apply inverse transfer fee using MintFee (bps + max cap).
/// Returns the fee amount to add to `post_fee_amount` to get the pre-fee amount.
#[inline(always)]
pub fn apply_transfer_inverse_fee(post_fee_amount: u64, fee: MintFee) -> u64 {
    if fee.bps == 0 || post_fee_amount == 0 {
        return 0;
    }
    let denom = 10_000u64.saturating_sub(fee.bps as u64);
    if denom == 0 {
        return post_fee_amount; // 100% fee edge case
    }
    let numer = (post_fee_amount as u128) * (fee.bps as u128);
    let calculated = ((numer + denom as u128 - 1) / denom as u128) as u64; // ceil division
    if fee.max > 0 && calculated > fee.max {
        fee.max
    } else {
        calculated
    }
}

pub fn get_transfer_fee_config(mint_info: &AccountInfo) -> Result<Option<TransferFeeConfig>> {
    if *mint_info.owner == SPL_TOKEN_ID {
        return Ok(None);
    }
    let mint_data = mint_info.try_borrow_data()?;
    let mint = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(
        &mint_data,
    )?;
    let fee = if let Ok(transfer_fee_config) = mint.get_extension::<TransferFeeConfig>() {
        Some(*transfer_fee_config)
    } else {
        None
    };
    Ok(fee)
}
