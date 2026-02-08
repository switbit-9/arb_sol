use anchor_lang::prelude::*;
use bytemuck::{Pod, Zeroable};

pub const FEE_RATE_DENOMINATOR_VALUE: u32 = 1_000_000;

/// Simplified AmmConfig for bytemuck zero-copy deserialization
#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct AmmConfigSimple {
    /// Bump to identify PDA
    pub bump: u8,
    pub index: u16,
    /// Address of the protocol owner
    pub owner: Pubkey,
    /// The protocol fee
    pub protocol_fee_rate: u32,
    /// The trade fee, denominated in hundredths of a bip (10^-6)
    pub trade_fee_rate: u32,
    /// The tick spacing
    pub tick_spacing: u16,
    /// The fund fee, denominated in hundredths of a bip (10^-6)
    pub fund_fee_rate: u32,
    /// padding space for upgrade
    pub padding_u32: u32,
    pub fund_owner: Pubkey,
    pub padding: [u64; 3],
}

unsafe impl Pod for AmmConfigSimple {}
unsafe impl Zeroable for AmmConfigSimple {}

impl Default for AmmConfigSimple {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl AmmConfigSimple {
    /// Size without discriminator: 1 + 2 + 32 + 4 + 4 + 2 + 4 + 4 + 32 + 24 = 109 bytes
    pub const LEN: usize = 1 + 2 + 32 + 4 + 4 + 2 + 4 + 4 + 32 + 24;

    pub fn try_from_bytes(data: &[u8]) -> Result<Self> {
        let struct_size = std::mem::size_of::<Self>();
        if data.len() < 8 + struct_size {
            return Err(anchor_lang::error::ErrorCode::AccountDidNotDeserialize.into());
        }
        Ok(bytemuck::pod_read_unaligned(&data[8..8 + struct_size]))
    }
}
