// Include all modules from damm_v2/src/lib.rs to maintain crate:: references
#![allow(unexpected_cfgs)]
#![allow(deprecated)]

#[macro_use]
#[path = "src/macros.rs"]
pub mod macros;

#[path = "src/const_pda.rs"]
pub mod const_pda;

#[path = "src/constants.rs"]
pub mod constants;

#[path = "src/error.rs"]
pub mod error;

#[path = "src/event.rs"]
pub mod event;

#[path = "src/curve.rs"]
pub mod curve;

#[path = "src/base_fee/mod.rs"]
pub mod base_fee;

#[path = "src/math/mod.rs"]
pub mod math;

#[path = "src/pool_action_access/mod.rs"]
pub mod pool_action_access;

#[path = "src/params/mod.rs"]
pub mod params;

#[path = "src/state/mod.rs"]
pub mod state;

#[path = "src/utils/mod.rs"]
pub mod utils;

// Re-export what we need (matching lib.rs exports)
pub use error::*;
pub use event::*;
pub use math::*;
pub use pool_action_access::*;
pub use utils::*;

// Re-export specific items for external use
pub use params::swap::TradeDirection;
pub use state::{fee::FeeMode, Pool};
pub use utils::activation_handler::ActivationType;
