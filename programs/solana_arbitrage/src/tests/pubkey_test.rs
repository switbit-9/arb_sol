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

// ========= Pump AMM =========
    "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA", // program_id
    "DEM6pi7aDBtEDk568jdVbUKexzqpYajgwaFqAU2bhpEN", // pool_id
    "xZXj8E31wvi6xFv9suvKMY1Q4c2ndrVUixrgi7WSd7Q",  // base_vault
    "ARPuqDhu6bCcn8PWZNJHYCjTwie2WSFkzCAR2DNe9sBK", // quote_vault
    "7K2iH6A7C1JTqT8qvfyPDcofvm6gXvSYXZrXav9zUsZJ", // base_token
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
    "AgLqLFUTNs27HkWcSCFVVRqCXKGFqYEnpHLcZyRRHeA2", // global_volume_accumulator 2
    "3XsBXwexm5MRLtwQknf4b65DDJQbTrbgd7yRZgZrALay", // vault_ata
    "6iifa5HTLRN1zrjco8E5wmp7qSkUPXnT9SxGxFatyiVd", // vault_authority

    // // ====== METEORA DLMM (Pool 1) ======
    // "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", // program_id
    // "ABq8CGNZUhpubpgUVrD1gEmcDSkysPyXqkdTDLpQ43rz", // pool_id
    // "ApaDWtaWRdVY6SFaEeBwE8gaQk4Tw7zjaCQ6eJz1QWEo", // base_vault
    // "2xuUQkkTCcDUpP5ptyGXhyZVHzsLGuCqBixFAW2Gghtg", // quote_vault
    // "atVjZ7uM8sVrLFi5Xe1JiLGW6mW9pvQdTCWzhNFpump", // base_token
    // "So11111111111111111111111111111111111111112", // quote_token
    // "3GJWdCj5vMYGHJrQWa15jLS88cpx1ihn413A4M7ueq6b", // oracle
    // "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", // host_fee_in
    // "D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6", // event_authority
    // "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", // bitmap_extension
    // "FTEGV3ZMtmoEi6TNqgKPMW5va5gPY1UJFKLq68eV2UxX", // buy_bin 1
    // "d6QzXuGM7WpkVoP4RopcCMdF7TciiAmmE4uV5Csc94a",  // buy_bin 2
    // "FTEGV3ZMtmoEi6TNqgKPMW5va5gPY1UJFKLq68eV2UxX", // buy_bin 3
    // "7xGW1S4h9yofGtvkATxSZ4VScfiHYe4sRRDNSBf14oHB", // buy_bin 4

    // // ====== METEORA DLMM (Pool 2) ======
    // "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", // program_id
    // "4sfi7mBGcxaMc1wNKqpZVgjSPM6roovr2HvcE9Ub4Toc", // pool_id
    // "73ht2DVSCbki9qY4w4WxL1QXWopshgkbCimszBsmBJFs", // base_vault
    // "8njr3X5aFiXXLgAeckmE8YALBiinnkWqLXW4s9aMsvjU", // quote_vault
    // "atVjZ7uM8sVrLFi5Xe1JiLGW6mW9pvQdTCWzhNFpump", // base_token
    // "So11111111111111111111111111111111111111112", // quote_token
    // "73P62gNyxHWb1XN4ZLz9Y32rEKg9kdcT6Vdkfpf14uVX", // oracle
    // "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", // host_fee_in
    // "D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6", // event_authority
    // "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", // bitmap_extension
    // "7Er5mV3m8uzv4BCHcE3yituiSsJYUbPfwG2AsFtTP9mz", // buy_bin 1
    // "Ftb1FunnJ3kcUdwETYmVD4UJcf94z9MNhFMFGdykdF8q", // buy_bin 2
    // "7Er5mV3m8uzv4BCHcE3yituiSsJYUbPfwG2AsFtTP9mz", // buy_bin 3
    // "7Er5mV3m8uzv4BCHcE3yituiSsJYUbPfwG2AsFtTP9mz", // buy_bin 4

    // // ====== Meteora DAMMV2 (Pool 1) ======
    // "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG", // program_id
    // "Cx291pZScKsprpWtt452zJfqX7t5TD8EjzaEd4kyPvBF", // pool_id
    // "4VreYqTn4qyHDjXguVQ2PWT3nY2sdnTst1RiRR7297yH", // base_vault
    // "Hx18o3RJEhD1cGTmt4CHSvL8cgxa7pZ9LBU1iFKAk3xr", // quote_vault
    // "atVjZ7uM8sVrLFi5Xe1JiLGW6mW9pvQdTCWzhNFpump", // base_token
    // "So11111111111111111111111111111111111111112", // quote_token
    // "HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC", // pool_authority
    // "3rmHSu74h1ZcmAisVcWerTCiRDQbUrBKmcwptYGjHfet", // event_authority
    // "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG", // trailing program

    // // ====== Meteora DAMMV2 (Pool 2) ======
    // "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG", // program_id
    // "Bd1LYDnwHHfsG52h86PiQAJXwSgzgJcr4VGrnjQznm3Z", // pool_id
    // "D8A2XG7yu7XNun72RNtJS41Ug7CwrBAYj1aHBngrvUMG", // base_vault
    // "jPCfCzPxYv1rhEVoCwF4ninyD4V2pz1uccDff46G2Q4",  // quote_vault
    // "atVjZ7uM8sVrLFi5Xe1JiLGW6mW9pvQdTCWzhNFpump", // base_token
    // "So11111111111111111111111111111111111111112", // quote_token
    // "HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC", // pool_authority
    // "3rmHSu74h1ZcmAisVcWerTCiRDQbUrBKmcwptYGjHfet", // event_authority
    // "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG", // trailing program

// =============== WHIRLPOOLS_LOG_ACCOUNTS ==============
    "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc", // program_id
    "DoDMhobdPWkyJqvfgEHnwS5zwU1JHoeNc5RyZbR3ej3i", // pool_id
    "gre9K8LTrSzKx6svpMgEm6ozmSPpuH4pmVu4Yy5YmsQ",  // base_vault
    "BUqpyoJsXmAesgUQKBW9KgJwkbcVzgkS1ih5AY1V6cgA", // quote_vault
    "So11111111111111111111111111111111111111112", // base_token
    "7K2iH6A7C1JTqT8qvfyPDcofvm6gXvSYXZrXav9zUsZJ", // quote_token
    "KxgSaQmNUL31PrpWCLKHfsbmx2j6Lw6atqejVadVSyX", // oracle_address
    "6xHXa4uwcL3X9PrC7U7AEK9w7hLx9kXHFT5aiCxGeFCd", // tick_array_0
    "27gnMNvdzHXNHWSj2qudHjFbs9TKkYyPhj3GPSLxZHgC", // tick_array_1
    "pxWaxZr4zmH6LcqSjyCw9aKYyBySVtb1YuNBrbqBXHL", // tick_array_2

];

    const ACCOUNTS_LENGTH: [u8; 5] = [19,  10, 0, 0, 0];
    const MODE: u8 = crate::arb_mode::SINGLE_PAIR_MULTI_MARKET;

    // =========================================================================
    //  TESTS
    // =========================================================================

    #[tokio::test]
    async fn test_from_pubkeys_loop() {
        loop {
            let result = run_from_pubkeys(
                PUBKEYS_LIST,
                ACCOUNTS_LENGTH,
                2,
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
            2,
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
