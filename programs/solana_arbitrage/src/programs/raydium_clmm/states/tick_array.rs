use anchor_lang::prelude::*;
use bytemuck::{Pod, Zeroable};
use crate::programs::raydium_clmm::libraries::tick_math;

pub const TICK_ARRAY_SIZE_USIZE: usize = 60;
pub const TICK_ARRAY_SIZE: i32 = 60;

/// Number of rewards tokens (same as REWARD_NUM)
pub const REWARD_NUM: usize = 3;

/// TickState represents a single tick in the tick array
#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct TickState {
    pub tick: i32,
    /// Amount of net liquidity added (subtracted) when tick is crossed from left to right (right to left)
    pub liquidity_net: i128,
    /// The total position liquidity that references this tick
    pub liquidity_gross: u128,
    /// Fee growth per unit of liquidity on the _other_ side of this tick (relative to the current tick)
    pub fee_growth_outside_0_x64: u128,
    pub fee_growth_outside_1_x64: u128,
    /// Reward growth per unit of liquidity like fee, array of Q64.64
    pub reward_growths_outside_x64: [u128; REWARD_NUM],
    /// Unused bytes for future upgrades.
    pub padding: [u32; 13],
}

unsafe impl Pod for TickState {}
unsafe impl Zeroable for TickState {}

impl Default for TickState {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl TickState {
    pub const LEN: usize = 4 + 16 + 16 + 16 + 16 + 16 * REWARD_NUM + 4 * 13;

    /// Check if this tick is initialized (has liquidity)
    pub fn is_initialized(&self) -> bool {
        self.liquidity_gross != 0
    }

    /// Check if tick is out of boundary
    pub fn check_is_out_of_boundary(tick: i32) -> bool {
        tick < tick_math::MIN_TICK || tick > tick_math::MAX_TICK
    }
}

/// TickArrayState contains 60 ticks for a price range
#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct TickArrayState {
    pub pool_id: Pubkey,
    pub start_tick_index: i32,
    pub ticks: [TickState; TICK_ARRAY_SIZE_USIZE],
    pub initialized_tick_count: u8,
    /// account update recent epoch
    pub recent_epoch: u64,
    /// Unused bytes for future upgrades.
    pub padding: [u8; 107],
}

unsafe impl Pod for TickArrayState {}
unsafe impl Zeroable for TickArrayState {}

impl Default for TickArrayState {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl TickArrayState {
    pub const LEN: usize = 8 + 32 + 4 + TickState::LEN * TICK_ARRAY_SIZE_USIZE + 1 + 8 + 107;

    /// Get the tick count per tick array for a given tick spacing
    pub fn tick_count(tick_spacing: u16) -> i32 {
        TICK_ARRAY_SIZE * i32::from(tick_spacing)
    }

    /// Get the array start index for a given tick and tick spacing
    pub fn get_array_start_index(tick_index: i32, tick_spacing: u16) -> i32 {
        let ticks_in_array = Self::tick_count(tick_spacing);
        let mut start = tick_index / ticks_in_array;
        if tick_index < 0 && tick_index % ticks_in_array != 0 {
            start = start - 1;
        }
        start * ticks_in_array
    }

    /// Check if this is a valid start index
    pub fn check_is_valid_start_index(tick_index: i32, tick_spacing: u16) -> bool {
        if TickState::check_is_out_of_boundary(tick_index) {
            if tick_index > tick_math::MAX_TICK {
                return false;
            }
            let min_start_index = Self::get_array_start_index(tick_math::MIN_TICK, tick_spacing);
            return tick_index == min_start_index;
        }
        tick_index % Self::tick_count(tick_spacing) == 0
    }

    /// Get the tick state at a specific index within this array
    pub fn get_tick_state(&self, tick_index: i32, tick_spacing: u16) -> Option<&TickState> {
        let offset = self.get_tick_offset_in_array(tick_index, tick_spacing)?;
        Some(&self.ticks[offset])
    }

