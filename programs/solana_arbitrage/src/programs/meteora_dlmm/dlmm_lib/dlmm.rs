/// Manual type definitions replacing `declare_program!(dlmm)`.
/// All structs that are read with `bytemuck::pod_read_unaligned` are
/// `#[repr(C)]` + `bytemuck::Pod + bytemuck::Zeroable`.
use pinocchio::pubkey::Pubkey;

// ─── nested types ──────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
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

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct VariableParameters {
    pub volatility_accumulator: u32,
    pub volatility_reference: u32,
    pub index_reference: i32,
    pub _padding: [u8; 4],
    pub last_update_timestamp: i64,
    pub _padding_1: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ProtocolFee {
    pub amount_x: u64,
    pub amount_y: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
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

// ─── accounts ──────────────────────────────────────────────────────────────────

pub mod accounts {
    use super::*;

    /// On-chain LbPair account (896 bytes after 8-byte discriminator).
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
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

    /// On-chain BinArray account (10128 bytes after 8-byte discriminator).
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    pub struct BinArray {
        pub index: i64,
        pub version: u8,
        pub _padding: [u8; 7],
        pub lb_pair: Pubkey,
        pub bins: [super::types::Bin; 70],
    }

    /// On-chain BinArrayBitmapExtension account (1568 bytes after 8-byte discriminator).
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
    pub struct BinArrayBitmapExtension {
        pub lb_pair: Pubkey,
        pub positive_bin_array_bitmap: [[u64; 8]; 12],
        pub negative_bin_array_bitmap: [[u64; 8]; 12],
    }
}

// ─── types ─────────────────────────────────────────────────────────────────────

pub mod types {
    /// A single DLMM bin (144 bytes, repr(C)).
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
    pub struct Bin {
        pub amount_x: u64,
        pub amount_y: u64,
        pub price: u128,
        pub liquidity_supply: u128,
        pub reward_per_token_stored: [u128; 2],
        pub fee_amount_x_per_token_stored: u128,
        pub fee_amount_y_per_token_stored: u128,
        pub amount_x_in: u128,
        pub amount_y_in: u128,
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum Rounding {
        Up,
        Down,
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum ActivationType {
        Slot,
        Timestamp,
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

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum TokenProgramFlags {
        TokenProgram,
        TokenProgram2022,
    }
}
