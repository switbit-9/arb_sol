use anchor_lang::AnchorSerialize;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_program_test::*;
use solana_sdk::{
    account::Account,
    address_lookup_table::{
        state::{AddressLookupTable, LookupTableMeta},
        AddressLookupTableAccount,
    },
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    message::{v0, VersionedMessage},
    pubkey::Pubkey,
    signature::Signer,
    transaction::VersionedTransaction,
};
use std::borrow::Cow;
use solana_arbitrage::InstructionData;
use solana_arbitrage::test_fixtures::{PUBKEYS_LIST, make_instruction_data};

fn get_api_url() -> String {
    let api_key = "f230200b-f911-43c1-a242-4e7b066d0993";
    format!("https://mainnet.helius-rpc.com/?api-key={}", api_key)
}

/// Anchor discriminator: first 8 bytes of sha256("global:initialize")
fn initialize_discriminator() -> [u8; 8] {
    let hash = <sha2::Sha256 as sha2::Digest>::digest(b"global:initialize");
    let mut disc = [0u8; 8];
    disc.copy_from_slice(&hash[..8]);
    disc
}

/// Build the `initialize` instruction data: Anchor discriminator + serialized InstructionData.
/// Uses test=true so the program skips profit checks and doesn't try to execute swaps via CPI.
fn build_instruction_data() -> Vec<u8> {
    let data = make_instruction_data(true);

    let disc = initialize_discriminator();
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(&disc);
    data.serialize(&mut buf).expect("borsh serialize");
    buf
}

async fn fetch_account_from_rpc(rpc: &RpcClient, pubkey: &Pubkey) -> Option<Account> {
    rpc.get_account(pubkey).await.ok()
}

async fn fetch_clock_from_rpc(rpc: &RpcClient) -> solana_sdk::clock::Clock {
    let clock_account = rpc
        .get_account(&solana_sdk::sysvar::clock::id())
        .await
        .expect("Failed to fetch clock sysvar from RPC");
    let data = &clock_account.data;
    assert!(data.len() >= 40, "Clock account data too short");
    solana_sdk::clock::Clock {
        slot: u64::from_le_bytes(data[0..8].try_into().unwrap()),
        epoch_start_timestamp: i64::from_le_bytes(data[8..16].try_into().unwrap()),
        epoch: u64::from_le_bytes(data[16..24].try_into().unwrap()),
        leader_schedule_epoch: u64::from_le_bytes(data[24..32].try_into().unwrap()),
        unix_timestamp: i64::from_le_bytes(data[32..40].try_into().unwrap()),
    }
}