    /// Get the offset of a tick within this array
    fn get_tick_offset_in_array(&self, tick_index: i32, tick_spacing: u16) -> Option<usize> {
        let start_tick_index = Self::get_array_start_index(tick_index, tick_spacing);
        if start_tick_index != self.start_tick_index {
            return None;
        }
        let offset = ((tick_index - self.start_tick_index) / i32::from(tick_spacing)) as usize;
        if offset >= TICK_ARRAY_SIZE_USIZE {
            return None;
        }
        Some(offset)
    }

    /// Get the first initialized tick in this array based on swap direction
    pub fn first_initialized_tick(&self, zero_for_one: bool) -> Option<&TickState> {
        if zero_for_one {
            // Searching from high to low
            let mut i = TICK_ARRAY_SIZE - 1;
            while i >= 0 {
                if self.ticks[i as usize].is_initialized() {
                    return Some(&self.ticks[i as usize]);
                }
                i -= 1;
            }
        } else {
            // Searching from low to high
            for i in 0..TICK_ARRAY_SIZE_USIZE {
                if self.ticks[i].is_initialized() {
                    return Some(&self.ticks[i]);
                }
            }
        }
        None
    }

    /// Get the next initialized tick in the swap direction
    /// Returns None if no initialized tick is found in this array
    pub fn next_initialized_tick(
        &self,
        current_tick_index: i32,
        tick_spacing: u16,
        zero_for_one: bool,
    ) -> Option<&TickState> {
        let current_tick_array_start_index =
            Self::get_array_start_index(current_tick_index, tick_spacing);
        if current_tick_array_start_index != self.start_tick_index {
            return None;
        }

        let mut offset_in_array =
            (current_tick_index - self.start_tick_index) / i32::from(tick_spacing);

        if zero_for_one {
            // Price decreasing, search from current offset down to 0
            while offset_in_array >= 0 {
                if self.ticks[offset_in_array as usize].is_initialized() {
                    return Some(&self.ticks[offset_in_array as usize]);
                }
                offset_in_array -= 1;
            }
        } else {
            // Price increasing, search from current offset + 1 up
            offset_in_array += 1;
            while offset_in_array < TICK_ARRAY_SIZE {
                if self.ticks[offset_in_array as usize].is_initialized() {
                    return Some(&self.ticks[offset_in_array as usize]);
                }
                offset_in_array += 1;
            }
        }
        None
    }

    /// Get the next tick array start index in the swap direction
    pub fn next_tick_array_start_index(&self, tick_spacing: u16, zero_for_one: bool) -> i32 {
        let ticks_in_array = TICK_ARRAY_SIZE * i32::from(tick_spacing);
        if zero_for_one {
            self.start_tick_index - ticks_in_array
        } else {
            self.start_tick_index + ticks_in_array
        }
    }

    /// Try to parse from account data (skipping 8-byte discriminator).
    /// Returns Box<Self> to avoid putting ~10KB TickArrayState on the stack,
    /// which would cause stack overflow in Solana's 32KB stack limit.
    pub fn try_from_bytes(data: &[u8]) -> Option<Box<Self>> {
        let struct_size = std::mem::size_of::<Self>();
        if data.len() < 8 + struct_size {
            return None;
        }
        unsafe {
            let mut boxed: Box<Self> = Box::new(std::mem::zeroed());
            std::ptr::copy_nonoverlapping(
                data[8..].as_ptr(),
                boxed.as_mut() as *mut Self as *mut u8,
                struct_size,
            );
            Some(boxed)
        }
    }
}

/// Compact tick: only the fields needed for swap calculation.
/// 20 bytes vs 168 bytes for full TickState.
#[derive(Clone, Debug)]
pub struct CompactTick {
    pub tick: i32,
    pub liquidity_net: i128,
}

/// Compact tick array: stores only initialized ticks.
/// Typically ~100-400 bytes vs ~10,232 bytes for full TickArrayState.
#[derive(Clone, Debug)]
pub struct CompactTickArray {
    pub start_tick_index: i32,
    pub ticks: Vec<CompactTick>,
}

impl CompactTickArray {
    // Byte offsets within raw account data
    const DISCRIMINATOR_LEN: usize = 8;
    const POOL_ID_OFFSET: usize = Self::DISCRIMINATOR_LEN;
    const START_TICK_OFFSET: usize = Self::POOL_ID_OFFSET + 32;
    const TICKS_OFFSET: usize = Self::START_TICK_OFFSET + 4;
    const TICK_STATE_SIZE: usize = 168;
    const TICK_COUNT: usize = TICK_ARRAY_SIZE_USIZE;
    const MIN_DATA_LEN: usize = Self::TICKS_OFFSET + Self::TICK_STATE_SIZE * Self::TICK_COUNT;

