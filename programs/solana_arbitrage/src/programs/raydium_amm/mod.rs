pub mod state;

use crate::programs::{PoolKind, ProgramMeta};
use crate::programs::programs::Result;
use crate::programs::SolarBError;
use crate::utils::cpi::invoke_cpi;
use crate::utils::token::{apply_transfer_fee, MintFee};
use crate::utils::utils::read_vault_data;
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};
use pinocchio::instruction::AccountMeta;
use pinocchio::sysvars::clock::Clock;

pub const PROGRAM_ID: Pubkey =
    five8_const::decode_32_const("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
const SWAP_BASE_IN_DISC: u8 = 16;
const SWAP_BASE_OUT_DISC: u8 = 17;
// -- Static account indices (from static_base, 2 accounts) --
pub const S_PROGRAM_ID: usize = 0;
pub const S_AMM_AUTHORITY: usize = 1;

// -- Dynamic account indices (from dyn_start, 4 accounts) --
pub const D_POOL: usize = 0;
pub const D_COIN_VAULT: usize = 1;
pub const D_PC_VAULT: usize = 2;
pub const D_OPEN_ORDERS: usize = 3;

pub const DYNAMIC_ACCOUNTS: usize = 4;

// Serum/OpenBook OpenOrders account layout offsets
// Layout: 5-byte head padding, u64 account_flags, Pubkey market, Pubkey owner,
//         u64 native_coin_free, u64 native_coin_total, u64 native_pc_free, u64 native_pc_total
const OO_NATIVE_COIN_TOTAL_OFFSET: usize = 85;
const OO_NATIVE_PC_TOTAL_OFFSET: usize = 101;

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

#[derive(Clone)]
pub struct RaydiumAmm {
    pub pool_id: Pubkey,
    pub base_token_pk: Pubkey,   // coin_vault_mint
    pub quote_token_pk: Pubkey,  // pc_vault_mint
    pub base_vault_amount: u64,  // effective coin reserve (vault - need_take_pnl)
    pub quote_vault_amount: u64, // effective pc reserve (vault - need_take_pnl)
    pub price: f64,
    pub fee_rate: f64,
    /// Pre-computed fee factor: 1 - fee_rate
    pub fee_factor: (f64, f64),
    pub static_base: usize,
    pub dyn_start: usize,
    pub buy_max_in: u64,
    pub buy_max_out: u64,
    pub sell_max_in: u64,
    pub sell_max_out: u64,
    pub prepared: bool,
}

impl ProgramMeta for RaydiumAmm {
    fn get_id(&self) -> &Pubkey {
        &PROGRAM_ID
    }

    fn get_pool_id(&self) -> &Pubkey {
        &self.pool_id
    }

    fn get_mints(&self) -> (&Pubkey, &Pubkey) {
        (&self.base_token_pk, &self.quote_token_pk)
    }

    fn name(&self) -> &'static str { "RaydiumAmm" }
    fn pool_kind(&self) -> PoolKind { PoolKind::RaydiumAmm }

    fn get_fee_factor(&self) -> Result<(f64, f64)> { Ok(self.fee_factor) }

    fn get_prices(&self) -> Result<(f64, f64)> {
        let inverse = if self.price > 0.0 { 1.0 / self.price } else { 0.0 };
        Ok((self.price, inverse))
    }

    fn get_vault_amounts(&self) -> Result<(u64, u64)> {
        Ok((self.base_vault_amount, self.quote_vault_amount))
    }

    fn get_max_amount_in(&self, _accounts: &[AccountInfo], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_in) } else { Ok(self.sell_max_in) }
    }

    fn get_max_amount_out(&self, _accounts: &[AccountInfo], mint: Pubkey) -> Result<u64> {
        if mint == self.base_token_pk { Ok(self.buy_max_out) } else { Ok(self.sell_max_out) }
    }

    fn get_cached_max_amounts(&self, input_mint: Pubkey) -> (u64, u64) {
        if input_mint == self.base_token_pk { (self.buy_max_in, self.buy_max_out) } else { (self.sell_max_in, self.sell_max_out) }
    }

    fn fast_quote(&mut self, _accounts: &[AccountInfo], input_mint: Pubkey, amount_in: u64, _profit_pct: f64) -> Result<(u64, u64)> {
        let (max_in, max_out) = self.get_cached_max_amounts(input_mint);
        let amount_in = amount_in.min(max_in);

        let (reserve_in, reserve_out) = if input_mint == self.base_token_pk {
            (self.base_vault_amount as u128, self.quote_vault_amount as u128)
        } else {
            (self.quote_vault_amount as u128, self.base_vault_amount as u128)
        };

        let swap_fee = (amount_in as f64 * self.fee_rate).ceil() as u128;
        let in_after_fee = (amount_in as u128).saturating_sub(swap_fee);

        // Constant product: out = reserve_out * in_after_fee / (reserve_in + in_after_fee)
        let denominator = reserve_in.saturating_add(in_after_fee);
        if denominator == 0 { return Ok((amount_in, 0)); }
        let out = reserve_out.saturating_mul(in_after_fee) / denominator;
        let out = out.min(u64::MAX as u128) as u64;
        Ok((amount_in, out.min(max_out)))
    }

    fn swap_base_in(
        &mut self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        amount_in: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        _clock: &Clock,
    ) -> Result<u64> {
        let coin_reserve = self.base_vault_amount as u128;
        let pc_reserve = self.quote_vault_amount as u128;

        let transfer_fee = apply_transfer_fee(amount_in, input_transfer_fee);
        let actual_amount_in = amount_in.checked_sub(transfer_fee).unwrap();

        let swap_fee = (actual_amount_in as f64 * self.fee_rate).ceil() as u128;
        let amount_in_after_fee = (actual_amount_in as u128)
            .checked_sub(swap_fee)
            .ok_or(ProgramError::InvalidArgument)?;

        // Constant product formula
        let amount_out = if input_mint == self.base_token_pk {
            let denominator = coin_reserve.checked_add(amount_in_after_fee).unwrap();
            pc_reserve
                .checked_mul(amount_in_after_fee)
                .unwrap()
                .checked_div(denominator)
                .unwrap()
        } else {
            let denominator = pc_reserve.checked_add(amount_in_after_fee).unwrap();
            coin_reserve
                .checked_mul(amount_in_after_fee)
                .unwrap()
                .checked_div(denominator)
                .unwrap()
        };

        let amount_out_u64 =
            u64::try_from(amount_out).map_err(|_| ProgramError::InvalidArgument)?;

        let transfer_fee_out = apply_transfer_fee(amount_out_u64, output_transfer_fee);
        let amount_out_after_fee = amount_out_u64
            .checked_sub(transfer_fee_out)
            .unwrap();

        Ok(amount_out_after_fee)
    }

    fn swap_base_out(
        &mut self,
        accounts: &[AccountInfo],
        output_mint: Pubkey,
        amount_out: u64,
        input_transfer_fee: MintFee,
        output_transfer_fee: MintFee,
        _clock: &Clock,
    ) -> Result<u64> {
        let coin_reserve = self.base_vault_amount as u128;
        let pc_reserve = self.quote_vault_amount as u128;

        let transfer_fee_out = apply_transfer_fee(amount_out, output_transfer_fee);
        let amount_out_before_transfer_fee = (amount_out as u128)
            .checked_add(transfer_fee_out as u128)
            .ok_or(ProgramError::InvalidArgument)?;

        // Inverse CP formula
        let amount_in_after_fee = if output_mint == self.base_token_pk {
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

        // Add back swap fee: amount_in = amount_in_after_fee / (1 - fee_rate)
        let fee_factor = self.fee_factor.0;
        let amount_in_before_fee = (amount_in_after_fee as f64 / fee_factor).ceil() as u128;

        let amount_in_u64 =
            u64::try_from(amount_in_before_fee).map_err(|_| ProgramError::InvalidArgument)?;

        let transfer_fee_in = apply_transfer_fee(amount_in_u64, input_transfer_fee);
        let total_amount_in = amount_in_u64
            .checked_add(transfer_fee_in)
            .ok_or(ProgramError::InvalidArgument)?;

        Ok(total_amount_in)
    }

    fn invoke_swap_base_in(
        &mut self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        amount_in: u64,
        min_amount_out: Option<u64>,
        payer: &AccountInfo,
        user_mint_1_token_account: &AccountInfo,
        user_mint_2_token_account: &AccountInfo,
        mint_1_account: &AccountInfo,
        _mint_2_account: &AccountInfo,
        mint_1_token_program: &AccountInfo,
        _mint_2_token_program: &AccountInfo,
    ) -> Result<()> {
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let coin_vault = &accounts[self.dyn_start + D_COIN_VAULT];
        let pc_vault = &accounts[self.dyn_start + D_PC_VAULT];
        let authority = &accounts[self.static_base + S_AMM_AUTHORITY];

        // Determine user source/destination based on input mint
        let (user_source, user_destination) = if input_mint == self.base_token_pk {
            // Input is coin: source = coin side, destination = pc side
            if *mint_1_account.key() == self.base_token_pk {
                (user_mint_1_token_account, user_mint_2_token_account)
            } else {
                (user_mint_2_token_account, user_mint_1_token_account)
            }
        } else {
            // Input is pc: source = pc side, destination = coin side
            if *mint_1_account.key() == self.quote_token_pk {
                (user_mint_1_token_account, user_mint_2_token_account)
            } else {
                (user_mint_2_token_account, user_mint_1_token_account)
            }
        };

        let min_out = min_amount_out.unwrap_or(0);

        // SwapBaseInV2 accounts: [spl_token, amm_pool, authority, coin_vault, pc_vault, user_source, user_destination, user_wallet]
        let metas = [
            AccountMeta::new(mint_1_token_program.key(), false, false), // spl_token
            AccountMeta::new(pool_id.key(), true, false),
            AccountMeta::new(authority.key(), false, false),
            AccountMeta::new(coin_vault.key(), true, false),
            AccountMeta::new(pc_vault.key(), true, false),
            AccountMeta::new(user_source.key(), true, false),
            AccountMeta::new(user_destination.key(), true, false),
            AccountMeta::new(payer.key(), false, true),
        ];

        // SwapBaseInV2: tag=16, amount_in, minimum_amount_out
        let mut data = [0u8; 17];
        data[0] = SWAP_BASE_IN_DISC;
        data[1..9].copy_from_slice(&amount_in.to_le_bytes());
        data[9..17].copy_from_slice(&min_out.to_le_bytes());

        let accs: [&AccountInfo; 8] = [
            mint_1_token_program,
            pool_id,
            authority,
            coin_vault,
            pc_vault,
            user_source,
            user_destination,
            payer,
        ];
        invoke_cpi(&PROGRAM_ID, &metas, &data, &accs)?;
        Ok(())
    }

    fn invoke_swap_base_out(
        &mut self,
        accounts: &[AccountInfo],
        input_mint: Pubkey,
        max_amount_in: u64,
        amount_out: Option<u64>,
        payer: &AccountInfo,
        user_mint_1_token_account: &AccountInfo,
        user_mint_2_token_account: &AccountInfo,
        mint_1_account: &AccountInfo,
        _mint_2_account: &AccountInfo,
        mint_1_token_program: &AccountInfo,
        _mint_2_token_program: &AccountInfo,
    ) -> Result<()> {
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let coin_vault = &accounts[self.dyn_start + D_COIN_VAULT];
        let pc_vault = &accounts[self.dyn_start + D_PC_VAULT];
        let authority = &accounts[self.static_base + S_AMM_AUTHORITY];

        // Determine user source/destination based on input mint
        let (user_source, user_destination) = if input_mint == self.base_token_pk {
            if *mint_1_account.key() == self.base_token_pk {
                (user_mint_1_token_account, user_mint_2_token_account)
            } else {
                (user_mint_2_token_account, user_mint_1_token_account)
            }
        } else {
            if *mint_1_account.key() == self.quote_token_pk {
                (user_mint_1_token_account, user_mint_2_token_account)
            } else {
                (user_mint_2_token_account, user_mint_1_token_account)
            }
        };

        let amount_out_value = amount_out.unwrap_or(0);

        // SwapBaseOutV2 accounts: [spl_token, amm_pool, authority, coin_vault, pc_vault, user_source, user_destination, user_wallet]
        let metas = [
            AccountMeta::new(mint_1_token_program.key(), false, false), // spl_token
            AccountMeta::new(pool_id.key(), true, false),
            AccountMeta::new(authority.key(), false, false),
            AccountMeta::new(coin_vault.key(), true, false),
            AccountMeta::new(pc_vault.key(), true, false),
            AccountMeta::new(user_source.key(), true, false),
            AccountMeta::new(user_destination.key(), true, false),
            AccountMeta::new(payer.key(), false, true),
        ];

        // SwapBaseOutV2: tag=17, max_amount_in, amount_out
        let mut data = [0u8; 17];
        data[0] = SWAP_BASE_OUT_DISC;
        data[1..9].copy_from_slice(&max_amount_in.to_le_bytes());
        data[9..17].copy_from_slice(&amount_out_value.to_le_bytes());

        let accs: [&AccountInfo; 8] = [
            mint_1_token_program,
            pool_id,
            authority,
            coin_vault,
            pc_vault,
            user_source,
            user_destination,
            payer,
        ];
        invoke_cpi(&PROGRAM_ID, &metas, &data, &accs)?;
        Ok(())
    }

    #[cfg(any(test, feature = "debug"))]
    fn log_accounts(&self, _accounts: &[AccountInfo]) -> Result<()> {
        pinocchio::log::sol_log("=== Raydium AMM ===");
        Ok(())
    }
}

