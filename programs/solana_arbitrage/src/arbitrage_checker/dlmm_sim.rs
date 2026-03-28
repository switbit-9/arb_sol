use super::{FEE_PRECISION, SCALE_OFFSET};

// ── Q64.64 fixed-point constants ──
const ONE: u128 = 1u128 << 64;
const BASIS_POINT_MAX: u128 = 10_000;
const MAX_FEE_RATE: u128 = 100_000_000; // 10%
const MAX_BIN_PER_ARRAY: i32 = 70;

// ── Data structures ──

#[derive(Clone, Copy, Debug)]
pub struct DlmmBin {
    pub id: i32,
    pub amount_x: u64,
    pub amount_y: u64,
    pub price: u128, // Q64.64, 0 means "compute from id"
}

/// Max non-empty bins per direction (one bin array = 70 slots).
const MAX_BINS_PER_DIR: usize = 70;

#[derive(Clone, Debug)]
pub struct DlmmPool {
    pub active_id: i32,
    pub bin_step: u16,
    // Fee params
    pub base_factor: u16,
    pub base_fee_power_factor: u8,
    pub variable_fee_control: u32,
    pub volatility_accumulator: u32,
    pub volatility_reference: u32,
    pub index_reference: i32,
    pub max_volatility_accumulator: u32,
    // Pre-computed base fee (constant for a given pool)
    base_fee_cached: u128,
    // Bins split by swap direction — each loaded from its own bin array (no merge/sort).
    // Already sorted (sequential within a single bin array).
    bins_sfy: [DlmmBin; MAX_BINS_PER_DIR],  // used when swap_for_y = true
    bins_sfy_count: usize,
    bins_sfx: [DlmmBin; MAX_BINS_PER_DIR],  // used when swap_for_y = false
    bins_sfx_count: usize,
}

impl DlmmPool {
    /// Access bins for a given swap direction.
    #[inline]
    pub fn bins(&self, swap_for_y: bool) -> &[DlmmBin] {
        if swap_for_y {
            &self.bins_sfy[..self.bins_sfy_count]
        } else {
            &self.bins_sfx[..self.bins_sfx_count]
        }
    }

    /// Construct from a slice of bins (for tests and off-chain callers).
    /// Bins are placed in both sfy and sfx arrays so the pool works in either direction.
    pub fn new(
        active_id: i32,
        bin_step: u16,
        base_factor: u16,
        base_fee_power_factor: u8,
        variable_fee_control: u32,
        volatility_accumulator: u32,
        volatility_reference: u32,
        index_reference: i32,
        max_volatility_accumulator: u32,
        bins: &[DlmmBin],
    ) -> Self {
        let count = bins.len().min(MAX_BINS_PER_DIR);
        let empty = [DlmmBin { id: 0, amount_x: 0, amount_y: 0, price: 0 }; MAX_BINS_PER_DIR];
        let mut sfy = empty;
        let mut sfx = empty;
        sfy[..count].clone_from_slice(&bins[..count]);
        sfx[..count].clone_from_slice(&bins[..count]);
        let base_fee_cached = (base_factor as u128)
            * (bin_step as u128)
            * 10u128
            * 10u128.pow(base_fee_power_factor as u32);
        Self {
            active_id,
            bin_step,
            base_factor,
            base_fee_power_factor,
            variable_fee_control,
            volatility_accumulator,
            volatility_reference,
            index_reference,
            max_volatility_accumulator,
            base_fee_cached,
            bins_sfy: sfy,
            bins_sfy_count: count,
            bins_sfx: sfx,
            bins_sfx_count: count,
        }
    }

    /// Push a bin for a given swap direction. Returns false if storage is full.
    #[inline]
    pub fn push_bin(&mut self, bin: DlmmBin, swap_for_y: bool) -> bool {
        if swap_for_y {
            if self.bins_sfy_count >= MAX_BINS_PER_DIR { return false; }
            self.bins_sfy[self.bins_sfy_count] = bin;
            self.bins_sfy_count += 1;
        } else {
            if self.bins_sfx_count >= MAX_BINS_PER_DIR { return false; }
            self.bins_sfx[self.bins_sfx_count] = bin;
            self.bins_sfx_count += 1;
        }
        true
    }

    /// Create an empty pool (bins added later via push_bin).
    pub fn empty(
        active_id: i32,
        bin_step: u16,
        base_factor: u16,
        base_fee_power_factor: u8,
        variable_fee_control: u32,
        volatility_accumulator: u32,
        volatility_reference: u32,
        index_reference: i32,
        max_volatility_accumulator: u32,
    ) -> Self {
        let base_fee_cached = (base_factor as u128)
            * (bin_step as u128)
            * 10u128
            * 10u128.pow(base_fee_power_factor as u32);
        let empty_bins = [DlmmBin { id: 0, amount_x: 0, amount_y: 0, price: 0 }; MAX_BINS_PER_DIR];
        Self {
            active_id,
            bin_step,
            base_factor,
            base_fee_power_factor,
            variable_fee_control,
            volatility_accumulator,
            volatility_reference,
            index_reference,
            max_volatility_accumulator,
            base_fee_cached,
            bins_sfy: empty_bins,
            bins_sfy_count: 0,
            bins_sfx: empty_bins,
            bins_sfx_count: 0,
        }
    }

