pub mod state;

use self::state::{AmmInfoFields, AMM_INFO_SIZE};
use crate::programs::ProgramMeta;
use crate::utils::token::{apply_transfer_fee, lookup_fee_rate};
use crate::utils::utils::parse_token_account;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use std::marker::PhantomData;

/// Ceiling division: (a + b - 1) / b
fn checked_ceil_div(a: u128, b: u128) -> Option<u128> {
    let quotient = a.checked_div(b)?;
    let remainder = a.checked_rem(b)?;
    if remainder != 0 {
        quotient.checked_add(1)
    } else {
        Some(quotient)
    }
}

pub struct RaydiumAmm<'info> {
    pub pool_id: Pubkey,
    pub base_token_pk: Pubkey,   // coin_vault_mint
    pub quote_token_pk: Pubkey,  // pc_vault_mint
    pub base_vault_amount: u64,  // effective coin reserve (vault - need_take_pnl)
    pub quote_vault_amount: u64, // effective pc reserve (vault - need_take_pnl)
    pub price: f64,
    pub inverse_price: f64,
    pub fee_rate: f64,
    pub swap_fee_numerator: u64,
    pub swap_fee_denominator: u64,
    pub static_base: usize,
    pub dyn_start: usize,
    pub buy_max_in: u64,
    pub buy_max_out: u64,
    pub sell_max_in: u64,
    pub sell_max_out: u64,
    pub base_fee_rate: f64,
    pub quote_fee_rate: f64,
    pub prepared: bool,
    _phantom: PhantomData<&'info ()>,
}

impl<'info> ProgramMeta for RaydiumAmm<'info> {
    fn get_id(&self) -> &Pubkey {
        &Self::PROGRAM_ID
    }

    fn get_pool_id(&self) -> &Pubkey {
        &self.pool_id
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (&self.base_token_pk, &self.quote_token_pk)
    }

    fn name(&self) -> &'static str { "RaydiumAmm" }

    fn get_fee_factor(&self) -> Result<(f64, f64)> { let f = 1.0 - self.fee_rate; Ok((f, f)) }

