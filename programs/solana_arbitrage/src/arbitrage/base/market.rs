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
        let price_left_to_right = if self.left.amount > 0 {
            self.right.amount as f64 / self.left.amount as f64
        } else {
            0.0
        };
        let price_right_to_left = if self.right.amount > 0 {
            self.left.amount as f64 / self.right.amount as f64
        } else {
            0.0
        };
        let program_id = *self.program.get_id();
        vec![
            Edge::new(
                program_id,
                EdgeSide::LeftToRight,
                price_left_to_right,
                self.left.clone(),
                self.right.clone(),
            ),
            Edge::new(
                program_id,
                EdgeSide::RightToLeft,
                price_right_to_left,
                self.right.clone(),
                self.left.clone(),
            ),
        ]
    }
}