    /// Heap-allocate an empty pool directly, avoiding the 5KB stack intermediate.
    pub fn box_empty(
        active_id: i32,
        bin_step: u16,
        base_factor: u16,
        base_fee_power_factor: u8,
        variable_fee_control: u32,
        volatility_accumulator: u32,
        volatility_reference: u32,
        index_reference: i32,
        max_volatility_accumulator: u32,
    ) -> Box<Self> {
        let base_fee_cached = (base_factor as u128)
            * (bin_step as u128)
            * 10u128
            * 10u128.pow(base_fee_power_factor as u32);
        // Allocate zeroed on heap via Vec to avoid putting 5KB DlmmPool on the stack.
        let size = core::mem::size_of::<Self>();
        let align = core::mem::align_of::<Self>();
        let mut bytes: Vec<u8> = vec![0u8; size + align];
        let offset = bytes.as_ptr().align_offset(align);
        let ptr = unsafe { bytes.as_mut_ptr().add(offset) as *mut Self };
        core::mem::forget(bytes); // prevent dealloc, Box will own it
        let mut pool = unsafe { Box::from_raw(ptr) };
        pool.active_id = active_id;
        pool.bin_step = bin_step;
        pool.base_factor = base_factor;
        pool.base_fee_power_factor = base_fee_power_factor;
        pool.variable_fee_control = variable_fee_control;
        pool.volatility_accumulator = volatility_accumulator;
        pool.volatility_reference = volatility_reference;
        pool.index_reference = index_reference;
        pool.max_volatility_accumulator = max_volatility_accumulator;
        pool.base_fee_cached = base_fee_cached;
        pool.bins_sfy_count = 0;
        pool.bins_sfx_count = 0;
        pool
    }
}

// ── Q64.64 math ──

/// (x * y) >> 64 using pure u128 limb arithmetic (no U256).
///
/// Split x = x_hi·2^64 + x_lo, y = y_hi·2^64 + y_lo.
/// Full 256-bit product: x·y = hh·2^128 + (hl + lh)·2^64 + ll
/// Shifted: (x·y) >> 64 = (hh << 64) + hl + lh + (ll >> 64)
/// Rounding bit: low 64 bits of ll.
#[inline]
pub(crate) fn mul_shr(x: u128, y: u128, _offset: u8, round_up: bool) -> Option<u128> {
    let x_hi = x >> 64;
    let x_lo = x & (u64::MAX as u128);
    let y_hi = y >> 64;
    let y_lo = y & (u64::MAX as u128);

    let ll = x_lo * y_lo;
    let hl = x_hi * y_lo;
    let lh = x_lo * y_hi;
    let hh = x_hi * y_hi;

    // hh << 64 overflows u128 when hh >= 2^64
    // (equivalent to U256 result not fitting in u128)
    if hh >> 64 != 0 {
        return None;
    }

    let result = (hh << 64)
        .checked_add(hl)?
        .checked_add(lh)?
        .checked_add(ll >> 64)?;

    if round_up && (ll & (u64::MAX as u128)) != 0 {
        result.checked_add(1)
    } else {
        Some(result)
    }
}

