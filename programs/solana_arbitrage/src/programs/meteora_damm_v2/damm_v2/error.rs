//! Error module includes error messages and codes of the program
use solana_program::program_error::ProgramError;

const POOL_ERROR_BASE: u32 = 6100;

/// Error messages and codes of the program
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolError {
    MathOverflow = 0,
    InvalidFee = 1,
    ExceededSlippage = 2,
    PoolDisabled = 3,
    ExceedMaxFeeBps = 4,
    InvalidAdmin = 5,
    AmountIsZero = 6,
    TypeCastFailed = 7,
    UnableToModifyActivationPoint = 8,
    InvalidAuthorityToCreateThePool = 9,
    InvalidActivationType = 10,
    InvalidActivationPoint = 11,
    InvalidQuoteMint = 12,
    InvalidFeeCurve = 13,
    InvalidPriceRange = 14,
    PriceRangeViolation = 15,
    InvalidParameters = 16,
    InvalidCollectFeeMode = 17,
    InvalidInput = 18,
    CannotCreateTokenBadgeOnSupportedMint = 19,
    InvalidTokenBadge = 20,
    InvalidMinimumLiquidity = 21,
    InvalidVestingInfo = 22,
    InsufficientLiquidity = 23,
    InvalidVestingAccount = 24,
    InvalidPoolStatus = 25,
    UnsupportNativeMintToken2022 = 26,
    InvalidRewardIndex = 27,
    InvalidRewardDuration = 28,
    RewardInitialized = 29,
    RewardUninitialized = 30,
    InvalidRewardVault = 31,
    MustWithdrawnIneligibleReward = 32,
    IdenticalRewardDuration = 33,
    RewardCampaignInProgress = 34,
    IdenticalFunder = 35,
    InvalidFunder = 36,
    RewardNotEnded = 37,
    FeeInverseIsIncorrect = 38,
    PositionIsNotEmpty = 39,
    InvalidPoolCreatorAuthority = 40,
    InvalidConfigType = 41,
    InvalidPoolCreator = 42,
    RewardVaultFrozenSkipRequired = 43,
    InvalidSplitPositionParameters = 44,
    UnsupportPositionHasVestingLock = 45,
    SamePosition = 46,
    InvalidBaseFeeMode = 47,
    InvalidFeeRateLimiter = 48,
    FailToValidateSingleSwapInstruction = 49,
    InvalidFeeScheduler = 50,
    UndeterminedError = 51,
    InvalidPoolVersion = 52,
}

impl From<PoolError> for ProgramError {
    fn from(e: PoolError) -> Self {
        ProgramError::Custom(POOL_ERROR_BASE + e as u32)
    }
}
