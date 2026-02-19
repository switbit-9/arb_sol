use super::{edge::Edge, edge::EdgeSide, pool::Pool};
use crate::programs::ProgramMeta;
use anchor_lang::solana_program::pubkey::Pubkey;
use std::collections::HashSet;

pub struct Market<'info, T: ProgramMeta + ?Sized> {
    program: &'info T,
    left: Pool,
    right: Pool,
}

impl<'info, T: ProgramMeta + ?Sized> Market<'info, T> {
    pub fn new(program: &'info T, left: Pool, right: Pool) -> Self {
        Market {
            program,
            left,
            right,
        }
    }
    pub fn get_unique_currencies(markets: &[Market<'info, T>]) -> HashSet<Pubkey> {
        let mut set: HashSet<Pubkey> = HashSet::new();
        for market in markets {
            set.insert(market.left.mint_account.to_owned());
            set.insert(market.right.mint_account.to_owned());
        }
        return set;
    }


    pub fn generate_edges(&'info self) -> Vec<Edge> {
        // Compute prices - using a simple division for now
        // In a real implementation, you'd want to use the program's compute_price methods
        let prices = self.program.get_prices().unwrap();
        let price = prices.0;
        let inverse_price = prices.1;
        let (fee_a_to_b, fee_b_to_a) = self.program.get_fee_factor().unwrap_or((1.0, 1.0));
        let program_id = *self.program.get_id();
        let pool_id = *self.program.get_pool_id();
        vec![
            Edge::new(
                program_id,
                pool_id,
                EdgeSide::LeftToRight,
                price,
                fee_a_to_b,
                fee_b_to_a,
                self.left.clone(),
                self.right.clone(),
            ),
            Edge::new(
                program_id,
                pool_id,
                EdgeSide::RightToLeft,
                inverse_price,
                fee_b_to_a,
                fee_a_to_b,
                self.right.clone(),
                self.left.clone(),
            ),
        ]
    }
}
