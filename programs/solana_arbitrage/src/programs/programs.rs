use anchor_lang::prelude::*;
use anchor_lang::solana_program::pubkey::Pubkey;

/// Type alias: ProgramInstance is now a boxed trait object instead of an enum.
/// All programs implement ProgramMeta, so callers use the trait interface directly.
pub type ProgramInstance<'info> = Box<dyn ProgramMeta + 'info>;

pub trait ProgramMeta {
    fn get_id(&self) -> &Pubkey;

    /// Get the unique pool ID for this instance
    fn get_pool_id(&self) -> &Pubkey;

    /// Human-readable program name (e.g. "PumpAmm", "MeteoraDLMM")
    fn name(&self) -> &'static str;

    /// Compute price for swap base in (base -> quote)
    fn compute_price_swap_base_in(&self, base_amount: u128, quote_amount: u128) -> Result<f64> {
        if base_amount > 0 {
            Ok(quote_amount as f64 / base_amount as f64)
        } else {
            Ok(0.0)
        }
    }

    /// Compute price for swap base out (quote -> base)
    fn compute_price_swap_base_out(&self, base_amount: u128, quote_amount: u128) -> Result<f64> {
        if quote_amount > 0 {
            Ok(base_amount as f64 / quote_amount as f64)
        } else {
            Ok(0.0)
        }
    }

    /// Get base and quote token mints
    fn get_mints(&self) -> (&Pubkey, &Pubkey);

    /// Get base token pubkey
    fn get_base_token(&self) -> Pubkey {
        *self.get_mints().0
    }

    /// Get quote token pubkey
    fn get_quote_token(&self) -> Pubkey {
        *self.get_mints().1
    }

    /// Calculate output amount for swap base in (base -> quote)
    fn swap_base_in<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        clock: Clock,
    ) -> Result<u64>;

    /// Calculate input amount needed to receive a specific output amount
    /// Given output_mint and amount_out, returns the required amount_in
    fn swap_base_out<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        clock: Clock,
    ) -> Result<u64>;

    /// Get prices for swap base in (base -> quote) and swap base out (quote -> base)
    fn get_prices(&self) -> Result<(f64, f64)>;

    /// Invoke swap base in (base -> quote)
    fn invoke_swap_base_in<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        max_amount_in: u64,
        amount_out: Option<u64>,
        payer: AccountInfo<'a>,
        user_mint_1_token_account: AccountInfo<'a>,
        user_mint_2_token_account: AccountInfo<'a>,
        mint_1_account: AccountInfo<'a>,
        mint_2_account: AccountInfo<'a>,
        mint_1_token_program: AccountInfo<'a>,
        mint_2_token_program: AccountInfo<'a>,
    ) -> Result<()>;

    /// Invoke swap base out (quote -> base)
    fn invoke_swap_base_out<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        min_amount_out: Option<u64>,
        payer: AccountInfo<'a>,
        user_mint_1_token_account: AccountInfo<'a>,
        user_mint_2_token_account: AccountInfo<'a>,
        mint_1_account: AccountInfo<'a>,
        mint_2_account: AccountInfo<'a>,
        mint_1_token_program: AccountInfo<'a>,
        mint_2_token_program: AccountInfo<'a>,
    ) -> Result<()>;

    /// Log account information for debugging
    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()>;

    /// Calculate optimal amount in for AMM types
    fn calculate_optimal_amount_in(&self, _input_mint: Pubkey, _target_price: f64) -> Result<u64> {
        Err(error!(crate::programs::SolarBError::InvalidProgramType))
    }

    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        Err(error!(crate::programs::SolarBError::InvalidProgramType))
    }

    /// Get directional fee factors: (fee_factor_a_to_b, fee_factor_b_to_a)
    /// Each is (1 - fee_rate) for that direction, e.g. 0.9975 for 0.25% fee.
    /// For AMMs with symmetric fees, both values are identical.
    fn get_fee_factor(&self) -> Result<(f64, f64)> {
        Err(error!(crate::programs::SolarBError::InvalidProgramType))
    }

    fn get_max_amounts_in_out<'a>(&self, _accounts: &[AccountInfo<'a>], _input_mint: Pubkey) -> Result<(u64, u64)> {
        Err(error!(crate::programs::SolarBError::InvalidProgramType))
    }

    /// Get liquidity for concentrated liquidity AMMs
    fn get_liquidity(&self) -> Result<u128> {
        Err(error!(crate::programs::SolarBError::InvalidProgramType))
    }

    /// Get sqrt_price for concentrated liquidity AMMs
    fn get_sqrt_price(&self) -> Result<u128> {
        Err(error!(crate::programs::SolarBError::InvalidProgramType))
    }

    /// Get max amount that can be input for a given mint direction
    fn get_max_amount_in<'a>(&self, _accounts: &[AccountInfo<'a>], _mint: Pubkey) -> Result<u64> {
        Err(error!(crate::programs::SolarBError::InvalidProgramType))
    }

    /// Get max amount that can be output for a given input mint direction
    fn get_max_amount_out<'a>(&self, _accounts: &[AccountInfo<'a>], _mint: Pubkey) -> Result<u64> {
        Err(error!(crate::programs::SolarBError::InvalidProgramType))
    }
}