/// (x << 64) / y using pure u128 arithmetic (no U256).
///
/// In DLMM swap paths, x is always a u64-range value so x << 64 fits in u128.
/// The general case uses long division: (x/y)<<64 + ((x%y)<<64)/y, splitting
/// the remainder shift into two 32-bit steps to stay within u128.
#[inline]
pub(crate) fn shl_div(x: u128, y: u128, _offset: u8, round_up: bool) -> Option<u128> {
    if y == 0 {
        return None;
    }
    // Fast path: x fits in 64 bits → x << 64 fits in u128
    if x <= u64::MAX as u128 {
        let num = x << 64;
        return Some(if round_up {
            (num + y - 1) / y
        } else {
            num / y
        });
    }
    // General case: x > u64::MAX, so x << 64 overflows u128.
    // Use identity: (x << 64) / y = (x / y) << 64 + ((x % y) << 64) / y
    let q_hi = x / y;
    let r = x % y;

    // If quotient >= 2^64, result overflows u128
    if q_hi > u64::MAX as u128 {
        return None;
    }

    // Compute (r << 64) / y where r < y, using two 32-bit shifts to stay in u128.
    // r < y <= u128::MAX, but r << 64 could be up to 192 bits.
    // Split: (r << 64) = ((r << 32) << 32)
    // Step 1: (r << 32) / y  and  (r << 32) % y
    // Step 2: (rem1 << 32) / y
    // Then: (r << 64) / y = step1_q << 32 + step2_q  (with remainder for rounding)

    // r << 32 fits in u128 if r < 2^96
    let (q_lo, has_remainder) = if r == 0 {
        (0u128, false)
    } else if r < (1u128 << 96) {
        let temp1 = r << 32;
        let q1 = temp1 / y;
        let r1 = temp1 % y;
        // r1 < y, and r1 << 32: r1 < y <= u128::MAX, r1 << 32 could overflow
        // but r1 < y and after first division r1 is reduced, in practice fits
        if r1 < (1u128 << 96) {
            let temp2 = r1 << 32;
            let q2 = temp2 / y;
            let r2 = temp2 % y;
            ((q1 << 32).checked_add(q2)?, r2 != 0)
        } else {
            // r1 >= 2^96: split further with 16-bit steps
            let temp2a = r1 << 16;
            let q2a = temp2a / y;
            let r2a = temp2a % y;
            let temp2b = r2a << 16;
            let q2b = temp2b / y;
            let r2b = temp2b % y;
            let q2 = (q2a << 16).checked_add(q2b)?;
            ((q1 << 32).checked_add(q2)?, r2b != 0)
        }
    } else {
        // r >= 2^96: use four 16-bit shifts
        let mut q_lo = 0u128;
        let mut rem = r;
        for shift in [16u8, 16, 16, 16] {
            let temp = rem << shift;
            let q_part = temp / y;
            rem = temp % y;
            q_lo = q_lo.checked_shl(shift as u32)?.checked_add(q_part)?;
        }
        (q_lo, rem != 0)
    };

    let result = (q_hi << 64).checked_add(q_lo)?;
    if round_up && has_remainder {
        result.checked_add(1)
    } else {
        Some(result)
    }
}

/// Compute (1 + bin_step/10000)^exp in Q64.64 using binary exponentiation.
pub fn get_price_from_id(bin_id: i32, bin_step: u16) -> Option<u128> {
    let bps = (bin_step as u128).checked_shl(SCALE_OFFSET as u32)? / BASIS_POINT_MAX;
    let base = ONE.checked_add(bps)?;
    pow_q64(base, bin_id)
}

fn pow_q64(base: u128, exp: i32) -> Option<u128> {
    if exp == 0 {
        return Some(ONE);
    }

    let mut invert = exp.is_negative();
    let exp: u32 = exp.unsigned_abs();

    if exp >= 0x80000 {
        return None;
    }

    let mut squared = base;
    let mut result = ONE;

    if squared >= result {
        squared = u128::MAX.checked_div(squared)?;
        invert = !invert;
    }

    let bits_needed = 32 - exp.leading_zeros(); // only iterate over meaningful bits
    for bit in 0..bits_needed {
        if bit > 0 {
            squared = (squared.checked_mul(squared)?) >> SCALE_OFFSET;
        }
        if exp & (1 << bit) > 0 {
            result = (result.checked_mul(squared)?) >> SCALE_OFFSET;
        }
    }

    if result == 0 {
        return None;
    }
    if invert {
        result = u128::MAX.checked_div(result)?;
    }
    Some(result)
}

// ── Fee calculation ──

impl DlmmPool {
    #[inline]
    pub fn get_base_fee(&self) -> u128 {
        self.base_fee_cached
    }

    pub fn compute_variable_fee(&self, volatility_accumulator: u32) -> u128 {
        if self.variable_fee_control == 0 {
            return 0;
        }
        let va: u128 = volatility_accumulator as u128;
        let bs: u128 = self.bin_step as u128;
        let vfc: u128 = self.variable_fee_control as u128;
        let sq = (va * bs) * (va * bs); // (va * bs)^2
        let v_fee = vfc * sq;
        (v_fee + 99_999_999_999) / 100_000_000_000
    }

    pub fn get_total_fee(&self, volatility_accumulator: u32) -> u128 {
        let total = self.get_base_fee() + self.compute_variable_fee(volatility_accumulator);
        total.min(MAX_FEE_RATE)
    }

    /// Fee on a raw amount: ceil(amount * fee_rate / (FEE_PRECISION - fee_rate))
    pub fn compute_fee(&self, amount: u64, total_fee_rate: u128) -> u64 {
        let denom = FEE_PRECISION - total_fee_rate;
        if denom == 0 {
            return amount;
        }
        let fee = ((amount as u128) * total_fee_rate + denom - 1) / denom;
        fee as u64
    }

    /// Fee extracted from an amount that already includes fee:
    /// ceil(amount_with_fees * fee_rate / FEE_PRECISION)
    pub fn compute_fee_from_amount(&self, amount_with_fees: u64, total_fee_rate: u128) -> u64 {
        let fee = ((amount_with_fees as u128) * total_fee_rate + FEE_PRECISION - 1) / FEE_PRECISION;
        fee as u64
    }

