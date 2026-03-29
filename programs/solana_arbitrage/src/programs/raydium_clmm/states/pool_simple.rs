use crate::compat::*;
use bytemuck::{Pod, Zeroable};

/// Number of rewards Token
pub const REWARD_NUM: usize = 3;

/// Simplified RewardInfo for bytemuck compatibility
#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct RewardInfoSimple {
    pub reward_state: u8,
    pub open_time: u64,
    pub end_time: u64,
    pub last_update_time: u64,
    pub emissions_per_second_x64: u128,
    pub reward_total_emissioned: u64,
    pub reward_claimed: u64,
    pub token_mint: Pubkey,
    pub token_vault: Pubkey,
    pub authority: Pubkey,
    pub reward_growth_global_x64: u128,
}

unsafe impl Pod for RewardInfoSimple {}
unsafe impl Zeroable for RewardInfoSimple {}

impl Default for RewardInfoSimple {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl RewardInfoSimple {
    pub const LEN: usize = 1 + 8 + 8 + 8 + 16 + 8 + 8 + 32 + 32 + 32 + 16;
}

/// Simplified PoolState for bytemuck zero-copy deserialization
/// PDA of `[POOL_SEED, config, token_mint_0, token_mint_1]`
#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct PoolStateSimple {
    /// Bump to identify PDA
    pub bump: [u8; 1],
    /// Which config the pool belongs to
    pub amm_config: Pubkey,
    /// Pool creator
    pub owner: Pubkey,

    /// Token pair of the pool, where token_mint_0 address < token_mint_1 address
    pub token_mint_0: Pubkey,
    pub token_mint_1: Pubkey,

    /// Token pair vault
    pub token_vault_0: Pubkey,
    pub token_vault_1: Pubkey,

    /// observation account key
    pub observation_key: Pubkey,

    /// mint0 and mint1 decimals
    pub mint_decimals_0: u8,
    pub mint_decimals_1: u8,

    /// The minimum number of ticks between initialized ticks
    pub tick_spacing: u16,
    /// The currently in range liquidity available to the pool.
    pub liquidity: u128,
    /// The current price of the pool as a sqrt(token_1/token_0) Q64.64 value
    pub sqrt_price_x64: u128,
    /// The current tick of the pool, i.e. according to the last tick transition that was run.
    pub tick_current: i32,

    pub padding3: u16,
    pub padding4: u16,

    /// The fee growth as a Q64.64 number, i.e. fees of token_0 and token_1 collected per
    /// unit of liquidity for the entire life of the pool.
    pub fee_growth_global_0_x64: u128,
    pub fee_growth_global_1_x64: u128,

    /// The amounts of token_0 and token_1 that are owed to the protocol.
    pub protocol_fees_token_0: u64,
    pub protocol_fees_token_1: u64,

    /// The amounts in and out of swap token_0 and token_1
    pub swap_in_amount_token_0: u128,
    pub swap_out_amount_token_1: u128,
    pub swap_in_amount_token_1: u128,
    pub swap_out_amount_token_0: u128,

    /// Bitwise representation of the state of the pool
    /// bit0, 1: disable open position and increase liquidity, 0: normal
    /// bit1, 1: disable decrease liquidity, 0: normal
    /// bit2, 1: disable collect fee, 0: normal
    /// bit3, 1: disable collect reward, 0: normal
    /// bit4, 1: disable swap, 0: normal
    pub status: u8,
    /// Leave blank for future use
    pub padding: [u8; 7],

    pub reward_infos: [RewardInfoSimple; REWARD_NUM],

    /// Packed initialized tick array state
    pub tick_array_bitmap: [u64; 16],

    /// except protocol_fee and fund_fee
    pub total_fees_token_0: u64,
    /// except protocol_fee and fund_fee
    pub total_fees_claimed_token_0: u64,
    pub total_fees_token_1: u64,
    pub total_fees_claimed_token_1: u64,

    pub fund_fees_token_0: u64,
    pub fund_fees_token_1: u64,

    /// The timestamp allowed for swap in the pool.
    pub open_time: u64,
    /// account recent update epoch
    pub recent_epoch: u64,

    /// Unused bytes for future upgrades.
    pub padding1: [u64; 24],
    pub padding2: [u64; 32],
}

unsafe impl Pod for PoolStateSimple {}
unsafe impl Zeroable for PoolStateSimple {}

impl Default for PoolStateSimple {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl PoolStateSimple {
    /// Size of PoolState without discriminator
    pub const LEN: usize = 1           // bump
        + 32 * 7                        // amm_config, owner, token_mint_0, token_mint_1, token_vault_0, token_vault_1, observation_key
        + 1                             // mint_decimals_0
        + 1                             // mint_decimals_1
        + 2                             // tick_spacing
        + 16                            // liquidity
        + 16                            // sqrt_price_x64
        + 4                             // tick_current
        + 2                             // padding3
        + 2                             // padding4
        + 16                            // fee_growth_global_0_x64
        + 16                            // fee_growth_global_1_x64
        + 8                             // protocol_fees_token_0
        + 8                             // protocol_fees_token_1
        + 16                            // swap_in_amount_token_0
        + 16                            // swap_out_amount_token_1
        + 16                            // swap_in_amount_token_1
        + 16                            // swap_out_amount_token_0
        + 1                             // status
        + 7                             // padding
        + RewardInfoSimple::LEN * REWARD_NUM  // reward_infos (169 * 3 = 507)
        + 8 * 16                        // tick_array_bitmap (128)
        + 8                             // total_fees_token_0
        + 8                             // total_fees_claimed_token_0
        + 8                             // total_fees_token_1
        + 8                             // total_fees_claimed_token_1
        + 8                             // fund_fees_token_0
        + 8                             // fund_fees_token_1
        + 8                             // open_time
        + 8                             // recent_epoch
        + 8 * 24                        // padding1 (192)
        + 8 * 32;                       // padding2 (256)
    // Total: 1536 bytes

    /// Check if swap is enabled
    pub fn swap_enabled(&self) -> bool {
        // bit4: 1 = disable swap, 0 = normal
        (self.status & (1 << 4)) == 0
    }
}
