use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};
use crate::programs::SolarBError;

/// Format a pinocchio Pubkey ([u8; 32]) as a short hex string for logging.
#[cfg(any(test, feature = "debug"))]
#[inline]
pub fn pk_short(k: &Pubkey) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}..{:02x}{:02x}{:02x}{:02x}",
        k[0], k[1], k[2], k[3], k[28], k[29], k[30], k[31]
    )
}

// SPL Token account layout: mint (32) + owner (32) + amount (8)
const TOKEN_ACCOUNT_AMOUNT_OFFSET: usize = 64;

/// Read only the amount field from an SPL Token account (offset 64).
/// ~10 CU vs ~150-200 CU for full TokenAccount deserialization.
#[inline(always)]
pub fn read_token_amount(account: &AccountInfo) -> Result<u64, ProgramError> {
    let data = unsafe { account.borrow_data_unchecked() };
    if data.len() < TOKEN_ACCOUNT_AMOUNT_OFFSET + 8 {
        return Err(SolarBError::InvalidAccountData.into());
    }
    Ok(u64::from_le_bytes(
        data[TOKEN_ACCOUNT_AMOUNT_OFFSET..TOKEN_ACCOUNT_AMOUNT_OFFSET + 8]
            .try_into()
            .map_err(|_| ProgramError::from(SolarBError::InvalidAccountData))?,
    ))
}

/// Read mint pubkey and amount from an SPL Token vault account.
/// Layout: mint (0..32) + owner (32..64) + amount (64..72)
#[inline(always)]
pub fn read_vault_data(account: &AccountInfo) -> Result<(Pubkey, u64), ProgramError> {
    let data = unsafe { account.borrow_data_unchecked() };
    if data.len() < TOKEN_ACCOUNT_AMOUNT_OFFSET + 8 {
        return Err(SolarBError::InvalidAccountData.into());
    }
    let mint: Pubkey = data[0..32]
        .try_into()
        .map_err(|_| ProgramError::from(SolarBError::InvalidAccountData))?;
    let amount = u64::from_le_bytes(
        data[TOKEN_ACCOUNT_AMOUNT_OFFSET..TOKEN_ACCOUNT_AMOUNT_OFFSET + 8]
            .try_into()
            .map_err(|_| ProgramError::from(SolarBError::InvalidAccountData))?,
    );
    Ok((mint, amount))
}

pub fn amount_with_slippage(amount: u64, slippage: f64, round_up: bool) -> u64 {
    if round_up {
        ((amount as f64) * (1_f64 + slippage)).ceil() as u64
    } else {
        ((amount as f64) * (1_f64 - slippage)).floor() as u64
    }
}

/// Get mint decimals from a mint account (offset 44 in SPL Token mint layout).
pub fn get_mint_decimals(mint_account: &AccountInfo) -> Result<u8, ProgramError> {
    let data = unsafe { mint_account.borrow_data_unchecked() };
    if data.len() >= 45 {
        Ok(data[44])
    } else {
        Ok(9)
    }
}
