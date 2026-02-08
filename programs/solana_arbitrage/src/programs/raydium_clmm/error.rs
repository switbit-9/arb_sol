use anchor_lang::prelude::*;

#[error_code]
pub enum ErrorCode {
    #[msg("Tick out of supported range")]
    TickOutOfRange,
    #[msg("Tick upper overflow")]
    TickUpperOverflow,
    #[msg("Tick lower overflow")]
    TickLowerOverflow,
    #[msg("Invalid sqrt price")]
    SqrtPriceX64,
    #[msg("Liquidity overflow")]
    LiquidityOverflow,
    #[msg("Liquidity underflow")]
    LiquidityUnderflow,
    #[msg("Amount overflow")]
    AmountOverflow,
    #[msg("Division by zero")]
    DivisionByZero,
    #[msg("Cast failed")]
    CastFailed,
    #[msg("Swap not enabled")]
    SwapNotEnabled,
    #[msg("Invalid tick index")]
    InvalidTickIndex,
    #[msg("Invalid tick array")]
    InvalidTickArray,
    #[msg("Insufficient liquidity for this direction")]
    InsufficientLiquidityForDirection,
    #[msg("Price limit reached")]
    PriceLimitReached,
    #[msg("Not approved")]
    NotApproved,
    #[msg("Full reward info")]
    FullRewardInfo,
    #[msg("Reward token already in use")]
    RewardTokenAlreadyInUse,
    #[msg("Except reward mint")]
    ExceptRewardMint,
    #[msg("Missing tick array bitmap extension account")]
    MissingTickArrayBitmapExtensionAccount,
}
