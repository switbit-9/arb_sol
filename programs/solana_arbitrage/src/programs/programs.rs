use anchor_lang::prelude::*;
use anchor_lang::solana_program::pubkey::Pubkey;
use anchor_spl::token_interface::TokenAccount;

pub trait ProgramMeta {
    fn get_id(&self) -> &Pubkey;

    /// Get base and quote vault/pool AccountInfo references
    /// Returns (base_vault, quote_vault)
    /// Each implementation should return references matching the struct's lifetime
    fn get_vaults(&self) -> (&AccountInfo<'_>, &AccountInfo<'_>);

    /// Parse vaults and return (base_amount, quote_amount) as u128
    fn parse_vaults(&self) -> Result<(TokenAccount, TokenAccount)> {
        let (base_vault, quote_vault) = self.get_vaults();

        let mut base_data = &base_vault.try_borrow_data()?[..];
        let base_token_account = TokenAccount::try_deserialize(&mut base_data)?;
        // let base_amount = base_token_account.amount as u128;

        let mut quote_data = &quote_vault.try_borrow_data()?[..];
        let quote_token_account = TokenAccount::try_deserialize(&mut quote_data)?;
        // let quote_amount = quote_token_account.amount as u128;

        Ok((base_token_account, quote_token_account))
    }

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
    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        panic!("get_mints not implemented for this program");
    }

    /// Calculate output amount for swap base in (base -> quote)
    fn swap_base_in(&self, amount_in: u64, clock: Clock) -> Result<u64>;

    /// Calculate input amount for swap base out (quote -> base)
    fn swap_base_out(&self, amount_in: u64, clock: Clock) -> Result<u64>;

    /// Invoke swap base in (base -> quote)
    fn invoke_swap_base_in<'a>(
        &self,
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
    fn log_accounts(&self) -> Result<()>;
}
