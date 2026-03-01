#[cfg(test)]
mod tests {
    use crate::start_bot;
    use crate::InstructionData;
    use crate::utils::test_utils::{
        create_mock_account_info, try_fetch_account_info_from_rpc, write_results_to_file,
    };
    use anchor_lang::prelude::Clock;
    use anchor_lang::solana_program::{account_info::AccountInfo, pubkey::Pubkey, system_program};
    use solana_client::nonblocking::rpc_client::RpcClient;
    use std::str::FromStr;

    fn get_api_url() -> String {
        let api_key = "f230200b-f911-43c1-a242-4e7b066d0993";
        format!("https://mainnet.helius-rpc.com/?api-key={}", api_key)
    }

    async fn get_clock_from_rpc(rpc_client: &RpcClient) -> Clock {
        use anchor_client::solana_sdk::sysvar;
        let clock_account = rpc_client
            .get_account(&sysvar::clock::ID)
            .await
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

    /// Run start_bot with a flat pubkey list — the full remaining_accounts.
    ///
    /// `pubkey_list` includes everything: the first 7 header accounts
    /// (payer, mint_1, mint_1_token_program, user_mint_1_token_account,
    ///  mint_2, mint_2_token_program, user_mint_2_token_account)
    /// followed by all pool accounts. start_bot handles splitting via accounts_length.
    ///
    /// All pubkeys are fetched from RPC; accounts not found on-chain get a mock fallback.
    async fn run_from_pubkeys(
        pubkey_list: &[&str],
        accounts_length: [u8; 5],
        mints: u8,
        mode: u8,
        test_mode: bool,
    ) -> Option<crate::arbitrage::algo_2::ArbitragePath> {
        let rpc_client = RpcClient::new(get_api_url());
        let system_id = system_program::id();

        let mut accounts: Vec<AccountInfo<'static>> = Vec::with_capacity(pubkey_list.len());

        for (i, pubkey_str) in pubkey_list.iter().enumerate() {
            let key = Pubkey::from_str(pubkey_str.trim())
                .unwrap_or_else(|_| panic!("Invalid pubkey at index {}: {}", i, pubkey_str));
            let account = match try_fetch_account_info_from_rpc(&rpc_client, key).await {
                Some(info) => info,
                None => {
                    eprintln!("  [{}] mock fallback: {}", i, key);
                    create_mock_account_info(key, system_id, None)
                }
            };
            accounts.push(account);
        }

        let data = InstructionData {
            mints,
            accounts_length,
            mode,
            test: test_mode,
        };

        let clock = get_clock_from_rpc(&rpc_client).await;

        eprintln!("\n=== start_bot ===");
        eprintln!(
            "mode={} test={} mints={} accounts_length={:?} total_accounts={}",
            mode, test_mode, mints, accounts_length, accounts.len()
        );

        let result = start_bot(&accounts, data, clock);
        match result {
            Ok(Some(path)) => {
                eprintln!(
                    "Arbitrage found! profit={} start_amount={} edges={}",
                    path.profit,
                    path.start_amount,
                    path.edges.len()
                );
                Some(path)
            }
            Ok(None) => {
                eprintln!("No arbitrage path found");
                None
            }
            Err(e) => {
                eprintln!("Error: {:?}", e);
                None
            }
        }
    }

    // =========================================================================
    //  PUBKEY LIST — full remaining_accounts: first 7 header + pool accounts
    //  You provide everything, start_bot handles the rest.
    // =========================================================================

    const PUBKEYS_LIST: &[&str] = &[
    "FYnaLRpfVbAi5CnupX1JuxqokiR773WiZPiCz3dzp7BP", // #1
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA", // #2 (Token Program)
    "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb", // #3 (Token 2022 Program)
    "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr", // #4 (Memo Program v2)

    "So11111111111111111111111111111111111111112",
    "Ft6ingqkyR9JkdddhFUhTtKozr2ZbZssA9nu7sPLNtsk",
    "7K2iH6A7C1JTqT8qvfyPDcofvm6gXvSYXZrXav9zUsZJ",
    "HefFPT8D5PKgXd67G2tULUxTRT3gz2ej5TjARQmg3b16",
    "GVUapDeHGQvDiBMkBb7opW5kMRF5RrQ1JjAbPsWNpump",
    "HefFPT8D5PKgXd67G2tULUxTRT3gz2ej5TjARQmg3b16",

// ========= Pump AMM =========
    "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA", // program_id
    "AADJrfmWoHVXZhF1UkbHvNC5tqrBpkGdSaxtMYteDm2x", // pool_id
    "6M13LRCjGdBSuo9ZvPZ7h4nj8mXvUu9RyRLAQ5rhScYs", // base_vault
    "C41sWzRvikSo3KH6U8zoejJ7cN5Ctv2ToT5B22U2M4N2", // quote_vault
    "AVF9F4C4j8b1Kh4BmNHqybDaHgnZpJ7W7yLvL7hUpump", // base_token
    "So11111111111111111111111111111111111111112", // quote_token
    "62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV", // protocol_fee_recipient
    "94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb", // protocol_fee_recipient_token_account
    "GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR", // event_authority
    "5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx", // fee_config
    "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ", // fee_program
    "WDKV514AGcLebNdbwrTFvAB1tzHJCUVnkieBiALz15i", // user_volume_accumulator
    "ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw", // global_config
    "11111111111111111111111111111111",             // system_program
    "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL", // assoc_token_account_program
    "C2aFPdENg4A2HQsmrd5rTw5TaYBX5Ku887cWjbFKtZpw", // global_volume_accumulator 1
    "62b5KqEUVBSnJPHtLCftqTkxCKLKoKaree6gsAokYcPm", // global_volume_accumulator 2
    "AepwkYPKLDwqacvBVfAZJVv7cjkZhYcQ3mSwjtRQm2HH", // vault_ata
    "BEPSWG5CBCcSVsQDk1aV7yB6oE3Hkyn2EJbZQe9G6foq", // vault_authority
    
    // ====== METEORA DLMM (Fee: 0.07296) ======
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", // program_id
    "53gLtVkrPQHWVSGLG5XF3tphdX16ik8Rp31Yez9HkeNx", // pool_id
    "FVNzSzamSuPKFL12cwH3dhjShjjGC3u6MhsrskeMiXpS", // base_vault
    "9JGU2Aa2mJW8Hh1jUvmfZQBrxegEvaX7bepyFiAKPkYP", // quote_vault
    "AVF9F4C4j8b1Kh4BmNHqybDaHgnZpJ7W7yLvL7hUpump", // base_token
    "So11111111111111111111111111111111111111112", // quote_token
    "9eqonXTPh3Fv1wUHr57YbSK36fM19ftYfnwkY9RppmN4", // oracle
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", // host_fee_in
    "D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6", // event_authority
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", // bitmap_extension
    "JDiAZiZKDcfCGWukgAEXP4TZ1fyhwv57kY6FAuAaCHUY", // buy_bin 1
    "JDiAZiZKDcfCGWukgAEXP4TZ1fyhwv57kY6FAuAaCHUY", // buy_bin 2
    "JDiAZiZKDcfCGWukgAEXP4TZ1fyhwv57kY6FAuAaCHUY", // buy_bin 3
    "EPZ17HGtH7WSY8ZXDb2jYnyudc1dwi59LJuhKmc96pre", // buy_bin 4
    
    // ========= Pump AMM =========
    "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA", // program_id
    "BDgZ5PvNRZ3xdzmQV4TgrAtAd16PK2r1mVfg8AeShEqR", // pool_id
    "7zLMQMNRci618BBFD8b2AzPbBA2SqSbVK2swfJfGi88Q", // base_vault
    "D5rgRPeZiHjvUWHdHcgQqVKZjxQVShT3GZ3VkgtXVccK", // quote_vault
    "GVUapDeHGQvDiBMkBb7opW5kMRF5RrQ1JjAbPsWNpump", // base_token
    "So11111111111111111111111111111111111111112", // quote_token
    "62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV", // protocol_fee_recipient
    "94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb", // protocol_fee_recipient_token_account
    "GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR", // event_authority
    "5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx", // fee_config
    "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ", // fee_program
    "WDKV514AGcLebNdbwrTFvAB1tzHJCUVnkieBiALz15i", // user_volume_accumulator
    "ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw", // global_config
    "11111111111111111111111111111111",             // system_program
    "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL", // assoc_token_account_program
    "C2aFPdENg4A2HQsmrd5rTw5TaYBX5Ku887cWjbFKtZpw", // global_volume_accumulator 1
    "6qrtTduxjUg8FqqxDzTDUKwbT8K7E5SLcui8wdxdpt52", // global_volume_accumulator 2
    "DDtrHGKEvmY75N1mBFNGAAWqQbX9GGG7b6Boh37QpLzB", // vault_ata
    "59GJ6jDHj5KBg4cSw44JLMkCea9wwFuZQE2UFNf5NkWq", // vault_authority


    // ====== METEORA DLMM (Fee: 0.07296) ======
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", // program_id
    "53gLtVkrPQHWVSGLG5XF3tphdX16ik8Rp31Yez9HkeNx", // pool_id
    "FVNzSzamSuPKFL12cwH3dhjShjjGC3u6MhsrskeMiXpS", // base_vault
    "9JGU2Aa2mJW8Hh1jUvmfZQBrxegEvaX7bepyFiAKPkYP", // quote_vault
    "GVUapDeHGQvDiBMkBb7opW5kMRF5RrQ1JjAbPsWNpump", // base_token
    "So11111111111111111111111111111111111111112", // quote_token
    "9eqonXTPh3Fv1wUHr57YbSK36fM19ftYfnwkY9RppmN4", // oracle
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", // host_fee_in
    "D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6", // event_authority
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", // bitmap_extension
    "JDiAZiZKDcfCGWukgAEXP4TZ1fyhwv57kY6FAuAaCHUY", // buy_bin 1
    "JDiAZiZKDcfCGWukgAEXP4TZ1fyhwv57kY6FAuAaCHUY", // buy_bin 2
    "JDiAZiZKDcfCGWukgAEXP4TZ1fyhwv57kY6FAuAaCHUY", // buy_bin 3
    "EPZ17HGtH7WSY8ZXDb2jYnyudc1dwi59LJuhKmc96pre", // buy_bin 4

];

    const ACCOUNTS_LENGTH: [u8; 5] = [19, 14, 19, 14, 0];
    const MODE: u8 = crate::arb_mode::MULTIPLE_TRADES;

    // =========================================================================
    //  TESTS
    // =========================================================================

    #[tokio::test]
    async fn test_from_pubkeys_loop() {
        loop {
            let result = run_from_pubkeys(
                PUBKEYS_LIST,
                ACCOUNTS_LENGTH,
                3,
                MODE,
                true,
            )
            .await;

            if let Some(path) = result {
                if path.profit > 0 {
                    write_results_to_file(&[Some(path)]);
                    eprintln!("Profitable path written to file");
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    #[tokio::test]
    async fn test_from_pubkeys_simple() {
        let result = run_from_pubkeys(
            PUBKEYS_LIST,
            ACCOUNTS_LENGTH,
            3,
            MODE,
            true,
        )
        .await;

        if let Some(path) = result {
            if path.profit > 0 {
                write_results_to_file(&[Some(path)]);
                eprintln!("Profitable path written to file");
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}
