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
"Dfh5DzRgSvvCFDoYc2ciTkMrbDfRKybA4SoFbPmApump",  // mint [6]
"FnWuaZR2r2XQyxJYM2xf7s3JMJf5bWxhDHhUatdhbR94",  // user_token_ata [7]
"675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",  // raydium_amm program_id [8]
"5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1",  // raydium_amm amm_authority [9]
"whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc",  // whirlpool program_id [10]
"8WwcNqdZjCY5Pt7AkhupAFknV2txca9sq6YBkGzLbvdt",  // raydium_amm [8WwcNqdZ] pool_id [11]
"CLTdpsLAs7JTmN1r6AfVSjBTgqpJAtKEWvGbenYrxMYT",  // raydium_amm [8WwcNqdZ] base_vault [12]
"EmyzSTZrb9NCBz8bK8Sfy8iaBT7Bkh1wpjiiKKQhjMxG",  // raydium_amm [8WwcNqdZ] quote_vault [13]
"CTweAnaHqDxevH2k9H9SmTRC3DEVhb3DkXDxVHiEW6kW",  // raydium_amm [8WwcNqdZ] open_orders [14]
"5t1DEPmxpyQzygiXT9Q9xvidHTSBeD1PYnoFYa7wG5mg",  // whirlpool [5t1DEPmx] pool_id [15]
"2ogJYLnBYsQzfh19pTxiv87GhRDWoRGc3oAEAZFhEAA7",  // whirlpool [5t1DEPmx] base_vault [16]
"EcoNf7JkBfLytGkqhZANz2RPECT55997whcEXC1ZhRq5",  // whirlpool [5t1DEPmx] quote_vault [17]
"B3PqJK4z6437oGywZi1DKdzoQRcoi6BC8uP7BMsWQGxg",  // whirlpool [5t1DEPmx] oracle [18]
"6R3gxZPUzLU7dLYNfiafUhNqrA47JuBxNtdDyFNFi9Xe",  // whirlpool [5t1DEPmx] tick_array_0 [19]
"AyzKp4uU4CuWuttm3jgvJynDUhPjY7En9SvYG92uD9G2",  // whirlpool [5t1DEPmx] tick_array_1 [20]
"9prbVZryXSj8nZfihvbHjJ7Z1GSsmsVynbrQUj7ZBnZX",  // whirlpool [5t1DEPmx] tick_array_2 [21]
];

    fn make_instruction_data(test_mode: bool) -> InstructionData {
        InstructionData                                                     {
        mints: 2,
        shared_statics_len: 3,
        pool_types: [5, 4, 0, 0, 0, 0, 0, 0],
        pool_lengths: [4, 7, 0, 0, 0, 0, 0, 0],
        type_static_offsets: [0, 2, 0, 0, 0, 0, 0, 0],
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
