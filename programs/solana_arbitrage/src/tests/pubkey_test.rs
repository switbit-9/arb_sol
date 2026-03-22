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
    /// `pubkey_list` layout:
    ///   [0]           payer
    ///   [1]           spl token program
    ///   [2]           token-2022 program
    ///   [3]           memo program
    ///   [4 + i*2]     mint_i
    ///   [4 + i*2 + 1] user_mint_i_token_account
    ///   then shared statics, then pool dynamic accounts.
    ///
    /// All pubkeys are fetched from RPC; accounts not found on-chain get a mock fallback.
    async fn run_from_pubkeys(
        pubkey_list: &[&str],
        data: InstructionData,
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

        let test_mode = data.test;

        let clock = get_clock_from_rpc(&rpc_client).await;

        eprintln!("\n=== start_bot ===");
        eprintln!(
            "mode={} test={} mints={} pool_types={:?} total_accounts={}",
            data.mode, test_mode, data.mints, data.pool_types, accounts.len()
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
"FYnaLRpfVbAi5CnupX1JuxqokiR773WiZPiCz3dzp7BP",  // payer [0]
"TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",  // token_program [1]
"TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",  // token_program_2022 [2]
"MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr",  // memo [3]
"So11111111111111111111111111111111111111112",  // wsol_mint [4]
"Ft6ingqkyR9JkdddhFUhTtKozr2ZbZssA9nu7sPLNtsk",  // user_wsol_ata [5]
"2TpMjYXnrgxoeVCq2i6EAR8vNWqe5MNvHCz3bENNpump",  // mint [6]
"9BvUUpvAW5PjWWuKEmyuoisHYjA1pjUv3LxTwqpy6YPY",  // user_token_ata [7]
"pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",  // pump_amm program_id [8]
"62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV",  // pump_amm protocol_fee_recipient [9]
"94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb",  // pump_amm protocol_fee_token_acc [10]
"GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR",  // pump_amm event_authority [11]
"5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx",  // pump_amm fee_config [12]
"pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ",  // pump_amm fee_program [13]
"ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw",  // pump_amm global [14]
"11111111111111111111111111111111",  // pump_amm system_program [15]
"ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",  // pump_amm assoc_token_prog [16]
"C2aFPdENg4A2HQsmrd5rTw5TaYBX5Ku887cWjbFKtZpw",  // pump_amm global_vol_acc [17]
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm program_id [18]
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm host_fee_in [19]
"D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6",  // meteora_dlmm event_authority [20]
"HJAqvquMLHxcx7BYwDixukJM4zYBaTDG69uDWbo18zv",  // pump_amm [HJAqvquM] pool_id [21]
"3J8k6Aw4CQV5BfFqmDeSEkmrmn6czQP17PX5avHSUWq6",  // pump_amm [HJAqvquM] base_vault [22]
"DWx3Z5qQYLp2eVCFJb8AzW4LYw35z7qArWS8WF5uXZ1a",  // pump_amm [HJAqvquM] quote_vault [23]
"WDKV514AGcLebNdbwrTFvAB1tzHJCUVnkieBiALz15i",  // pump_amm [HJAqvquM] user_volume_acc [24]
"4shRJJF5itY9W29tVSJWVQxxSBmu6ny1BR3X1z5XyqzS",  // pump_amm [HJAqvquM] pool_v2 [25]
"C8uZReppTGXRTnTuWBPrSgjk33fskeyCCTSxKRKSbPN2",  // pump_amm [HJAqvquM] user_vol_wsol_ata [26]
"EfXELLTEt6H8Kgsf5LPB2A55Pue8B4GNxViWr9Aiz9AT",  // pump_amm [HJAqvquM] vault_ata [27]
"CWmKWBryCUW5mBQcrncCEs9CamP9qWa8S6q6i6gMFTb1",  // pump_amm [HJAqvquM] vault_authority [28]
"8dem2dfPchbvP5HauNYWBY7sUgEg2ayUZwtL2nkQGJDB",  // meteora_dlmm [8dem2dfP] pool_id [29]
"7NZMKCnCdfuYfrdvi5RKSUbe1JSv5RPHUuE1vDLPJWXF",  // meteora_dlmm [8dem2dfP] base_vault [30]
"EE3iaQ6gU6i2ZmteqQkxM7ticyNdHoFrwKXgeShggovP",  // meteora_dlmm [8dem2dfP] quote_vault [31]
"J5JAe9KTZ7JPZQpgwZQegd8TqecgW9BB1vmgwyMmeBan",  // meteora_dlmm [8dem2dfP] oracle [32]
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm [8dem2dfP] bitmap_ext [33]
"FsTcjmBZJhTzN92gFdUgTjcR7ri6GcnDvGP7GArhUsg4",  // meteora_dlmm [8dem2dfP] bin_array_buy_0 [34]
"4oifhkxP8jm2PmwF9Foje7Ybj7527eeqnyVy7L6Spjtw",  // meteora_dlmm [8dem2dfP] bin_array_buy_1 [35]
"FsTcjmBZJhTzN92gFdUgTjcR7ri6GcnDvGP7GArhUsg4",  // meteora_dlmm [8dem2dfP] bin_array_sell_0 [36]
"3Xxya2MBpD9zNuDrgdav2PqNh82xr49y1NRy2vnrAokC",  // meteora_dlmm [8dem2dfP] bin_array_sell_1 [37]
"7ik5kwcwjD9W22THNqU5spBA9pELjhzfPg2eGvKtNEGn",  // meteora_dlmm [7ik5kwcw] pool_id [38]
"7JQ4WYhJhxBAhCghN3miadj5CL876khFFK7Bd5Kvf2mT",  // meteora_dlmm [7ik5kwcw] base_vault [39]
"GzWDHkTcovg7r6GxS1qcyJD5kVWHccx3Ms2MkvXTQssP",  // meteora_dlmm [7ik5kwcw] quote_vault [40]
"ZC1SdfhtapP6oavipqSSBjisawVQmb9R1HxrHxw7QuE",  // meteora_dlmm [7ik5kwcw] oracle [41]
"DAet93xfSzooiqSXN3LWhD7jTj5CNyMvNmuLSEiyPyYn",  // meteora_dlmm [7ik5kwcw] bitmap_ext [42]
"3dt6sLTeJANVt3XKXB8pkRW5z6g5TZ7ciUWMoMhQdeDc",  // meteora_dlmm [7ik5kwcw] bin_array_buy_0 [43]
"7cXZVVeb7jXcwrhc88fxQPNe1MToRDP2zBaNagSm9rnZ",  // meteora_dlmm [7ik5kwcw] bin_array_buy_1 [44]
"3dt6sLTeJANVt3XKXB8pkRW5z6g5TZ7ciUWMoMhQdeDc",  // meteora_dlmm [7ik5kwcw] bin_array_sell_0 [45]
"4i3o7s328RF2p3CQgYqcMwTm8mLjdX1QJpN68Y98Ssxt",  // meteora_dlmm [7ik5kwcw] bin_array_sell_1 [46]
];

    fn make_instruction_data(test_mode: bool) -> InstructionData {
        InstructionData                                              {
        mints: 2,
        shared_statics_len: 13,
        pool_types: [9, 3, 3, 0, 0, 0, 0, 0],
        type_static_offsets: [0, 10, 10, 0, 0, 0, 0, 0],
        mode: 0,
        test: false,
        group_sizes: [0, 0, 0, 0],
        pool_fees: vec![5000],
    }
    }

    // =========================================================================
    //  TESTS
    // =========================================================================

    #[tokio::test]
    async fn test_from_pubkeys_loop() {
        loop {
            let result = run_from_pubkeys(
                PUBKEYS_LIST,
                make_instruction_data(true),
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
            make_instruction_data(true),
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
