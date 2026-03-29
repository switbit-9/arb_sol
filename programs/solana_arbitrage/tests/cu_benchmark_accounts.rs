use solana_arbitrage::test_fixtures::{PUBKEYS_LIST, make_instruction_data};
use solana_arbitrage::InstructionData;
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

fn get_api_url() -> String {
    let api_key = "f230200b-f911-43c1-a242-4e7b066d0993";
    format!("https://mainnet.helius-rpc.com/?api-key={}", api_key)
}

/// Build instruction data matching the program's custom wire format from InstructionData:
///   [8-byte discriminator (ignored)] [24 fixed bytes] [4-byte LE fees_len] [fees_len × 4-byte LE u32]
fn build_instruction_data(data: &InstructionData) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);

    // 8-byte discriminator (program skips these, value doesn't matter)
    buf.extend_from_slice(&[0u8; 8]);

    // Fixed 24 bytes:
    buf.push(data.mints);
    buf.push(data.shared_statics_len);
    buf.extend_from_slice(&data.pool_types);
    buf.extend_from_slice(&data.type_static_offsets);
    buf.push(data.mode);
    buf.push(if data.test { 1 } else { 0 });
    buf.extend_from_slice(&data.group_sizes);

    // pool_fees: Vec<u32> as [len: u32 LE] [data: len × u32 LE]
    buf.extend_from_slice(&(data.pool_fees.len() as u32).to_le_bytes());
    for &fee in &data.pool_fees {
        buf.extend_from_slice(&fee.to_le_bytes());
    }

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

    let ix_data_struct = make_instruction_data(false);

    // Parse all pubkeys and fetch accounts from RPC
    let mut pubkeys: Vec<Pubkey> = Vec::with_capacity(PUBKEYS_LIST.len());
    let mut account_metas: Vec<AccountMeta> = Vec::with_capacity(PUBKEYS_LIST.len());

    let mut program_test = ProgramTest::new("solana_arbitrage", program_id, None);
    program_test.set_compute_max_units(1_400_000);

    // Deduplicate: ProgramTest doesn't like duplicate add_account calls
    let mut added: std::collections::HashSet<Pubkey> = std::collections::HashSet::new();

    for (pubkey_str, is_writable) in PUBKEYS_LIST {
        let pk: Pubkey = pubkey_str.parse().expect("invalid pubkey");
        pubkeys.push(pk);

        if *is_writable {
            account_metas.push(AccountMeta::new(pk, false));
        } else {
            account_metas.push(AccountMeta::new_readonly(pk, false));
        }

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

    let ix_data = build_instruction_data(&ix_data_struct);

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

    // Print logs first so we can debug failures
    let logs: Vec<&str> = metadata
        .log_messages
        .iter()
        .map(|s| s.as_str())
        .collect();

    if let Err(e) = result.result {
        println!("\nTransaction FAILED: {:?}", e);
        println!("\nFull logs:");
        for log in &logs {
            println!("  {}", log);
        }
        panic!("Transaction failed: {:?}", e);
    }

    // ── Parse and display CU results ──
    println!("\n╔══════════════════════════════════════════════════════════════════════════════╗");
    println!("║  CU BENCHMARK: Real Accounts — Checker vs Analytical                        ║");
    println!("╠══════════════════════════════════════════════════════════════════════════════╣");

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
