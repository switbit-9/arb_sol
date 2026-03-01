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
    pub price: f64,
    pub fee_factor: f64, // Directional fee factor for this edge's direction (1.0 - fee_rate)
    pub inverse_fee_factor: f64, // Fee factor for the opposite direction (1.0 - fee_rate_opposite)
    /// Pre-computed price * fee_factor scaled by PRICE_SCALE (10^9) for integer-only swap estimation
    pub scaled_price_with_fee: u128,
    /// Max input amount this edge can accept (cached from program instance)
    pub max_amount_in: u64,
    /// Max output amount this edge can produce (cached from program instance)
    pub max_amount_out: u64,
    pub left: Pool,
    pub right: Pool,
}

/// Scaling factor for fixed-point arithmetic (10^9)
const PRICE_SCALE: f64 = 1_000_000_000.0;

impl Edge {
    pub fn new(program: Pubkey, pool_id: Pubkey, side: EdgeSide, price: f64, fee_factor: f64, inverse_fee_factor: f64, max_amount_in: u64, max_amount_out: u64, left: Pool, right: Pool) -> Self {
        let scaled_price_with_fee = (price * fee_factor * PRICE_SCALE) as u128;
        Edge {
            program,
            pool_id,
            side,
            price,
            fee_factor,
            inverse_fee_factor,
            scaled_price_with_fee,
            max_amount_in,
            max_amount_out,
            left,
            right,
        }
    }

    #[inline]
    pub fn get_price(&self) -> f64 {
        self.price
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