impl RaydiumAmm {


    /// Parse native_coin_total and native_pc_total from a Serum/OpenBook OpenOrders account.
    /// Returns (0, 0) if the account is the system program (no open orders).
    fn parse_open_orders(open_orders: &AccountInfo) -> (u64, u64) {
        let data = unsafe { open_orders.borrow_data_unchecked() };
        // OpenOrders minimum size: 5 (head padding) + 8 (flags) + 32 (market) + 32 (owner)
        //   + 8 (coin_free) + 8 (coin_total) + 8 (pc_free) + 8 (pc_total) = 109
        if data.len() < 109 {
            return (0, 0);
        }
        let native_coin_total = u64::from_le_bytes(
            data[OO_NATIVE_COIN_TOTAL_OFFSET..OO_NATIVE_COIN_TOTAL_OFFSET + 8]
                .try_into()
                .unwrap(),
        );
        let native_pc_total = u64::from_le_bytes(
            data[OO_NATIVE_PC_TOTAL_OFFSET..OO_NATIVE_PC_TOTAL_OFFSET + 8]
                .try_into()
                .unwrap(),
        );
        (native_coin_total, native_pc_total)
    }

    pub fn new(
        accounts: &[AccountInfo],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
    ) -> Result<Self> {

        let pool_acc = &accounts[dyn_start + D_POOL];
        let coin_vault = &accounts[dyn_start + D_COIN_VAULT];
        let pc_vault = &accounts[dyn_start + D_PC_VAULT];

        // Read need_take_pnl_coin and need_take_pnl_pc directly from pool data
        let (need_take_pnl_coin, need_take_pnl_pc) = {
            let pool_data = unsafe { pool_acc.borrow_data_unchecked() };
            let need_take_pnl_coin = u64::from_le_bytes(pool_data[192..200].try_into().unwrap());
            let need_take_pnl_pc = u64::from_le_bytes(pool_data[200..208].try_into().unwrap());
            (need_take_pnl_coin, need_take_pnl_pc)
        };

        // Read vault amounts from actual vault token accounts
        let (coin_vault_mint, coin_vault_amount) = read_vault_data(&coin_vault)?;
        let (pc_vault_mint, pc_vault_amount) = read_vault_data(&pc_vault)?;

        // Read open orders amounts from Serum/OpenBook (if account is provided)
        let (oo_native_coin, oo_native_pc) =
            if dyn_start + D_OPEN_ORDERS < dyn_end {
                let open_orders = &accounts[dyn_start + D_OPEN_ORDERS];
                Self::parse_open_orders(open_orders)
            } else {
                (0, 0)
            };

        // Compute effective reserves:
        // total = vault_amount + open_orders_amount - need_take_pnl
        let base_vault_amount = coin_vault_amount
            .saturating_add(oo_native_coin)
            .saturating_sub(need_take_pnl_coin);
        let quote_vault_amount = pc_vault_amount
            .saturating_add(oo_native_pc)
            .saturating_sub(need_take_pnl_pc);

        #[cfg(test)]
        let base_vault_amount = (base_vault_amount as f64 * 0.98) as u64;

        let price = quote_vault_amount as f64 / base_vault_amount as f64;

        // Placeholder: fee rate will be read from on-chain account data
        let fee_rate = 0.0;

        debug_eprintln!("RaydiumAmm: pool_id {:?} , price {}, inverse_price {}, fee_rate {}", pool_acc.key(), price, 1.0 / price, fee_rate);

        // Defer max amounts and transfer fees to prepare_for_execution()
        let instance = RaydiumAmm {
            pool_id: *pool_acc.key(),
            base_token_pk: coin_vault_mint,
            quote_token_pk: pc_vault_mint,
            base_vault_amount,
            quote_vault_amount,
            price,
            fee_rate,
            fee_factor: { let f = 1.0 - fee_rate; (f, f) },
            static_base,
            dyn_start,
            buy_max_in: 0,
            buy_max_out: 0,
            sell_max_in: 0,
            sell_max_out: 0,
            prepared: false,
        };
        // instance.log_accounts(accounts)?;
        Ok(instance)
    }

