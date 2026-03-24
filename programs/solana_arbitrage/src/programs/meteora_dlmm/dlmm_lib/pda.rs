use super::seeds::*;
use pinocchio::pubkey::Pubkey;

// DLMM program ID from IDL
const DLMM_PROGRAM_ID: Pubkey =
    five8_const::decode_32_const("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");

fn get_dlmm_id() -> Pubkey {
    DLMM_PROGRAM_ID
}

pub fn derive_bin_array_pda(lb_pair: Pubkey, bin_array_index: i64) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(
        &[BIN_ARRAY, &lb_pair, &bin_array_index.to_le_bytes()],
        &get_dlmm_id(),
    )
}

pub fn derive_bin_array_bitmap_extension(lb_pair: Pubkey) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(
        &[BIN_ARRAY_BITMAP_SEED, &lb_pair],
        &get_dlmm_id(),
    )
}

pub fn derive_event_authority_pda() -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(&[b"__event_authority"], &get_dlmm_id())
}
