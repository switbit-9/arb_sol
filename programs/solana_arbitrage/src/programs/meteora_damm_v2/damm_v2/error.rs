//! Error module includes error messages and codes of the program
use pinocchio::program_error::ProgramError;

/// Error messages and codes of the program
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PoolError {
    MathOverflow,
    InvalidFee,
    ExceededSlippage,
    PoolDisabled,
    ExceedMaxFeeBps,
    InvalidAdmin,
    AmountIsZero,
    TypeCastFailed,
    UnableToModifyActivationPoint,
    InvalidAuthorityToCreateThePool,
    InvalidActivationType,
    InvalidActivationPoint,
    InvalidQuoteMint,
    InvalidFeeCurve,
    InvalidPriceRange,
    PriceRangeViolation,
    InvalidParameters,
    InvalidCollectFeeMode,
    InvalidInput,
    CannotCreateTokenBadgeOnSupportedMint,
    InvalidTokenBadge,
    InvalidMinimumLiquidity,
    InvalidVestingInfo,
    InsufficientLiquidity,
    InvalidVestingAccount,
    InvalidPoolStatus,
    UnsupportNativeMintToken2022,
    InvalidRewardIndex,
    InvalidRewardDuration,
    RewardInitialized,
    RewardUninitialized,
    InvalidRewardVault,
    MustWithdrawnIneligibleReward,
    IdenticalRewardDuration,
    RewardCampaignInProgress,
    IdenticalFunder,
    InvalidFunder,
    RewardNotEnded,
    FeeInverseIsIncorrect,
    PositionIsNotEmpty,
    InvalidPoolCreatorAuthority,
    InvalidConfigType,
    InvalidPoolCreator,
    RewardVaultFrozenSkipRequired,
    InvalidSplitPositionParameters,
    UnsupportPositionHasVestingLock,
    SamePosition,
    InvalidBaseFeeMode,
    InvalidFeeRateLimiter,
    FailToValidateSingleSwapInstruction,
    InvalidFeeScheduler,
    UndeterminedError,
    InvalidPoolVersion,
}

impl From<PoolError> for ProgramError {
    #[inline(always)]
    fn from(e: PoolError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