    /// Compute initial volatility accumulator the same way the on-chain
    /// `update_references` does before the first bin swap.
    pub fn initial_vol_acc(&self) -> u32 {
        let delta_id = ((self.index_reference as i64) - (self.active_id as i64)).unsigned_abs();
        let va = (self.volatility_reference as u64)
            .saturating_add(delta_id.saturating_mul(BASIS_POINT_MAX as u64));
        va.min(self.max_volatility_accumulator as u64) as u32
    }

    pub fn update_volatility_accumulator(&self, current_vol_acc: u32, active_id: i32) -> u32 {
        let delta_id = ((self.index_reference as i64) - (active_id as i64)).unsigned_abs();
        let va = (self.volatility_reference as u64)
            .saturating_add(delta_id.saturating_mul(BASIS_POINT_MAX as u64));
        va.min(self.max_volatility_accumulator as u64) as u32
    }

    pub fn find_bin_index(&self, bin_id: i32, swap_for_y: bool) -> Option<usize> {
        self.bins(swap_for_y).binary_search_by_key(&bin_id, |b| b.id).ok()
    }

    /// Find starting cursor for sequential bin walk from active_id.
    /// Returns the index of the active bin (or nearest in the walk direction).
    #[inline]
    pub fn start_cursor(&self, swap_for_y: bool) -> Option<usize> {
        let bins = self.bins(swap_for_y);
        match bins.binary_search_by_key(&self.active_id, |b| b.id) {
            Ok(idx) => Some(idx),
            Err(idx) => {
                // active_id not found — pick nearest in walk direction
                if swap_for_y {
                    // walking down: pick the bin just below
                    if idx > 0 { Some(idx - 1) } else { None }
                } else {
                    // walking up: pick the bin at insert point
                    if idx < bins.len() { Some(idx) } else { None }
                }
            }
        }
    }

    /// Advance cursor to next bin in walk direction. Returns None if out of bounds.
    #[inline]
    pub fn advance_cursor(&self, cursor: usize, swap_for_y: bool) -> Option<usize> {
        if swap_for_y {
            cursor.checked_sub(1)
        } else {
            let next = cursor + 1;
            if next < self.bins(swap_for_y).len() { Some(next) } else { None }
        }
    }

    /// Simulate swap_exact_in: given `amount_in` of input token, return output amount.
    /// Uses sequential cursor instead of binary search per bin.
    /// Returns (amount_out, total_fee)
    pub fn quote_exact_in(&self, amount_in: u64, swap_for_y: bool) -> Option<(u64, u64)> {
        if amount_in == 0 {
            return Some((0, 0));
        }

        debug_eprintln!(
            "[dlmm_sim::quote_exact_in] amount_in={} swap_for_y={} active_id={} bin_step={} num_bins={}",
            amount_in, swap_for_y, self.active_id, self.bin_step, self.bins(swap_for_y).len()
        );

        let mut cursor = self.start_cursor(swap_for_y)?;
        let mut amount_left = amount_in;
        let mut total_out: u64 = 0;
        let mut total_fee: u64 = 0;
        let mut vol_acc = self.initial_vol_acc();
        let mut first_bin = true;
        let bins = self.bins(swap_for_y);

        debug_eprintln!("[dlmm_sim::quote_exact_in] start_cursor={}", cursor);

        loop {
            if amount_left == 0 || cursor >= bins.len() { break; }
            let bin = &bins[cursor];

            if !first_bin {
                vol_acc = self.update_volatility_accumulator(vol_acc, bin.id);
            }
            first_bin = false;

            let total_fee_rate = self.get_total_fee(vol_acc);
            let price = bin.price; // pre-computed

            let max_amount_out = if swap_for_y { bin.amount_y } else { bin.amount_x };
            debug_eprintln!(
                "[dlmm_sim::quote_exact_in] bin[{}] id={} amt_x={} amt_y={} price={} max_out={} amount_left={} fee_rate={}",
                cursor, bin.id, bin.amount_x, bin.amount_y, bin.price, max_amount_out, amount_left, total_fee_rate
            );
            if max_amount_out == 0 {
                cursor = match self.advance_cursor(cursor, swap_for_y) { Some(c) => c, None => break };
                continue;
            }

            let max_amount_in_raw = if swap_for_y {
                shl_div(max_amount_out as u128, price, SCALE_OFFSET, true)? as u64
            } else {
                mul_shr(max_amount_out as u128, price, SCALE_OFFSET, true)? as u64
            };

            let max_fee = self.compute_fee(max_amount_in_raw, total_fee_rate);
            let max_amount_in_with_fee = max_amount_in_raw.checked_add(max_fee)?;

            if amount_left > max_amount_in_with_fee {
                debug_eprintln!(
                    "[dlmm_sim::quote_exact_in] bin[{}] DRAIN: max_in_raw={} max_fee={} max_in_w_fee={} out={} remaining={}",
                    cursor, max_amount_in_raw, max_fee, max_amount_in_with_fee, max_amount_out, amount_left.saturating_sub(max_amount_in_with_fee)
                );
                total_out = total_out.checked_add(max_amount_out)?;
                total_fee = total_fee.checked_add(max_fee)?;
                amount_left = amount_left.checked_sub(max_amount_in_with_fee)?;
                cursor = match self.advance_cursor(cursor, swap_for_y) { Some(c) => c, None => break };
            } else {
                let fee = self.compute_fee_from_amount(amount_left, total_fee_rate);
                let amount_in_after_fee = amount_left.checked_sub(fee)?;
                let amount_out = if swap_for_y {
                    mul_shr(price, amount_in_after_fee as u128, SCALE_OFFSET, false)? as u64
                } else {
                    shl_div(amount_in_after_fee as u128, price, SCALE_OFFSET, false)? as u64
                };
                let amount_out = amount_out.min(max_amount_out);

                debug_eprintln!(
                    "[dlmm_sim::quote_exact_in] bin[{}] PARTIAL: amount_left={} fee={} in_after_fee={} out={}",
                    cursor, amount_left, fee, amount_in_after_fee, amount_out
                );
                total_out = total_out.checked_add(amount_out)?;
                total_fee = total_fee.checked_add(fee)?;
                amount_left = 0;
            }
        }

        debug_eprintln!(
            "[dlmm_sim::quote_exact_in] RESULT: amount_in={} total_out={} total_fee={} amount_left={}",
            amount_in, total_out, total_fee, amount_left
        );
        if total_out == 0 && first_bin { None } else { Some((total_out, total_fee)) }
    }