    fn get_prices(&self) -> Result<(f64, f64)> {
        Ok((self.price, self.inverse_price))
    }

    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        Ok((self.base_vault_amount, self.quote_vault_amount))
    }

    fn get_max_amount_in<'a>(&self, _accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_in) } else { Ok(self.sell_max_in) }
    }

    fn get_max_amount_out<'a>(&self, _accounts: &[AccountInfo<'a>], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_out) } else { Ok(self.sell_max_out) }
    }

    fn get_cached_max_amounts(&self, input_mint: Pubkey) -> (u64, u64) {
        if input_mint == self.base_token_pk { (self.buy_max_in, self.buy_max_out) } else { (self.sell_max_in, self.sell_max_out) }
    }

    fn fast_quote(&mut self, input_mint: Pubkey, amount_in: u64, _profit_pct: f64) -> Result<(u64, u64)> {
        let (max_in, max_out) = self.get_cached_max_amounts(input_mint);
        let amount_in = amount_in.min(max_in);

        let (reserve_in, reserve_out) = if input_mint == self.base_token_pk {
            (self.base_vault_amount as u128, self.quote_vault_amount as u128)
        } else {
            (self.quote_vault_amount as u128, self.base_vault_amount as u128)
        };

        // Ceiling division fee (matching Raydium processor)
        let swap_fee = checked_ceil_div(
            amount_in as u128 * self.swap_fee_numerator as u128,
            self.swap_fee_denominator as u128,
        )
        .unwrap_or(0);
        let in_after_fee = (amount_in as u128).saturating_sub(swap_fee);

        // Constant product: out = reserve_out * in_after_fee / (reserve_in + in_after_fee)
        let denominator = reserve_in.saturating_add(in_after_fee);
        if denominator == 0 { return Ok((amount_in, 0)); }
        let out = reserve_out.saturating_mul(in_after_fee) / denominator;
        let out = out.min(u64::MAX as u128) as u64;
        Ok((amount_in, out.min(max_out)))
    }

    fn swap_base_in<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        _clock: &Clock,
    ) -> Result<u64> {
        let coin_reserve = self.base_vault_amount as u128;
        let pc_reserve = self.quote_vault_amount as u128;

        let (fee_in, fee_out) = if input_mint == self.base_token_pk {
            (self.base_fee_rate, self.quote_fee_rate)
        } else {
            (self.quote_fee_rate, self.base_fee_rate)
        };

        // Apply transfer fee on input
        let transfer_fee = apply_transfer_fee(amount_in, fee_in);
        let actual_amount_in = amount_in.checked_sub(transfer_fee).unwrap();

        // Deduct swap fee (ceiling div, matching Raydium processor)
        let swap_fee = checked_ceil_div(
            actual_amount_in as u128 * self.swap_fee_numerator as u128,
            self.swap_fee_denominator as u128,
        )
        .ok_or(ProgramError::InvalidArgument)?;
        let amount_in_after_fee = (actual_amount_in as u128)
            .checked_sub(swap_fee)
            .ok_or(ProgramError::InvalidArgument)?;

        // Constant product formula
        let amount_out = if input_mint == self.base_token_pk {
            // Coin2PC: amount_out = pc * amount_in / (coin + amount_in)
            let denominator = coin_reserve.checked_add(amount_in_after_fee).unwrap();
            pc_reserve
                .checked_mul(amount_in_after_fee)
                .unwrap()
                .checked_div(denominator)
                .unwrap()
        } else {
            // PC2Coin: amount_out = coin * amount_in / (pc + amount_in)
            let denominator = pc_reserve.checked_add(amount_in_after_fee).unwrap();
            coin_reserve
                .checked_mul(amount_in_after_fee)
                .unwrap()
                .checked_div(denominator)
                .unwrap()
        };

        let amount_out_u64 =
            u64::try_from(amount_out).map_err(|_| ProgramError::InvalidArgument)?;

        // Apply transfer fee on output
        let transfer_fee_out = apply_transfer_fee(amount_out_u64, fee_out);
        let amount_out_after_fee = amount_out_u64
            .checked_sub(transfer_fee_out)
            .unwrap();

        Ok(amount_out_after_fee)
    }

    fn swap_base_out<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        output_mint: Pubkey,
        amount_out: u64,
        _clock: &Clock,
    ) -> Result<u64> {
        let coin_reserve = self.base_vault_amount as u128;
        let pc_reserve = self.quote_vault_amount as u128;

        let (fee_in, fee_out) = if output_mint == self.base_token_pk {
            (self.quote_fee_rate, self.base_fee_rate)
        } else {
            (self.base_fee_rate, self.quote_fee_rate)
        };

        // Add transfer fee to desired output to get amount needed from pool
        let transfer_fee_out = apply_transfer_fee(amount_out, fee_out);
        let amount_out_before_transfer_fee = (amount_out as u128)
            .checked_add(transfer_fee_out as u128)
            .ok_or(ProgramError::InvalidArgument)?;

        // Inverse CP formula: amount_in_after_fee = same_reserve * amount_out / (other_reserve - amount_out)
        let amount_in_after_fee = if output_mint == self.base_token_pk {
            // PC2Coin: input is PC, output is Coin
            let denominator = coin_reserve
                .checked_sub(amount_out_before_transfer_fee)
                .ok_or(ProgramError::InvalidArgument)?;
            checked_ceil_div(
                pc_reserve
                    .checked_mul(amount_out_before_transfer_fee)
                    .ok_or(ProgramError::InvalidArgument)?,
                denominator,
            )
            .ok_or(ProgramError::InvalidArgument)?
        } else {
            // Coin2PC: input is Coin, output is PC
            let denominator = pc_reserve
                .checked_sub(amount_out_before_transfer_fee)
                .ok_or(ProgramError::InvalidArgument)?;
            checked_ceil_div(
                coin_reserve
                    .checked_mul(amount_out_before_transfer_fee)
                    .ok_or(ProgramError::InvalidArgument)?,
                denominator,
            )
            .ok_or(ProgramError::InvalidArgument)?
        };

        // Add back swap fee: amount_in = amount_in_after_fee * denom / (denom - num)
        let fee_denom = self.swap_fee_denominator as u128;
        let fee_num = self.swap_fee_numerator as u128;
        let amount_in_before_fee = checked_ceil_div(
            amount_in_after_fee
                .checked_mul(fee_denom)
                .ok_or(ProgramError::InvalidArgument)?,
            fee_denom
                .checked_sub(fee_num)
                .ok_or(ProgramError::InvalidArgument)?,
        )
        .ok_or(ProgramError::InvalidArgument)?;

        let amount_in_u64 =
            u64::try_from(amount_in_before_fee).map_err(|_| ProgramError::InvalidArgument)?;

        // Add transfer fee on input
        let transfer_fee_in = apply_transfer_fee(amount_in_u64, fee_in);
        let total_amount_in = amount_in_u64
            .checked_add(transfer_fee_in)
            .ok_or(ProgramError::InvalidArgument)?;

        Ok(total_amount_in)
    }

    fn invoke_swap_base_in<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        min_amount_out: Option<u64>,
        payer: AccountInfo<'a>,
        user_mint_1_token_account: AccountInfo<'a>,
        user_mint_2_token_account: AccountInfo<'a>,
        mint_1_account: AccountInfo<'a>,
        _mint_2_account: AccountInfo<'a>,
        mint_1_token_program: AccountInfo<'a>,
        _mint_2_token_program: AccountInfo<'a>,
    ) -> Result<()> {
        let pool_id = &accounts[self.dyn_start + Self::D_POOL];
        let coin_vault = &accounts[self.dyn_start + Self::D_COIN_VAULT];
        let pc_vault = &accounts[self.dyn_start + Self::D_PC_VAULT];
        let authority = &accounts[self.static_base + Self::S_AMM_AUTHORITY];

        // Determine user source/destination based on input mint
        let (user_source, user_destination) = if input_mint == self.base_token_pk {
            // Input is coin: source = coin side, destination = pc side
            if *mint_1_account.key == self.base_token_pk {
                (user_mint_1_token_account, user_mint_2_token_account)
            } else {
                (user_mint_2_token_account, user_mint_1_token_account)
            }
        } else {
            // Input is pc: source = pc side, destination = coin side
            if *mint_1_account.key == self.quote_token_pk {
                (user_mint_1_token_account, user_mint_2_token_account)
            } else {
                (user_mint_2_token_account, user_mint_1_token_account)
            }
        };

        let min_out = min_amount_out.unwrap_or(0);

        // SwapBaseInV2 accounts: [spl_token, amm_pool, authority, coin_vault, pc_vault, user_source, user_destination, user_wallet]
        let metas = vec![
            AccountMeta::new_readonly(*mint_1_token_program.key, false), // spl_token
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new_readonly(*authority.key, false),
            AccountMeta::new(*coin_vault.key, false),
            AccountMeta::new(*pc_vault.key, false),
            AccountMeta::new(*user_source.key, false),
            AccountMeta::new(*user_destination.key, false),
            AccountMeta::new_readonly(*payer.key, true),
        ];

        // SwapBaseInV2: tag=16, amount_in, minimum_amount_out
        let mut data = [0u8; 17];
        data[0] = Self::SWAP_BASE_IN_DISC;
        data[1..9].copy_from_slice(&amount_in.to_le_bytes());
        data[9..17].copy_from_slice(&min_out.to_le_bytes());

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data: data.to_vec(),
        };

        let accounts_arr = [
            mint_1_token_program.clone(),
            pool_id.clone(),
            authority.clone(),
            coin_vault.clone(),
            pc_vault.clone(),
            unsafe { std::mem::transmute(user_source.to_account_info()) },
            unsafe { std::mem::transmute(user_destination.to_account_info()) },
            unsafe { std::mem::transmute(payer.to_account_info()) },
        ];

        unsafe {
            let accounts_slice: &[AccountInfo<'a>] = std::mem::transmute(accounts_arr.as_slice());
            invoke(&swap_ix, accounts_slice)?;
        }
        Ok(())
    }

    fn invoke_swap_base_out<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        max_amount_in: u64,
        amount_out: Option<u64>,
        payer: AccountInfo<'a>,
        user_mint_1_token_account: AccountInfo<'a>,
        user_mint_2_token_account: AccountInfo<'a>,
        mint_1_account: AccountInfo<'a>,
        _mint_2_account: AccountInfo<'a>,
        mint_1_token_program: AccountInfo<'a>,
        _mint_2_token_program: AccountInfo<'a>,
    ) -> Result<()> {
        let pool_id = &accounts[self.dyn_start + Self::D_POOL];
        let coin_vault = &accounts[self.dyn_start + Self::D_COIN_VAULT];
        let pc_vault = &accounts[self.dyn_start + Self::D_PC_VAULT];
        let authority = &accounts[self.static_base + Self::S_AMM_AUTHORITY];

        // Determine user source/destination based on input mint
        let (user_source, user_destination) = if input_mint == self.base_token_pk {
            if *mint_1_account.key == self.base_token_pk {
                (user_mint_1_token_account, user_mint_2_token_account)
            } else {
                (user_mint_2_token_account, user_mint_1_token_account)
            }
        } else {
            if *mint_1_account.key == self.quote_token_pk {
                (user_mint_1_token_account, user_mint_2_token_account)
            } else {
                (user_mint_2_token_account, user_mint_1_token_account)
            }
        };

        let amount_out_value = amount_out.unwrap_or(0);

        // SwapBaseOutV2 accounts: [spl_token, amm_pool, authority, coin_vault, pc_vault, user_source, user_destination, user_wallet]
        let metas = vec![
            AccountMeta::new_readonly(*mint_1_token_program.key, false), // spl_token
            AccountMeta::new(*pool_id.key, false),
            AccountMeta::new_readonly(*authority.key, false),
            AccountMeta::new(*coin_vault.key, false),
            AccountMeta::new(*pc_vault.key, false),
            AccountMeta::new(*user_source.key, false),
            AccountMeta::new(*user_destination.key, false),
            AccountMeta::new_readonly(*payer.key, true),
        ];

        // SwapBaseOutV2: tag=17, max_amount_in, amount_out
        let mut data = [0u8; 17];
        data[0] = Self::SWAP_BASE_OUT_DISC;
        data[1..9].copy_from_slice(&max_amount_in.to_le_bytes());
        data[9..17].copy_from_slice(&amount_out_value.to_le_bytes());

        let swap_ix = Instruction {
            program_id: Self::PROGRAM_ID,
            accounts: metas,
            data: data.to_vec(),
        };

        let accounts_arr = [
            mint_1_token_program.clone(),
            pool_id.clone(),
            authority.clone(),
            coin_vault.clone(),
            pc_vault.clone(),
            unsafe { std::mem::transmute(user_source.to_account_info()) },
            unsafe { std::mem::transmute(user_destination.to_account_info()) },
            unsafe { std::mem::transmute(payer.to_account_info()) },
        ];

        unsafe {
            let accounts_slice: &[AccountInfo<'a>] = std::mem::transmute(accounts_arr.as_slice());
            invoke(&swap_ix, accounts_slice)?;
        }
        Ok(())
    }

    #[cfg(any(test, feature = "debug"))]
    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        msg!("=== Raydium AMM ===");
        msg!("[S0] program_id: {}", accounts[self.static_base + Self::S_PROGRAM_ID].key);
        msg!("[S1] amm_authority: {}", accounts[self.static_base + Self::S_AMM_AUTHORITY].key);
        msg!("[D0] pool: {}", accounts[self.dyn_start + Self::D_POOL].key);
        msg!("[D1] coin_vault: {}", accounts[self.dyn_start + Self::D_COIN_VAULT].key);
        msg!("[D2] pc_vault: {}", accounts[self.dyn_start + Self::D_PC_VAULT].key);
        msg!("[D3] open_orders: {}", accounts[self.dyn_start + Self::D_OPEN_ORDERS].key);
        Ok(())
    }
}

