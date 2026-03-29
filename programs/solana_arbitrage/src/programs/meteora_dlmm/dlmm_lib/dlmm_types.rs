use crate::compat::Pubkey;

// ── Plain types (not accounts) ──────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rounding {
    Up,
    Down,
}

#[derive(Clone, Copy, Debug)]
pub enum PairStatus {
    Enabled,
    Disabled,
}

#[derive(Clone, Copy, Debug)]
pub enum PairType {
    Permissionless,
    Permission,
    CustomizablePermissionless,
    PermissionlessV2,
}

#[derive(Clone, Copy, Debug)]
pub enum ActivationType {
    Slot,
    Timestamp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenProgramFlags {
    TokenProgram,
    TokenProgram2022,
}

// ── Zero-copy / bytemuck structs ────────────────────────────────────────────
// Field order and sizes match the on-chain IDL exactly.
// All repr(C) structs are laid out for direct casting from account data
// (after skipping the 8-byte Anchor discriminator).

/// Bin — 128 bytes, repr(C)
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Bin {
    /// Amount of token X in the bin (excluding protocol fees).
    pub amount_x: u64,
    /// Amount of token Y in the bin (excluding protocol fees).
    pub amount_y: u64,
    /// Bin price.
    pub price: u128,
    /// Liquidities of the bin (same as LP mint supply, q-number).
    pub liquidity_supply: u128,
    /// reward_per_token_stored — [u128; 2]
    pub reward_per_token_stored: [u128; 2],
    /// Swap fee amount of token X per liquidity deposited.
    pub fee_amount_x_per_token_stored: u128,
    /// Swap fee amount of token Y per liquidity deposited.
    pub fee_amount_y_per_token_stored: u128,
    /// Total token X swap into the bin (tracking only).
    pub amount_x_in: u128,
    /// Total token Y swap into the bin (tracking only).
    pub amount_y_in: u128,
}

// Safety: Bin is repr(C) and all fields are plain numeric types.
unsafe impl bytemuck::Zeroable for Bin {}
unsafe impl bytemuck::Pod for Bin {}

/// StaticParameters — repr(C), bytemuck
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct StaticParameters {
    pub base_factor: u16,
    pub filter_period: u16,
    pub decay_period: u16,
    pub reduction_factor: u16,
    pub variable_fee_control: u32,
    pub max_volatility_accumulator: u32,
    pub min_bin_id: i32,
    pub max_bin_id: i32,
    pub protocol_share: u16,
    pub base_fee_power_factor: u8,
    pub _padding: [u8; 5],
}

unsafe impl bytemuck::Zeroable for StaticParameters {}
unsafe impl bytemuck::Pod for StaticParameters {}

/// VariableParameters — repr(C), bytemuck
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VariableParameters {
    pub volatility_accumulator: u32,
    pub volatility_reference: u32,
    pub index_reference: i32,
    pub _padding: [u8; 4],
    pub last_update_timestamp: i64,
    pub _padding_1: [u8; 8],
}

unsafe impl bytemuck::Zeroable for VariableParameters {}
unsafe impl bytemuck::Pod for VariableParameters {}

/// ProtocolFee — repr(C), bytemuck
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ProtocolFee {
    pub amount_x: u64,
    pub amount_y: u64,
}

unsafe impl bytemuck::Zeroable for ProtocolFee {}
unsafe impl bytemuck::Pod for ProtocolFee {}

/// RewardInfo — repr(C), bytemuck
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct RewardInfo {
    pub mint: Pubkey,
    pub vault: Pubkey,
    pub funder: Pubkey,
    pub reward_duration: u64,
    pub reward_duration_end: u64,
    pub reward_rate: u128,
    pub last_update_time: u64,
    pub cumulative_seconds_with_empty_liquidity_reward: u64,
}

unsafe impl bytemuck::Zeroable for RewardInfo {}
unsafe impl bytemuck::Pod for RewardInfo {}

// ── On-chain account structs (8-byte discriminator prefix) ──────────────────

/// LbPair account data (after 8-byte discriminator).
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct LbPair {
    pub parameters: StaticParameters,
    pub v_parameters: VariableParameters,
    pub bump_seed: [u8; 1],
    pub bin_step_seed: [u8; 2],
    pub pair_type: u8,
    pub active_id: i32,
    pub bin_step: u16,
    pub status: u8,
    pub require_base_factor_seed: u8,
    pub base_factor_seed: [u8; 2],
    pub activation_type: u8,
    pub creator_pool_on_off_control: u8,
    pub token_x_mint: Pubkey,
    pub token_y_mint: Pubkey,
    pub reserve_x: Pubkey,
    pub reserve_y: Pubkey,
    pub protocol_fee: ProtocolFee,
    pub _padding_1: [u8; 32],
    pub reward_infos: [RewardInfo; 2],
    pub oracle: Pubkey,
    pub bin_array_bitmap: [u64; 16],
    pub last_updated_at: i64,
    pub _padding_2: [u8; 32],
    pub pre_activation_swap_address: Pubkey,
    pub base_key: Pubkey,
    pub activation_point: u64,
    pub pre_activation_duration: u64,
    pub _padding_3: [u8; 8],
    pub _padding_4: u64,
    pub creator: Pubkey,
    pub token_mint_x_program_flag: u8,
    pub token_mint_y_program_flag: u8,
    pub _reserved: [u8; 22],
}

unsafe impl bytemuck::Zeroable for LbPair {}
unsafe impl bytemuck::Pod for LbPair {}

/// BinArray account data (after 8-byte discriminator).
/// Contains 70 bins.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct BinArray {
    pub index: i64,
    pub version: u8,
    pub _padding: [u8; 7],
    pub lb_pair: Pubkey,
    pub bins: [Bin; 70],
}

unsafe impl bytemuck::Zeroable for BinArray {}
unsafe impl bytemuck::Pod for BinArray {}

/// BinArrayBitmapExtension account data (after 8-byte discriminator).
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct BinArrayBitmapExtension {
    pub lb_pair: Pubkey,
    pub positive_bin_array_bitmap: [[u64; 8]; 12],
    pub negative_bin_array_bitmap: [[u64; 8]; 12],
}

unsafe impl bytemuck::Zeroable for BinArrayBitmapExtension {}
unsafe impl bytemuck::Pod for BinArrayBitmapExtension {}

// ── Discriminators (first 8 bytes of each account) ──────────────────────────

impl LbPair {
    pub const DISCRIMINATOR: [u8; 8] = [33, 11, 49, 98, 181, 101, 177, 13];
}

impl BinArray {
    pub const DISCRIMINATOR: [u8; 8] = [92, 142, 92, 220, 5, 148, 70, 181];
}

impl BinArrayBitmapExtension {
    pub const DISCRIMINATOR: [u8; 8] = [80, 111, 124, 113, 55, 237, 18, 5];
}
