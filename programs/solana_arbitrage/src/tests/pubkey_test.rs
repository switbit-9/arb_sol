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
    "FYnaLRpfVbAi5CnupX1JuxqokiR773WiZPiCz3dzp7BP",
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
    "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
    "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr",

    "So11111111111111111111111111111111111111112",
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
    "Ft6ingqkyR9JkdddhFUhTtKozr2ZbZssA9nu7sPLNtsk",
    "FeR8VBqNRSUD5NtXAj2n3j1dAHkZHfyDktKuLXD4pump",
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
    "H3Lw811Y1iG1LtXhLACrEKW6mCAWTnpjHeNaoYBR9Mfj",

    // # --- Section 2: Meteora DAMM V4 ---
    // # Count: 8
    "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
    "3bC2e2RxcfvF9oP22LvbaNsVwoS2T98q6ErCRoayQYdq",
    "D1GHCUZCEjraHeGaooBYbvtBhe3t4186EUmKwMjoRWoe",
    "AWqyxgarzsDrADVmcv9r3vzeL2s5WWPmZhLgnYKjyndF",
    "So11111111111111111111111111111111111111112",
    "FeR8VBqNRSUD5NtXAj2n3j1dAHkZHfyDktKuLXD4pump",
    "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1",
    "D16qTJVZx9j17uvGVVX5WWJMcYgWKdAxr4SSPDwp1tmP",

    // # --- Section 3: Meteora DLMM (Pool HrNE8C) ---
    // # Count: 16
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
    "HrNE8CpBBPyawPR8PvMDz7XWrbAsiDGSGA23iKAeFqg",
    "EQyk87YXjtmV4nAFERtwAk2kSQT8DhpQR8tQNTU7QpPX",
    "88G2DvkPHz5FAtQsQXwY7vJTAFNZZPvnaNv7VU9B9vff",
    "FeR8VBqNRSUD5NtXAj2n3j1dAHkZHfyDktKuLXD4pump",
    "So11111111111111111111111111111111111111112",
    "GKamevH94PFSy8dEfgfWPT4GX4qHmTjL9gyT4FdjJmeF",
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
    "D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6",
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
    "6HJEQj73REULye76nYuAUBun5HFvrro7uvWkKv6oGxkX",
    "8Ap2CGYGozb2phXg9Fg1UBsrfUddCUFTinZ63hJ2LG4U",
    "6HJEQj73REULye76nYuAUBun5HFvrro7uvWkKv6oGxkX",
    "5HtkgL2aFAZrknfuz5NybqkfHaRDj8rf5PTR6tTVH59J",

    // # --- Section 4: Meteora DLMM (Pool Hr3byz) ---
    // # Count: 17
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
    "Hr3byzHKDa7U9MgnfNr9tvRxzvX7fz3sZLgfDgPHGQ15",
    "3ycrVXfmvfPuYx2QGecgUDTCc9gpvtutE5ZMkSmaV78K",
    "D3AfXff1BMQdmsxnfCuTDm8degSNb6a2xbTeaYhUPyED",
    "FeR8VBqNRSUD5NtXAj2n3j1dAHkZHfyDktKuLXD4pump",
    "So11111111111111111111111111111111111111112",
    "CzkAKBSK8ZonaoCm8athRBnXfZejV57UW532PtiBiHdD",
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
    "D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6",
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
    "2vz7oJj9HDNYpZUYknWoUqEeVtT386PfAhvKGHmBDKHX",
    "4PX4ofpHzy32wtrQxhNA2gaZELkHiXfmfKaAWvZVQLph",
    "2vz7oJj9HDNYpZUYknWoUqEeVtT386PfAhvKGHmBDKHX",
    "HFhe2G3Z3RnpHtKpjKoUuB8nT258QBU7y4dERbo3zXRo",

    // # --- Section 5: Meteora DLMM (Pool 94GevK) ---
    // # Count: 17
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
    "94GevKgX3yofzQgRrNfdyVwJUBWLBbxgAxpT5SRfw7nB",
    "3viwHWCENmeFfKPDzzHimUfwsGS8Xu4QyzJUAbsxqchK",
    "hUUHesUUpdLWPNe8r5CJgbGVUa3rj9sYPipXYnFDVCG",
    "FeR8VBqNRSUD5NtXAj2n3j1dAHkZHfyDktKuLXD4pump",
    "So11111111111111111111111111111111111111112",
    "26QNuPHJi9ZYZ5rqnp8snGAnEaimpqm74KT9CWum9EVa",
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
    "D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6",
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
    "A4CaDaHQ6KvKRYpXzt9zddrvRxPiSr7CfNjDVgYsJ1JX",
    "8t9Uk7AmycTNdbWBF4sQV4tNRRXiaKiUQWrMqLuGowj1",
    "A4CaDaHQ6KvKRYpXzt9zddrvRxPiSr7CfNjDVgYsJ1JX",
    "HohHKrXBotEs7GZuhvmb1SY13831ZhbjTAFo5My3ukTy",

    // # --- Section 6: Meteora DLMM (Pool Cuytim) ---
    // # Count: 15
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
    "CuytimJCi9bETqRZc8CXSsrez7i5sjyi8L9GQtm4GRkT",
    "C2DZwio2a5HHoi2YQtD1vSTvDw929aAqps65KzDnLEv2",
    "2fwgwLiN1NnLn4HaTmAfjroiamjnwJyRXzWFbM4TFCV3",
    "FeR8VBqNRSUD5NtXAj2n3j1dAHkZHfyDktKuLXD4pump",
    "So11111111111111111111111111111111111111112",
    "AU8nRRxNA3foxh6FcZrGmqTdUoqLUM17dfrxdoj2JdaM",
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
    "D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6",
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
    "CX8LzRGkSLFkpqdE369CBJD8wNN56vp9DUcKyQEoxw64",
    "24T1AhTs2vcpj1xf7xXC5WKBDdwRXvcfgibPS3uCQ4qp",
    "CX8LzRGkSLFkpqdE369CBJD8wNN56vp9DUcKyQEoxw64",
    "CX8LzRGkSLFkpqdE369CBJD8wNN56vp9DUcKyQEoxw64"

];

    const ACCOUNTS_LENGTH: [u8; 5] = [8, 14, 14, 14, 14];
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
