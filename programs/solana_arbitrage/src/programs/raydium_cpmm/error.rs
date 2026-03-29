use solana_program::program_error::ProgramError;

const CPMM_ERROR_BASE: u32 = 6200;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    NotApproved = 0,
    InvalidOwner = 1,
    EmptySupply = 2,
    InvalidInput = 3,
    IncorrectLpMint = 4,
    ExceededSlippage = 5,
    ZeroTradingTokens = 6,
    NotSupportMint = 7,
    InvalidVault = 8,
    InitLpAmountTooLess = 9,
    TransferFeeCalculateNotMatch = 10,
    MathOverflow = 11,
    InsufficientVault = 12,
    InvalidFeeModel = 13,
    NoFeeCollect = 14,
}

impl From<ErrorCode> for ProgramError {
    fn from(e: ErrorCode) -> Self {
        ProgramError::Custom(CPMM_ERROR_BASE + e as u32)
    }
}
