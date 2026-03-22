pub mod arbitrage_path;
pub use arbitrage_path::{ArbitragePath, EdgeArray};

#[cfg(any(test, feature = "debug"))]
mod simple_method;
#[cfg(any(test, feature = "debug"))]
pub use simple_method::{
    check_arbitrage,
    get_edges,
    find_cross_arbitrage_iterative,
    find_triangular_arbitrage_iterative,
};

#[cfg(any(test, feature = "debug"))]
mod optimized_method;
#[cfg(any(test, feature = "debug"))]
pub use optimized_method::find_cross_arbitrage_optimized;

pub mod optimal_amount_in_v2;
pub use optimal_amount_in_v2::find_optimal_amount_in_v2;


