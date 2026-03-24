use crate::arbitrage::algo_2::ArbitragePath;
use pinocchio::account_info::AccountInfo;
use pinocchio::pubkey::Pubkey;

// Pinocchio's internal Account header is #[repr(C)] with this layout:
//   [0]:     borrow_state: u8
//   [1]:     is_signer: u8
//   [2]:     is_writable: u8
//   [3]:     executable: u8
//   [4..8]:  original_data_len: u32
//   [8..40]: key: [u8; 32]
//   [40..72]: owner: [u8; 32]
//   [72..80]: lamports: u64
//   [80..88]: data_len: u64
//   [88..]:  data bytes
// AccountInfo is #[repr(C)] { raw: *mut Account } — a single pointer.
// We allocate the header+data contiguously and transmute the pointer.
const ACCOUNT_HEADER_SIZE: usize = 88;

/// Create a pinocchio AccountInfo backed by a leaked heap buffer.
/// Safe to use in tests — the buffer is intentionally never freed.
pub fn create_mock_account_info(
    key: Pubkey,
    owner: Pubkey,
    data: Option<Vec<u8>>,
) -> AccountInfo {
    let data = data.unwrap_or_default();
    make_pinocchio_account_info(key, owner, 0, false, data)
}

/// Build a pinocchio AccountInfo from raw parts, backed by a static heap buffer.
pub fn make_pinocchio_account_info(
    key: Pubkey,
    owner: Pubkey,
    lamports: u64,
    executable: bool,
    data: Vec<u8>,
) -> AccountInfo {
    let data_len = data.len();
    let total = ACCOUNT_HEADER_SIZE + data_len;
    let mut buf = vec![0u8; total].into_boxed_slice();
    buf[3] = executable as u8;
    buf[8..40].copy_from_slice(&key);
    buf[40..72].copy_from_slice(&owner);
    buf[72..80].copy_from_slice(&lamports.to_le_bytes());
    buf[80..88].copy_from_slice(&(data_len as u64).to_le_bytes());
    if data_len > 0 {
        buf[88..].copy_from_slice(&data);
    }
    let raw = Box::into_raw(buf) as *mut u8;
    // SAFETY: AccountInfo is #[repr(C)] containing a single *mut Account.
    // Account is #[repr(C)] with the layout we've constructed above.
    // The buffer is leaked so the pointer remains valid for the test lifetime.
    unsafe { std::mem::transmute::<*mut u8, AccountInfo>(raw) }
}

// Helper function to fetch account from RPC with fallback - returns Option
pub async fn try_fetch_account_info_from_rpc(
    rpc_client: &solana_client::nonblocking::rpc_client::RpcClient,
    key: Pubkey,
) -> Option<AccountInfo> {
    use solana_sdk::pubkey::Pubkey as SdkPubkey;
    let sdk_pubkey = SdkPubkey::new_from_array(key);
    let account = rpc_client.get_account(&sdk_pubkey).await.ok()?;
    Some(sdk_account_to_pinocchio(key, account))
}

/// Convert a solana_sdk Account to a pinocchio AccountInfo.
pub fn sdk_account_to_pinocchio(
    key: Pubkey,
    account: solana_sdk::account::Account,
) -> AccountInfo {
    let owner: Pubkey = account.owner.to_bytes();
    make_pinocchio_account_info(key, owner, account.lamports, account.executable, account.data)
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
                let left = &edge.left.mint_account;
                let right = &edge.right.mint_account;
                log_lines.push_str(&format!(
                    "  Edge {}: {:02x}{:02x}{:02x}{:02x}..->[{}]->{:02x}{:02x}{:02x}{:02x}..\n",
                    i + 1,
                    left[0], left[1], left[2], left[3],
                    edge.get_price(),
                    right[0], right[1], right[2], right[3],
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
