use super::super::programs::ProgramMeta;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{account_info::next_account_info, pubkey::Pubkey};

pub const DLMM_PROGRAM_ID: Pubkey = pubkey!("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");

#[derive(Clone)]
pub struct MeteoraDlmm<'info> {
    pub program_id: AccountInfo<'info>,
    pub lb_pair: AccountInfo<'info>,
    pub bin_array_bitmap_extension: Option<AccountInfo<'info>>,
    pub reserve_x: AccountInfo<'info>,
    pub reserve_y: AccountInfo<'info>,
    pub token_x_mint: AccountInfo<'info>,
    pub token_y_mint: AccountInfo<'info>,
    pub oracle: AccountInfo<'info>,
    pub bin_array_lower: AccountInfo<'info>,
    pub bin_array_upper: AccountInfo<'info>,
    pub user_token_x: AccountInfo<'info>,
    pub user_token_y: AccountInfo<'info>,
    pub event_authority: AccountInfo<'info>,
}

impl<'info> ProgramMeta for MeteoraDlmm<'info> {
    fn get_id(&self) -> &Pubkey {
        &DLMM_PROGRAM_ID
    }

    fn get_vaults(&self) -> (&AccountInfo<'_>, &AccountInfo<'_>) {
        unsafe {
            (
                &*(&self.reserve_x as *const AccountInfo<'info> as *const AccountInfo<'_>),
                &*(&self.reserve_y as *const AccountInfo<'info> as *const AccountInfo<'_>),
            )
        }
    }

    fn swap_base_in(&self, amount_in: u64, clock: Clock) -> Result<u64> {
        self.swap_base_in_impl(amount_in, clock)
    }

