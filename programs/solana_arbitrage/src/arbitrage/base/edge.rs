use std::{fmt::Debug, hash::Hash};

use anchor_lang::prelude::Pubkey;

use super::pool::Pool;

#[derive(Clone, Debug, PartialEq)]
pub enum EdgeSide {
    LeftToRight,
    RightToLeft,
}

#[derive(Clone)]
pub struct Edge {
    pub program: Pubkey,
    pub side: EdgeSide,
    pub price: f64, // Stored as scaled integer: actual_price * 1_000_000_000
    pub left: Pool,
    pub right: Pool,
}

impl Edge {
    pub fn new(program: Pubkey, side: EdgeSide, price: f64, left: Pool, right: Pool) -> Self {
        Edge {
            program,
            side,
            price,
            left,
            right,
        }
    }

    pub fn get_price(&self) -> f64 {
        return self.price;
    }

    fn get_pools_amount_difference(&self) -> u128 {
        return self.right.get_amount() - self.left.get_amount();
    }

    pub fn compute_amount(&mut self, amount: u128) -> u128 {
        (amount as f64 * self.get_price()) as u128
    }
}

impl Debug for Edge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            format!(
                "({}) {}->[{}]->{}",
                self.program,
                self.left.mint_account,
                self.get_price(),
                self.right.mint_account
            )
            .as_str(),
        )
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
        let program_id = self.program;
        let left_pool = self.left.mint_account;
        let right_pool = self.right.mint_account;
        format!("{} {} {}", program_id, left_pool, right_pool).hash(state);
    }
}

impl Eq for Edge {}
