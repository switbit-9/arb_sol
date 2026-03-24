//! Zero-allocation CPI helper using pinocchio's C-ABI syscall.
//!
//! `sol_invoke_signed_c` takes a `SolInstruction*` (C struct with raw pointers —
//! no Vecs, no allocations). pinocchio's `Instruction` matches this layout exactly,
//! so we get zero-heap CPI for free. The old `FakeStableVec/RawStableInstruction`
//! hack is no longer needed.

use pinocchio::{
    account_info::AccountInfo,
    instruction::{AccountMeta, Instruction},
    instruction::Signer,
    program::slice_invoke,
    program::slice_invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
};

/// Invoke a CPI with zero heap allocations.
///
/// `metas` and `data` live on the caller's stack.
#[inline(always)]
pub fn invoke_cpi(
    program_id: &Pubkey,
    metas: &[AccountMeta],
    data: &[u8],
    account_infos: &[&AccountInfo],
) -> Result<(), ProgramError> {
    let ix = Instruction {
        program_id,
        accounts: metas,
        data,
    };
    slice_invoke(&ix, account_infos)
}

/// Invoke a signed CPI with zero heap allocations.
#[inline(always)]
pub fn invoke_cpi_signed(
    program_id: &Pubkey,
    metas: &[AccountMeta],
    data: &[u8],
    account_infos: &[&AccountInfo],
    signers_seeds: &[Signer],
) -> Result<(), ProgramError> {
    let ix = Instruction {
        program_id,
        accounts: metas,
        data,
    };
    slice_invoke_signed(&ix, account_infos, signers_seeds)
}
