use anchor_lang::prelude::*;
use crate::programs::{ProgramInstance, ProgramMeta};
use crate::dex_type;
use crate::utils::bot_config::BotConfig;
use crate::pool_vec::PoolVec;
use crate::{parse_accounts, InstructionData, arb_mode, WSOL};

#[cfg(target_os = "solana")]
fn sol_log_compute_units() {
    unsafe { solana_msg::syscalls::sol_log_compute_units_() }
}
#[cfg(not(target_os = "solana"))]
fn sol_log_compute_units() {}

/// Public wrapper so other modules can log remaining CU.
pub fn log_cu() {
    sol_log_compute_units();
}

#[cfg(target_os = "solana")]
fn sol_remaining_compute_units() -> u64 {
    extern "C" { fn sol_remaining_compute_units() -> u64; }
    unsafe { sol_remaining_compute_units() }
}
#[cfg(not(target_os = "solana"))]
fn sol_remaining_compute_units() -> u64 { 0 }