    /// Simulate swap_exact_out: given desired `amount_out`, return required input amount.
    /// Uses sequential cursor instead of binary search per bin.
    /// Returns (amount_in_with_fees, total_fee)
    pub fn quote_exact_out(&self, amount_out: u64, swap_for_y: bool) -> Option<(u64, u64)> {
        if amount_out == 0 {
            return Some((0, 0));
        }

        let mut cursor = self.start_cursor(swap_for_y)?;
        let mut remaining_out = amount_out;
        let mut total_in: u64 = 0;
        let mut total_fee: u64 = 0;
        let mut vol_acc = self.initial_vol_acc();
        let mut first_bin = true;
        let bins = self.bins(swap_for_y);

        loop {
            if remaining_out == 0 || cursor >= bins.len() { break; }
            let bin = &bins[cursor];

            if !first_bin {
                vol_acc = self.update_volatility_accumulator(vol_acc, bin.id);
            }
            first_bin = false;

            let total_fee_rate = self.get_total_fee(vol_acc);
            let price = bin.price; // pre-computed

            let max_amount_out = if swap_for_y { bin.amount_y } else { bin.amount_x };
            if max_amount_out == 0 {
                cursor = match self.advance_cursor(cursor, swap_for_y) { Some(c) => c, None => break };
                continue;
            }

            if remaining_out >= max_amount_out {
                let max_in = if swap_for_y {
                    shl_div(max_amount_out as u128, price, SCALE_OFFSET, true)? as u64
                } else {
                    mul_shr(max_amount_out as u128, price, SCALE_OFFSET, true)? as u64
                };
                let fee = self.compute_fee(max_in, total_fee_rate);

                total_in = total_in.checked_add(max_in)?;
                total_fee = total_fee.checked_add(fee)?;
                remaining_out = remaining_out.checked_sub(max_amount_out)?;
                cursor = match self.advance_cursor(cursor, swap_for_y) { Some(c) => c, None => break };
            } else {
                let amount_in = if swap_for_y {
                    shl_div(remaining_out as u128, price, SCALE_OFFSET, true)? as u64
                } else {
                    mul_shr(remaining_out as u128, price, SCALE_OFFSET, true)? as u64
                };
                let fee = self.compute_fee(amount_in, total_fee_rate);

                total_in = total_in.checked_add(amount_in)?;
                total_fee = total_fee.checked_add(fee)?;
                remaining_out = 0;
            }
        }

        if total_in == 0 && first_bin { None } else {
            let total_in_with_fees = total_in.checked_add(total_fee)?;
            Some((total_in_with_fees, total_fee))
        }
    }
}

// ── Convenience: build DlmmPool from raw bin array bytes ──

const BIN_ARRAY_HEADER_SIZE: usize = 56;
const BIN_DATA_SIZE: usize = 144;