    fn compute_cached_max(base_vault: u64, quote_vault: u64, fee_rate: f64) -> (u64, u64, u64, u64) {
        fn cp_max(x: u64, y: u64, fee_rate: f64) -> (u64, u64) {
            let ff = 1.0 - fee_rate;
            if ff <= 0.0 || y == 0 {
                return (0, y);
            }
            let dx = (x as f64 * 99.0 / ff) as u64;
            (dx, y)
        }
        let (buy_in, buy_out) = cp_max(base_vault, quote_vault, fee_rate);
        let (sell_in, sell_out) = cp_max(quote_vault, base_vault, fee_rate);
        (buy_in, buy_out, sell_in, sell_out)
    }

    /// Compute deferred fields: max amounts, transfer fee rates.
    /// Called only for instances that participate in a profitable arb path.
    pub fn prepare_for_execution(
        &mut self,
        _accounts: &[AccountInfo],
    ) {
        if self.prepared {
            return;
        }
        self.prepared = true;

        let (buy_in, buy_out, sell_in, sell_out) =
            Self::compute_cached_max(self.base_vault_amount, self.quote_vault_amount, self.fee_rate);
        self.buy_max_in = buy_in;
        self.buy_max_out = buy_out;
        self.sell_max_in = sell_in;
        self.sell_max_out = sell_out;
    }
}

// TODO: rewrite tests using LiteSVM/Mollusk

#[cfg(test)]
mod tests {}
