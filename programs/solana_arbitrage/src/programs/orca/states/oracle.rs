use bytemuck::{Pod, Zeroable};

/// Oracle account discriminator: SHA256("account:Oracle")[0..8]
pub const ORACLE_DISCRIMINATOR: [u8; 8] = [0x8b, 0xc2, 0x83, 0xb3, 0x8c, 0xb3, 0xe5, 0xf4];

pub const VOLATILITY_ACCUMULATOR_SCALE_FACTOR: u64 = 10_000;
pub const ADAPTIVE_FEE_CONTROL_FACTOR_DENOMINATOR: u64 = 100_000;
/// Maximum total fee rate (static + adaptive): 10%
pub const FEE_RATE_HARD_LIMIT: u32 = 100_000;

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct AdaptiveFeeConstants {
    pub filter_period: u16,
    pub decay_period: u16,
    pub reduction_factor: u16,
    pub adaptive_fee_control_factor: u32,
    pub max_volatility_accumulator: u32,
    pub tick_group_size: u16,
    pub major_swap_threshold_ticks: u16,
    pub reserved: [u8; 16],
}

unsafe impl Pod for AdaptiveFeeConstants {}
unsafe impl Zeroable for AdaptiveFeeConstants {}

impl AdaptiveFeeConstants {
    pub const LEN: usize = 2 + 2 + 2 + 4 + 4 + 2 + 2 + 16; // 34 bytes
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct AdaptiveFeeVariables {
    pub last_reference_update_timestamp: u64,
    pub last_major_swap_timestamp: u64,
    pub volatility_reference: u32,
    pub tick_group_index_reference: i32,
    pub volatility_accumulator: u32,
    pub reserved: [u8; 16],
}

unsafe impl Pod for AdaptiveFeeVariables {}
unsafe impl Zeroable for AdaptiveFeeVariables {}

impl AdaptiveFeeVariables {
    pub const LEN: usize = 8 + 8 + 4 + 4 + 4 + 16; // 44 bytes
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct OracleSimple {
    pub whirlpool: [u8; 32],
    pub trade_enable_timestamp: u64,
    pub adaptive_fee_constants: AdaptiveFeeConstants,
    pub adaptive_fee_variables: AdaptiveFeeVariables,
    // 128 bytes reserved omitted — we only parse what we need
}

unsafe impl Pod for OracleSimple {}
unsafe impl Zeroable for OracleSimple {}

impl OracleSimple {
    /// Size of fields we parse (without discriminator, without trailing reserved)
    pub const LEN: usize = 32 + 8 + AdaptiveFeeConstants::LEN + AdaptiveFeeVariables::LEN;

    /// Parse Oracle from account data (includes 8-byte discriminator)
    pub fn try_from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 8 + Self::LEN {
            return None;
        }
        // Verify discriminator
        if data[0..8] != ORACLE_DISCRIMINATOR {
            return None;
        }
        Some(bytemuck::pod_read_unaligned(&data[8..8 + Self::LEN]))
    }

    /// Compute the adaptive fee rate from oracle state.
    /// Returns 0 if adaptive fees are disabled (control_factor == 0).
    /// Formula: ceil(control_factor * (volatility_accumulator * tick_group_size)^2
    ///              / (ADAPTIVE_FEE_CONTROL_FACTOR_DENOMINATOR * VOLATILITY_ACCUMULATOR_SCALE_FACTOR^2))
    pub fn compute_adaptive_fee_rate(&self) -> u32 {
        let control_factor = self.adaptive_fee_constants.adaptive_fee_control_factor;
        if control_factor == 0 {
            return 0;
        }

        let volatility_accumulator = self.adaptive_fee_variables.volatility_accumulator;
        let tick_group_size = self.adaptive_fee_constants.tick_group_size as u32;

        let crossed = volatility_accumulator.saturating_mul(tick_group_size);
        let squared = crossed as u128 * crossed as u128;

        let denominator = ADAPTIVE_FEE_CONTROL_FACTOR_DENOMINATOR as u128
            * VOLATILITY_ACCUMULATOR_SCALE_FACTOR as u128
            * VOLATILITY_ACCUMULATOR_SCALE_FACTOR as u128;

        let numerator = control_factor as u128 * squared;

        // Ceiling division
        let fee_rate = if denominator == 0 {
            0u128
        } else {
            (numerator + denominator - 1) / denominator
        };

        if fee_rate > FEE_RATE_HARD_LIMIT as u128 {
            FEE_RATE_HARD_LIMIT
        } else {
            fee_rate as u32
        }
    }
}
