/// Errors that may be returned by the TokenSwap program.
use pinocchio::program_error::ProgramError;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ErrorCode {
    NotApproved,
    InvalidOwner,
    EmptySupply,
    InvalidInput,
    IncorrectLpMint,
    ExceededSlippage,
    ZeroTradingTokens,
    NotSupportMint,
    InvalidVault,
    InitLpAmountTooLess,
    TransferFeeCalculateNotMatch,
    MathOverflow,
    InsufficientVault,
    InvalidFeeModel,
    NoFeeCollect,
}

impl From<ErrorCode> for ProgramError {
    #[inline(always)]
    fn from(e: ErrorCode) -> Self {
        ProgramError::Custom(e as u32)
    }
}