/// Parse bins from raw bin array account data (includes 8-byte discriminator).
/// Returns (bin_array_index, Vec<DlmmBin>) for all non-empty bins.
pub fn parse_bins_from_account_data(data: &[u8]) -> Option<(i64, Vec<DlmmBin>)> {
    if data.len() < 8 + BIN_ARRAY_HEADER_SIZE {
        return None;
    }
    let inner = &data[8..]; // skip discriminator
    let bin_array_index: i64 = i64::from_le_bytes(inner[0..8].try_into().ok()?);
    let lower_bin_id = (bin_array_index as i32) * MAX_BIN_PER_ARRAY;

    let mut bins = Vec::new();
    for i in 0..MAX_BIN_PER_ARRAY {
        let offset = BIN_ARRAY_HEADER_SIZE + (i as usize * BIN_DATA_SIZE);
        if offset + BIN_DATA_SIZE > inner.len() {
            break;
        }
        let bin_data = &inner[offset..offset + BIN_DATA_SIZE];
        // Bin layout: amount_x(8) + amount_y(8) + price(16) + liquidity_supply(16) + ...
        let amount_x = u64::from_le_bytes(bin_data[0..8].try_into().ok()?);
        let amount_y = u64::from_le_bytes(bin_data[8..16].try_into().ok()?);
        let price = u128::from_le_bytes(bin_data[16..32].try_into().ok()?);

        if amount_x > 0 || amount_y > 0 {
            bins.push(DlmmBin {
                id: lower_bin_id + i,
                amount_x,
                amount_y,
                price,
            });
        }
    }

    Some((bin_array_index, bins))
}

/// Parse LbPair fee params from raw account data (includes 8-byte discriminator).
/// Extracts only the fields needed for swap simulation.
pub fn parse_lb_pair_params(data: &[u8]) -> Option<DlmmPoolParams> {
    if data.len() < 8 + 200 {
        return None;
    }
    let d = &data[8..]; // skip discriminator

    // LbPair layout (from bytemuck Pod):
    // offset 0: parameters (StaticParameters, 32 bytes)
    // offset 32: v_parameters (VariableParameters, 32 bytes)
    // offset 64: bump_seed (2 bytes)
    // offset 66: bin_step_seed (2 bytes)
    // offset 68: pair_type (1 byte)
    // offset 69: active_id (4 bytes, i32)
    // offset 73: bin_step (2 bytes, u16)
    // ...

    // StaticParameters (at offset 0):
    // base_factor: u16 (offset 0)
    // filter_period: u16 (offset 2)
    // decay_period: u16 (offset 4)
    // reduction_factor: u16 (offset 6)
    // variable_fee_control: u32 (offset 8)
    // max_volatility_accumulator: u32 (offset 12)
    // ...
    // base_fee_power_factor: u8 (offset 24)
    // ...
    // protocol_share: u16 (offset 16)

    let base_factor = u16::from_le_bytes(d[0..2].try_into().ok()?);
    let variable_fee_control = u32::from_le_bytes(d[8..12].try_into().ok()?);
    let max_volatility_accumulator = u32::from_le_bytes(d[12..16].try_into().ok()?);
    let base_fee_power_factor = d[24];

    // VariableParameters (at offset 32):
    // volatility_accumulator: u32 (offset 0)
    // volatility_reference: u32 (offset 4)
    // index_reference: i32 (offset 8)
    // ...
    let volatility_accumulator = u32::from_le_bytes(d[32..36].try_into().ok()?);
    let volatility_reference = u32::from_le_bytes(d[36..40].try_into().ok()?);
    let index_reference = i32::from_le_bytes(d[40..44].try_into().ok()?);

    // active_id at offset 69, bin_step at offset 73
    let active_id = i32::from_le_bytes(d[69..73].try_into().ok()?);
    let bin_step = u16::from_le_bytes(d[73..75].try_into().ok()?);

    Some(DlmmPoolParams {
        active_id,
        bin_step,
        base_factor,
        base_fee_power_factor,
        variable_fee_control,
        volatility_accumulator,
        volatility_reference,
        index_reference,
        max_volatility_accumulator,
    })
}

#[derive(Clone, Debug)]
pub struct DlmmPoolParams {
    pub active_id: i32,
    pub bin_step: u16,
    pub base_factor: u16,
    pub base_fee_power_factor: u8,
    pub variable_fee_control: u32,
    pub volatility_accumulator: u32,
    pub volatility_reference: u32,
    pub index_reference: i32,
    pub max_volatility_accumulator: u32,
}

