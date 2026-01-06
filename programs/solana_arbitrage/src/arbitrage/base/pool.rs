use anchor_lang::prelude::Pubkey;

#[derive(Debug, Clone)]
pub struct Pool {
    pub mint_account: Pubkey,
    pub amount: u128,
}

impl Pool {
    pub fn new(mint_account: &Pubkey, amount: u128) -> Self {
        Pool {
            mint_account: *mint_account,
            amount,
        }
    }

    pub fn get_amount(&self) -> &u128 {
        &self.amount
    }
}
