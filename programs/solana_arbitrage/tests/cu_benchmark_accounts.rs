use anchor_lang::AnchorSerialize;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_program_test::*;
use solana_sdk::{
    account::Account,
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Signer,
    transaction::Transaction,
};
use solana_arbitrage::InstructionData;

fn get_api_url() -> String {
    let api_key = "f230200b-f911-43c1-a242-4e7b066d0993";
    format!("https://mainnet.helius-rpc.com/?api-key={}", api_key)
}

/// Anchor discriminator: first 8 bytes of sha256("global:benchmark_cu_accounts")
fn benchmark_cu_accounts_discriminator() -> [u8; 8] {
    let hash = <sha2::Sha256 as sha2::Digest>::digest(b"global:benchmark_cu_accounts");
    let mut disc = [0u8; 8];
    disc.copy_from_slice(&hash[..8]);
    disc
}

/// Same pubkey list as in test_fixtures.rs
const PUBKEYS_LIST: &[&str] = &[
    "FYnaLRpfVbAi5CnupX1JuxqokiR773WiZPiCz3dzp7BP",  // payer [0]
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",  // token_program [1]
    "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",  // token_program_2022 [2]
    "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr",  // memo [3]
    "So11111111111111111111111111111111111111112",  // wsol_mint [4]
    "Ft6ingqkyR9JkdddhFUhTtKozr2ZbZssA9nu7sPLNtsk",  // user_wsol_ata [5]
    "91E4cACQcnWDf7wYmwwM81HjLvCe6FR6vDT518ospump",  // mint [6]
    "9itvEmA6itbjnQFCbFvQfaTiGkXMdv52QNu2RNXUM2mr",  // user_token_ata [7]
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
    "9d5sUYZdiPf97PkKzhRVFVrkW8NdQeYUPw5wpaqKfNCU",  // pump_amm [9d5sUYZd] pool_id [21]
    "7RRjycKSXn1nFqDfPLMFextqLG6Gh7zxUbx9Abd3j6ii",  // pump_amm [9d5sUYZd] base_vault [22]
    "DtWwsopVgTicpz5L5MLKH5c9CFToz6fEX4nQSMhf4BYx",  // pump_amm [9d5sUYZd] quote_vault [23]
    "WDKV514AGcLebNdbwrTFvAB1tzHJCUVnkieBiALz15i",  // pump_amm [9d5sUYZd] user_volume_acc [24]
    "4shRJJF5itY9W29tVSJWVQxxSBmu6ny1BR3X1z5XyqzS",  // pump_amm [9d5sUYZd] pool_v2 [25]
    "7obsBPmgJHPTXDLctkDqGooYB4Zs3SDcKEtrTRnJJEgb",  // pump_amm [9d5sUYZd] user_vol_wsol_ata [26]
    "BfwRZk29NRKRknEAVZRzbQ69FJL8SNm7WBd4b9Znfopr",  // pump_amm [9d5sUYZd] vault_ata [27]
    "7kYNa3216WzYAN9Ju5vBDcqCAjBFco5uvf7FteDtNXrX",  // pump_amm [9d5sUYZd] vault_authority [28]
    "2rPUFNTqqdeVWpwLHcQZjddCQ11cZqxc2LKZbfBdtnnU",  // meteora_dlmm [2rPUFNTq] pool_id [29]
    "AjTQ5SUW8bCP9zSAbSdZhAVTfviGW4vyUKwNuN9SEnk5",  // meteora_dlmm [2rPUFNTq] base_vault [30]
    "F3joK831kJoebXNXk88EZT8arySpEis1Z6wpq7i4XU4K",  // meteora_dlmm [2rPUFNTq] quote_vault [31]
    "5bmpWkm6uwmVJWudtb7w5Y8vQoRbhWvcXxxczawPazy9",  // meteora_dlmm [2rPUFNTq] oracle [32]
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm [2rPUFNTq] bitmap_ext [33]
    "6qZjehg9owZBvfcXwBPKfADmf49bD5EweC76RkbWjf5f",  // meteora_dlmm [2rPUFNTq] bin_array_buy_0 [34]
    "6qZjehg9owZBvfcXwBPKfADmf49bD5EweC76RkbWjf5f",  // meteora_dlmm [2rPUFNTq] bin_array_buy_1 [35]
    "7jREzkE2gd4bzPYomSFTPhrjDFYxt832eeBExMuTd6f6",  // meteora_dlmm [7jREzkE2] pool_id [36]
    "3GrZnyU43Vye5J1ndym7riZBaiF5EuvM14ADghq6WFoT",  // meteora_dlmm [7jREzkE2] base_vault [37]
    "4g71Hrk2fiuLbBro51WL4jFPpkspfzw9YrkAxsGn9xME",  // meteora_dlmm [7jREzkE2] quote_vault [38]
    "7CsLnX4iEDsvheTcEn8eEtxzQmXrzoxWpnyPeZPESWt",  // meteora_dlmm [7jREzkE2] oracle [39]
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",  // meteora_dlmm [7jREzkE2] bitmap_ext [40]
    "HtemaCxvHsgjx8f23ax7cMCUP6z2HcMZ5HENFY66yEKB",  // meteora_dlmm [7jREzkE2] bin_array_buy_0 [41]
    "HtemaCxvHsgjx8f23ax7cMCUP6z2HcMZ5HENFY66yEKB",  // meteora_dlmm [7jREzkE2] bin_array_buy_1 [42]
];

