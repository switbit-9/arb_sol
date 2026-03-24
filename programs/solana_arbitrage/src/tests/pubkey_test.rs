#[cfg(test)]
mod tests {
    use crate::start_bot;
    use crate::InstructionData;
    use crate::utils::test_utils::{
        create_mock_account_info, try_fetch_account_info_from_rpc, write_results_to_file,
        sdk_account_to_pinocchio,
    };
    use pinocchio::account_info::AccountInfo;
    use pinocchio::pubkey::Pubkey;
    use pinocchio::sysvars::clock::Clock;
    use solana_client::nonblocking::rpc_client::RpcClient;
    use solana_sdk::pubkey::Pubkey as SdkPubkey;
    use std::str::FromStr;

    fn get_api_url() -> String {
        let api_key = "f230200b-f911-43c1-a242-4e7b066d0993";
        format!("https://mainnet.helius-rpc.com/?api-key={}", api_key)
    }

    async fn get_clock_from_rpc(rpc_client: &RpcClient) -> Clock {
        use solana_sdk::sysvar;
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
        // System program pubkey = all zeros
        let system_id: Pubkey = [0u8; 32];

        let mut accounts: Vec<AccountInfo> = Vec::with_capacity(pubkey_list.len());

        for (i, pubkey_str) in pubkey_list.iter().enumerate() {
            let sdk_key = SdkPubkey::from_str(pubkey_str.trim())
                .unwrap_or_else(|_| panic!("Invalid pubkey at index {}: {}", i, pubkey_str));
            let key: Pubkey = sdk_key.to_bytes();
            let account = match try_fetch_account_info_from_rpc(&rpc_client, key).await {
                Some(info) => info,
                None => {
                    eprintln!("  [{}] mock fallback: {}", i, sdk_key);
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
"84cAEWqiDsV5xXh6CB69Hi3HcnumBbdjH4THfyorpump",  // mint [6]
"7fxc9ZCxePt2BZFp7kXVjAz1keiSkLtpcPPTWwahUZjz",  // user_token_ata [7]
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
"whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc",  // whirlpool program_id [18]
"EgpJVVi6pBFhbmso2vu93EA7HmW74Tjs3E1cjPtMsUUc",  // pump_amm [EgpJVVi6] pool_id [19]
"E4vB9iFu8xrAFiU35xJnpoLhzXBvQKTJ24eKNt8N8742",  // pump_amm [EgpJVVi6] base_vault [20]
"3GGCbxEZ6fRu8WuSksgFa2iD9Ycm8sjEJPNxQEqMATD6",  // pump_amm [EgpJVVi6] quote_vault [21]
"WDKV514AGcLebNdbwrTFvAB1tzHJCUVnkieBiALz15i",  // pump_amm [EgpJVVi6] user_volume_acc [22]
"4shRJJF5itY9W29tVSJWVQxxSBmu6ny1BR3X1z5XyqzS",  // pump_amm [EgpJVVi6] pool_v2 [23]
"2L3qWhNRTKLkYGGLL3YXZ28HFY41M8QJxX5aE4WNp5hZ",  // pump_amm [EgpJVVi6] user_vol_wsol_ata [24]
"5ZGY3sjvmDLwPbb7qXVSMkh8iCG4ErUSpwyg1HdQHm1E",  // pump_amm [EgpJVVi6] vault_ata [25]
"9FppjisnoaWhVVJn3m9tBKscAWZNs1j45KrWjYDjR2NX",  // pump_amm [EgpJVVi6] vault_authority [26]
"3oU24aYf87eoY5XHmn5tjGvtHNCrCSas3wd5hNoTACDr",  // whirlpool [3oU24aYf] pool_id [27]
"TpA3fxmURsGY8ihsGHE4Q22XcTnsBwciQa8veKHuXbe",  // whirlpool [3oU24aYf] base_vault [28]
"LJciDmhZ2E7JEdPcW2yPXKRoPHUnNBcQkPfqxpw4CWk",  // whirlpool [3oU24aYf] quote_vault [29]
"BDopgozjxaTrXqx7oNEeGqUxeigJgDJCGaFWXzAYBVuo",  // whirlpool [3oU24aYf] oracle [30]
"AS6HuRchG6G7Va36oeV12HxnUTMvTyf1pUeScqX3DEqt",  // whirlpool [3oU24aYf] tick_array_0 [31]
"8fji6iEut5ins9Suq7egzxvmMF9xHiGb5bTPzwsmnu65",  // whirlpool [3oU24aYf] tick_array_1 [32]
"6BmUYXkedFU8toMxxLU4r2b4yxKx6n3nUWeZEKMbpJcA",  // whirlpool [3oU24aYf] tick_array_2 [33]
];

    fn make_instruction_data(test_mode: bool) -> InstructionData {
        InstructionData                                                              {
        mints: 2,
        shared_statics_len: 11,
        pool_types: [9, 4, 0, 0, 0, 0, 0, 0],
        type_static_offsets: [0, 10, 0, 0, 0, 0, 0, 0],
        mode: 0,
        test: false,
        group_sizes: [0, 0, 0, 0],
        pool_fees: vec![8000],
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
