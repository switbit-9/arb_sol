#[cfg(test)]
mod tests {
    use crate::start_bot;
    use crate::InstructionData;
    use crate::utils::test_utils::{
        create_mock_account_info, try_fetch_account_info_from_rpc,
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
    ) {
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
            }
            Ok(None) => {
                eprintln!("No arbitrage path found");
            }
            Err(e) => {
                eprintln!("Error: {:?}", e);
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
    "9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump",
    "HefFPT8D5PKgXd67G2tULUxTRT3gz2ej5TjARQmg3b16",

    // // WHIRLPOOLS_LOG_ACCOUNTS (Pool 3)
    // "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc",
    // "HUGNqTa2qqkaAVsQmYzBorZfusMUY4ToHq99WKJY43Vb",
    // "7uoGBLkUen3xWdzCGg1EDMdu412Et8Dv1TRY1m5YfnYa",
    // "BWeT3zKnp7RzkJLytDGPytvj2Mg4xSU1bdmQzuCSawnc",
    // "So11111111111111111111111111111111111111112",
    // "9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump",
    // "419wKKxoadMbKmy22XYSNg8Q4JnGrZxVpYEEQuBuvZBi",
    // "91Gzcm3e6JeYyNcrSa4xtVW8GFXKPEBATej74YqFmUpG",
    // "Dg1D3joM7mn9cXVkUPaaVZUSp3hFs39siik54j6LhfVr",
    // "AGP8fCE3QxX3USfdA298LaNFT26ZbmCMqNfhhqJKx1q1",

    // WHIRLPOOLS_LOG_ACCOUNTS (Pool 4)
    "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc",
    "DZH9RqkyqeDvYzK6Sfe1RZyeiJ3BTyp7s6jm8tkKniPK",
    "7wAcS15Cr3MZ6fXPkELh3QtosgmvnRBummyLmfgrc4CF",
    "3wcQQKpPJsdQT8Cen3RJo3WsTH9TD9Z72sqwnZcLu2nt",
    "So11111111111111111111111111111111111111112",
    "9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump",
    "GBLUSBMKjQ8qosyMpGAeri3S77eXimASe3p8Ay7AY75Y",
    "rTwJuFgqsB8Tmm1JiuYBKnKM36BvN5hLaYTr11MrVRm",
    "8ozxzqUUoPiozQ7oK8xzF5KNUvyGMqF4WfMLEf7yoTaC",
    "2WLAR24hcygRjmGw7QAJDWyE6A4mrFLDtyjYaP7wSNFn",

// // METEORA DLMM (Pool 9)
//     "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
//     "Ccd1Xr9eQHPGuTEgL9UZyXc9zNbHvGNHuQuP5Fqw3XWu",
//     "BJRi7Ji382SkBhDK3umnBCAJVYWdQWfZQz64nVBpgeng",
//     "4jvmrwiBcFffxz9VMwxVV1ibVZJXbptzjGcSeAt6Q1DY",
//     "9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump",
//     "So11111111111111111111111111111111111111112",
//     "7KCWbRoSMMhSyYioUg7zaQ4KMzrMhhWbRrH8pMjtT77a",
//     "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
//     "D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6",
//     "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
//     "8ZZALK4v3TXymNAxKwnZSw5jYq16vwo5wQX5LTYkKPB6",
//     "5eanfPwztsAnLWdogV1HS8JSfXd6mDbqvy9k6UnXU9ZT",
//     "8ZZALK4v3TXymNAxKwnZSw5jYq16vwo5wQX5LTYkKPB6",
//     "FVof1so77vD45USocQ7FyHCr2WB6az9HexW4mKNeePD5",

    // Pump AMM
    "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
    "AmmpSnW5xVeKHTAU9fMjyKEMPgrzmUj3ah5vgvHhAB5J",
    "E1gaED81eE56vJfbtGscMrWYaT5gUevVkqTFjz5rfhb9",
    "4uRCZ3YcYpvkn7ueoVTKK5RaUMEoyJrR5xYZbTzC7pxy",
    "9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump",
    "So11111111111111111111111111111111111111112",
    "62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV",
    "94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb",
    "GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR",
    "5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx",
    "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ",
    "WDKV514AGcLebNdbwrTFvAB1tzHJCUVnkieBiALz15i",
    "ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw",
    "11111111111111111111111111111111",
    "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
    "C2aFPdENg4A2HQsmrd5rTw5TaYBX5Ku887cWjbFKtZpw",
    "Ei6iux5MMYG8JxCTr58goADqFTtMroL9TXJityF3fAQc",
    "8N3GDaZ2iwN65oxVatKTLPNooAVUJTbfiVJ1ahyqwjSk",

];

    const ACCOUNTS_LENGTH: [u8; 5] = [10, 18, 0, 0, 0];
    const MODE: u8 = crate::arb_mode::SINGLE_PAIR_MULTI_MARKET;

    // =========================================================================
    //  TESTS
    // =========================================================================

    #[tokio::test]
    async fn test_from_pubkeys() {
        run_from_pubkeys(
            PUBKEYS_LIST,
            ACCOUNTS_LENGTH,
            2,
            MODE,
            true,
        )
        .await;
    }
}
