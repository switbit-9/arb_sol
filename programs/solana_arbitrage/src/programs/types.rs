use anchor_lang::solana_program::instruction::Instruction;
use num_enum::{IntoPrimitive, TryFromPrimitive};
/// A struct containing swap instructions and metadata.
/// This is used across all swap-related program implementations.
#[derive(Debug)]
pub struct SwapInstructions {
    /// A vector of Solana `Instruction` objects required to execute the swap.
    pub instructions: Vec<Instruction>,

    /// The expected output amount from the swap.
    pub amount_out: u64,
}

/// Trade (swap) direction
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, IntoPrimitive, TryFromPrimitive)]
pub enum TradeDirection {
    /// Input token A, output token B
    AtoB,
    /// Input token B, output token A
    BtoA,
}