impl<'info> RaydiumAmm<'info> {
    pub const PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
    const SWAP_BASE_IN_DISC: u8 = 16;
    const SWAP_BASE_OUT_DISC: u8 = 17;
    // ── Static account indices (from static_base, 2 accounts) ──
    pub const S_PROGRAM_ID: usize = 0;
    pub const S_AMM_AUTHORITY: usize = 1;

    // ── Dynamic account indices (from dyn_start, 4 accounts) ──
    pub const D_POOL: usize = 0;
    pub const D_COIN_VAULT: usize = 1;
    pub const D_PC_VAULT: usize = 2;
    pub const D_OPEN_ORDERS: usize = 3;

    pub const MIN_ACCOUNTS: usize = 4;

    // Serum/OpenBook OpenOrders account layout offsets
    // Layout: 5-byte head padding, u64 account_flags, Pubkey market, Pubkey owner,
    //         u64 native_coin_free, u64 native_coin_total, u64 native_pc_free, u64 native_pc_total
    const OO_NATIVE_COIN_TOTAL_OFFSET: usize = 85;
    const OO_NATIVE_PC_TOTAL_OFFSET: usize = 101;

    /// Parse native_coin_total and native_pc_total from a Serum/OpenBook OpenOrders account.
    /// Returns (0, 0) if the account is the system program (no open orders).
    fn parse_open_orders(open_orders: &AccountInfo) -> (u64, u64) {
        let data = match open_orders.try_borrow_data() {
            Ok(d) => d,
            Err(_) => return (0, 0),
        };
        // OpenOrders minimum size: 5 (head padding) + 8 (flags) + 32 (market) + 32 (owner)
        //   + 8 (coin_free) + 8 (coin_total) + 8 (pc_free) + 8 (pc_total) = 109
        if data.len() < 109 {
            return (0, 0);
        }
        let native_coin_total = u64::from_le_bytes(
            data[Self::OO_NATIVE_COIN_TOTAL_OFFSET..Self::OO_NATIVE_COIN_TOTAL_OFFSET + 8]
                .try_into()
                .unwrap(),
        );
        let native_pc_total = u64::from_le_bytes(
            data[Self::OO_NATIVE_PC_TOTAL_OFFSET..Self::OO_NATIVE_PC_TOTAL_OFFSET + 8]
                .try_into()
                .unwrap(),
        );
        (native_coin_total, native_pc_total)
    }