impl DlmmPool {
    pub fn from_params(params: DlmmPoolParams, bins: &[DlmmBin]) -> Self {
        let mut pool = Self::new(
            params.active_id,
            params.bin_step,
            params.base_factor,
            params.base_fee_power_factor,
            params.variable_fee_control,
            params.volatility_accumulator,
            params.volatility_reference,
            params.index_reference,
            params.max_volatility_accumulator,
            bins,
        );
        // Pre-compute all bin prices eagerly to avoid repeated pow_q64 calls
        for i in 0..pool.bins_sfy_count {
            if pool.bins_sfy[i].price == 0 {
                pool.bins_sfy[i].price =
                    get_price_from_id(pool.bins_sfy[i].id, params.bin_step).unwrap_or(0);
            }
        }
        for i in 0..pool.bins_sfx_count {
            if pool.bins_sfx[i].price == 0 {
                pool.bins_sfx[i].price =
                    get_price_from_id(pool.bins_sfx[i].id, params.bin_step).unwrap_or(0);
            }
        }
        pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_price_from_id_zero() {
        let price = get_price_from_id(0, 1).unwrap();
        assert_eq!(price, ONE, "price at bin 0 should be 1.0 in Q64.64");
    }

    #[test]
    fn test_price_from_id_positive() {
        // bin_step=1 (0.01%), bin_id=1 => price = 1.0001
        let price = get_price_from_id(1, 1).unwrap();
        assert!(price > ONE);
        // Should be very close to 1.0001 * 2^64
        let expected = (1.0001f64 * (ONE as f64)) as u128;
        let diff = if price > expected {
            price - expected
        } else {
            expected - price
        };
        assert!(diff < 1_000, "price precision: diff={}", diff);
    }

    #[test]
    fn test_price_from_id_negative() {
        let price = get_price_from_id(-1, 1).unwrap();
        assert!(price < ONE);
    }

    #[test]
    fn test_simple_swap_single_bin() {
        // Create a pool with a single bin at id=0, bin_step=100 (1%)
        // Bin has 1000 Y tokens, price=1.0 (Q64.64 = ONE)
        let pool = DlmmPool::new(
            0, 100, 10, 0, 0, 0, 0, 0, 0,
            &[DlmmBin {
                id: 0,
                amount_x: 0,
                amount_y: 1_000_000_000, // 1B lamports
                price: ONE,
            }],
        );

        // Swap 100M X for Y (swap_for_y=true)
        let (out, fee) = pool.quote_exact_in(100_000_000, true).unwrap();
        assert!(out > 0, "should get some Y output");
        assert!(fee > 0, "should have some fee");
        assert!(out < 100_000_000, "output should be less than input due to fee");
    }

    // ── mul_shr / shl_div precision tests ──
    // These lock in exact results BEFORE removing U256.
    // After switching to pure-u128 limb math, every assert must still pass.

    #[test]
    fn test_mul_shr_both_small() {
        // Both fit in u64 — fast path
        let r = mul_shr(1_000_000, 2_000_000, SCALE_OFFSET, false).unwrap();
        // (1e6 * 2e6) >> 64 = 2e12 >> 64 = 0 (product < 2^64)
        assert_eq!(r, 0);
        // with round_up: fractional bits exist so result = 1
        let r_up = mul_shr(1_000_000, 2_000_000, SCALE_OFFSET, true).unwrap();
        assert_eq!(r_up, 1);
    }

    #[test]
    fn test_mul_shr_one_large() {
        // x has high bits, y is small — medium path (one limb nonzero)
        let x: u128 = (3u128 << 64) + 7; // x_hi=3, x_lo=7
        let y: u128 = 5;
        let r = mul_shr(x, y, SCALE_OFFSET, false).unwrap();
        // x*y = ((3<<64)+7)*5 = 15<<64 + 35
        // >> 64 = 15 + (35 >> 64) = 15
        assert_eq!(r, 15);
        let r_up = mul_shr(x, y, SCALE_OFFSET, true).unwrap();
        // 35 has nonzero low bits → rounds up
        assert_eq!(r_up, 16);
    }

    #[test]
    fn test_mul_shr_both_large_u256_path() {
        // BOTH x_hi and y_hi nonzero — this is the U256 fallback path.
        // Mimics a real high-price bin × large amount.
        let x: u128 = (1u128 << 64) | (1u128 << 63); // x_hi=1, x_lo=2^63
        let y: u128 = (2u128 << 64) | 1;              // y_hi=2, y_lo=1
        let r = mul_shr(x, y, SCALE_OFFSET, false).unwrap();
        // x = 2^64 + 2^63, y = 2^65 + 1
        // x*y = (2^64+2^63)*(2^65+1)
        //     = 2^129 + 2^128 + 2^64 + 2^63
        // >> 64 = 2^65 + 2^64 + 1 + (2^63 >> 64)
        //       = 2^65 + 2^64 + 1    (2^63 < 2^64, so low bits present)
        let expected = (1u128 << 65) + (1u128 << 64) + 1;
        assert_eq!(r, expected);
        let r_up = mul_shr(x, y, SCALE_OFFSET, true).unwrap();
        // low 64 bits of x_lo*y_lo = 2^63 != 0, so rounds up
        assert_eq!(r_up, expected + 1);
    }

    #[test]
    fn test_mul_shr_max_values() {
        // Near-max u128 values — stress the overflow detection
        let x: u128 = u128::MAX;
        let y: u128 = 1;
        let r = mul_shr(x, y, SCALE_OFFSET, false).unwrap();
        // (MAX * 1) >> 64 = MAX >> 64 = 2^64 - 1
        assert_eq!(r, u64::MAX as u128);

        // u128::MAX * u128::MAX → hh = (2^64-1)^2 > 2^64, so hh>>64 != 0 → overflow → None
        let r_max = mul_shr(u128::MAX, u128::MAX, SCALE_OFFSET, false);
        assert!(r_max.is_none(), "u128::MAX * u128::MAX should overflow");
    }

    #[test]
    fn test_mul_shr_real_dlmm_price() {
        // Real-world: price of a bin far from 0 (bin_step=100, id=1000)
        // These prices have both high and low limbs set.
        let price = get_price_from_id(1000, 100).unwrap();
        let amount: u128 = 500_000_000; // 0.5 SOL in lamports

        let out_down = mul_shr(price, amount, SCALE_OFFSET, false).unwrap();
        let out_up = mul_shr(price, amount, SCALE_OFFSET, true).unwrap();

        // Lock in exact values — these MUST NOT change after refactor
        assert!(out_down > 0, "must produce output");
        assert!(out_up >= out_down, "round_up >= round_down");
        assert!(out_up - out_down <= 1, "rounding diff must be 0 or 1");

        // Hardcoded from U256 run — must match exactly after refactor
        assert_eq!(price, 386628180051795546469259);
        assert_eq!(out_down, 10479577818906);
        assert_eq!(out_up, 10479577818907);
    }

    #[test]
    fn test_shl_div_basic() {
        // (100 << 64) / ONE = 100 (dividing by 1.0 in Q64.64)
        let r = shl_div(100, ONE, SCALE_OFFSET, false).unwrap();
        assert_eq!(r, 100);
    }

    #[test]
    fn test_shl_div_round_up() {
        // (1 << 64) / 3 — not exact, rounding matters
        let down = shl_div(1, 3, SCALE_OFFSET, false).unwrap();
        let up = shl_div(1, 3, SCALE_OFFSET, true).unwrap();
        assert_eq!(up, down + 1, "round_up should be exactly 1 more");
        // exact: 2^64 / 3 = 6148914691236517205.333...
        assert_eq!(down, 6148914691236517205);
    }

    #[test]
    fn test_shl_div_with_real_price() {
        // Simulates the swap math: amount / price in Q64.64
        let price = get_price_from_id(500, 50).unwrap();
        let amount: u64 = 1_000_000_000;

        let r_down = shl_div(amount as u128, price, SCALE_OFFSET, false).unwrap();
        let r_up = shl_div(amount as u128, price, SCALE_OFFSET, true).unwrap();

        assert!(r_down > 0);
        assert!(r_up >= r_down);
        assert!(r_up - r_down <= 1);
    }

    #[test]
    fn test_shl_div_zero_denominator() {
        assert!(shl_div(100, 0, SCALE_OFFSET, false).is_none());
    }

    // ── End-to-end swap result lock-in ──
    // These capture full quote results so ANY internal math change is caught.

    #[test]
    fn test_quote_exact_in_multibin_lockdown() {
        // 3-bin pool with realistic prices, fees, and partial fills
        let pool = DlmmPool::new(
            100, 20, 50, 0, 40_000, 5_000, 2_000, 100, 350_000,
            &[
                DlmmBin {
                    id: 99,
                    amount_x: 0,
                    amount_y: 500_000_000,
                    price: get_price_from_id(99, 20).unwrap(),
                },
                DlmmBin {
                    id: 100,
                    amount_x: 200_000_000,
                    amount_y: 300_000_000,
                    price: get_price_from_id(100, 20).unwrap(),
                },
                DlmmBin {
                    id: 101,
                    amount_x: 400_000_000,
                    amount_y: 0,
                    price: get_price_from_id(101, 20).unwrap(),
                },
            ],
        );

        // swap_for_y: X→Y, walks down through bins 100, 99
        let (out_y, fee_y) = pool.quote_exact_in(250_000_000, true).unwrap();
        // swap_for_x: Y→X, walks up through bins 100, 101
        let (out_x, fee_x) = pool.quote_exact_in(250_000_000, false).unwrap();

        // These must be nonzero and sane
        assert!(out_y > 0 && fee_y > 0);
        assert!(out_x > 0 && fee_x > 0);

        // LOCK IN: run this test once with U256, then hardcode the values below.
        // After refactor, if any value shifts by even 1, the test fails.
        // Hardcoded with bins sorted [99, 100, 101] — must match exactly after refactor
        assert_eq!(out_y, 305274779, "swap_for_y out changed!");
        assert_eq!(fee_y, 3584, "swap_for_y fee changed!");
        assert_eq!(out_x, 204711194, "swap_for_x out changed!");
        assert_eq!(fee_x, 3611, "swap_for_x fee changed!");
    }
}
