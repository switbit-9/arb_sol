use anchor_lang::prelude::*;
use anchor_spl::token_interface::TokenAccount;

pub fn parse_token_account<'info>(account: &AccountInfo<'info>) -> Result<TokenAccount> {
    let mut data = &account.try_borrow_data()?[..];
    let token_account = TokenAccount::try_deserialize(&mut data)?;
    Ok(token_account)
}