    pub fn new(
        accounts: &[AccountInfo<'info>],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
        mint_fees: &[(Pubkey, f64)],
    ) -> Result<Self> {
        require!(
            dyn_end - dyn_start >= Self::MIN_ACCOUNTS,
            crate::programs::SolarBError::InsufficientAccounts
        );
        require!(
            dyn_end <= accounts.len(),
            crate::programs::SolarBError::InsufficientAccounts
        );

        let pool_id = accounts[dyn_start + Self::D_POOL].clone();
        let coin_vault = accounts[dyn_start + Self::D_COIN_VAULT].clone();
        let pc_vault = accounts[dyn_start + Self::D_PC_VAULT].clone();

        // Parse AmmInfo from pool account data (raw 752 bytes, no discriminator)
        let pool_data = pool_id.try_borrow_data()?;
        if pool_data.len() < AMM_INFO_SIZE {
            return Err(ProgramError::InvalidAccountData.into());
        }
        let amm = AmmInfoFields::from_bytes(&pool_data);

        // Token mints come from the pool state data (no longer passed as accounts)
        let coin_vault_mint = amm.coin_vault_mint;
        let pc_vault_mint = amm.pc_vault_mint;

        drop(pool_data);

        // Read vault amounts from actual vault token accounts
        let coin_vault_amount = parse_token_account(&coin_vault)?.amount;
        let pc_vault_amount = parse_token_account(&pc_vault)?.amount;

        // Read open orders amounts from Serum/OpenBook (if account is provided)
        let (oo_native_coin, oo_native_pc) =
            if dyn_start + Self::D_OPEN_ORDERS < dyn_end {
                let open_orders = &accounts[dyn_start + Self::D_OPEN_ORDERS];
                Self::parse_open_orders(open_orders)
            } else {
                (0, 0)
            };

        // Compute effective reserves:
        // total = vault_amount + open_orders_amount - need_take_pnl
        let base_vault_amount = coin_vault_amount
            .saturating_add(oo_native_coin)
            .saturating_sub(amm.need_take_pnl_coin);
        let quote_vault_amount = pc_vault_amount
            .saturating_add(oo_native_pc)
            .saturating_sub(amm.need_take_pnl_pc);

        #[cfg(test)]
        let base_vault_amount = (base_vault_amount as f64 * 1.05) as u64;

        // Compute price with decimal adjustment
        let raw_price = quote_vault_amount as f64 / base_vault_amount as f64;
        let price = raw_price;
        let inverse_price = 1.0 / price;

        let fee_rate = amm.swap_fee_numerator as f64 / amm.swap_fee_denominator as f64;

        // Defer max amounts and transfer fees to prepare_for_execution()
        let instance = RaydiumAmm {
            pool_id: *pool_id.key,
            base_token_pk: coin_vault_mint,
            quote_token_pk: pc_vault_mint,
            base_vault_amount,
            quote_vault_amount,
            price,
            inverse_price,
            fee_rate,
            swap_fee_numerator: amm.swap_fee_numerator,
            swap_fee_denominator: amm.swap_fee_denominator,
            static_base,
            dyn_start,
            buy_max_in: 0,
            buy_max_out: 0,
            sell_max_in: 0,
            sell_max_out: 0,
            base_fee_rate: 0.0,
            quote_fee_rate: 0.0,
            prepared: false,
            _phantom: PhantomData,
        };
        // instance.log_accounts(accounts)?;
        Ok(instance)
    }

    fn compute_cached_max(base_vault: u64, quote_vault: u64, fee_rate: f64) -> (u64, u64, u64, u64) {
        fn cp_max(x: f64, y: f64, fee_factor: f64) -> (u64, u64) {
            let target = y * 0.99;
            if target <= 0.0 || y <= target || fee_factor <= 0.0 {
                return (0, y as u64);
            }
            let denom = y - target;
            let dx = (x / fee_factor) * ((y / denom) - 1.0);
            (dx.max(0.0).min(u64::MAX as f64) as u64, y as u64)
        }
        let ff = 1.0 - fee_rate;
        let (buy_in, buy_out) = cp_max(base_vault as f64, quote_vault as f64, ff);
        let (sell_in, sell_out) = cp_max(quote_vault as f64, base_vault as f64, ff);
        (buy_in, buy_out, sell_in, sell_out)
    }

    /// Compute deferred fields: max amounts, transfer fee rates.
    /// Called only for instances that participate in a profitable arb path.
    pub fn prepare_for_execution(
        &mut self,
        _accounts: &[AccountInfo<'info>],
        mint_fees: &[(Pubkey, f64)],
    ) {
        if self.prepared {
            return;
        }
        self.prepared = true;

        self.base_fee_rate = lookup_fee_rate(mint_fees, &self.base_token_pk);
        self.quote_fee_rate = lookup_fee_rate(mint_fees, &self.quote_token_pk);

        let (buy_in, buy_out, sell_in, sell_out) =
            Self::compute_cached_max(self.base_vault_amount, self.quote_vault_amount, self.fee_rate);
        self.buy_max_in = buy_in;
        self.buy_max_out = buy_out;
        self.sell_max_in = sell_in;
        self.sell_max_out = sell_out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::prelude::Clock;
    use anchor_lang::solana_program::{account_info::AccountInfo, pubkey::Pubkey, system_program};
    use solana_client::nonblocking::rpc_client::RpcClient;
    use solana_sdk::pubkey::Pubkey as SdkPubkey;

    fn create_mock_account_info_with_data(
        key: Pubkey,
        owner: Pubkey,
        data: Option<Vec<u8>>,
    ) -> AccountInfo<'static> {
        let data_vec = data.unwrap_or_else(|| vec![0u8; 8]);
        let data_vec = Box::leak(Box::new(data_vec));
        let lamports = Box::leak(Box::new(0u64));
        let owner_static = Box::leak(Box::new(owner));
        let key_static = Box::leak(Box::new(key));

        AccountInfo::new(
            key_static,
            false,
            true,
            lamports,
            data_vec,
            owner_static,
            false,
            0,
        )
    }

    fn account_to_account_info(
        key: Pubkey,
        account: solana_sdk::account::Account,
    ) -> AccountInfo<'static> {
        let data = Box::leak(Box::new(account.data));
        let lamports = Box::leak(Box::new(account.lamports));
        let owner_bytes: [u8; 32] = account.owner.to_bytes();
        let owner = Pubkey::try_from(owner_bytes.as_ref()).unwrap();
        let owner_static = Box::leak(Box::new(owner));
        let key_static = Box::leak(Box::new(key));
        AccountInfo::new(
            key_static,
            false,
            false,
            lamports,
            data,
            owner_static,
            account.executable,
            account.rent_epoch,
        )
    }

    async fn get_clock(rpc_client: &RpcClient) -> anyhow::Result<Clock> {
        use anchor_client::solana_sdk::sysvar;
        let clock_account = rpc_client.get_account(&sysvar::clock::ID).await?;
        let data = &clock_account.data;
        let slot = u64::from_le_bytes(data[0..8].try_into()?);
        let epoch_start_timestamp = i64::from_le_bytes(data[8..16].try_into()?);
        let epoch = u64::from_le_bytes(data[16..24].try_into()?);
        let leader_schedule_epoch = u64::from_le_bytes(data[24..32].try_into()?);
        let unix_timestamp = i64::from_le_bytes(data[32..40].try_into()?);
        Ok(Clock {
            slot,
            epoch_start_timestamp,
            epoch,
            leader_schedule_epoch,
            unix_timestamp,
        })
    }

    fn read_pubkey(data: &[u8], offset: usize) -> Pubkey {
        Pubkey::try_from(&data[offset..offset + 32]).unwrap()
    }

    #[tokio::test]
    async fn test_raydium_amm_round_trip_swap() {
        use anchor_client::Cluster;

        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        // SOL/USDC Raydium AMM pool on mainnet
        let pool_id_key = Pubkey::from_str_const("FaDoeere161VKUFqcrQEM8it6kSCHKrLyq7wWyPvBkPq");
        
        // Fetch pool account (AmmInfo - 752 bytes, no Anchor discriminator)
        let pool_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool_id_key.to_bytes().as_ref()).unwrap())
            .await;

