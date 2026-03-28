//! Zero-allocation CPI helper.
//!
//! `invoke_signed_unchecked` from solana-cpi builds an `Instruction` (2× Vec alloc),
//! then clones it into a `StableInstruction` (2× more Vec alloc) — 4 heap allocations
//! per CPI call. On Solana each `sol_alloc_free` costs ~5 000 CU.
//!
//! This module calls `sol_invoke_signed_rust` directly with a `#[repr(C)]`
//! struct matching `StableInstruction`'s layout, where the inner "vecs" just
//! point at the caller's stack slices.  Total heap allocations: **zero**.

use anchor_lang::solana_program::{
    account_info::AccountInfo,
    instruction::AccountMeta,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use std::ptr::NonNull;

// ── Layout that matches StableVec<T> exactly: {ptr, cap, len} ────────────
#[cfg_attr(not(target_os = "solana"), allow(dead_code))]
#[repr(C)]
struct FakeStableVec<T> {
    ptr: NonNull<T>,
    cap: usize,
    len: usize,
}

// ── Layout that matches StableInstruction exactly ────────────────────────
// StableInstruction = { accounts: StableVec<AccountMeta>, data: StableVec<u8>, program_id: Pubkey }
#[cfg_attr(not(target_os = "solana"), allow(dead_code))]
#[repr(C)]
struct RawStableInstruction {
    accounts: FakeStableVec<AccountMeta>,
    data: FakeStableVec<u8>,
    program_id: Pubkey,
}

// ── Syscall import ───────────────────────────────────────────────────────
#[cfg(target_os = "solana")]
extern "C" {
    fn sol_invoke_signed_rust(
        instruction_addr: *const u8,
        account_infos_addr: *const u8,
        account_infos_len: u64,
        signers_seeds_addr: *const u8,
        signers_seeds_len: u64,
    ) -> u64;
}

/// Invoke a CPI with **zero heap allocations**.
///
/// `metas` and `data` may live on the caller's stack.
/// `account_infos` is the usual `&[AccountInfo]` slice.
///
/// # Safety
/// Same contract as `invoke_signed_unchecked`: caller is responsible for
/// providing correct account infos that match the metas.
#[inline(always)]
pub fn invoke_cpi<'a>(
    program_id: &Pubkey,
    metas: &[AccountMeta],
    data: &[u8],
    account_infos: &[AccountInfo<'a>],
) -> Result<(), ProgramError> {
    invoke_cpi_signed(program_id, metas, data, account_infos, &[])
}

/// Invoke a signed CPI with **zero heap allocations**.
#[inline(always)]
pub fn invoke_cpi_signed<'a>(
    program_id: &Pubkey,
    metas: &[AccountMeta],
    data: &[u8],
    account_infos: &[AccountInfo<'a>],
    signers_seeds: &[&[&[u8]]],
) -> Result<(), ProgramError> {
    #[cfg(target_os = "solana")]
    {
        // SAFETY: FakeStableVec is #[repr(C)] {ptr, cap, len} — identical to StableVec.
        // We point ptr at the caller's slices, set cap = len (no alloc).
        // RawStableInstruction has no Drop impl so nothing tries to free the memory.
        let ix = RawStableInstruction {
            accounts: FakeStableVec {
                ptr: unsafe { NonNull::new_unchecked(metas.as_ptr() as *mut AccountMeta) },
                cap: metas.len(),
                len: metas.len(),
            },
            data: FakeStableVec {
                ptr: unsafe { NonNull::new_unchecked(data.as_ptr() as *mut u8) },
                cap: data.len(),
                len: data.len(),
            },
            program_id: *program_id,
        };

        let result = unsafe {
            sol_invoke_signed_rust(
                &ix as *const _ as *const u8,
                account_infos as *const _ as *const u8,
                account_infos.len() as u64,
                signers_seeds as *const _ as *const u8,
                signers_seeds.len() as u64,
            )
        };

        match result {
            0 => Ok(()),
            e => Err(e.into()),
        }
    }

    #[cfg(not(target_os = "solana"))]
    {
        // Off-chain (test) fallback — delegate to the standard path.
        use anchor_lang::solana_program::{instruction::Instruction, program::invoke_signed_unchecked};
        let ix = Instruction {
            program_id: *program_id,
            accounts: metas.to_vec(),
            data: data.to_vec(),
        };
        invoke_signed_unchecked(&ix, account_infos, signers_seeds)?;
        Ok(())
    }
}
