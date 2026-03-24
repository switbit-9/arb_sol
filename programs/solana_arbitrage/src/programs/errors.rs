use pinocchio::program_error::ProgramError;

/// Program-specific error codes. Variants map to ProgramError::Custom(N).
/// Numbering starts at 0 (unlike Anchor which starts at 6000).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolarBError {
    InsufficientAccounts = 0,
    InsufficientBinArray = 1,
    AccountMismatch = 2,
    AccountSpanMismatch = 3,
    InvalidAccountsLength = 4,
    UnknownProgram = 5,
    TrailingAccounts = 6,
    TransferFeeCalculateNotMatch = 7,
    NoProfitFound = 8,
    NoProfitFound2 = 9,
    InsufficientFunds = 10,
    TransferFeeCalculationError = 11,
    InvalidPathLength = 12,
    InvalidPathType = 13,
    InvalidProgramType = 14,
    FeeOverflow = 15,
    InvalidAccountData = 16,
    InvalidMode = 17,
    Unauthorized = 18,
}

impl From<SolarBError> for ProgramError {
    #[inline(always)]
    fn from(e: SolarBError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
