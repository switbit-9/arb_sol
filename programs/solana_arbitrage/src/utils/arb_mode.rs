/// CASE 1: Single token pair, multiple markets (SOL -> TOKEN1 -> SOL)
pub const SINGLE_PAIR_MULTI_MARKET: u8 = 0;
/// CASE 2: Multi-hop chain (SOL -> TOKEN1 -> USDC -> SOL)
pub const MULTI_HOP_CHAIN: u8 = 1;
/// CASE 3: Multiple independent trades to evaluate
pub const MULTIPLE_TRADES: u8 = 2;
