use anchor_lang::prelude::*;

#[error_code]
pub enum SolarBError {
    #[msg("insufficient accounts provided for the requested program")]
    InsufficientAccounts,
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
    #[msg("no profitable arbitrage opportunity found")]
    NoProfitFound,
    #[msg("insufficient funds in payer account")]
    InsufficientFunds,
    #[msg("TransferFee calculation error")]
    TransferFeeCalculationError,
}
