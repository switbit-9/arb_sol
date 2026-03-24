use pinocchio::pubkey::Pubkey;

pub const AMM_INFO_SIZE: usize = 752;

// Byte offsets into the 752-byte AmmInfo account (no Anchor discriminator)
const NONCE_OFFSET: usize = 8;
const COIN_DECIMALS_OFFSET: usize = 32;
const PC_DECIMALS_OFFSET: usize = 40;
// Fees block starts at offset 128 (after 16 u64 fields)
const SWAP_FEE_NUMERATOR_OFFSET: usize = 176; // 128 + 6*8
const SWAP_FEE_DENOMINATOR_OFFSET: usize = 184; // 128 + 7*8
// StateData block starts at offset 192 (128 + 64)
const NEED_TAKE_PNL_COIN_OFFSET: usize = 192;
const NEED_TAKE_PNL_PC_OFFSET: usize = 200;
// Pubkeys after StateData (192 + 144 = 336)
const COIN_VAULT_MINT_OFFSET: usize = 400; // 336 + 2*32
const PC_VAULT_MINT_OFFSET: usize = 432;   // 336 + 3*32
const OPEN_ORDERS_OFFSET: usize = 496;     // 336 + 5*32

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

fn read_pubkey(data: &[u8], offset: usize) -> Pubkey {
    Pubkey::try_from(&data[offset..offset + 32]).unwrap()
}

pub struct AmmInfoFields {
    pub nonce: u8,
    pub coin_decimals: u64,
    pub pc_decimals: u64,
    pub swap_fee_numerator: u64,
    pub swap_fee_denominator: u64,
    pub need_take_pnl_coin: u64,
    pub need_take_pnl_pc: u64,
    pub coin_vault_mint: Pubkey,
    pub pc_vault_mint: Pubkey,
    pub open_orders: Pubkey,
}

impl AmmInfoFields {
    pub fn from_bytes(data: &[u8]) -> Self {
        assert!(data.len() >= AMM_INFO_SIZE, "Account data too short for AmmInfo");
        AmmInfoFields {
            nonce: read_u64(data, NONCE_OFFSET) as u8,
            coin_decimals: read_u64(data, COIN_DECIMALS_OFFSET),
            pc_decimals: read_u64(data, PC_DECIMALS_OFFSET),
            swap_fee_numerator: read_u64(data, SWAP_FEE_NUMERATOR_OFFSET),
            swap_fee_denominator: read_u64(data, SWAP_FEE_DENOMINATOR_OFFSET),
            need_take_pnl_coin: read_u64(data, NEED_TAKE_PNL_COIN_OFFSET),
            need_take_pnl_pc: read_u64(data, NEED_TAKE_PNL_PC_OFFSET),
            coin_vault_mint: read_pubkey(data, COIN_VAULT_MINT_OFFSET),
            pc_vault_mint: read_pubkey(data, PC_VAULT_MINT_OFFSET),
            open_orders: read_pubkey(data, OPEN_ORDERS_OFFSET),
        }
    }
}
