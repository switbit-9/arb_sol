/// DEX type IDs — must match client-side DEX_TYPE_ID mapping
pub const METEORA_DAMM_V1: u8 = 1;
pub const METEORA_DAMM_V2: u8 = 2;
pub const METEORA_DLMM: u8 = 3;
pub const WHIRLPOOL: u8 = 4;
pub const RAYDIUM_AMM: u8 = 5;
pub const RAYDIUM_CLMM: u8 = 6;
pub const RAYDIUM_CPMM: u8 = 7;
pub const METEORA_DBC: u8 = 8;
pub const PUMP_AMM: u8 = 9;

/// Number of fee slots consumed from pool_fees Vec per pool type.
#[inline(always)]
pub const fn fee_slot_count(pool_type: u8) -> usize {
    match pool_type {
        PUMP_AMM => 1,
        _ => 0,
    }
}

/// Fixed number of dynamic accounts per pool type.
#[inline(always)]
pub fn dynamic_account_count(pool_type: u8) -> usize {
    match pool_type {
        PUMP_AMM => crate::programs::pump_amm::DYNAMIC_ACCOUNTS,
        METEORA_DAMM_V1 | METEORA_DBC => crate::programs::meteora_damm_v1::MeteoraDammV1::DYNAMIC_ACCOUNTS,
        METEORA_DAMM_V2 => crate::programs::meteora_damm_v2::DYNAMIC_ACCOUNTS,
        METEORA_DLMM => crate::programs::meteora_dlmm::DYNAMIC_ACCOUNTS,
        WHIRLPOOL => crate::programs::orca::DYNAMIC_ACCOUNTS,
        RAYDIUM_AMM => crate::programs::raydium_amm::DYNAMIC_ACCOUNTS,
        RAYDIUM_CLMM => crate::programs::raydium_clmm::DYNAMIC_ACCOUNTS,
        RAYDIUM_CPMM => crate::programs::raydium_cpmm::DYNAMIC_ACCOUNTS,
        _ => 0,
    }
}