        let pool_account = match pool_account {
            Ok(acc) => acc,
            Err(e) => {
                eprintln!("Warning: Could not fetch pool account: {:?}", e);
                return;
            }
        };

        if pool_account.data.len() < state::AMM_INFO_SIZE {
            eprintln!(
                "Pool account data too short: {} bytes, expected at least {}",
                pool_account.data.len(),
                state::AMM_INFO_SIZE
            );
            return;
        }

        let amm = state::AmmInfoFields::from_bytes(&pool_account.data);

        // Read coin_vault and pc_vault pubkeys from AmmInfo
        // coin_vault is at offset 336, pc_vault at 368
        let coin_vault_key = read_pubkey(&pool_account.data, 336);
        let pc_vault_key = read_pubkey(&pool_account.data, 368);

        eprintln!("coin_vault: {}", coin_vault_key);
        eprintln!("pc_vault: {}", pc_vault_key);
        eprintln!("coin_vault_mint: {}", amm.coin_vault_mint);
        eprintln!("pc_vault_mint: {}", amm.pc_vault_mint);
        eprintln!(
            "fee: {}/{}",
            amm.swap_fee_numerator, amm.swap_fee_denominator
        );

        // Fetch vault accounts
        let coin_vault_account = rpc_client
            .get_account(&SdkPubkey::try_from(coin_vault_key.to_bytes().as_ref()).unwrap())
            .await;
        let pc_vault_account = rpc_client
            .get_account(&SdkPubkey::try_from(pc_vault_key.to_bytes().as_ref()).unwrap())
            .await;

