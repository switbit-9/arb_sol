/// Q64 resolution (2^64)
pub const Q64: u128 = 1u128 << 64;

/// Q64 mask for fractional part
pub const Q64_MASK: u128 = Q64 - 1;

/// Number of bits in Q64 resolution
pub const Q64_RESOLUTION: u8 = 64;