    // Field offsets within each packed TickState
    const LIQ_NET_OFFSET: usize = 4;    // i128, after tick (i32)
    const LIQ_GROSS_OFFSET: usize = 20; // u128, after liquidity_net (i128)

    /// Parse directly from raw account data without allocating the full ~10KB TickArrayState.
    /// Only extracts initialized ticks (typically 2-10 per array).
    #[inline(never)]
    pub fn try_from_account_data(data: &[u8], expected_pool_id: &Pubkey) -> Option<Self> {
        if data.len() < Self::MIN_DATA_LEN {
            return None;
        }

        // Compare pool_id bytes directly (no Pubkey allocation)
        let expected_bytes = expected_pool_id.to_bytes();
        if data[Self::POOL_ID_OFFSET..Self::POOL_ID_OFFSET + 32] != expected_bytes {
            return None;
        }

        let start_tick_index = i32::from_le_bytes(
            data[Self::START_TICK_OFFSET..Self::START_TICK_OFFSET + 4]
                .try_into()
                .ok()?,
        );

        let mut ticks = Vec::new();
        for i in 0..Self::TICK_COUNT {
            let base = Self::TICKS_OFFSET + i * Self::TICK_STATE_SIZE;

            // Check liquidity_gross != 0 (initialized tick)
            let lg_off = base + Self::LIQ_GROSS_OFFSET;
            let liquidity_gross =
                u128::from_le_bytes(data[lg_off..lg_off + 16].try_into().ok()?);

            if liquidity_gross != 0 {
                let tick =
                    i32::from_le_bytes(data[base..base + 4].try_into().ok()?);
                let ln_off = base + Self::LIQ_NET_OFFSET;
                let liquidity_net =
                    i128::from_le_bytes(data[ln_off..ln_off + 16].try_into().ok()?);
                ticks.push(CompactTick { tick, liquidity_net });
            }
        }

        Some(CompactTickArray { start_tick_index, ticks })
    }

    /// Find next initialized tick from current position in swap direction.
    pub fn next_initialized_tick(
        &self,
        current_tick_index: i32,
        tick_spacing: u16,
        zero_for_one: bool,
    ) -> Option<&CompactTick> {
        // Verify current tick belongs to this array
        let current_start =
            TickArrayState::get_array_start_index(current_tick_index, tick_spacing);
        if current_start != self.start_tick_index {
            return None;
        }

        if zero_for_one {
            // Price decreasing - highest initialized tick <= current_tick_index
            self.ticks.iter().rev().find(|t| t.tick <= current_tick_index)
        } else {
            // Price increasing - lowest initialized tick > current_tick_index
            self.ticks.iter().find(|t| t.tick > current_tick_index)
        }
    }

    /// Get the first initialized tick in this array for the given swap direction.
    pub fn first_initialized_tick(&self, zero_for_one: bool) -> Option<&CompactTick> {
        if zero_for_one {
            self.ticks.last()  // highest tick
        } else {
            self.ticks.first() // lowest tick
        }
    }
}