        if coin_vault_account.is_err() || pc_vault_account.is_err() {
            eprintln!("Warning: Could not fetch vault accounts. Pool may be closed.");
            return;
        }

        let coin_vault_account = coin_vault_account.unwrap();
        let pc_vault_account = pc_vault_account.unwrap();

        // Derive authority PDA: create_program_address(&[b"amm authority", &[nonce]], program_id)
        let authority_key = Pubkey::create_program_address(
            &[b"amm authority", &[amm.nonce]],
            &RaydiumAmm::PROGRAM_ID,
        )
        .expect("Failed to derive authority PDA");

        eprintln!("authority: {}", authority_key);
        eprintln!("open_orders: {}", amm.open_orders);

        // Fetch open orders account from Serum/OpenBook
        let open_orders_account = rpc_client
            .get_account(&SdkPubkey::try_from(amm.open_orders.to_bytes().as_ref()).unwrap())
            .await;
        let open_orders_account = match open_orders_account {
            Ok(acc) => acc,
            Err(e) => {
                eprintln!("Warning: Could not fetch open_orders account: {:?}", e);
                return;
            }
        };

        // Convert to AccountInfo
        let pool_id_account_info = account_to_account_info(pool_id_key, pool_account);
        let coin_vault_info = account_to_account_info(coin_vault_key, coin_vault_account);
        let pc_vault_info = account_to_account_info(pc_vault_key, pc_vault_account);
        let open_orders_info = account_to_account_info(amm.open_orders, open_orders_account);

