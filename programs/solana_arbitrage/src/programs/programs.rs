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

    /// Get base and quote token mints
    fn get_mints(&self) -> (&Pubkey, &Pubkey);

    /// Get base token pubkey
    fn get_base_token(&self) -> Pubkey {
        *self.get_mints().0
    }

    /// Calculate output amount for swap base in (base -> quote)
    fn swap_base_in<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        clock: &Clock,
    ) -> Result<u64>;

    /// Calculate input amount needed to receive a specific output amount
    /// Given output_mint and amount_out, returns the required amount_in
    fn swap_base_out<'a>(
        &self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        clock: &Clock,
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

    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        Err(error!(crate::programs::SolarBError::InvalidProgramType))
    }

    /// Get directional fee factors: (fee_factor_a_to_b, fee_factor_b_to_a)
    /// Each is (1 - fee_rate) for that direction, e.g. 0.9975 for 0.25% fee.
    /// For AMMs with symmetric fees, both values are identical.
    fn get_fee_factor(&self) -> Result<(f64, f64)> {
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

    /// Max input the active bin can absorb before crossing to the next bin (DLMM only).
    /// Returns the amount BEFORE fees are added.
    fn get_active_bin_max_in(&self, _input_mint: Pubkey) -> Result<u64> {
        Err(error!(crate::programs::SolarBError::InvalidProgramType))
    }

    /// Get cached max amounts for a given input direction. Returns (max_in, max_out).
    /// Uses pre-computed values from initialization — no accounts needed.
    fn get_cached_max_amounts(&self, _input_mint: Pubkey) -> (u64, u64) {
        (u64::MAX, u64::MAX)
    }

    /// Whether the pool has output liquidity for this input direction.
    /// Cached from initialization — zero cost to call.
    /// Default true for AMMs (vault amounts always imply liquidity).
    fn has_output_liquidity(&self, _input_mint: Pubkey) -> bool {
        true
    }
}