    fn swap_base_out(&self, amount_in: u64, clock: Clock) -> Result<u64> {
        self.swap_base_out_impl(amount_in, clock)
    }

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
    ) -> Result<()> {
        self.invoke_swap_base_in_impl(
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
}

impl<'info> MeteoraDlmm<'info> {
    pub const PROGRAM_ID: Pubkey = DLMM_PROGRAM_ID;
    pub fn new(accounts: &[AccountInfo<'info>]) -> Result<Self> {
        let mut iter = accounts.iter();
        let program_id = next_account_info(&mut iter)?;
        let lb_pair = next_account_info(&mut iter)?;
        let bin_array_bitmap_extension = next_account_info(&mut iter)?;
        let reserve_x = next_account_info(&mut iter)?;
        let reserve_y = next_account_info(&mut iter)?;
        let token_x_mint = next_account_info(&mut iter)?;
        let token_y_mint = next_account_info(&mut iter)?;
        let oracle = next_account_info(&mut iter)?;
        let bin_array_lower = next_account_info(&mut iter)?;
        let bin_array_upper = next_account_info(&mut iter)?;
        let user_token_x = next_account_info(&mut iter)?;
        let user_token_y = next_account_info(&mut iter)?;
        let event_authority = next_account_info(&mut iter)?;

        Ok(MeteoraDlmm {
            program_id: program_id.clone(),
            lb_pair: lb_pair.clone(),
            bin_array_bitmap_extension: Some(bin_array_bitmap_extension.clone()),
            reserve_x: reserve_x.clone(),
            reserve_y: reserve_y.clone(),
            token_x_mint: token_x_mint.clone(),
            token_y_mint: token_y_mint.clone(),
            oracle: oracle.clone(),
            bin_array_lower: bin_array_lower.clone(),
            bin_array_upper: bin_array_upper.clone(),
            user_token_x: user_token_x.clone(),
            user_token_y: user_token_y.clone(),
            event_authority: event_authority.clone(),
        })
    }

    pub fn swap_base_in_impl(&self, amount_in: u64, _clock: Clock) -> Result<u64> {
        // TODO: Implement proper DLMM quote calculation
        // For now, return a mock value
        msg!("DLMM swap_base_in calculation not yet implemented, returning mock value");
        Ok(amount_in / 100) // Mock: assume 1% price impact
    }

    pub fn swap_base_out_impl(&self, amount_out: u64, _clock: Clock) -> Result<u64> {
        // TODO: Implement proper DLMM quote calculation
        // For now, return a mock value
        msg!("DLMM swap_base_out calculation not yet implemented, returning mock value");
        Ok(amount_out * 101) // Mock: assume 1% price impact
    }

    pub fn invoke_swap_base_in_impl<'a>(
        &self,
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
        // TODO: Implement CPI call to DLMM swap instruction
        msg!("DLMM swap_base_in CPI not yet implemented");
        Ok(())
    }

    pub fn invoke_swap_base_out_impl<'a>(
        &self,
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
        // TODO: Implement CPI call to DLMM swap instruction
        msg!("DLMM swap_base_out CPI not yet implemented");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::prelude::{Clock, InterfaceAccount};
    use anchor_lang::solana_program::{account_info::AccountInfo, pubkey::Pubkey, system_program};
    use anchor_spl::token_interface::Mint;
    use dlmm;

    // Helper function to create a mock AccountInfo with provided data
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

    // Helper to convert solana_sdk::account::Account to AccountInfo
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
            false, // is_signer
            false, // is_writable
            lamports,
            data,
            owner_static,
            account.executable,
            account.rent_epoch,
        )
    }

    // Helper function to fetch account from RPC and convert to AccountInfo
    async fn fetch_account_info_from_rpc(
        rpc_client: &solana_client::nonblocking::rpc_client::RpcClient,
        key: Pubkey,
    ) -> AccountInfo<'static> {
        use solana_sdk::pubkey::Pubkey as SdkPubkey;

        let sdk_pubkey = SdkPubkey::try_from(key.to_bytes().as_ref())
            .expect("Failed to convert Pubkey to SdkPubkey");
        let account = rpc_client
            .get_account(&sdk_pubkey)
            .await
            .expect(&format!("Failed to fetch account {}", key));
        account_to_account_info(key, account)
    }

    /// Get on chain clock
    async fn get_clock(
        rpc_client: &solana_client::nonblocking::rpc_client::RpcClient,
    ) -> std::result::Result<Clock, Box<dyn std::error::Error>> {
        use solana_sdk::sysvar::clock as clock_sysvar;
        let clock_account = rpc_client.get_account(&clock_sysvar::ID).await?;
        let clock_state: Clock = bincode::deserialize(clock_account.data.as_ref())?;
        Ok(clock_state)
    }

    /// Convert raw RPC account to InterfaceAccount<Mint>
    fn account_to_interface_mint(
        account: solana_sdk::account::Account,
        pubkey: Pubkey,
    ) -> InterfaceAccount<'static, Mint> {
        let data = Box::leak(Box::new(account.data));
        let lamports = Box::leak(Box::new(account.lamports));
        let owner = Box::leak(Box::new(account.owner));
        let key = Box::leak(Box::new(pubkey));

        // Create AccountInfo with 'static lifetime
        let account_info: &'static AccountInfo<'static> = Box::leak(Box::new(AccountInfo::new(
            key, false, false, lamports, data, owner, false, 0,
        )));

        // Create InterfaceAccount from AccountInfo
        // Since AccountInfo is 'static, InterfaceAccount will also be 'static
        InterfaceAccount::<Mint>::try_from(account_info).expect("Failed to create InterfaceAccount")
    }

    #[tokio::test]
    async fn test_swap_quote_exact_in() {
        use anchor_client::Cluster;
        use solana_client::nonblocking::rpc_client::RpcClient;
        use std::collections::HashMap;

        // RPC client. No gPA is required.
        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        let sol_usdc = Pubkey::from_str_const("Cgnuirsk5dQ9Ka1Grnru7J8YW1sYncYUjiXvYxT7G4iZ");

        let lb_pair_account = rpc_client.get_account(&sol_usdc).await.unwrap();

        let lb_pair: dlmm::dlmm::accounts::LbPair =
            bytemuck::pod_read_unaligned(&lb_pair_account.data[8..]);

        eprintln!("base_token: {:?}", lb_pair.token_x_mint);
        eprintln!("quote_token: {:?}", lb_pair.token_y_mint);

        let mut mint_accounts = rpc_client
            .get_multiple_accounts(&[lb_pair.token_x_mint, lb_pair.token_y_mint])
            .await
            .unwrap();

        let mint_x_account = mint_accounts[0].take().unwrap();
        let mint_y_account = mint_accounts[1].take().unwrap();

        // 3 bin arrays to left, and right is enough to cover most of the swap, and stay under 1.4m CU constraint.
        // Get 3 bin arrays to the left from the active bin
        let left_bin_array_pubkeys =
            dlmm::get_bin_array_pubkeys_for_swap(sol_usdc, &lb_pair, None, true, 3).unwrap();

        // Get 3 bin arrays to the right the from active bin
        let right_bin_array_pubkeys =
            dlmm::get_bin_array_pubkeys_for_swap(sol_usdc, &lb_pair, None, false, 3).unwrap();

        // Fetch bin arrays separately for buy and sell
        let all_bin_array_pubkeys: Vec<Pubkey> = left_bin_array_pubkeys
            .iter()
            .chain(right_bin_array_pubkeys.iter())
            .cloned()
            .collect();

        let bin_array_accounts = rpc_client
            .get_multiple_accounts(&all_bin_array_pubkeys)
            .await
            .unwrap();

        // Process left arrays (buy) and right arrays (sell) separately
        let mut bin_array_buy_infos = Vec::new();
        let mut bin_array_sell_infos = Vec::new();
        let mut bin_arrays_buy_map = HashMap::new();
        let mut bin_arrays_sell_map = HashMap::new();
        let mut bin_arrays_combined = HashMap::new(); // Combined for quote function

        // Process left arrays (buy)
        for (account_opt, key) in bin_array_accounts
            .iter()
            .take(left_bin_array_pubkeys.len())
            .zip(left_bin_array_pubkeys.iter())
        {
            if let Some(account) = account_opt {
                let bin_array = bytemuck::pod_read_unaligned::<dlmm::dlmm::accounts::BinArray>(
                    &account.data[8..],
                );
                let account_info = account_to_account_info(*key, account.clone());
                bin_array_buy_infos.push(account_info);
                bin_arrays_buy_map.insert(*key, bin_array);
                bin_arrays_combined.insert(*key, bin_array);
            }
        }

        // Process right arrays (sell)
        for (account_opt, key) in bin_array_accounts
            .iter()
            .skip(left_bin_array_pubkeys.len())
            .zip(right_bin_array_pubkeys.iter())
        {
            if let Some(account) = account_opt {
                let bin_array = bytemuck::pod_read_unaligned::<dlmm::dlmm::accounts::BinArray>(
                    &account.data[8..],
                );
                let account_info = account_to_account_info(*key, account.clone());
                bin_array_sell_infos.push(account_info);
                bin_arrays_sell_map.insert(*key, bin_array);
                bin_arrays_combined.insert(*key, bin_array);
            }
        }

        // Use combined map for quote function
        let bin_arrays = bin_arrays_combined;

        // Derive vault PDAs
        let (base_vault_key, _) = dlmm::derive_reserve_pda(lb_pair.token_x_mint, sol_usdc);
        let (quote_vault_key, _) = dlmm::derive_reserve_pda(lb_pair.token_y_mint, sol_usdc);

        // Derive other PDAs
        let (oracle_key, _) = dlmm::derive_oracle_pda(sol_usdc);
        let (bitmap_extension_key, _) = dlmm::derive_bin_array_bitmap_extension(sol_usdc);
        let (event_authority_key, _) = dlmm::derive_event_authority_pda();

        // Use placeholder keys for optional accounts
        let host_fee_in_key = Pubkey::new_unique();
        let memo_key = Pubkey::new_unique();

        // Convert RPC accounts to AccountInfo
        let lb_pair_account_info = account_to_account_info(sol_usdc, lb_pair_account);
        let base_vault = fetch_account_info_from_rpc(&rpc_client, base_vault_key).await;
        let quote_vault = fetch_account_info_from_rpc(&rpc_client, quote_vault_key).await;
        let base_token = account_to_account_info(lb_pair.token_x_mint, mint_x_account);
        let quote_token = account_to_account_info(lb_pair.token_y_mint, mint_y_account);
        let oracle = fetch_account_info_from_rpc(&rpc_client, oracle_key).await;
        let bitmap_extension = fetch_account_info_from_rpc(&rpc_client, bitmap_extension_key).await;

        // Create mock accounts for optional fields
        let host_fee_in =
            create_mock_account_info_with_data(host_fee_in_key, system_program::id(), None);
        let memo = create_mock_account_info_with_data(memo_key, system_program::id(), None);
        let event_authority =
            create_mock_account_info_with_data(event_authority_key, system_program::id(), None);

        let mut accounts = vec![
            lb_pair_account_info,
            base_vault,
            quote_vault,
            base_token,
            quote_token,
            oracle,
            host_fee_in,
            memo,
            event_authority,
            bitmap_extension,
        ];

        // Add bin array accounts: buy arrays, then SOL MINT separator, then sell arrays
        accounts.extend(bin_array_buy_infos);
        // Add SOL MINT as separator - fetch it from RPC
        let sol_mint_key = anchor_spl::token::spl_token::native_mint::id();
        let sol_mint_account_info = fetch_account_info_from_rpc(&rpc_client, sol_mint_key).await;
        accounts.push(sol_mint_account_info);
        accounts.extend(bin_array_sell_infos);

        let meteora_dlmm = MeteoraDlmm::new(&accounts).unwrap();

        // 1 SOL -> USDC
        let in_sol_amount = 1_000_000_000;

        let clock1 = get_clock(&rpc_client).await.unwrap();

        let sol_mint = Pubkey::from_str_const("So11111111111111111111111111111111111111112");

        // Determine swap_for_y: if SOL is token_x, we swap X for Y (swap_for_y = true)
        // If SOL is token_y, we swap Y for X (swap_for_y = false)
        let swap_for_y = lb_pair.token_x_mint == sol_mint;

        if swap_for_y {
            let quote_result = meteora_dlmm.swap_base_in(in_sol_amount, clock1).unwrap();
            eprintln!("1 SOL -> {:?} TOKEN", quote_result as f64);
        } else {
            let quote_result = meteora_dlmm.swap_base_out(in_sol_amount, clock1).unwrap();
            eprintln!("1 SOL -> {:?} TOKEN", quote_result as f64);
        }

        // Fetch mint accounts again for the second quote call
        let mut mint_accounts2 = rpc_client
            .get_multiple_accounts(&[lb_pair.token_x_mint, lb_pair.token_y_mint])
            .await
            .unwrap();
        let mint_x_account2 = mint_accounts2[0].take().unwrap();
        let mint_y_account2 = mint_accounts2[1].take().unwrap();

        let clock2 = get_clock(&rpc_client).await.unwrap();

        let mint_x_interface = account_to_interface_mint(mint_x_account2, lb_pair.token_x_mint);
        let mint_y_interface = account_to_interface_mint(mint_y_account2, lb_pair.token_y_mint);

        let quote_result = dlmm::quote_exact_in(
            sol_usdc,
            &lb_pair,
            in_sol_amount,
            swap_for_y,
            bin_arrays.clone(),
            None,
            &clock2,
            &mint_x_interface,
            &mint_y_interface,
        )
        .unwrap();

        let amount_out_2 = quote_result.amount_out;

        eprintln!("1 SOL -> {:?} TOKEN", amount_out_2);

        // For TOKEN -> SOL: if SOL is token_x, we swap Y for X (swap_for_y = false)
        // If SOL is token_y, we swap X for Y (swap_for_y = true)
        let swap_for_y_reverse = !swap_for_y;

        if swap_for_y_reverse {
            let quote_result = meteora_dlmm.swap_base_in(amount_out_2, clock2).unwrap();
            eprintln!(
                "{:?} TOKEN -> {:?} SOL",
                amount_out_2,
                quote_result as f64 / 1_000_000_000.0
            );
        } else {
            let quote_result = meteora_dlmm.swap_base_out(amount_out_2, clock2).unwrap();
            eprintln!(
                "{:?} TOKEN -> {:?} SOL",
                amount_out_2,
                quote_result as f64 / 1_000_000_000.0
            );
        }

        // Fetch clock again for the quote call (clock2 was moved in swap_base_in/swap_base_out)
        let clock3 = get_clock(&rpc_client).await.unwrap();

        let quote_result = dlmm::quote_exact_in(
            sol_usdc,
            &lb_pair,
            amount_out_2,
            swap_for_y_reverse,
            bin_arrays.clone(),
            None,
            &clock3,
            &mint_x_interface,
            &mint_y_interface,
        )
        .unwrap();

        eprintln!(
            "{:?} TOKEN -> {:?} SOL",
            amount_out_2,
            quote_result.amount_out as f64 / 1_000_000_000.0
        );
    }
}
