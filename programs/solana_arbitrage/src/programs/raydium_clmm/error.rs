use solana_program::program_error::ProgramError;

const CLMM_ERROR_BASE: u32 = 6300;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    TickOutOfRange = 0,
    TickUpperOverflow = 1,
    TickLowerOverflow = 2,
    SqrtPriceX64 = 3,
    LiquidityOverflow = 4,
    LiquidityUnderflow = 5,
    AmountOverflow = 6,
    DivisionByZero = 7,
    CastFailed = 8,
    SwapNotEnabled = 9,
    InvalidTickIndex = 10,
    InvalidTickArray = 11,
    InsufficientLiquidityForDirection = 12,
    PriceLimitReached = 13,
    NotApproved = 14,
    FullRewardInfo = 15,
    RewardTokenAlreadyInUse = 16,
    ExceptRewardMint = 17,
    MissingTickArrayBitmapExtensionAccount = 18,
}

impl From<ErrorCode> for ProgramError {
    fn from(e: ErrorCode) -> Self {
        ProgramError::Custom(CLMM_ERROR_BASE + e as u32)
    }
}