        let program_id_account =
            create_mock_account_info_with_data(RaydiumAmm::PROGRAM_ID, system_program::id(), None);
        let authority_account =
            create_mock_account_info_with_data(authority_key, system_program::id(), None);

        // Accounts order:
        //   [0] program_id (static S0)
        //   [1] authority  (static S1)
        //   [2] pool_id    (dynamic D0)
        //   [3] coin_vault (dynamic D1)
        //   [4] pc_vault   (dynamic D2)
        //   [5] open_orders(dynamic D3)
        let accounts = vec![
            program_id_account,  // static_base = 0
            authority_account,   // static_base + 1
            pool_id_account_info, // dyn_start = 2
            coin_vault_info,
            pc_vault_info,
            open_orders_info,
        ];

        let static_base = 0;
        let dyn_start = 2;
        let dyn_end = accounts.len();
        let mut raydium_amm =
            RaydiumAmm::new(&accounts, static_base, dyn_start, dyn_end, &[]).expect("Failed to create RaydiumAmm");

        eprintln!(
            "Price: {:?}, Inverse Price: {:?}",
            raydium_amm.price, raydium_amm.inverse_price
        );
        eprintln!(
            "Base vault: {}, Quote vault: {}",
            raydium_amm.base_vault_amount, raydium_amm.quote_vault_amount
        );
        eprintln!(
            "Fee rate: {} ({}/{})",
            raydium_amm.fee_rate,
            raydium_amm.swap_fee_numerator,
            raydium_amm.swap_fee_denominator
        );

        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");

