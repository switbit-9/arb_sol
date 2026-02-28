use anchor_lang::prelude::declare_program;
use anyhow::*;

macro_rules! debug_eprintln {
    ($($arg:tt)*) => {{
        #[cfg(any(test, feature = "debug"))]
        {
            eprintln!($($arg)*);
        }
    }};
}

declare_program!(dlmm);

use dlmm::accounts::*;
use dlmm::types::*;

// Note: ID access - the dlmm module is created by declare_program!
// Submodules can access it via crate::dlmm::program::ID if the module is in scope
// For now, we'll access it directly where needed

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
