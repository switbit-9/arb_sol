use crate::compat::*;
use crate::utils::token::get_transfer_fee_config;
use spl_token_2022::extension::transfer_fee::TransferFeeConfig;

pub struct MintInfo<'info> {
    pub mint: AccountInfo<'info>,
    pub token_program: AccountInfo<'info>,
    pub transfer_fee: Option<TransferFeeConfig>,
    pub user_token_account: AccountInfo<'info>,
}

impl<'info> MintInfo<'info> {
    pub fn new(
        mint: AccountInfo<'info>,
        token_program: AccountInfo<'info>,
        transfer_fee: Option<TransferFeeConfig>,
        user_token_account: AccountInfo<'info>,
    ) -> Self {
        Self {
            mint,
            token_program,
            transfer_fee,
            user_token_account,
        }
    }

    pub fn get_transfer_fee(&mut self) -> Result<Option<TransferFeeConfig>> {
        self.transfer_fee = get_transfer_fee_config(&self.mint)?;
        Ok(self.transfer_fee)
    }

    pub fn amount_after_fee(&self, epoch: u64, amount_in: u64) -> u64 {
        if let Some(ref fee_config) = self.transfer_fee {
            fee_config
                .calculate_epoch_fee(epoch, amount_in)
                .unwrap_or(amount_in)
        } else {
            amount_in
        }
    }

    pub fn amount_before_fee(&self, epoch: u64, amount_in: u64) -> u64 {
        if let Some(ref fee_config) = self.transfer_fee {
            fee_config
                .calculate_inverse_epoch_fee(epoch, amount_in)
                .unwrap_or(amount_in)
        } else {
            amount_in
        }
    }
}

#[derive(Clone)]
pub struct BotConfig {
    pub start_token: Option<Pubkey>,
    pub max_amount_in: u64,
    pub min_profit: i128,
    pub mints: u8,
    pub mode: u8,
    pub clock: Clock,
    pub test: bool,
}

impl BotConfig {
    pub fn new(
        start_token: Option<Pubkey>,
        max_amount_in: u64,
        min_profit: i128,
        mints: u8,
        mode: u8,
        clock: Clock,
        test: bool,
    ) -> Self {
        Self {
            start_token,
            max_amount_in,
            min_profit,
            mints,
            mode,
            clock,
            test,
        }
    }
}
