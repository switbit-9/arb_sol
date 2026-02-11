use anchor_lang::prelude::*;
use anchor_spl::token_interface::TokenAccount;

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