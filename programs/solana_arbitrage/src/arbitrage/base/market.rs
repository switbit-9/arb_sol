use super::{edge::Edge, edge::EdgeSide, pool::Pool};
use crate::programs::ProgramMeta;
use pinocchio::pubkey::Pubkey;
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
        let (price, inverse_price) = self.program.get_prices().unwrap();
        let (fee_a_to_b, fee_b_to_a) = self.program.get_fee_factor().unwrap_or((1.0, 1.0));
        let program_id = *self.program.get_id();
        let pool_id = *self.program.get_pool_id();
        let (buy_max_in, buy_max_out) = self.program.get_cached_max_amounts(self.left.mint_account);
        let (sell_max_in, sell_max_out) = self.program.get_cached_max_amounts(self.right.mint_account);

        vec![
            Edge::new(
                program_id,
                pool_id,
                EdgeSide::LeftToRight,
                price,
                fee_a_to_b,
                fee_b_to_a,
                buy_max_in,
                buy_max_out,
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
                sell_max_in,
                sell_max_out,
                self.right.clone(),
                self.left.clone(),
            ),
        ]
    }
}