        let token_mint = if raydium_amm.base_token_pk == sol_mint {
            raydium_amm.quote_token_pk
        } else if raydium_amm.quote_token_pk == sol_mint {
            raydium_amm.base_token_pk
        } else {
            eprintln!("Warning: Pool does not contain SOL, skipping round trip test");
            return;
        };

        // Test round trip: SOL -> TOKEN -> SOL
        let sol_in = 1_000_000_000; // 1 SOL

        eprintln!("================================================");
        // Step 1: Swap SOL -> TOKEN
        let clock1 = get_clock(&rpc_client).await.unwrap();
        let token_out = raydium_amm
            .swap_base_in(&accounts, sol_mint, sol_in, &clock1)
            .expect("swap_base_in failed");
        eprintln!(
            "Step 1 (swap_base_in): {} SOL -> {} TOKEN",
            sol_in as f64 / 1_000_000_000.0,
            token_out as f64 / 1_000_000.0,
        );

        let max_sol_in = raydium_amm
            .swap_base_out(&accounts, token_mint, token_out, &clock1)
            .expect("swap_base_out failed");
        eprintln!(
            "Step 1 (swap_base_out): MAX SOL IN {} -> {} TOKEN OUT",
            max_sol_in as f64 / 1_000_000_000.0,
            token_out as f64 / 1_000_000.0,
        );

        eprintln!("================================================");

        // Step 2: Swap TOKEN -> SOL
        let sol_out = raydium_amm
            .swap_base_in(&accounts, token_mint, token_out, &clock1)
            .expect("second swap_base_in failed");
        eprintln!(
            "Step 2 (swap_base_in): {} TOKEN -> {} SOL",
            token_out as f64 / 1_000_000.0,
            sol_out as f64 / 1_000_000_000.0,
        );

        let max_token_in = raydium_amm
            .swap_base_out(&accounts, sol_mint, sol_out, &clock1)
            .expect("second swap_base_out failed");
        eprintln!(
            "Step 2 (swap_base_out): {} MAX TOKEN IN -> {} SOL OUT",
            max_token_in as f64 / 1_000_000.0,
            sol_out as f64 / 1_000_000_000.0
        );

        eprintln!("================================================");
        let loss = sol_in as i128 - sol_out as i128;
        let loss_pct = loss as f64 / sol_in as f64 * 100.0;
        eprintln!(
            "Round trip: {} SOL in -> {} SOL out (loss: {} = {:.4}%)",
            sol_in as f64 / 1_000_000_000.0,
            sol_out as f64 / 1_000_000_000.0,
            loss as f64 / 1_000_000_000.0,
            loss_pct,
        );
        eprintln!("Round trip completed!");
    }
}