#[tokio::test]
async fn test_start_bot_cu() {
    let program_id: Pubkey = "BJREZ2NxHAqSf4jeaogmdoyF2nhexVpeewokt5iqqCMt"
        .parse()
        .unwrap();

    let rpc = RpcClient::new(get_api_url());

    // Fetch live clock from RPC so the program sees realistic timestamps
    let clock = fetch_clock_from_rpc(&rpc).await;
    eprintln!(
        "RPC clock: slot={} epoch={} unix_timestamp={}",
        clock.slot, clock.epoch, clock.unix_timestamp
    );

    let mut pubkeys: Vec<Pubkey> = Vec::with_capacity(PUBKEYS_LIST.len());
    let mut account_metas: Vec<AccountMeta> = Vec::with_capacity(PUBKEYS_LIST.len());

    let mut program_test = ProgramTest::new("solana_arbitrage", program_id, None);
    program_test.set_compute_max_units(1_400_000);

    // Deduplicate: ProgramTest doesn't like duplicate add_account calls
    let mut added: std::collections::HashSet<Pubkey> = std::collections::HashSet::new();
    let mut loaded_count = 0u32;
    let mut mock_count = 0u32;

    for (i, (pubkey_str, is_writable)) in PUBKEYS_LIST.iter().enumerate() {
        let pk: Pubkey = pubkey_str.parse().expect("invalid pubkey");
        pubkeys.push(pk);

        if *is_writable {
            account_metas.push(AccountMeta::new(pk, false));
        } else {
            account_metas.push(AccountMeta::new_readonly(pk, false));
        }

        if added.contains(&pk) {
            eprintln!("  [{}] dedup skip: {}", i, pk);
            continue;
        }
        added.insert(pk);

        // Skip system program — already in test validator
        if pk == solana_sdk::system_program::id() {
            eprintln!("  [{}] system prog (built-in): {}", i, pk);
            continue;
        }

        match fetch_account_from_rpc(&rpc, &pk).await {
            Some(account) => {
                eprintln!(
                    "  [{}] loaded: {} (owner={}, data_len={})",
                    i, pk, account.owner, account.data.len()
                );
                loaded_count += 1;
                program_test.add_account(pk, account);
            }
            None => {
                eprintln!("  [{}] MOCK:   {} *** empty data ***", i, pk);
                mock_count += 1;
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

    eprintln!(
        "\n=== Account summary: {} loaded, {} mocked, {} total ===\n",
        loaded_count, mock_count, PUBKEYS_LIST.len()
    );

    // Serialize clock sysvar manually (bincode le layout: 5 fields, 8 bytes each)
    let mut clock_data = Vec::with_capacity(40);
    clock_data.extend_from_slice(&clock.slot.to_le_bytes());
    clock_data.extend_from_slice(&clock.epoch_start_timestamp.to_le_bytes());
    clock_data.extend_from_slice(&clock.epoch.to_le_bytes());
    clock_data.extend_from_slice(&clock.leader_schedule_epoch.to_le_bytes());
    clock_data.extend_from_slice(&clock.unix_timestamp.to_le_bytes());

    // Set the clock sysvar to match mainnet
    program_test.add_account(
        solana_sdk::sysvar::clock::id(),
        Account {
            lamports: 1_000_000,
            data: clock_data,
            owner: solana_sdk::sysvar::id(),
            executable: false,
            rent_epoch: 0,
        },
    );

    // ── Build Address Lookup Table ──
    // All non-payer accounts go into the ALT to stay under the 38-account static key limit
    let alt_key = Pubkey::new_unique();
    let alt_addresses: Vec<Pubkey> = pubkeys[1..].to_vec();

    let alt_table = AddressLookupTable {
        meta: LookupTableMeta::default(), // deactivation_slot = MAX, last_extended_slot = 0
        addresses: Cow::Owned(alt_addresses.clone()),
    };
    let alt_data = alt_table
        .serialize_for_tests()
        .expect("serialize ALT");

    program_test.add_account(
        alt_key,
        Account {
            lamports: 1_000_000,
            data: alt_data,
            owner: solana_sdk::address_lookup_table::program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );

    let (mut banks_client, payer, recent_blockhash) = program_test.start().await;

    // Replace account[0] (payer) with the test payer — writable + signer
    account_metas[0] = AccountMeta::new(payer.pubkey(), true);

    let ix_data = build_instruction_data();

    let ix = Instruction {
        program_id,
        accounts: account_metas,
        data: ix_data,
    };

    // Build versioned transaction with ALT
    let address_lookup_table_account = AddressLookupTableAccount {
        key: alt_key,
        addresses: alt_addresses,
    };

    let v0_msg = v0::Message::try_compile(
        &payer.pubkey(),
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
            ComputeBudgetInstruction::request_heap_frame(256 * 1024),
            ix,
        ],
        &[address_lookup_table_account],
        recent_blockhash,
    )
    .expect("Failed to compile v0 message");

    let tx = VersionedTransaction::try_new(VersionedMessage::V0(v0_msg), &[&payer])
        .expect("Failed to sign versioned tx");

    let result = banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();

    let metadata = result.metadata.expect("transaction metadata");
    let cu_consumed = metadata.compute_units_consumed;

    // ── Display results ──
    println!("\n╔══════════════════════════════════════════════════════════════════════════════╗");
    println!("║  CU BENCHMARK: start_bot (initialize) — Full Execution                     ║");
    println!("╠══════════════════════════════════════════════════════════════════════════════╣");
    println!("║");
    println!("║  Total CU consumed: {}", cu_consumed);
    println!("║");

    let logs: Vec<&str> = metadata
        .log_messages
        .iter()
        .map(|s| s.as_str())
        .collect();

    // Show any program messages (profit, errors, etc.)
    for log in &logs {
        if log.starts_with("Program log:") {
            let cleaned = log.strip_prefix("Program log: ").unwrap_or(log).trim();
            println!("║  {}", cleaned);
        }
    }

    println!("║");
    println!("╚══════════════════════════════════════════════════════════════════════════════╝");
    println!("\nFull logs:");
    for log in &logs {
        println!("  {}", log);
    }
}
