use bytemuck::{Pod, Zeroable};

use super::NUM_REWARDS;

/// Tick array size (number of ticks per array)
pub const TICK_ARRAY_SIZE: i32 = 88;
pub const TICK_ARRAY_SIZE_USIZE: usize = 88;

/// Max and min tick indices
pub const MAX_TICK_INDEX: i32 = 443636;
pub const MIN_TICK_INDEX: i32 = -443636;

/// Tick structure - represents liquidity at a specific price point
#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct Tick {
    /// Whether this tick has been initialized
    pub initialized: bool,
    /// Net liquidity change when crossing this tick
    pub liquidity_net: i128,
    /// Gross liquidity referenced by this tick
    pub liquidity_gross: u128,
    /// Q64.64 fee growth for token A outside this tick
    pub fee_growth_outside_a: u128,
    /// Q64.64 fee growth for token B outside this tick
    pub fee_growth_outside_b: u128,
    /// Q64.64 reward growths outside this tick
    pub reward_growths_outside: [u128; NUM_REWARDS],
}

unsafe impl Pod for Tick {}
unsafe impl Zeroable for Tick {}

impl Default for Tick {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl Tick {
    pub const LEN: usize = 1 + 16 + 16 + 16 + 16 + (16 * NUM_REWARDS); // 113 bytes

    /// Check if tick index is out of bounds
    pub fn check_is_out_of_bounds(tick_index: i32) -> bool {
        tick_index < MIN_TICK_INDEX || tick_index > MAX_TICK_INDEX
    }

    /// Check if tick is usable with given tick spacing
    pub fn check_is_usable_tick(tick_index: i32, tick_spacing: u16) -> bool {
        if Tick::check_is_out_of_bounds(tick_index) {
            return false;
        }
        tick_index % tick_spacing as i32 == 0
    }

    /// Bound tick index to valid range
    pub fn bound_tick_index(tick_index: i32) -> i32 {
        tick_index.clamp(MIN_TICK_INDEX, MAX_TICK_INDEX)
    }

    /// Get full range tick indices for given tick spacing
    pub fn full_range_indexes(tick_spacing: u16) -> (i32, i32) {
        let lower_index = MIN_TICK_INDEX / tick_spacing as i32 * tick_spacing as i32;
        let upper_index = MAX_TICK_INDEX / tick_spacing as i32 * tick_spacing as i32;
        (lower_index, upper_index)
    }
}

/// TickArray account discriminator
pub const TICK_ARRAY_DISCRIMINATOR: [u8; 8] = [69, 97, 189, 190, 110, 7, 66, 187];

/// Tick array header (before the actual ticks).
///
/// On-chain Orca TickArray layout (after 8-byte discriminator):
///   start_tick_index (4 bytes) | ticks (88 * 113 bytes) | whirlpool (32 bytes)
///
/// Only start_tick_index comes before the tick data — whirlpool is at the END.
#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct TickArrayHeader {
    /// Start tick index for this array
    pub start_tick_index: i32,
}

unsafe impl Pod for TickArrayHeader {}
unsafe impl Zeroable for TickArrayHeader {}

impl TickArrayHeader {
    pub const LEN: usize = 4; // only start_tick_index before tick data
}

/// Simplified tick array for reading on-chain
/// Note: We read ticks lazily to avoid blowing up the heap
pub struct TickArraySimple<'a> {
    /// Start tick index
    pub start_tick_index: i32,
    /// Reference to the raw tick data (after header)
    pub data: &'a [u8],
}

impl<'a> TickArraySimple<'a> {
    /// Parse tick array header and get reference to tick data
    /// Does NOT load all ticks into memory - lazy loading
    pub fn try_from_bytes(data: &'a [u8]) -> Option<Self> {
        // Account: 8 (discriminator) + 4 (start_tick_index) + ticks + 32 (whirlpool at end)
        if data.len() < 8 + TickArrayHeader::LEN + Tick::LEN {
            return None;
        }

        // Verify discriminator
        if &data[0..8] != &TICK_ARRAY_DISCRIMINATOR {
            return None;
        }

        // Read start tick index
        let start_tick_index = i32::from_le_bytes(data[8..12].try_into().ok()?);

        // Get reference to tick data (after header)
        let tick_data_start = 8 + TickArrayHeader::LEN;
        let tick_data = &data[tick_data_start..];

        Some(TickArraySimple {
            start_tick_index,
            data: tick_data,
        })
    }

