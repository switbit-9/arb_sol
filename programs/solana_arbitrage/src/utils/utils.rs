use anchor_lang::prelude::*;
use anchor_spl::token_interface::TokenAccount;
use crate::programs::SolarBError;

// SPL Token account layout: mint (32) + owner (32) + amount (8)
const TOKEN_ACCOUNT_AMOUNT_OFFSET: usize = 64;

/// Read only the amount field from an SPL Token account (offset 64).
/// ~10 CU vs ~150-200 CU for full TokenAccount::try_deserialize.
#[inline(always)]
pub fn read_token_amount(account: &AccountInfo) -> Result<u64> {
    let data = account.try_borrow_data()?;
    if data.len() < TOKEN_ACCOUNT_AMOUNT_OFFSET + 8 {
        return Err(error!(SolarBError::InvalidAccountData));
    }
    Ok(u64::from_le_bytes(
        data[TOKEN_ACCOUNT_AMOUNT_OFFSET..TOKEN_ACCOUNT_AMOUNT_OFFSET + 8]
            .try_into()
            .map_err(|_| error!(SolarBError::InvalidAccountData))?,
    ))
}

/// Read mint pubkey and amount from an SPL Token vault account.
/// Layout: mint (0..32) + owner (32..64) + amount (64..72)
#[inline(always)]
pub fn read_vault_data(account: &AccountInfo) -> Result<(Pubkey, u64)> {
    let data = account.try_borrow_data()?;
    if data.len() < TOKEN_ACCOUNT_AMOUNT_OFFSET + 8 {
        return Err(error!(SolarBError::InvalidAccountData));
    }
    let mint = Pubkey::new_from_array(
        data[0..32]
            .try_into()
            .map_err(|_| error!(SolarBError::InvalidAccountData))?,
    );
    let amount = u64::from_le_bytes(
        data[TOKEN_ACCOUNT_AMOUNT_OFFSET..TOKEN_ACCOUNT_AMOUNT_OFFSET + 8]
            .try_into()
            .map_err(|_| error!(SolarBError::InvalidAccountData))?,
    );
    Ok((mint, amount))
}

pub fn parse_token_account<'info>(account: &AccountInfo<'info>) -> Result<TokenAccount> {
    let mut data = &account.try_borrow_data()?[..];
    let token_account = TokenAccount::try_deserialize(&mut data)?;
    Ok(token_account)
}



pub fn amount_with_slippage(amount: u64, slippage: f64, round_up: bool) -> u64 {
    if round_up {
        ((amount as f64) * (1_f64 + slippage)).ceil() as u64
    } else {
        ((amount as f64) * (1_f64 - slippage)).floor() as u64
    }
}

/// Get mint decimals from a mint account
pub fn get_mint_decimals(mint_account: &AccountInfo) -> Result<u8> {
    let data = mint_account.try_borrow_data()?;
    // Mint decimals are at offset 44 in SPL Token mint layout
    if data.len() >= 45 {
        Ok(data[44])
    } else {
        Ok(9) // Default to 9 decimals
    }
}