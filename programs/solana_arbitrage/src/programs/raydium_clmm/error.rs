use pinocchio::program_error::ProgramError;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ErrorCode {
    TickOutOfRange,
    TickUpperOverflow,
    TickLowerOverflow,
    SqrtPriceX64,
    LiquidityOverflow,
    LiquidityUnderflow,
    AmountOverflow,
    DivisionByZero,
    CastFailed,
    SwapNotEnabled,
    InvalidTickIndex,
    InvalidTickArray,
    InsufficientLiquidityForDirection,
    PriceLimitReached,
    NotApproved,
    FullRewardInfo,
    RewardTokenAlreadyInUse,
    ExceptRewardMint,
    MissingTickArrayBitmapExtensionAccount,
}

impl From<ErrorCode> for ProgramError {
    #[inline(always)]
    fn from(e: ErrorCode) -> Self {
        ProgramError::Custom(e as u32)
    }
}
