use solana_program_test::*;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    instruction::Instruction,
    pubkey::Pubkey,
    signature::Signer,
    transaction::Transaction,
};

/// Anchor discriminator: first 8 bytes of sha256("global:benchmark_cu")
fn benchmark_cu_discriminator() -> [u8; 8] {
    use std::hash::Hasher;
    let hash = <sha2::Sha256 as sha2::Digest>::digest(b"global:benchmark_cu");
    let mut disc = [0u8; 8];
    disc.copy_from_slice(&hash[..8]);
    disc
}

#[tokio::test]
async fn test_cu_benchmark() {
    let program_id: Pubkey = "BJREZ2NxHAqSf4jeaogmdoyF2nhexVpeewokt5iqqCMt"
        .parse()
        .unwrap();

    let mut program_test = ProgramTest::new("solana_arbitrage", program_id, None);
    program_test.set_compute_max_units(1_400_000);

    let (mut banks_client, payer, recent_blockhash) = program_test.start().await;

    let ix = Instruction {
        program_id,
        accounts: vec![],
        data: benchmark_cu_discriminator().to_vec(),
    };

    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
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

    // Print all program logs — CU numbers are between the marker lines
    let metadata = result.metadata.expect("transaction metadata");
    println!("\n╔══════════════════════════════════════════════════════════╗");
    println!("║  CU BENCHMARK: Checker vs Analytical                    ║");
    println!("╠══════════════════════════════════════════════════════════╣");

    let logs: Vec<&str> = metadata
        .log_messages
        .iter()
        .map(|s| s.as_str())
        .collect();

    // Parse CU pairs: sol_log_compute_units logs "Program consumption: XXXXX units remaining"
    let mut prev_cu: Option<u64> = None;
    let mut current_label = String::new();

    for log in &logs {
        if log.contains("===") {
            current_label = log.trim().to_string();
            // Strip "Program log: " prefix if present
            if let Some(stripped) = current_label.strip_prefix("Program log: ") {
                current_label = stripped.to_string();
            }
            println!("║");
            println!("║  {}", current_label);
        }
        if log.contains("units remaining") {
            // Parse: "Program consumption: 1234567 units remaining"
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
        if log.contains("amt=") || log.contains("result=") {
            let cleaned = log
                .strip_prefix("Program log: ")
                .unwrap_or(log)
                .trim();
            println!("║    {}", cleaned);
        }
    }

    println!("║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!("\nFull logs:");
    for log in &logs {
        println!("  {}", log);
    }
}
