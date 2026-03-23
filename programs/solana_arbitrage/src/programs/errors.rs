use anchor_lang::prelude::*;

#[error_code]
pub enum SolarBError {
    #[msg("insufficient accounts provided for the requested program")]
    InsufficientAccounts,
    #[msg("bin array not found in provided accounts")]
    InsufficientBinArray,
    #[msg("account pubkey does not match expected template")]
    AccountMismatch,
    #[msg("provided accounts length does not match registered span")]
    AccountSpanMismatch,
    #[msg("provided accounts length cannot be represented on this platform")]
    InvalidAccountsLength,
    #[msg("no registered program matched the supplied program id")]
    UnknownProgram,
    #[msg("unused accounts remain after parsing instruction data")]
    TrailingAccounts,
    #[msg("TransferFee calculate not match")]
    TransferFeeCalculateNotMatch,
    #[msg("Not Found")]
    NoProfitFound,
    #[msg("Not")]
    NoProfitFound2,
    #[msg("insufficient funds in payer account")]
    InsufficientFunds,
    #[msg("TransferFee calculation error")]
    TransferFeeCalculationError,
    #[msg("Invalid path length for optimization")]
    InvalidPathLength,
    #[msg("Invalid path type for this optimization method")]
    InvalidPathType,
    #[msg("Invalid program type for this operation")]
    InvalidProgramType,
    #[msg("Fee overflow")]
    FeeOverflow,
    #[msg("Invalid account data format")]
    InvalidAccountData,
    #[msg("Invalid arbitrage mode specified")]
    InvalidMode,
    #[msg("Unauthorized: invalid auth key")]
    Unauthorized,
}
