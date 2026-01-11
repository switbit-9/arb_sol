use anchor_lang::prelude::*;
use anchor_lang::solana_program::{account_info::next_account_info, pubkey::Pubkey};
use anchor_spl::token_interface::TokenAccount;

use crate::programs::ProgramMeta;

pub struct MeteoraDammV1<'info> {
    pub pool_id: AccountInfo<'info>,
    pub base_vault: AccountInfo<'info>,
    pub quote_vault: AccountInfo<'info>,
    pub base_token: AccountInfo<'info>,
    pub quote_token: AccountInfo<'info>,
    pub oracle: AccountInfo<'info>,
    pub host_fee_in: AccountInfo<'info>,
    pub bitmap_extension: AccountInfo<'info>,
    pub memo: AccountInfo<'info>,
    pub event_authority: AccountInfo<'info>,
}

impl<'info> ProgramMeta for MeteoraDammV1<'info> {
    fn get_id(&self) -> &Pubkey {
        &Self::PROGRAM_ID
    }

    fn get_vaults(&self) -> (&AccountInfo<'_>, &AccountInfo<'_>) {
        unsafe {
            (
                &*(&self.base_vault as *const AccountInfo<'info> as *const AccountInfo<'_>),
                &*(&self.quote_vault as *const AccountInfo<'info> as *const AccountInfo<'_>),
            )
        }
    }

    fn swap_base_in(&self, input_mint: Pubkey, amount_in: u64, clock: Clock) -> Result<u64> {
        self.swap_base_in_impl(input_mint, amount_in, clock)
    }

    fn swap_base_out(&self, input_mint: Pubkey, amount_in: u64, clock: Clock) -> Result<u64> {
        self.swap_base_out_impl(input_mint, amount_in, clock)
    }

    fn invoke_swap_base_in<'a>(
        &self,
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
    ) -> Result<()> {
        self.invoke_swap_base_in_impl(
            input_mint,
            max_amount_in,
            amount_out,
            payer,
            user_mint_1_token_account,
            user_mint_2_token_account,
            mint_1_account,
            mint_2_account,
            mint_1_token_program,
            mint_2_token_program,
        )
    }

    fn invoke_swap_base_out<'a>(
        &self,
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
    ) -> Result<()> {
        self.invoke_swap_base_out_impl(
            input_mint,
            amount_in,
            min_amount_out,
            payer,
            user_mint_1_token_account,
            user_mint_2_token_account,
            mint_1_account,
            mint_2_account,
            mint_1_token_program,
            mint_2_token_program,
        )
    }

    fn log_accounts(&self) -> Result<()> {
        msg!(
            "Meteora DAMM v1 accounts: pool={}, base_vault={}, quote_vault={}, base_token={}, quote_token={}, oracle={}, host_fee_in={}, bitmap_extension={}, memo={}, event_authority={}",
            self.pool_id.key,
            self.base_vault.key,
            self.quote_vault.key,
            self.base_token.key,
            self.quote_token.key,
            self.oracle.key,
            self.host_fee_in.key,
            self.bitmap_extension.key,
            self.memo.key,
            self.event_authority.key,
        );
        Ok(())
    }
}

impl<'info> MeteoraDammV1<'info> {
    pub const PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("dbcij3LWUppWqq96dh6gJWwBifmcGfLSB5D4DuSMaqN");
    pub fn new(accounts: &[AccountInfo<'info>]) -> Result<Self> {
        let mut iter = accounts.iter();
        let pool_id = next_account_info(&mut iter)?;
        let base_vault = next_account_info(&mut iter)?;
        let quote_vault = next_account_info(&mut iter)?;
        let base_token = next_account_info(&mut iter)?;
        let quote_token = next_account_info(&mut iter)?;
        let oracle = next_account_info(&mut iter)?;
        let host_fee_in = next_account_info(&mut iter)?;
        let bitmap_extension = next_account_info(&mut iter)?;
        let memo = next_account_info(&mut iter)?;
        let event_authority = next_account_info(&mut iter)?;

        Ok(MeteoraDammV1 {
            pool_id: pool_id.clone(),
            base_vault: base_vault.clone(),
            quote_vault: quote_vault.clone(),
            base_token: base_token.clone(),
            quote_token: quote_token.clone(),
            oracle: oracle.clone(),
            host_fee_in: host_fee_in.clone(),
            bitmap_extension: bitmap_extension.clone(),
            memo: memo.clone(),
            event_authority: event_authority.clone(),
        })
    }

    pub fn swap_base_in_impl(
        &self,
        _input_mint: Pubkey,
        _amount_in: u64,
        _clock: Clock,
    ) -> Result<u64> {
        Ok(0)
    }

    pub fn swap_base_out_impl(
        &self,
        _input_mint: Pubkey,
        _amount_in: u64,
        _clock: Clock,
    ) -> Result<u64> {
        Ok(0)
    }

    pub fn invoke_swap_base_in_impl<'a>(
        &self,
        _input_mint: Pubkey,
        _max_amount_in: u64,
        _amount_out: Option<u64>,
        _payer: AccountInfo<'a>,
        _user_mint_1_token_account: AccountInfo<'a>,
        _user_mint_2_token_account: AccountInfo<'a>,
        _mint_1_account: AccountInfo<'a>,
        _mint_2_account: AccountInfo<'a>,
        _mint_1_token_program: AccountInfo<'a>,
        _mint_2_token_program: AccountInfo<'a>,
    ) -> Result<()> {
        Ok(())
    }

    pub fn invoke_swap_base_out_impl<'a>(
        &self,
        _input_mint: Pubkey,
        _amount_in: u64,
        _min_amount_out: Option<u64>,
        _payer: AccountInfo<'a>,
        _user_mint_1_token_account: AccountInfo<'a>,
        _user_mint_2_token_account: AccountInfo<'a>,
        _mint_1_account: AccountInfo<'a>,
        _mint_2_account: AccountInfo<'a>,
        _mint_1_token_program: AccountInfo<'a>,
        _mint_2_token_program: AccountInfo<'a>,
    ) -> Result<()> {
        Ok(())
    }
}
