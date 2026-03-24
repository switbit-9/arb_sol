use anyhow::*;

macro_rules! debug_eprintln {
    ($($arg:tt)*) => {{
        #[cfg(any(test, feature = "debug"))]
        {
            eprintln!($($arg)*);
        }
    }};
}

pub mod dlmm;
pub use dlmm::accounts::*;
pub use dlmm::types::*;
pub use dlmm::{ProtocolFee, RewardInfo, StaticParameters, VariableParameters};

pub mod constants;
pub use constants::*;

pub mod conversions;
pub use conversions::*;

pub mod extensions;
pub use extensions::*;

pub mod pda;
pub use pda::*;

pub mod quote;
pub use quote::*;

pub mod seeds;
pub use seeds::*;

pub mod math;
pub use math::*;

pub mod typedefs;
pub use typedefs::*;

pub mod token;
pub use token::*;
