use anchor_lang::prelude::Pubkey;

#[derive(Debug, Clone)]
pub struct Pool {
    pub mint_account: Pubkey,
}

impl Pool {
    pub fn new(mint_account: &Pubkey) -> Self {
        Pool {
            mint_account: *mint_account,
        }
    }
}