/// Same instruction data as test_fixtures.rs, serialized with Anchor discriminator.
fn build_instruction_data() -> Vec<u8> {
    let data = InstructionData {
        mints: 2,
        shared_statics_len: 13,
        pool_types: [9, 3, 3, 0, 0, 0, 0, 0],
        type_static_offsets: [0, 10, 10, 0, 0, 0, 0, 0],
        mode: 0,
        test: false,
        group_sizes: [0, 0, 0, 0],
        pool_fees: vec![10500],
    };

    let disc = benchmark_cu_accounts_discriminator();
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(&disc);
    data.serialize(&mut buf).expect("borsh serialize");
    buf
}

async fn fetch_account_from_rpc(rpc: &RpcClient, pubkey: &Pubkey) -> Option<Account> {
    rpc.get_account(pubkey).await.ok()
}

#[tokio::test]
async fn test_cu_benchmark_accounts() {
    let program_id: Pubkey = "BJREZ2NxHAqSf4jeaogmdoyF2nhexVpeewokt5iqqCMt"
        .parse()
        .unwrap();

    let rpc = RpcClient::new(get_api_url());

    // Parse all pubkeys and fetch accounts from RPC
    let mut pubkeys: Vec<Pubkey> = Vec::with_capacity(PUBKEYS_LIST.len());
    let mut account_metas: Vec<AccountMeta> = Vec::with_capacity(PUBKEYS_LIST.len());

    let mut program_test = ProgramTest::new("solana_arbitrage", program_id, None);
    program_test.set_compute_max_units(1_400_000);

    // Deduplicate: ProgramTest doesn't like duplicate add_account calls
    let mut added: std::collections::HashSet<Pubkey> = std::collections::HashSet::new();

    for pubkey_str in PUBKEYS_LIST {
        let pk: Pubkey = pubkey_str.parse().expect("invalid pubkey");
        pubkeys.push(pk);

        // All accounts passed as remaining_accounts are not signers, read-only
        account_metas.push(AccountMeta::new_readonly(pk, false));

        if added.contains(&pk) {
            continue;
        }
        added.insert(pk);

        // Skip system program — already in test validator
        if pk == solana_sdk::system_program::id() {
            continue;
        }

        match fetch_account_from_rpc(&rpc, &pk).await {
            Some(account) => {
                eprintln!("  loaded: {}", pk);
                program_test.add_account(pk, account);
            }
            None => {
                eprintln!("  mock:   {}", pk);
                // Add a minimal account so the transaction doesn't fail
                program_test.add_account(
                    pk,
                    Account {
                        lamports: 1_000_000,
                        data: vec![0u8; 8],
                        owner: solana_sdk::system_program::id(),
                        executable: false,
                        rent_epoch: 0,
                    },
                );
            }
        }
    }

    // Also need sysvar::clock for Clock::get() inside the program
    let (mut banks_client, payer, recent_blockhash) = program_test.start().await;

    let ix_data = build_instruction_data();

    let ix = Instruction {
        program_id,
        accounts: account_metas,
        data: ix_data,
    };

    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
            ComputeBudgetInstruction::request_heap_frame(256 * 1024),
            ix,
        ],
        Some(&payer.pubkey()),
        &[&payer],
        recent_blockhash,
    );

    let result = banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();

    let metadata = result.metadata.expect("transaction metadata");

    // ── Parse and display CU results ──
    println!("\n╔══════════════════════════════════════════════════════════════════════════════╗");
    println!("║  CU BENCHMARK: Real Accounts — Checker vs Analytical                        ║");
    println!("╠══════════════════════════════════════════════════════════════════════════════╣");

    let logs: Vec<&str> = metadata
        .log_messages
        .iter()
        .map(|s| s.as_str())
        .collect();

    let mut prev_cu: Option<u64> = None;
    let mut current_label = String::new();

    for log in &logs {
        if log.contains("===") {
            current_label = log.trim().to_string();
            if let Some(stripped) = current_label.strip_prefix("Program log: ") {
                current_label = stripped.to_string();
            }
            println!("║");
            println!("║  {}", current_label);
        }
        if log.contains("units remaining") {
            let cu: u64 = log
                .split_whitespace()
                .find_map(|w| w.parse::<u64>().ok())
                .unwrap_or(0);

            if let Some(before) = prev_cu {
                let used = before.saturating_sub(cu);
                println!("║    CU used: {}", used);
                prev_cu = None;
            } else {
                prev_cu = Some(cu);
            }
        }
        if log.contains("paths=") || log.contains("profit=") || log.contains("pools_parsed=") || log.contains("n_pools=") {
            let cleaned = log
                .strip_prefix("Program log: ")
                .unwrap_or(log)
                .trim();
            println!("║    {}", cleaned);
        }
    }

    println!("║");
    println!("╚══════════════════════════════════════════════════════════════════════════════╝");
    println!("\nFull logs:");
    for log in &logs {
        println!("  {}", log);
    }
}
