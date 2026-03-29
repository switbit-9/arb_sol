use crate::compat::*;
use bytemuck::{Pod, Zeroable};

/// Number of reward tokens supported
pub const NUM_REWARDS: usize = 3;

/// Whirlpool account discriminator
pub const WHIRLPOOL_DISCRIMINATOR: [u8; 8] = [63, 149, 209, 12, 225, 128, 99, 9];

/// Simplified WhirlpoolRewardInfo for bytemuck compatibility
#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct WhirlpoolRewardInfoSimple {
    /// Reward token mint
    pub mint: Pubkey,
    /// Reward token vault
    pub vault: Pubkey,
    /// Authority for the reward
    pub authority: Pubkey,
    /// Q64.64 reward emissions per second
    pub emissions_per_second_x64: u128,
    /// Q64.64 accumulated reward per unit of liquidity
    pub growth_global_x64: u128,
}

unsafe impl Pod for WhirlpoolRewardInfoSimple {}
unsafe impl Zeroable for WhirlpoolRewardInfoSimple {}

impl Default for WhirlpoolRewardInfoSimple {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl WhirlpoolRewardInfoSimple {
    pub const LEN: usize = 32 + 32 + 32 + 16 + 16; // 128 bytes
}

/// Simplified Whirlpool state for bytemuck zero-copy deserialization
/// This contains only the fields needed for swap calculations
#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct WhirlpoolSimple {
    /// Whirlpool config account
    pub whirlpools_config: Pubkey,
    /// Bump seed
    pub whirlpool_bump: [u8; 1],
    /// Tick spacing (determines price granularity)
    pub tick_spacing: u16,
    /// Fee tier index seed
    pub tick_spacing_seed: [u8; 2],
    /// Fee rate in hundredths of a basis point (max 60,000 = 6%)
    pub fee_rate: u16,
    /// Protocol fee rate in basis points (max 2,500 = 25% of fee)
    pub protocol_fee_rate: u16,
    /// Currently active liquidity in the pool
    pub liquidity: u128,
    /// Current sqrt price as Q64.64
    pub sqrt_price: u128,
    /// Current tick index
    pub tick_current_index: i32,
    /// Protocol fee owed for token A
    pub protocol_fee_owed_a: u64,
    /// Protocol fee owed for token B
    pub protocol_fee_owed_b: u64,
    /// Token A mint
    pub token_mint_a: Pubkey,
    /// Token A vault
    pub token_vault_a: Pubkey,
    /// Q64.64 fee growth for token A
    pub fee_growth_global_a: u128,
    /// Token B mint
    pub token_mint_b: Pubkey,
    /// Token B vault
    pub token_vault_b: Pubkey,
    /// Q64.64 fee growth for token B
    pub fee_growth_global_b: u128,
    /// Timestamp of last reward update
    pub reward_last_updated_timestamp: u64,
    /// Reward infos (3 rewards)
    pub reward_infos: [WhirlpoolRewardInfoSimple; NUM_REWARDS],
}

unsafe impl Pod for WhirlpoolSimple {}
unsafe impl Zeroable for WhirlpoolSimple {}

impl Default for WhirlpoolSimple {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl WhirlpoolSimple {
    /// Size of WhirlpoolSimple (without 8-byte discriminator)
    pub const LEN: usize = 32          // whirlpools_config
        + 1                            // whirlpool_bump
        + 2                            // tick_spacing
        + 2                            // tick_spacing_seed
        + 2                            // fee_rate
        + 2                            // protocol_fee_rate
        + 16                           // liquidity
        + 16                           // sqrt_price
        + 4                            // tick_current_index
        + 8                            // protocol_fee_owed_a
        + 8                            // protocol_fee_owed_b
        + 32                           // token_mint_a
        + 32                           // token_vault_a
        + 16                           // fee_growth_global_a
        + 32                           // token_mint_b
        + 32                           // token_vault_b
        + 16                           // fee_growth_global_b
        + 8                            // reward_last_updated_timestamp
        + WhirlpoolRewardInfoSimple::LEN * NUM_REWARDS; // 128 * 3 = 384

    /// Parse whirlpool from account data (includes 8-byte discriminator)
    pub fn try_from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() < 8 + Self::LEN {
            return Err(solar_error!(crate::programs::SolarBError::InsufficientAccounts));
        }

        // Verify discriminator
        let discriminator = &data[0..8];
        if discriminator != WHIRLPOOL_DISCRIMINATOR {
            return Err(solar_error!(crate::programs::SolarBError::AccountMismatch));
        }

        // Parse pool state (after discriminator)
        let pool: WhirlpoolSimple = bytemuck::pod_read_unaligned(&data[8..8 + Self::LEN]);
        Ok(pool)
    }

    /// Get input token mint based on swap direction
    pub fn input_token_mint(&self, a_to_b: bool) -> Pubkey {
        if a_to_b {
            self.token_mint_a
        } else {
            self.token_mint_b
        }
    }

    /// Get output token vault based on swap direction
    pub fn output_token_vault(&self, a_to_b: bool) -> Pubkey {
        if a_to_b {
            self.token_vault_b
        } else {
            self.token_vault_a
        }
    }
}
