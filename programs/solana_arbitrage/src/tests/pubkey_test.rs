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
        accounts_length: [u32; 5],
        mints: u16,
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
    "FYnaLRpfVbAi5CnupX1JuxqokiR773WiZPiCz3dzp7BP",
        "So11111111111111111111111111111111111111112",
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
    "Ft6ingqkyR9JkdddhFUhTtKozr2ZbZssA9nu7sPLNtsk",
    "6P985Tsjw9n4JnJdeJhTPmXLUsWzXScscmA2BcUgpump",
    "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
    "Gefw2y2tcGw2fjwa7dgcWrWiGJtDgoc94vJ8tuWBUVL",
    "3sAFdH2ANF8rWoW2NGLLdgnqHUmUdRLcAC9Vd5REpump",
    "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
    "Gefw2y2tcGw2fjwa7dgcWrWiGJtDgoc94vJ8tuWBUVL",

// // # --- Raydium CPMM (Pool 1: 8aQWnm...) ---
    "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C",
    "8aQWnmHrtFXuyBT4XaxxkNT2NnMSjC51asumXS7EsirX",
    "7ZVpFipQYRqZ5Lsn7MBBtZYeUspSJfQ1zCyrHMaYxqsp",
    "HZ7jm8WtcnMotkfnpRKWcSt5BwdXiAo7EmssY2jfYEZB",
    "So11111111111111111111111111111111111111112",
    "EojSqgayhMTaH6wn5bJpFQAo8cr65iyjR4STWvWPE1pU",
    "BgxH5ifebqHDuiADWKhLjXGP5hWZeZLoCdmeWJLkRqLP",
    "Hi5FYEYunSD2fE91QYkdPKtf9WDzXPw1KGDB3L22hGYf",
    "GpMZbSM2GgvTKHJirzeGfMFoaZ8UR2X7F4v8vHTvxFbL",

    // # --- Raydium CPMM (Pool 2: J5x39q...) ---
    "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C",
    "J5x39qNfUCHS6bC9LPbamjgGWjgckBa12UKMSDHWJJzm",
    "F6wapbNSYXhWLHeU37446wJ4wuNDQjZ6fsFqMvZSfg32",
    "755aCsZM6or1uSLYhXuikGWe4sNQHFBTrgdZMcm9qb7F",
    "So11111111111111111111111111111111111111112",
    "EojSqgayhMTaH6wn5bJpFQAo8cr65iyjR4STWvWPE1pU",
    "D4FPEruKEHrG5TenZ2mpDGEfu1iUvTiqBxvpU8HLBvC2",
    "DUfqjoHBZ9cza7GcCo3vwMk971pxJ1ZvkX4miERYMHW5",
    "GpMZbSM2GgvTKHJirzeGfMFoaZ8UR2X7F4v8vHTvxFbL",

// # --- PUMP.FUN AMM ---
    // # Count: 18
    "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
    "FMLkMu3z3mzUi61VwZqCdcFsmrcYq3oTEL3k3KiTMQHr",
    "Ed5Q5i8UqWAHsj6B6VCAwsXDgXHAgmSuZGVse82KqiZ4",
    "7aQVeouoBj14ZEgjKdSv5SX8d2vnUz1TnHWgS882GGw6",
    "3sAFdH2ANF8rWoW2NGLLdgnqHUmUdRLcAC9Vd5REpump",
    "So11111111111111111111111111111111111111112",
    "62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV",
    "94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb",
    "GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR",
    "5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx",
    "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ",
    "VxQVMifqhpcoCLGC959GjUb8gwfoyjJb97yV1F1Uf2b",
    "ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw",
    "11111111111111111111111111111111",
    "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
    "C2aFPdENg4A2HQsmrd5rTw5TaYBX5Ku887cWjbFKtZpw",
    "FjYRd67pXeMMA4V3yN6oCPVavMD9bmdnhyRE6LRDMwiu",
    "3eVLYcqTtv5KstU6HYXrxJaisRFo8hBmWyaAsVRBt4rj",

    // # --- METEORA DLMM ---
    // # Count: 17
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
    "GrmRbawGYqH3dm521q9Kk1GD1YCxusyQ7GtmG4vmWLVt",
    "gPDKDUWogb6ExFbPh4b688DiP2Sb36gK2zTDJXv29vP",
    "CTzmzRvKcJXzFd7DPUkTv2ncC87uB7yjAGEEvnLXefDb",
    "3sAFdH2ANF8rWoW2NGLLdgnqHUmUdRLcAC9Vd5REpump",
    "So11111111111111111111111111111111111111112",
    "Af3uPqaGT4iXPJ1xUNdrJGYiAeW3VKNhbPb51w7EaP3x",
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
    "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr",
    "D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6",
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
    "HoX7GirweqiRXfPCEftL5d8fHeCXFaubUH9rVetU8ua9",
    "FQbCJSSLDsN18Y3Fa4DVg8RCnzakGPk3ZkntwXjnKd3x",
    "5uWF6m2tRbhkWqn9Thng4hDoX3t5hQ4fZhNYbtPEGAhX",
    "So11111111111111111111111111111111111111112",
    "HoX7GirweqiRXfPCEftL5d8fHeCXFaubUH9rVetU8ua9",
    "26QRTNHuESWtNTXRmsvUh19y6nrm3BSTr1a8kJbXzPxC",
    "34Ug7cCSVknBBPGUp3eWEsM14VdUnbQUpHd5ugswqGWN"
];

    const ACCOUNTS_LENGTH: [u32; 5] = [9, 9, 18, 18, 0];
    const MODE: u8 = crate::arb_mode::MULTIPLE_TRADES;

    // =========================================================================
    //  TESTS
    // =========================================================================

    #[tokio::test]
    async fn test_from_pubkeys() {
        run_from_pubkeys(
            PUBKEYS_LIST,
            ACCOUNTS_LENGTH,
            3,
            MODE,
            true,
        )
        .await;
    }
}
