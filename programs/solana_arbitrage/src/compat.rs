//! Compatibility layer for Anchor → Pinocchio migration.
//!
//! Re-exports types used across the codebase so that individual files
//! can switch from `use anchor_lang::prelude::*` to `use crate::compat::*`
//! without changing any logic.
//!
//! Phase 1: re-exports from solana-program (drop-in for anchor_lang::prelude)
//! Phase 2: swap to pinocchio types (final migration step)

// ── Core types ─────────────────────────────────────────────────────────────
pub use solana_program::account_info::AccountInfo;
pub use solana_program::clock::Clock;
pub use solana_program::sysvar::Sysvar;
pub use solana_program::instruction::AccountMeta;
pub use solana_program::program_error::ProgramError;
pub use solana_program::pubkey::Pubkey;
pub use solana_program::msg;

// ── Result alias (replaces anchor_lang::Result) ────────────────────────────
pub type Result<T> = core::result::Result<T, ProgramError>;

// ── Constants (replaces anchor_spl re-exports) ─────────────────────────────
pub use spl_token::native_mint::ID as WSOL;

/// SPL Token program ID (replaces `Token::id()`)
pub const SPL_TOKEN_ID: Pubkey = spl_token::ID;

// ── Error macros ───────────────────────────────────────────────────────────
// These will replace anchor's error!/err!/require! macros.
// During the transition period (anchor still present), files that have been
// migrated to use crate::compat::* will use these via explicit path:
//   crate::solar_error!(...) / crate::solar_err!(...) / crate::solar_require!(...)
// Once anchor is fully removed, we'll rename them to error!/err!/require!.

/// Replaces `error!(SolarBError::Variant)` → returns `ProgramError::Custom(code)`
#[macro_export]
macro_rules! solar_error {
    ($err:expr) => {
        solana_program::program_error::ProgramError::from($err)
    };
}

/// Replaces `err!(SolarBError::Variant)` → returns `Err(ProgramError::Custom(code))`
#[macro_export]
macro_rules! solar_err {
    ($err:expr) => {
        Err(solana_program::program_error::ProgramError::from($err))
    };
}

/// Replaces `require!(cond, SolarBError::Variant)`
#[macro_export]
macro_rules! solar_require {
    ($cond:expr, $err:expr) => {
        if !$cond {
            return Err(solana_program::program_error::ProgramError::from($err));
        }
    };
}