    /// Get a specific tick by its index within the array (0-87)
    /// Returns None if index is out of bounds
    pub fn get_tick(&self, index: usize) -> Option<Tick> {
        if index >= TICK_ARRAY_SIZE_USIZE {
            return None;
        }

        let offset = index * Tick::LEN;
        if offset + Tick::LEN > self.data.len() {
            return None;
        }

        let tick: Tick = bytemuck::pod_read_unaligned(&self.data[offset..offset + Tick::LEN]);
        Some(tick)
    }

    /// Get tick by tick index (converts to array offset)
    pub fn get_tick_by_index(&self, tick_index: i32, tick_spacing: u16) -> Option<Tick> {
        let offset = self.tick_offset(tick_index, tick_spacing)?;
        self.get_tick(offset as usize)
    }

    /// Calculate offset within array for a given tick index
    pub fn tick_offset(&self, tick_index: i32, tick_spacing: u16) -> Option<i32> {
        let ticks_in_array = TICK_ARRAY_SIZE * tick_spacing as i32;

        // Check if tick is in this array
        if tick_index < self.start_tick_index
            || tick_index >= self.start_tick_index + ticks_in_array
        {
            return None;
        }

        let offset = (tick_index - self.start_tick_index) / tick_spacing as i32;
        if offset < 0 || offset >= TICK_ARRAY_SIZE {
            return None;
        }

        Some(offset)
    }

    /// Check if this array contains the given tick index
    pub fn contains_tick(&self, tick_index: i32, tick_spacing: u16) -> bool {
        let ticks_in_array = TICK_ARRAY_SIZE * tick_spacing as i32;
        tick_index >= self.start_tick_index
            && tick_index < self.start_tick_index + ticks_in_array
    }

    /// Get the boundary tick index of this array in the swap direction.
    /// Used as a sentinel when no initialized tick is found, so the caller
    /// can advance past this array on the next iteration.
    ///
    /// For a_to_b: returns start_tick_index.
    ///   Caller sets current_tick = boundary - 1, which falls below this array's range.
    /// For b_to_a: returns start_tick_index + TICK_ARRAY_SIZE * tick_spacing (one past the array).
    ///   Caller sets current_tick = boundary, which is outside this array's range,
    ///   so the next search will find the next tick array.
    pub fn get_boundary_tick(&self, a_to_b: bool, tick_spacing: u16) -> i32 {
        if a_to_b {
            self.start_tick_index
        } else {
            self.start_tick_index + TICK_ARRAY_SIZE * tick_spacing as i32
        }
    }

    /// Get the next initialized tick in the swap direction
    /// Returns (tick_index, tick) or None if no initialized tick found
    pub fn get_next_initialized_tick(
        &self,
        tick_index: i32,
        tick_spacing: u16,
        a_to_b: bool,
    ) -> Option<(i32, Tick)> {
        let offset = self.tick_offset(tick_index, tick_spacing)?;

        if a_to_b {
            // Searching left (decreasing tick index)
            for i in (0..=offset as usize).rev() {
                if let Some(tick) = self.get_tick(i) {
                    if tick.initialized {
                        let found_tick_index = self.start_tick_index + (i as i32 * tick_spacing as i32);
                        return Some((found_tick_index, tick));
                    }
                }
            }
        } else {
            // Searching right (increasing tick index)
            // b_to_a search is exclusive of the current tick (offset+1),
            // matching official Whirlpool behavior
            let start = (offset as usize) + 1;
            for i in start..TICK_ARRAY_SIZE_USIZE {
                if let Some(tick) = self.get_tick(i) {
                    if tick.initialized {
                        let found_tick_index = self.start_tick_index + (i as i32 * tick_spacing as i32);
                        return Some((found_tick_index, tick));
                    }
                }
            }
        }

        None
    }
}

