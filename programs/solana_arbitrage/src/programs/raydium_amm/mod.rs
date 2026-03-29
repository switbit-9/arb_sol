pub mod state;

use crate::programs::{PoolKind, ProgramMeta};
use crate::utils::cpi::invoke_cpi;
use crate::utils::token::{apply_transfer_fee, MintFee};
use crate::utils::utils::{read_vault_data};
use crate::compat::*;

pub const PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
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
    /// Fee in millionths (e.g. 2500 = 0.25%) for the integer checker
    pub fee_millionths: u64,
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

    fn get_checker_info(&self) -> Result<((f64, f64), (f64, f64), (&Pubkey, &Pubkey), PoolKind)> {
        let inverse = if self.price > 0.0 { 1.0 / self.price } else { 0.0 };
        Ok(((self.price, inverse), self.fee_factor, (&self.base_token_pk, &self.quote_token_pk), PoolKind::RaydiumAmm))
    }

    fn get_prices(&self) -> Result<(f64, f64)> {
        let inverse = if self.price > 0.0 { 1.0 / self.price } else { 0.0 };
        Ok((self.price, inverse))
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

    fn fast_quote<'a>(&mut self, _accounts: &[AccountInfo<'a>], input_mint: Pubkey, amount_in: u64, _profit_pct: f64) -> Result<(u64, u64)> {
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

    fn swap_base_in<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
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

    fn swap_base_out<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
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

    fn invoke_swap_base_in<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        amount_in: u64,
        min_amount_out: Option<u64>,
        payer: &AccountInfo<'a>,
        user_mint_1_token_account: &AccountInfo<'a>,
        user_mint_2_token_account: &AccountInfo<'a>,
        mint_1_account: &AccountInfo<'a>,
        _mint_2_account: &AccountInfo<'a>,
        mint_1_token_program: &AccountInfo<'a>,
        _mint_2_token_program: &AccountInfo<'a>,
    ) -> Result<()> {
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let coin_vault = &accounts[self.dyn_start + D_COIN_VAULT];
        let pc_vault = &accounts[self.dyn_start + D_PC_VAULT];
        let authority = &accounts[self.static_base + S_AMM_AUTHORITY];

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
        let metas = [
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
        data[0] = SWAP_BASE_IN_DISC;
        data[1..9].copy_from_slice(&amount_in.to_le_bytes());
        data[9..17].copy_from_slice(&min_out.to_le_bytes());

        let mut accs: [core::mem::MaybeUninit<AccountInfo<'a>>; 8] = unsafe { core::mem::MaybeUninit::uninit().assume_init() };
        let mut ai = 0usize;
        macro_rules! push_acc {
            (ref $e:expr) => { accs[ai] = core::mem::MaybeUninit::new(unsafe { core::ptr::read($e as *const _) }); ai += 1; };
        }
        push_acc!(ref mint_1_token_program);
        push_acc!(ref pool_id);
        push_acc!(ref authority);
        push_acc!(ref coin_vault);
        push_acc!(ref pc_vault);
        push_acc!(ref user_source);
        push_acc!(ref user_destination);
        push_acc!(ref payer);

        let accs_slice = unsafe { core::slice::from_raw_parts(accs.as_ptr().cast::<AccountInfo<'a>>(), ai) };
        invoke_cpi(&PROGRAM_ID, &metas, &data, accs_slice)?;
        Ok(())
    }

    fn invoke_swap_base_out<'a>(
        &mut self,
        accounts: &[AccountInfo<'a>],
        input_mint: Pubkey,
        max_amount_in: u64,
        amount_out: Option<u64>,
        payer: &AccountInfo<'a>,
        user_mint_1_token_account: &AccountInfo<'a>,
        user_mint_2_token_account: &AccountInfo<'a>,
        mint_1_account: &AccountInfo<'a>,
        _mint_2_account: &AccountInfo<'a>,
        mint_1_token_program: &AccountInfo<'a>,
        _mint_2_token_program: &AccountInfo<'a>,
    ) -> Result<()> {
        let pool_id = &accounts[self.dyn_start + D_POOL];
        let coin_vault = &accounts[self.dyn_start + D_COIN_VAULT];
        let pc_vault = &accounts[self.dyn_start + D_PC_VAULT];
        let authority = &accounts[self.static_base + S_AMM_AUTHORITY];

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
        let metas = [
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
        data[0] = SWAP_BASE_OUT_DISC;
        data[1..9].copy_from_slice(&max_amount_in.to_le_bytes());
        data[9..17].copy_from_slice(&amount_out_value.to_le_bytes());

        let mut accs: [core::mem::MaybeUninit<AccountInfo<'a>>; 8] = unsafe { core::mem::MaybeUninit::uninit().assume_init() };
        let mut ai = 0usize;
        macro_rules! push_acc {
            (ref $e:expr) => { accs[ai] = core::mem::MaybeUninit::new(unsafe { core::ptr::read($e as *const _) }); ai += 1; };
        }
        push_acc!(ref mint_1_token_program);
        push_acc!(ref pool_id);
        push_acc!(ref authority);
        push_acc!(ref coin_vault);
        push_acc!(ref pc_vault);
        push_acc!(ref user_source);
        push_acc!(ref user_destination);
        push_acc!(ref payer);

        let accs_slice = unsafe { core::slice::from_raw_parts(accs.as_ptr().cast::<AccountInfo<'a>>(), ai) };
        invoke_cpi(&PROGRAM_ID, &metas, &data, accs_slice)?;
        Ok(())
    }

    #[cfg(any(test, feature = "debug"))]
    fn log_accounts<'a>(&self, accounts: &[AccountInfo<'a>]) -> Result<()> {
        msg!("=== Raydium AMM ===");
        msg!("[S0] program_id: {}", accounts[self.static_base + S_PROGRAM_ID].key);
        msg!("[S1] amm_authority: {}", accounts[self.static_base + S_AMM_AUTHORITY].key);
        msg!("[D0] pool: {}", accounts[self.dyn_start + D_POOL].key);
        msg!("[D1] coin_vault: {}", accounts[self.dyn_start + D_COIN_VAULT].key);
        msg!("[D2] pc_vault: {}", accounts[self.dyn_start + D_PC_VAULT].key);
        msg!("[D3] open_orders: {}", accounts[self.dyn_start + D_OPEN_ORDERS].key);
        Ok(())
    }
}

impl RaydiumAmm {


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

    pub fn new<'a>(
        accounts: &[AccountInfo<'a>],
        static_base: usize,
        dyn_start: usize,
        dyn_end: usize,
    ) -> Result<Self> {

        let pool_acc = &accounts[dyn_start + D_POOL];
        let coin_vault = &accounts[dyn_start + D_COIN_VAULT];
        let pc_vault = &accounts[dyn_start + D_PC_VAULT];

        // Read need_take_pnl and swap fees directly from pool data
        let (need_take_pnl_coin, need_take_pnl_pc, swap_fee_numerator, swap_fee_denominator) = {
            let pool_data = pool_acc.try_borrow_data()?;
            let need_take_pnl_coin = u64::from_le_bytes(pool_data[192..200].try_into().unwrap());
            let need_take_pnl_pc = u64::from_le_bytes(pool_data[200..208].try_into().unwrap());
            let swap_fee_num = u64::from_le_bytes(pool_data[176..184].try_into().unwrap());
            let swap_fee_den = u64::from_le_bytes(pool_data[184..192].try_into().unwrap());
            (need_take_pnl_coin, need_take_pnl_pc, swap_fee_num, swap_fee_den)
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
        let base_vault_amount = (base_vault_amount as f64 * 1.0) as u64;

        let price = quote_vault_amount as f64 / base_vault_amount as f64;

        // Compute fee rate from on-chain swap_fee_numerator / swap_fee_denominator
        let fee_rate = if swap_fee_denominator > 0 {
            swap_fee_numerator as f64 / swap_fee_denominator as f64
        } else {
            0.0
        };

        debug_eprintln!("RaydiumAmm: pool_id {} , price {}, inverse_price {}, fee_rate {}%", *pool_acc.key, price, 1.0 / price, fee_rate * 100.0);

        // Defer max amounts and transfer fees to prepare_for_execution()
        let instance = RaydiumAmm {
            pool_id: *pool_acc.key,
            base_token_pk: coin_vault_mint,
            quote_token_pk: pc_vault_mint,
            base_vault_amount,
            quote_vault_amount,
            price,
            fee_rate,
            fee_millionths: if swap_fee_denominator > 0 {
                (swap_fee_numerator as u128 * 1_000_000 / swap_fee_denominator as u128) as u64
            } else {
                0
            },
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
    pub fn prepare_for_execution<'a>(
        &mut self,
        _accounts: &[AccountInfo<'a>],
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

#[cfg(test)]
mod tests {
    use super::*;
    use solana_program::clock::Clock;
    use solana_program::{account_info::AccountInfo, pubkey::Pubkey, system_program};
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

    async fn fetch_account_info_from_rpc(
        rpc_client: &RpcClient,
        key: Pubkey,
    ) -> AccountInfo<'static> {
        let sdk_pubkey = SdkPubkey::try_from(key.to_bytes().as_ref()).unwrap();
        let account = rpc_client.get_account(&sdk_pubkey).await
            .unwrap_or_else(|e| panic!("Failed to fetch account {}: {}", key, e));
        account_to_account_info(key, account)
    }

    async fn get_clock_from_rpc(rpc_client: &RpcClient) -> Clock {
        use solana_program::sysvar;
        let clock_account = rpc_client.get_account(&sysvar::clock::ID).await
            .expect("Failed to fetch clock");
        let data = &clock_account.data;
        assert!(data.len() >= 40, "Clock account data too short");
        Clock {
            slot: u64::from_le_bytes(data[0..8].try_into().unwrap()),
            epoch_start_timestamp: i64::from_le_bytes(data[8..16].try_into().unwrap()),
            epoch: u64::from_le_bytes(data[16..24].try_into().unwrap()),
            leader_schedule_epoch: u64::from_le_bytes(data[24..32].try_into().unwrap()),
            unix_timestamp: i64::from_le_bytes(data[32..40].try_into().unwrap()),
        }
    }

    fn get_rpc_client() -> RpcClient {
        let api_key = "f230200b-f911-43c1-a242-4e7b066d0993";
        RpcClient::new(format!("https://mainnet.helius-rpc.com/?api-key={}", api_key))
    }

    fn read_pubkey(data: &[u8], offset: usize) -> Pubkey {
        Pubkey::try_from(&data[offset..offset + 32]).unwrap()
    }

    /// Build a RaydiumAmm instance from a pool_id by fetching all needed accounts from RPC.
    /// Returns (instance, accounts_vec, clock) ready for testing.
    async fn build_from_pool_id(
        pool_id: Pubkey,
    ) -> (RaydiumAmm, Vec<AccountInfo<'static>>, Clock) {
        let rpc_client = get_rpc_client();

        // Fetch pool account (AmmInfo - 752 bytes, no Anchor discriminator)
        let pool_account = rpc_client
            .get_account(&SdkPubkey::try_from(pool_id.to_bytes().as_ref()).unwrap())
            .await
            .unwrap_or_else(|e| panic!("Failed to fetch pool {}: {}", pool_id, e));

        assert!(
            pool_account.data.len() >= state::AMM_INFO_SIZE,
            "Pool account data too short: {} bytes, expected at least {}",
            pool_account.data.len(),
            state::AMM_INFO_SIZE,
        );

        let amm = state::AmmInfoFields::from_bytes(&pool_account.data);

        // Read coin_vault and pc_vault pubkeys from AmmInfo
        let coin_vault_key = read_pubkey(&pool_account.data, 336);
        let pc_vault_key = read_pubkey(&pool_account.data, 368);

        eprintln!("Pool: {}", pool_id);
        eprintln!("  coin_vault: {}", coin_vault_key);
        eprintln!("  pc_vault: {}", pc_vault_key);
        eprintln!("  coin_vault_mint: {}", amm.coin_vault_mint);
        eprintln!("  pc_vault_mint: {}", amm.pc_vault_mint);
        eprintln!("  fee: {}/{}", amm.swap_fee_numerator, amm.swap_fee_denominator);

        // Derive authority PDA
        let authority_key = Pubkey::create_program_address(
            &[b"amm authority", &[amm.nonce]],
            &PROGRAM_ID,
        )
        .expect("Failed to derive authority PDA");

        eprintln!("  authority: {}", authority_key);
        eprintln!("  open_orders: {}", amm.open_orders);

        // Fetch all needed accounts from RPC
        let pool_id_account_info = account_to_account_info(pool_id, pool_account);
        let coin_vault_info = fetch_account_info_from_rpc(&rpc_client, coin_vault_key).await;
        let pc_vault_info = fetch_account_info_from_rpc(&rpc_client, pc_vault_key).await;
        let open_orders_info = fetch_account_info_from_rpc(&rpc_client, amm.open_orders).await;

        let program_id_account =
            create_mock_account_info_with_data(PROGRAM_ID, system_program::id(), None);
        let authority_account =
            create_mock_account_info_with_data(authority_key, system_program::id(), None);

        // Layout:
        // Static (static_base=0): [program_id, authority]
        // Dynamic (dyn_start=2): [pool, coin_vault, pc_vault, open_orders]
        let accounts = vec![
            program_id_account,   // S0
            authority_account,    // S1
            pool_id_account_info, // D0
            coin_vault_info,      // D1
            pc_vault_info,        // D2
            open_orders_info,     // D3
        ];

        let static_base: usize = 0;
        let dyn_start: usize = 2;
        let dyn_end: usize = accounts.len();

        let mut raydium_amm = RaydiumAmm::new(&accounts, static_base, dyn_start, dyn_end)
            .expect("RaydiumAmm::new failed");

        let clock = get_clock_from_rpc(&rpc_client).await;

        eprintln!("  price: {}", raydium_amm.price);
        eprintln!("  fee_rate: {}", raydium_amm.fee_rate);

        (raydium_amm, accounts, clock)
    }

    // ---- Tests ----

    #[tokio::test]
    async fn test_raydium_amm_round_trip() {
        // SOL/USDC Raydium AMM pool on mainnet
        let pool_id = Pubkey::from_str_const("8WwcNqdZjCY5Pt7AkhupAFknV2txca9sq6YBkGzLbvdt");
        let (mut raydium_amm, accounts, clock) = build_from_pool_id(pool_id).await;

        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
        let no_fee = MintFee::ZERO;

        // 1. Print all account pubkeys
        eprintln!("\n=== Account Pubkeys ===");
        eprintln!("base_mint        : {}", raydium_amm.base_token_pk);
        eprintln!("quote_mint       : {}", raydium_amm.quote_token_pk);
        eprintln!("pool_id          : {}", accounts[2 + D_POOL].key);
        eprintln!("coin_vault       : {}", accounts[2 + D_COIN_VAULT].key);
        eprintln!("pc_vault         : {}", accounts[2 + D_PC_VAULT].key);
        eprintln!("open_orders      : {}", accounts[2 + D_OPEN_ORDERS].key);
        eprintln!("program_id       : {}", accounts[S_PROGRAM_ID].key);
        eprintln!("amm_authority    : {}", accounts[S_AMM_AUTHORITY].key);

        // 2. Print price and inverse_price
        let (price, inverse_price) = raydium_amm.get_prices().unwrap();
        eprintln!("\n=== Prices ===");
        eprintln!("price            : {}", price);
        eprintln!("inverse_price    : {}", inverse_price);

        // 3. Print fees
        let (fee_factor, fee_factor_2) = raydium_amm.get_fee_factor().unwrap();
        eprintln!("\n=== Fees ===");
        eprintln!("fee_rate         : {}", raydium_amm.fee_rate);
        eprintln!("fee_factor       : {}", fee_factor);
        eprintln!("fee_factor_2     : {}", fee_factor_2);

        // 4. prepare_for_execution
        raydium_amm.prepare_for_execution(&accounts);
        eprintln!("\n=== After prepare_for_execution ===");
        eprintln!("buy_max_in       : {}", raydium_amm.buy_max_in);
        eprintln!("buy_max_out      : {}", raydium_amm.buy_max_out);
        eprintln!("sell_max_in      : {}", raydium_amm.sell_max_in);
        eprintln!("sell_max_out     : {}", raydium_amm.sell_max_out);

        // 5. Round-trip with start_amount = 1 WSOL
        let start_amount: u64 = 1_000_000_000; // 1 SOL

        let other_mint = if raydium_amm.base_token_pk == sol_mint {
            raydium_amm.quote_token_pk
        } else {
            raydium_amm.base_token_pk
        };

        let rpc = get_rpc_client();
        let other_sdk_pk = SdkPubkey::try_from(other_mint.to_bytes().as_ref()).unwrap();
        let other_mint_account = rpc.get_account(&other_sdk_pk).await.unwrap();
        let token_decimals = other_mint_account.data[44] as i32;
        let sol_decimals: i32 = 9;
        let sol_div = 10f64.powi(sol_decimals);
        let tok_div = 10f64.powi(token_decimals);

        // Direction 1: SOL -> TOKEN -> SOL
        eprintln!("\n=== Direction 1: SOL -> TOKEN -> SOL ===");
        let token_out = raydium_amm.swap_base_in(
            &accounts, sol_mint, start_amount, no_fee, no_fee, &clock,
        ).unwrap();
        let max_sol_in = raydium_amm.swap_base_out(
            &accounts, other_mint, token_out, no_fee, no_fee, &clock,
        ).unwrap();
        eprintln!("AMOUNT_IN {} -> AMOUNT_OUT {} -> MAX_AMOUNT_IN {}", start_amount as f64 / sol_div, token_out as f64 / tok_div, max_sol_in as f64 / sol_div);

        // Direction 2: TOKEN -> SOL -> TOKEN
        eprintln!("\n=== Direction 2: TOKEN -> SOL -> TOKEN ===");
        let sol_out = raydium_amm.swap_base_in(
            &accounts, other_mint, token_out, no_fee, no_fee, &clock,
        ).unwrap();
        let max_token_in = raydium_amm.swap_base_out(
            &accounts, sol_mint, sol_out, no_fee, no_fee, &clock,
        ).unwrap();
        eprintln!("AMOUNT_IN {} -> AMOUNT_OUT {} -> MAX_AMOUNT_IN {}", token_out as f64 / tok_div, sol_out as f64 / sol_div, max_token_in as f64 / tok_div);
    }
}
