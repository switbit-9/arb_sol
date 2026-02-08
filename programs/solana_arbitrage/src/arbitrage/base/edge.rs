use std::{fmt::Debug, hash::Hash};

use anchor_lang::prelude::Pubkey;

use super::pool::Pool;

#[derive(Clone, Debug, PartialEq)]
pub enum EdgeSide {
    LeftToRight,
    RightToLeft,
}

#[derive(Clone, Debug)]
pub struct Edge {
    pub program: Pubkey,
    pub pool_id: Pubkey,
    pub side: EdgeSide,
    pub price: f64, // Stored as scaled integer: actual_price * 1_000_000_000
    pub fee_factor: f64, // 1.0 - fee_rate (e.g., 0.9975 for 0.25% fee)
    pub left: Pool,
    pub right: Pool,
    pub amount_in: u64,
    pub amount_out: u64,
}

impl Edge {
    pub fn new(program: Pubkey, pool_id: Pubkey, side: EdgeSide, price: f64, fee_factor: f64, left: Pool, right: Pool) -> Self {
        Edge {
            program,
            pool_id,
            side,
            price,
            fee_factor,
            left,
            right,
            amount_in: 0, // TODO: Remove this
            amount_out: 0, // TODO: Remove this
        }
    }

    pub fn get_price(&self) -> f64 {
        return self.price;
    }
}

impl PartialEq for Edge {
    fn eq(&self, other: &Edge) -> bool {
        return self.program.eq(&other.program)
            && self.left.mint_account.eq(&other.left.mint_account)
            && self.right.mint_account.eq(&other.right.mint_account);
    }
}

impl Hash for Edge {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Direct byte hashing - avoids String allocation from format!()
        state.write(self.program.as_ref());
        state.write(self.left.mint_account.as_ref());
        state.write(self.right.mint_account.as_ref());
    }
}

impl Eq for Edge {}