// ── DynamicTickArray support ──
//
// Orca Whirlpool v2 uses DynamicTickArrays with variable-size ticks.
// Layout (after 8-byte discriminator):
//   start_tick_index: i32    (4 bytes)
//   whirlpool: Pubkey        (32 bytes)
//   tick_bitmap: u128        (16 bytes) — bit i = tick i is initialized
//   ticks: [DynamicTick; 88] (variable)
//
// DynamicTick encoding:
//   0x00 = Uninitialized (1 byte)
//   0x01 = Initialized   (1 + 112 bytes = 113 bytes of DynamicTickData)

pub const DYNAMIC_TICK_ARRAY_DISCRIMINATOR: [u8; 8] = [17, 216, 246, 142, 225, 199, 218, 56];

const DYNAMIC_HEADER_LEN: usize = 4 + 32 + 16; // start_tick_index + whirlpool + tick_bitmap

/// Parse a DynamicTickArray and extract initialized ticks into a TickArraySimple-compatible format.
/// Returns (start_tick_index, Vec of (slot_index, Tick)) for initialized ticks only.
pub fn parse_dynamic_tick_array(data: &[u8]) -> Option<(i32, [(usize, Tick); TICK_ARRAY_SIZE_USIZE])> {
    let min_len = 8 + DYNAMIC_HEADER_LEN + TICK_ARRAY_SIZE_USIZE; // 88 uninitialized = 88 bytes
    if data.len() < min_len {
        return None;
    }
    if &data[0..8] != &DYNAMIC_TICK_ARRAY_DISCRIMINATOR {
        return None;
    }

    let start_tick_index = i32::from_le_bytes(data[8..12].try_into().ok()?);
    // tick_bitmap at offset 44..60
    let tick_bitmap = u128::from_le_bytes(data[44..60].try_into().ok()?);

    let mut result = [(0usize, Tick::default()); TICK_ARRAY_SIZE_USIZE];
    let mut result_count = 0usize;
    let mut cursor = 8 + DYNAMIC_HEADER_LEN; // offset 60

    for i in 0..TICK_ARRAY_SIZE_USIZE {
        if cursor >= data.len() { break; }
        let tag = data[cursor];
        if tag == 0 {
            // Uninitialized — 1 byte
            cursor += 1;
        } else {
            // Initialized — 1 byte tag + 112 bytes DynamicTickData
            if cursor + 113 > data.len() { break; }
            let td = &data[cursor + 1..cursor + 113];
            let tick = Tick {
                initialized: true,
                liquidity_net: i128::from_le_bytes(td[0..16].try_into().ok()?),
                liquidity_gross: u128::from_le_bytes(td[16..32].try_into().ok()?),
                fee_growth_outside_a: u128::from_le_bytes(td[32..48].try_into().ok()?),
                fee_growth_outside_b: u128::from_le_bytes(td[48..64].try_into().ok()?),
                reward_growths_outside: [
                    u128::from_le_bytes(td[64..80].try_into().ok()?),
                    u128::from_le_bytes(td[80..96].try_into().ok()?),
                    u128::from_le_bytes(td[96..112].try_into().ok()?),
                ],
            };
            if result_count < TICK_ARRAY_SIZE_USIZE {
                result[result_count] = (i, tick);
                result_count += 1;
            }
            cursor += 113;
        }
    }

    // Sentinel: mark end with slot_index=TICK_ARRAY_SIZE_USIZE
    // Caller uses result_count to know how many entries are valid
    // We encode count in the first unused slot
    if result_count < TICK_ARRAY_SIZE_USIZE {
        result[result_count].0 = TICK_ARRAY_SIZE_USIZE; // sentinel
    }

    let _ = tick_bitmap; // bitmap is redundant with tag-based parsing
    Some((start_tick_index, result))
}

