use crate::arbitrage::algo_2::ArbitragePath;
use anchor_lang::solana_program::account_info::AccountInfo;
use anchor_lang::solana_program::pubkey::Pubkey;

// Helper function to create a mock AccountInfo with provided data
pub fn create_mock_account_info_with_data(
    key: Pubkey,
    owner: Pubkey,
    data: Option<Vec<u8>>,
) -> AccountInfo<'static> {
    let data_vec = data.unwrap_or_else(|| vec![0u8; 8]);
    let data_vec = Box::leak(Box::new(data_vec));
    let lamports = Box::leak(Box::new(0u64));
    let owner_static = Box::leak(Box::new(owner));
    let key_static = Box::leak(Box::new(key));

    AccountInfo::new(
        key_static,
        false,
        true,
        lamports,
        data_vec,
        owner_static,
        false,
        0,
    )
}

// Helper function to fetch account from RPC with fallback - returns Option
pub async fn try_fetch_account_info_from_rpc(
    rpc_client: &solana_client::nonblocking::rpc_client::RpcClient,
    key: Pubkey,
) -> Option<AccountInfo<'static>> {
    use solana_sdk::pubkey::Pubkey as SdkPubkey;

    let sdk_pubkey = SdkPubkey::try_from(key.to_bytes().as_ref()).ok()?;
    let account = rpc_client.get_account(&sdk_pubkey).await.ok()?;
    Some(account_to_account_info(key, account))
}

// Helper to convert solana_sdk::account::Account to AccountInfo
pub fn account_to_account_info(
    key: Pubkey,
    account: solana_sdk::account::Account,
) -> AccountInfo<'static> {
    let data = Box::leak(Box::new(account.data));
    let lamports = Box::leak(Box::new(account.lamports));
    let owner_bytes: [u8; 32] = account.owner.to_bytes();
    let owner = Pubkey::try_from(owner_bytes.as_ref()).unwrap();
    let owner_static = Box::leak(Box::new(owner));
    let key_static = Box::leak(Box::new(key));
    AccountInfo::new(
        key_static,
        false, // is_signer
        false, // is_writable
        lamports,
        data,
        owner_static,
        account.executable,
        account.rent_epoch,
    )
}

// Helper function to fetch account from RPC and convert to AccountInfo
pub async fn fetch_account_info_from_rpc(
    rpc_client: &solana_client::nonblocking::rpc_client::RpcClient,
    key: Pubkey,
) -> AccountInfo<'static> {
    use solana_sdk::pubkey::Pubkey as SdkPubkey;

    let sdk_pubkey = SdkPubkey::try_from(key.to_bytes().as_ref())
        .expect("Failed to convert Pubkey to SdkPubkey");
    let account = rpc_client
        .get_account(&sdk_pubkey)
        .await
        .expect(&format!("Failed to fetch account {}", key));
    account_to_account_info(key, account)
}

// Helper function to create a minimal mock AccountInfo
pub fn create_mock_account_info(
    key: Pubkey,
    owner: Pubkey,
    account_data: Option<Vec<u8>>,
) -> AccountInfo<'static> {
    let data = if let Some(provided_data) = account_data {
        Box::leak(Box::new(provided_data))
    } else {
        Box::leak(Box::new(Vec::new()))
    };
    let lamports = Box::leak(Box::new(0u64));
    let owner_static = Box::leak(Box::new(owner));
    let key_static = Box::leak(Box::new(key));

    AccountInfo::new(
        key_static,
        false,
        false,
        lamports,
        data,
        owner_static,
        false,
        0,
    )
}

pub(crate) fn write_results_to_file(results: &[Option<ArbitragePath>]) {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    if results.is_empty() {
        return;
    }

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut log_lines = String::new();

    for (idx, maybe_path) in results.iter().enumerate() {
        let label = match idx {
            0 => "Strategy V2",
            1 => "Strategy V1",
            _ => "Strategy",
        };

        if let Some(path) = maybe_path {
            log_lines.push_str(&format!(
                "=== {} ===\n\
                 timestamp: {}\n\
                 start_amount: {}\n\
                 profit: {}\n\
                 edges:\n",
                label, ts, path.start_amount, path.profit as f64 / 1_000_000_000.0
            ));
            for (i, edge) in path.edges.iter().enumerate() {
                log_lines.push_str(&format!(
                    "  Edge {}: {}->[{}]->{}\n",
                    i + 1,
                    edge.left.mint_account,
                    edge.get_price(),
                    edge.right.mint_account
                ));
            }
            log_lines.push_str("\n");
        } else {
            log_lines.push_str(&format!("=== {} ===\nmissing path\n\n", label));
        }
    }

    if log_lines.is_empty() {
        return;
    }

    let log_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("profit_log.txt");

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("failed to open profit_log.txt for append");
    file.write_all(log_lines.as_bytes())
        .expect("failed to write profit log");
}
