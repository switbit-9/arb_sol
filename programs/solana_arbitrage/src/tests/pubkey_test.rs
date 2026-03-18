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
            "mode={} test={} mints={} pool_types={:?} pool_lengths={:?} total_accounts={}",
            data.mode, test_mode, data.mints, data.pool_types, data.pool_lengths, accounts.len()
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
"AVF9F4C4j8b1Kh4BmNHqybDaHgnZpJ7W7yLvL7hUpump",  // mint [6]
"EUBjD4k32r7TH3fa23hPpX3B2mfMJu83JMvcnTpi61mq",  // user_token_ata [7]
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
"cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG",  // meteora_damm_v2 program_id [18]
"HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC",  // meteora_damm_v2 pool_authority [19]
"3rmHSu74h1ZcmAisVcWerTCiRDQbUrBKmcwptYGjHfet",  // meteora_damm_v2 event_authority [20]
"cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG",  // meteora_damm_v2 referral_token_acc [21]
"AADJrfmWoHVXZhF1UkbHvNC5tqrBpkGdSaxtMYteDm2x",  // pump_amm [AADJrfmW] pool_id [22]
"6M13LRCjGdBSuo9ZvPZ7h4nj8mXvUu9RyRLAQ5rhScYs",  // pump_amm [AADJrfmW] base_vault [23]
"C41sWzRvikSo3KH6U8zoejJ7cN5Ctv2ToT5B22U2M4N2",  // pump_amm [AADJrfmW] quote_vault [24]
"WDKV514AGcLebNdbwrTFvAB1tzHJCUVnkieBiALz15i",  // pump_amm [AADJrfmW] user_volume_acc [25]
"62b5KqEUVBSnJPHtLCftqTkxCKLKoKaree6gsAokYcPm",  // pump_amm [AADJrfmW] pool_v2 [26]
"4shRJJF5itY9W29tVSJWVQxxSBmu6ny1BR3X1z5XyqzS",  // pump_amm [AADJrfmW] user_vol_wsol_ata [27]
"AepwkYPKLDwqacvBVfAZJVv7cjkZhYcQ3mSwjtRQm2HH",  // pump_amm [AADJrfmW] vault_ata [28]
"BEPSWG5CBCcSVsQDk1aV7yB6oE3Hkyn2EJbZQe9G6foq",  // pump_amm [AADJrfmW] vault_authority [29]
"62b5KqEUVBSnJPHtLCftqTkxCKLKoKaree6gsAokYcPm",  // pump_amm [AADJrfmW] dyn_8 [30]
"6rfbVR7qZ8ftSpegyCCKXPX5vaZzjDzhPWP5Ag93YV4W",  // meteora_damm_v2 [6rfbVR7q] pool_id [31]
"TVQj3UiD1Z3TpTbH9QpZzMcNctyEiP6E3XqLhAg5fCz",  // meteora_damm_v2 [6rfbVR7q] base_vault [32]
"5xiGRLirA7TXxy7ydvtyeLqUTd31Xu1oQ4DqtwCApBbN",  // meteora_damm_v2 [6rfbVR7q] quote_vault [33]
];

    fn make_instruction_data(test_mode: bool) -> InstructionData {
        InstructionData                                             {
        mints: 2,
        shared_statics_len: 14,
        pool_types: [0, 2, 0, 0, 0, 0, 0, 0],
        pool_lengths: [9, 3, 0, 0, 0, 0, 0, 0],
        type_static_offsets: [0, 10, 0, 0, 0, 0, 0, 0],
        mode: 0,
        test: false,
        group_sizes: [0, 0, 0, 0],
        mint_fee_bps: vec![],
        mint_fee_max: vec![],
        pool_fees: vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
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
