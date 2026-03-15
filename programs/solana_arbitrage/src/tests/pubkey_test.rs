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
"8jiVXftnn2ZG6bugK7HAH5j2G3D6TpsG521gqsWwpump",  // mint [8]
"Gbaoo9WppCzcfzZPFTdbQdMjsaXPsnw13gbaRxPQ8nGw",  // user_token_ata [9]
"pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",  // pump_amm program_id [10]
"62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV",  // pump_amm protocol_fee_recipient [11]
"94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb",  // pump_amm protocol_fee_token_acc [12]
"GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR",  // pump_amm event_authority [13]
"5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx",  // pump_amm fee_config [14]
"pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ",  // pump_amm fee_program [15]
"ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw",  // pump_amm global [16]
"11111111111111111111111111111111",  // pump_amm system_program [17]
"ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",  // pump_amm assoc_token_prog [18]
"C2aFPdENg4A2HQsmrd5rTw5TaYBX5Ku887cWjbFKtZpw",  // pump_amm global_vol_acc [19]
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm program_id [20]
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm host_fee_in [21]
"D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6",  // meteora_dlmm event_authority [22]
"AADJrfmWoHVXZhF1UkbHvNC5tqrBpkGdSaxtMYteDm2x",  // pump_amm [AADJrfmW] pool_id [23]
"6M13LRCjGdBSuo9ZvPZ7h4nj8mXvUu9RyRLAQ5rhScYs",  // pump_amm [AADJrfmW] base_vault [24]
"C41sWzRvikSo3KH6U8zoejJ7cN5Ctv2ToT5B22U2M4N2",  // pump_amm [AADJrfmW] quote_vault [25]
"WDKV514AGcLebNdbwrTFvAB1tzHJCUVnkieBiALz15i",  // pump_amm [AADJrfmW] user_volume_acc [26]
"62b5KqEUVBSnJPHtLCftqTkxCKLKoKaree6gsAokYcPm",  // pump_amm [AADJrfmW] pool_v2 [27]
"4shRJJF5itY9W29tVSJWVQxxSBmu6ny1BR3X1z5XyqzS",  // pump_amm [AADJrfmW] user_vol_wsol_ata [28]
"AepwkYPKLDwqacvBVfAZJVv7cjkZhYcQ3mSwjtRQm2HH",  // pump_amm [AADJrfmW] vault_ata [29]
"BEPSWG5CBCcSVsQDk1aV7yB6oE3Hkyn2EJbZQe9G6foq",  // pump_amm [AADJrfmW] vault_authority [30]
"62b5KqEUVBSnJPHtLCftqTkxCKLKoKaree6gsAokYcPm",  // pump_amm [AADJrfmW] dyn_8 [31]
"23XcWM2E2wFnN6gEq3Y2kBz5m6vecwwjePL34fENPRue",  // meteora_dlmm [23XcWM2E] pool_id [32]
"9AKwQqY3Ry58UaY3v5JNUsMMLqeBviPXMsDPFZzwpGTe",  // meteora_dlmm [23XcWM2E] base_vault [33]
"Hgr3B7kz1DWqmJC9KiM1K2v9u6e5wWG6T2mEr6wktHmv",  // meteora_dlmm [23XcWM2E] quote_vault [34]
"71VzCVBUqhWaiCBGR3L57AWe4QDDwWr3tciJUJUqVH4K",  // meteora_dlmm [23XcWM2E] oracle [35]
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm [23XcWM2E] bitmap_ext [36]
"FZoQsL26okvpKD6kQjgA6dAqbruBUp8TANAaF5AYEh6Q",  // meteora_dlmm [23XcWM2E] bin_array_buy_0 [37]
"8w6Cg4vJwpP95NxNXAMnZqGYaiKhjh4cxNEk3iuXh6nR",  // meteora_dlmm [23XcWM2E] bin_array_buy_1 [38]
"FZoQsL26okvpKD6kQjgA6dAqbruBUp8TANAaF5AYEh6Q",  // meteora_dlmm [23XcWM2E] bin_array_sell_0 [39]
"EpnQAtgLKDBha22PqTuidJ4R6WpCPaJNWDTcewAVi35k",  // meteora_dlmm [23XcWM2E] bin_array_sell_1 [40]
"53Gc9uyzrU1Cn82YxDqsgfRjptXecSsr3mLYYH7VWpjV",  // meteora_dlmm [53Gc9uyz] pool_id [41]
"9U5JhcqD234bwejnfiiiYWSKFhwEhNKeRqw5oeULvobj",  // meteora_dlmm [53Gc9uyz] base_vault [42]
"G1mHjydgtt2z8Pn6fH8Whf4mFGXk8j54xUNGTugfFSB3",  // meteora_dlmm [53Gc9uyz] quote_vault [43]
"2ThJTTzrhY2My5at1K5tJsUEETLKjvn2CihqY9MGvbJY",  // meteora_dlmm [53Gc9uyz] oracle [44]
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm [53Gc9uyz] bitmap_ext [45]
"FrTvt3avtJYWsEarCfAw3tuR4JZKhphG2a3zdT5ZKzEu",  // meteora_dlmm [53Gc9uyz] bin_array_buy_0 [46]
"G3NMtQrbtaDACcFmLECS3Qgzz9ob6AfvSpPtAPxEwUyt",  // meteora_dlmm [53Gc9uyz] bin_array_buy_1 [47]
"FrTvt3avtJYWsEarCfAw3tuR4JZKhphG2a3zdT5ZKzEu",  // meteora_dlmm [53Gc9uyz] bin_array_sell_0 [48]
"H5gYhh8zJpr7VXnhtsQhcHFMaWrY8JDyjXPpfxrbxaLu",  // meteora_dlmm [53Gc9uyz] bin_array_sell_1 [49]
"6T7YrYsufaVdTPVDv1Z5hXtHwQpgnXX5U1uK6anC4nPr",  // meteora_dlmm [6T7YrYsu] pool_id [50]
"Hswh4jLoZLykNVLStzCBLwVveN8XVQX4R79SWmv6chSN",  // meteora_dlmm [6T7YrYsu] base_vault [51]
"GsYx9vE7orfuFsajCyrr8FiKtscGRDQZcSFaZhxsVP6e",  // meteora_dlmm [6T7YrYsu] quote_vault [52]
"FDXGMGMHoZ9z343VP1kCaRYvfnnTzFny4jrhdHT4oYwP",  // meteora_dlmm [6T7YrYsu] oracle [53]
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm [6T7YrYsu] bitmap_ext [54]
"94L9V9VuBnwtH4pPTwy52WtNH3wLVYyJA4aumAwj4HVc",  // meteora_dlmm [6T7YrYsu] bin_array_buy_0 [55]
"CTx9aY3cWCqtC1Skq7TLHdgcR9bpEM9MaemnTEeWqiQr",  // meteora_dlmm [6T7YrYsu] bin_array_buy_1 [56]
"94L9V9VuBnwtH4pPTwy52WtNH3wLVYyJA4aumAwj4HVc",  // meteora_dlmm [6T7YrYsu] bin_array_sell_0 [57]
"FdyVEEpMRmmr1hdPbp6gxwk8Xs7jGqBPhES3pwii4syM",  // meteora_dlmm [6T7YrYsu] bin_array_sell_1 [58]
"GAecfDy7L91voFyaDBic3WVzpgf7w9arRvAPhaP398PB",  // meteora_dlmm [GAecfDy7] pool_id [59]
"2JHF8mQq5im3Q2HYMV5D7dNxfhuzQ23wCyHjjWQ3oiZX",  // meteora_dlmm [GAecfDy7] base_vault [60]
"3ohNeMAvDoCuqA7rydt8qkqjeQWferZuvUSqUjTvGMbm",  // meteora_dlmm [GAecfDy7] quote_vault [61]
"YVM4rEwfrzWiAa2wnjEfkRvS6PNY71Upy11bmNkDrhm",  // meteora_dlmm [GAecfDy7] oracle [62]
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm [GAecfDy7] bitmap_ext [63]
"EYxNnv9qfNkPTL1px4QguqvvrfZfCn8D6w4SWFf73v3S",  // meteora_dlmm [GAecfDy7] bin_array_buy_0 [64]
"CTLcJ3wN4wRA3vvB7dDwTst4i6aMFDEnwy7XRdqWprSk",  // meteora_dlmm [GAecfDy7] bin_array_buy_1 [65]
"EYxNnv9qfNkPTL1px4QguqvvrfZfCn8D6w4SWFf73v3S",  // meteora_dlmm [GAecfDy7] bin_array_sell_0 [66]
"7GPaHAPQussd61TWuaJRuqWMAyuC5ahMoGmkwnCaYZVf",  // meteora_dlmm [GAecfDy7] bin_array_sell_1 [67]
"GWZoQyHR1ZmHUgNEqPAU8EyWFZMy5gWooy22ckrSEkTn",  // meteora_dlmm [GWZoQyHR] pool_id [68]
"3zPZCXoPgKyi9fqGsa1avwDUQEFhGGn1gQqcKD3q8DzV",  // meteora_dlmm [GWZoQyHR] base_vault [69]
"EwooxJuTzhiDYftCM516zJg4FNUd7Gd6pDAAACktEg4f",  // meteora_dlmm [GWZoQyHR] quote_vault [70]
"E857EUguVExfnwiMEGECATcLp4cRtGt2z5sV217LR4pi",  // meteora_dlmm [GWZoQyHR] oracle [71]
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm [GWZoQyHR] bitmap_ext [72]
"BQxkTgLWm2LdohCSkiKvh1piyXQFZE9LdPLMZwKkw86i",  // meteora_dlmm [GWZoQyHR] bin_array_buy_0 [73]
"7DT2txL31uHMqJEWcFKadNv54FAiRscEbQxr8srEjmNJ",  // meteora_dlmm [GWZoQyHR] bin_array_buy_1 [74]
"BQxkTgLWm2LdohCSkiKvh1piyXQFZE9LdPLMZwKkw86i",  // meteora_dlmm [GWZoQyHR] bin_array_sell_0 [75]
"BnH16da3n6kvgyKYCZxArwcoXtu6VLfsHaahbyQDfLd8",  // meteora_dlmm [GWZoQyHR] bin_array_sell_1 [76]
"JE224CmtMJBENvxtUmQ5a76wnnwJ9mHdEvtUDYaHvEVG",  // meteora_dlmm [JE224Cmt] pool_id [77]
"5dZxDeViZ7ZMuJXeUk3bz3kjCmF7vzyM8zyuG6BtSSMq",  // meteora_dlmm [JE224Cmt] base_vault [78]
"6jFuARTWaJm7nx4i5WaC8UgernU3QPvq6BBA1ftzAsKN",  // meteora_dlmm [JE224Cmt] quote_vault [79]
"2XeSccN9Ho7QYLq2PzWu5tcB579HeWHMUr6apnUYC6Ym",  // meteora_dlmm [JE224Cmt] oracle [80]
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm [JE224Cmt] bitmap_ext [81]
"FizCYNXQjzcoDZeqq1EhtveeFWYna3VTDRDxCq1PZAnj",  // meteora_dlmm [JE224Cmt] bin_array_buy_0 [82]
"6MPYyG7xKJh19mt1Ai786LEtPspKEK7iScpz2Zn2eGgB",  // meteora_dlmm [JE224Cmt] bin_array_buy_1 [83]
"FizCYNXQjzcoDZeqq1EhtveeFWYna3VTDRDxCq1PZAnj",  // meteora_dlmm [JE224Cmt] bin_array_sell_0 [84]
"ERWHNJ5Hzm2yYDrZSkaESgrFsL6mh9SyL5XkhTiEPmjG",  // meteora_dlmm [JE224Cmt] bin_array_sell_1 [85]
"8QYcaoqEcJ12N1CE5Wn8YqoDAidM3kH4QL9o78ibEE8w",  // pump_amm [8QYcaoqE] pool_id [86]
"6dhbsKAi15pdcquFRuYxsYDwi62TeW1hYXyLUVJfb8Fq",  // pump_amm [8QYcaoqE] base_vault [87]
"7m66H1WXhugEK9jk6bR1Zh37TWuQfwyhxktyzj8hKPFB",  // pump_amm [8QYcaoqE] quote_vault [88]
"WDKV514AGcLebNdbwrTFvAB1tzHJCUVnkieBiALz15i",  // pump_amm [8QYcaoqE] user_volume_acc [89]
"6i1pL2Vf5WiLbXoiXum4xNVJMda6MGw8PJmSzRRiYxox",  // pump_amm [8QYcaoqE] pool_v2 [90]
"4shRJJF5itY9W29tVSJWVQxxSBmu6ny1BR3X1z5XyqzS",  // pump_amm [8QYcaoqE] user_vol_wsol_ata [91]
"2mxufurrmHRbM8jwtzMMNfWgJ8G2WD1CcAH5GZvXZ1N1",  // pump_amm [8QYcaoqE] vault_ata [92]
"5gMLubiUnm73ckeQCQEyWgZA4F4eRXk4YZDVgvDbEHuX",  // pump_amm [8QYcaoqE] vault_authority [93]
"6i1pL2Vf5WiLbXoiXum4xNVJMda6MGw8PJmSzRRiYxox",  // pump_amm [8QYcaoqE] dyn_8 [94]
"7RJsuWNKCZfFyynQwu78W8EyWbSLEBTjWtwuhuWQcVwM",  // meteora_dlmm [7RJsuWNK] pool_id [95]
"Ctr84BD4vLNUq1YMF95uY1nJuYCRtBo7ZgHNNebwomMS",  // meteora_dlmm [7RJsuWNK] base_vault [96]
"GBGPph4uuvMvi33B5B5NeJTUjzECBLCpbY73MiqZw7Ww",  // meteora_dlmm [7RJsuWNK] quote_vault [97]
"6FoGDX4b7CLq5pzh2atnpyWSx2xbMhptjcxbcidz53mx",  // meteora_dlmm [7RJsuWNK] oracle [98]
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm [7RJsuWNK] bitmap_ext [99]
"6kMXtcKGPEQrTrKmtbboYkKgkwfDFUhiRtMqwYpXV8iy",  // meteora_dlmm [7RJsuWNK] bin_array_buy_0 [100]
"4rKn1R9idVyVpKaFo3r5GfgFctbsGsnctt29zh3Bodmz",  // meteora_dlmm [7RJsuWNK] bin_array_buy_1 [101]
"6kMXtcKGPEQrTrKmtbboYkKgkwfDFUhiRtMqwYpXV8iy",  // meteora_dlmm [7RJsuWNK] bin_array_sell_0 [102]
"DWmg3LdA3imfS7EdgMdyAtzZKB97iLR13NHLNjbiGtYN",  // meteora_dlmm [7RJsuWNK] bin_array_sell_1 [103]
"4ZeqkcDAetGxbDMgSvcBYTMsDJ2wDzLkD77TR7fsunrS",  // meteora_dlmm [4ZeqkcDA] pool_id [104]
"25z5UGjxFnCpBh9sx9NmQvoEjFpUWJLvaLiHEHJjyGyy",  // meteora_dlmm [4ZeqkcDA] base_vault [105]
"BvZ9Yo6bHvVGrECao1JbobBHip4Gx9snERJ9bt5r9Sis",  // meteora_dlmm [4ZeqkcDA] quote_vault [106]
"DfVsA72e2ps3HDihHXoqsVD1F1jSVYomFpm3aXKdzoWS",  // meteora_dlmm [4ZeqkcDA] oracle [107]
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm [4ZeqkcDA] bitmap_ext [108]
"3vsDUJnGbQ6v1RxLLQE7az8Ui4uNAJ4khfHUmQDyYwTf",  // meteora_dlmm [4ZeqkcDA] bin_array_buy_0 [109]
"BDBjXZK78wXk2GX9Zf45qAJV6SfAdF4RMSUsWg6WZUwA",  // meteora_dlmm [4ZeqkcDA] bin_array_buy_1 [110]
"3vsDUJnGbQ6v1RxLLQE7az8Ui4uNAJ4khfHUmQDyYwTf",  // meteora_dlmm [4ZeqkcDA] bin_array_sell_0 [111]
"9nvtRFL5Yd3TpB2Vsyrm4tC7J2Dt5PxGsdZR34wS915L",  // meteora_dlmm [4ZeqkcDA] bin_array_sell_1 [112]
"cyNiDyv8QqCdtGEXeqFVYaPLr5GLL7v1PkVSiZunAPX",  // meteora_dlmm [cyNiDyv8] pool_id [113]
"9ZPYnVQ3QrkWTLYFbz4QL4rKUUwy5UNyeGZUJySzCzDC",  // meteora_dlmm [cyNiDyv8] base_vault [114]
"FbBUMeFqJq7ongSS1vbBaMTPniqjdSxYn9DKGu2zKnNW",  // meteora_dlmm [cyNiDyv8] quote_vault [115]
"79e7go2VpRHQvsw8pVXZw5u9CA9ypcHpWtGGvUBVqKea",  // meteora_dlmm [cyNiDyv8] oracle [116]
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm [cyNiDyv8] bitmap_ext [117]
"Cfemvi7eSmVNCCpANd2KvSS4TbZcXh9q8YJs5yrSm1s",  // meteora_dlmm [cyNiDyv8] bin_array_buy_0 [118]
"3qQ3pWT83yUjbAMyvt5u1MUXfReYVHEBCjZFTZgwR7BW",  // meteora_dlmm [cyNiDyv8] bin_array_buy_1 [119]
"Cfemvi7eSmVNCCpANd2KvSS4TbZcXh9q8YJs5yrSm1s",  // meteora_dlmm [cyNiDyv8] bin_array_sell_0 [120]
"91EXARsg2n4crB1ZbBpiyj9CTrfs965ZWrETWmdFrZ8b",  // meteora_dlmm [cyNiDyv8] bin_array_sell_1 [121]
"45zdGiLkvEhENPpM8KAbijgRdsQw4prAbG3mQun5c5oP",  // meteora_dlmm [45zdGiLk] pool_id [122]
"GZtc8PyBJYcgrFu875WpAtotLMS1BYUXxMVqGPqiFEuA",  // meteora_dlmm [45zdGiLk] base_vault [123]
"DuW2fYro31ZWgMJXM6n6SyFfr4biLL7KVKNnkan1c34L",  // meteora_dlmm [45zdGiLk] quote_vault [124]
"2rxREoa3JMC8NS47TdkFTBxiPtfiv2d8es1bBnXjZ1gU",  // meteora_dlmm [45zdGiLk] oracle [125]
"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm [45zdGiLk] bitmap_ext [126]
"3MQU2trb3iGJdaEkVXhQsqTnw5dBLGU6tpMiGw4ThwYu",  // meteora_dlmm [45zdGiLk] bin_array_buy_0 [127]
"4YihJC8WcbTNEjctB4drJaWt7wvajtQ8cLKL9icMTAT5",  // meteora_dlmm [45zdGiLk] bin_array_buy_1 [128]
"3MQU2trb3iGJdaEkVXhQsqTnw5dBLGU6tpMiGw4ThwYu",  // meteora_dlmm [45zdGiLk] bin_array_sell_0 [129]
"94Ne1myF8fEUhUnEHJ7h98NGEgkaDvK4hnAqKSjQWaBG",  // meteora_dlmm [45zdGiLk] bin_array_sell_1 [130]
];

    const MODE: u8 = crate::arb_mode::MULTIPLE_TRADES;

    fn make_instruction_data(test_mode: bool) -> InstructionData {
        InstructionData {
        mints: 3,
        shared_statics_len: 13,
        pool_types: [0, 3, 3, 3, 3, 3, 3, 0],
        pool_lengths: [9, 9, 9, 9, 9, 9, 9, 9],
        type_static_offsets: [0, 10, 10, 10, 10, 10, 10, 0],
        mode: 2,
        test: false,
        group_sizes: [0; 4],
        // fee_bps: [0; 3],
        // fee_max: [0; 3],
        // pool_fees: [0; 16],
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
