use pinocchio::pubkey::Pubkey;

#[derive(Debug, Clone, Copy)]
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