/// Check if a DynamicTickArray contains a given tick and find the next initialized tick.
/// Returns the same (tick_index, Tick) format as TickArraySimple::get_next_initialized_tick.
pub fn dynamic_find_next_initialized_tick(
    data: &[u8],
    current_tick: i32,
    tick_spacing: u16,
    a_to_b: bool,
) -> Option<(i32, Option<Tick>)> {
    let min_len = 8 + DYNAMIC_HEADER_LEN;
    if data.len() < min_len { return None; }
    if &data[0..8] != &DYNAMIC_TICK_ARRAY_DISCRIMINATOR { return None; }

    let start_tick_index = i32::from_le_bytes(data[8..12].try_into().ok()?);
    let ticks_in_array = TICK_ARRAY_SIZE * tick_spacing as i32;

    // Check if this array contains the current tick
    if current_tick < start_tick_index || current_tick >= start_tick_index + ticks_in_array {
        return None;
    }

    let offset = (current_tick - start_tick_index) / tick_spacing as i32;
    let tick_bitmap = u128::from_le_bytes(data[44..60].try_into().ok()?);

    // Walk through tick data to build offset→cursor map
    // We need to find the tick data position for the target slot
    let mut cursor = 8 + DYNAMIC_HEADER_LEN; // offset 60
    let mut slot_cursors = [0u32; TICK_ARRAY_SIZE_USIZE];
    for i in 0..TICK_ARRAY_SIZE_USIZE {
        if cursor >= data.len() { break; }
        slot_cursors[i] = cursor as u32;
        if data[cursor] == 0 {
            cursor += 1; // uninitialized
        } else {
            cursor += 113; // initialized
        }
    }

    if a_to_b {
        // Search left (decreasing), inclusive of current offset
        for i in (0..=offset as usize).rev() {
            if i >= TICK_ARRAY_SIZE_USIZE { continue; }
            let c = slot_cursors[i] as usize;
            if c < data.len() && data[c] != 0 && c + 113 <= data.len() {
                let td = &data[c + 1..c + 113];
                let tick = Tick {
                    initialized: true,
                    liquidity_net: i128::from_le_bytes(td[0..16].try_into().ok()?),
                    liquidity_gross: u128::from_le_bytes(td[16..32].try_into().ok()?),
                    fee_growth_outside_a: u128::from_le_bytes(td[32..48].try_into().ok()?),
                    fee_growth_outside_b: u128::from_le_bytes(td[48..64].try_into().ok()?),
                    reward_growths_outside: [
                        u128::from_le_bytes(td[64..80].try_into().ok()?),
                        u128::from_le_bytes(td[80..96].try_into().ok()?),
                        u128::from_le_bytes(td[96..112].try_into().ok()?),
                    ],
                };
                let found_tick = start_tick_index + (i as i32) * tick_spacing as i32;
                return Some((found_tick, Some(tick)));
            }
        }
    } else {
        // Search right (increasing), exclusive of current offset
        let start = (offset as usize) + 1;
        for i in start..TICK_ARRAY_SIZE_USIZE {
            let c = slot_cursors[i] as usize;
            if c < data.len() && data[c] != 0 && c + 113 <= data.len() {
                let td = &data[c + 1..c + 113];
                let tick = Tick {
                    initialized: true,
                    liquidity_net: i128::from_le_bytes(td[0..16].try_into().ok()?),
                    liquidity_gross: u128::from_le_bytes(td[16..32].try_into().ok()?),
                    fee_growth_outside_a: u128::from_le_bytes(td[32..48].try_into().ok()?),
                    fee_growth_outside_b: u128::from_le_bytes(td[48..64].try_into().ok()?),
                    reward_growths_outside: [
                        u128::from_le_bytes(td[64..80].try_into().ok()?),
                        u128::from_le_bytes(td[80..96].try_into().ok()?),
                        u128::from_le_bytes(td[96..112].try_into().ok()?),
                    ],
                };
                let found_tick = start_tick_index + (i as i32) * tick_spacing as i32;
                return Some((found_tick, Some(tick)));
            }
        }
    }

    // No initialized tick found — return boundary
    let boundary = if a_to_b {
        start_tick_index
    } else {
        start_tick_index + ticks_in_array
    };
    Some((boundary, None))
}

/// Get the start index of the tick array containing the given tick
pub fn get_tick_array_start_index(tick_index: i32, tick_spacing: u16) -> i32 {
    let ticks_in_array = TICK_ARRAY_SIZE * tick_spacing as i32;
    let mut start = tick_index / ticks_in_array * ticks_in_array;
    if tick_index < 0 && tick_index % ticks_in_array != 0 {
        start -= ticks_in_array;
    }
    start
}
