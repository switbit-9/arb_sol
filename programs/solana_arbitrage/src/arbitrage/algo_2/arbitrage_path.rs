use crate::arbitrage::base::Edge;

#[derive(Clone, Debug)]
pub struct ArbitragePath {
    pub edges: Vec<Edge>,
    pub profit: i128,
    pub final_amount: u128,
    pub start_amount: u64,
}
