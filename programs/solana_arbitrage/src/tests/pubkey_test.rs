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
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm program_id [22]
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm host_fee_in [23]
"D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6",  // meteora_dlmm event_authority [24]
"AADJrfmWoHVXZhF1UkbHvNC5tqrBpkGdSaxtMYteDm2x",  // pump_amm [AADJrfmW] pool_id [25]
"6M13LRCjGdBSuo9ZvPZ7h4nj8mXvUu9RyRLAQ5rhScYs",  // pump_amm [AADJrfmW] base_vault [26]
"C41sWzRvikSo3KH6U8zoejJ7cN5Ctv2ToT5B22U2M4N2",  // pump_amm [AADJrfmW] quote_vault [27]
"WDKV514AGcLebNdbwrTFvAB1tzHJCUVnkieBiALz15i",  // pump_amm [AADJrfmW] user_volume_acc [28]
"62b5KqEUVBSnJPHtLCftqTkxCKLKoKaree6gsAokYcPm",  // pump_amm [AADJrfmW] pool_v2 [29]
"4shRJJF5itY9W29tVSJWVQxxSBmu6ny1BR3X1z5XyqzS",  // pump_amm [AADJrfmW] user_vol_wsol_ata [30]
"AepwkYPKLDwqacvBVfAZJVv7cjkZhYcQ3mSwjtRQm2HH",  // pump_amm [AADJrfmW] vault_ata [31]
"BEPSWG5CBCcSVsQDk1aV7yB6oE3Hkyn2EJbZQe9G6foq",  // pump_amm [AADJrfmW] vault_authority [32]
"62b5KqEUVBSnJPHtLCftqTkxCKLKoKaree6gsAokYcPm",  // pump_amm [AADJrfmW] dyn_8 [33]
"6rfbVR7qZ8ftSpegyCCKXPX5vaZzjDzhPWP5Ag93YV4W",  // meteora_damm_v2 [6rfbVR7q] pool_id [34]
"TVQj3UiD1Z3TpTbH9QpZzMcNctyEiP6E3XqLhAg5fCz",  // meteora_damm_v2 [6rfbVR7q] base_vault [35]
"5xiGRLirA7TXxy7ydvtyeLqUTd31Xu1oQ4DqtwCApBbN",  // meteora_damm_v2 [6rfbVR7q] quote_vault [36]
"23XcWM2E2wFnN6gEq3Y2kBz5m6vecwwjePL34fENPRue",  // meteora_dlmm [23XcWM2E] pool_id [37]
"9AKwQqY3Ry58UaY3v5JNUsMMLqeBviPXMsDPFZzwpGTe",  // meteora_dlmm [23XcWM2E] base_vault [38]
"Hgr3B7kz1DWqmJC9KiM1K2v9u6e5wWG6T2mEr6wktHmv",  // meteora_dlmm [23XcWM2E] quote_vault [39]
"71VzCVBUqhWaiCBGR3L57AWe4QDDwWr3tciJUJUqVH4K",  // meteora_dlmm [23XcWM2E] oracle [40]
"Gjny2yzYRCiNTfW8iLFRZ7v1VCZV1iJFSvj7B5VhbZgp",  // meteora_dlmm [23XcWM2E] bitmap_ext [41]
"12GF4qoTEu49E1bHsLymCxDQ4nMqM6UH5b6YQKTZcmLZ",  // meteora_dlmm [23XcWM2E] bin_array_buy_0 [42]
"FAUGZajAntxHT8V7wZFaEtpwAZDrEdBhvWSzxpFXKHXF",  // meteora_dlmm [23XcWM2E] bin_array_buy_1 [43]
"12GF4qoTEu49E1bHsLymCxDQ4nMqM6UH5b6YQKTZcmLZ",  // meteora_dlmm [23XcWM2E] bin_array_sell_0 [44]
"3kDgUd721qmFZKrgkDyWZQSTWUNCor1vvfUYgkoJL8o9",  // meteora_dlmm [23XcWM2E] bin_array_sell_1 [45]
"53Gc9uyzrU1Cn82YxDqsgfRjptXecSsr3mLYYH7VWpjV",  // meteora_dlmm [53Gc9uyz] pool_id [46]
"9U5JhcqD234bwejnfiiiYWSKFhwEhNKeRqw5oeULvobj",  // meteora_dlmm [53Gc9uyz] base_vault [47]
"G1mHjydgtt2z8Pn6fH8Whf4mFGXk8j54xUNGTugfFSB3",  // meteora_dlmm [53Gc9uyz] quote_vault [48]
"2ThJTTzrhY2My5at1K5tJsUEETLKjvn2CihqY9MGvbJY",  // meteora_dlmm [53Gc9uyz] oracle [49]
"3c8o2BP3wkiisCmqtiYZh5PN4Zy6PQ7MovrD542A8Vys",  // meteora_dlmm [53Gc9uyz] bitmap_ext [50]
"G3NMtQrbtaDACcFmLECS3Qgzz9ob6AfvSpPtAPxEwUyt",  // meteora_dlmm [53Gc9uyz] bin_array_buy_0 [51]
"B7QbNcE42VgZW6eh5wpaeAD5AAvdxnuHue52U4KXArBn",  // meteora_dlmm [53Gc9uyz] bin_array_buy_1 [52]
"G3NMtQrbtaDACcFmLECS3Qgzz9ob6AfvSpPtAPxEwUyt",  // meteora_dlmm [53Gc9uyz] bin_array_sell_0 [53]
"FrTvt3avtJYWsEarCfAw3tuR4JZKhphG2a3zdT5ZKzEu",  // meteora_dlmm [53Gc9uyz] bin_array_sell_1 [54]
"6T7YrYsufaVdTPVDv1Z5hXtHwQpgnXX5U1uK6anC4nPr",  // meteora_dlmm [6T7YrYsu] pool_id [55]
"Hswh4jLoZLykNVLStzCBLwVveN8XVQX4R79SWmv6chSN",  // meteora_dlmm [6T7YrYsu] base_vault [56]
"GsYx9vE7orfuFsajCyrr8FiKtscGRDQZcSFaZhxsVP6e",  // meteora_dlmm [6T7YrYsu] quote_vault [57]
"FDXGMGMHoZ9z343VP1kCaRYvfnnTzFny4jrhdHT4oYwP",  // meteora_dlmm [6T7YrYsu] oracle [58]
"ADZ7ZQLtnL7GhB3HxS5x7pmXjH8YstfQAbCNhtw9yxHB",  // meteora_dlmm [6T7YrYsu] bitmap_ext [59]
"CTx9aY3cWCqtC1Skq7TLHdgcR9bpEM9MaemnTEeWqiQr",  // meteora_dlmm [6T7YrYsu] bin_array_buy_0 [60]
"4MQpPCU1xurqwibto4RthMEk26Yu5R28JMsETUD9rsfD",  // meteora_dlmm [6T7YrYsu] bin_array_buy_1 [61]
"CTx9aY3cWCqtC1Skq7TLHdgcR9bpEM9MaemnTEeWqiQr",  // meteora_dlmm [6T7YrYsu] bin_array_sell_0 [62]
"94L9V9VuBnwtH4pPTwy52WtNH3wLVYyJA4aumAwj4HVc",  // meteora_dlmm [6T7YrYsu] bin_array_sell_1 [63]
"JE224CmtMJBENvxtUmQ5a76wnnwJ9mHdEvtUDYaHvEVG",  // meteora_dlmm [JE224Cmt] pool_id [64]
"5dZxDeViZ7ZMuJXeUk3bz3kjCmF7vzyM8zyuG6BtSSMq",  // meteora_dlmm [JE224Cmt] base_vault [65]
"6jFuARTWaJm7nx4i5WaC8UgernU3QPvq6BBA1ftzAsKN",  // meteora_dlmm [JE224Cmt] quote_vault [66]
"2XeSccN9Ho7QYLq2PzWu5tcB579HeWHMUr6apnUYC6Ym",  // meteora_dlmm [JE224Cmt] oracle [67]
"3v9sJxF4sVLyyATGo95gFgske6ZXoR2zhfxq22RXukru",  // meteora_dlmm [JE224Cmt] bitmap_ext [68]
"4AxRfBKNiFAzp4vUh4niogxA3yhunU7YxREn8AbsWSjp",  // meteora_dlmm [JE224Cmt] bin_array_buy_0 [69]
"Boo1CTRmw3kebxmn9RtjRPAUyXGY4CpXaoPEFfKmbtEE",  // meteora_dlmm [JE224Cmt] bin_array_buy_1 [70]
"4AxRfBKNiFAzp4vUh4niogxA3yhunU7YxREn8AbsWSjp",  // meteora_dlmm [JE224Cmt] bin_array_sell_0 [71]
"JAxYyZw713pXvAmnorxnh85nr8VJzg8Vxdy4HCgRZzto",  // meteora_dlmm [JE224Cmt] bin_array_sell_1 [72]
];

    fn make_instruction_data(test_mode: bool) -> InstructionData {
        InstructionData                             {
        mints: 2,
        shared_statics_len: 17,
        pool_types: [0, 2, 3, 3, 3, 3, 0, 0],
        pool_lengths: [9, 3, 9, 9, 9, 9, 0, 0],
        type_static_offsets: [0, 10, 14, 14, 14, 14, 0, 0],
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
