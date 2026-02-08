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

    /// Try to parse from account data (skipping 8-byte discriminator)
    pub fn try_from_bytes(data: &[u8]) -> Option<Self> {
        let struct_size = std::mem::size_of::<Self>();
        if data.len() < 8 + struct_size {
            return None;
        }
        Some(bytemuck::pod_read_unaligned(&data[8..8 + struct_size]))
    }
}
