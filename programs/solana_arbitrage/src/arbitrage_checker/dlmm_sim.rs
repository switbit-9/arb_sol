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
    // Account-backed bin access: bins are read from account data on demand.
    // acc_idx = index into the accounts[] slice passed to read_bin / quote methods.
    pub(crate) sfy_acc_idx: usize,
    pub(crate) sfx_acc_idx: usize,
    pub(crate) sfy_lower_bin_id: i32,
    pub(crate) sfx_lower_bin_id: i32,
    pub(crate) sfy_range: (u8, u8),  // (start, end) bin positions in the 70-slot array
    pub(crate) sfx_range: (u8, u8),
}

impl DlmmPool {
    /// Construct a pool with config only. Bin access is account-backed:
    /// call `set_bin_source()` to specify which accounts hold the bin arrays,
    /// then use `read_bin()` to read individual bins on demand.
    pub fn new_config(
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
            sfy_acc_idx: usize::MAX,
            sfx_acc_idx: usize::MAX,
            sfy_lower_bin_id: 0,
            sfx_lower_bin_id: 0,
            sfy_range: (0, 0),
            sfx_range: (0, 0),
        }
    }

    /// Set which account holds bin data for a given swap direction.
    /// `acc_idx`: index into the accounts[] slice.
    /// `lower_bin_id`: first bin id of the bin array (bin_array_index * 70).
    /// `start`, `end`: range of bin positions (0..70) to iterate.
    #[inline]
    pub fn set_bin_source(&mut self, swap_for_y: bool, acc_idx: usize, lower_bin_id: i32, start: u8, end: u8) {
        if swap_for_y {
            self.sfy_acc_idx = acc_idx;
            self.sfy_lower_bin_id = lower_bin_id;
            self.sfy_range = (start, end);
        } else {
            self.sfx_acc_idx = acc_idx;
            self.sfx_lower_bin_id = lower_bin_id;
            self.sfx_range = (start, end);
        }
    }

    /// Read a single bin at the given position from account data.
    /// `position` is an absolute index (0..69) within the 70-slot bin array.
    #[inline]
    pub fn read_bin(&self, accounts: &[crate::compat::AccountInfo], position: usize, swap_for_y: bool) -> Option<DlmmBin> {
        let (acc_idx, lower_bin_id) = if swap_for_y {
            (self.sfy_acc_idx, self.sfy_lower_bin_id)
        } else {
            (self.sfx_acc_idx, self.sfx_lower_bin_id)
        };
        if acc_idx >= accounts.len() { return None; }
        let data = accounts[acc_idx].try_borrow_data().ok()?;
        let inner = &data[8..]; // skip discriminator
        let base = 48 + position * 144;
        if base + 32 > inner.len() { return None; }
        let amount_x = u64::from_le_bytes(inner[base..base + 8].try_into().ok()?);
        let amount_y = u64::from_le_bytes(inner[base + 8..base + 16].try_into().ok()?);
        let price = u128::from_le_bytes(inner[base + 16..base + 32].try_into().ok()?);
        Some(DlmmBin {
            id: lower_bin_id + position as i32,
            amount_x,
            amount_y,
            price,
        })
    }

    /// Number of bin positions in the iteration range for a given direction.
    #[inline]
    pub fn bin_range(&self, swap_for_y: bool) -> (usize, usize) {
        let (s, e) = if swap_for_y { self.sfy_range } else { self.sfx_range };
        (s as usize, e as usize)
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

    /// Find starting cursor (bin position) for sequential bin walk from active_id.
    /// Cursor is an absolute position (0..69) within the 70-slot bin array.
    /// Returns the position of active_id (or nearest in the walk direction).
    #[inline]
    pub fn start_cursor(&self, swap_for_y: bool) -> Option<usize> {
        let (start, end) = self.bin_range(swap_for_y);
        if start >= end { return None; }
        let lower = if swap_for_y { self.sfy_lower_bin_id } else { self.sfx_lower_bin_id };
        let active_pos = (self.active_id - lower).max(0).min(69) as usize;
        if swap_for_y {
            // walking down: start at active_id position or top of range
            if active_pos >= start { Some(active_pos.min(end - 1)) }
            else { None }
        } else {
            // walking up: start at active_id position or bottom of range
            if active_pos < end { Some(active_pos.max(start)) }
            else { None }
        }
    }

    /// Advance cursor to next bin position in walk direction. Returns None if out of range.
    #[inline]
    pub fn advance_cursor(&self, cursor: usize, swap_for_y: bool) -> Option<usize> {
        let (start, end) = self.bin_range(swap_for_y);
        if swap_for_y {
            if cursor > start { Some(cursor - 1) } else { None }
        } else {
            let next = cursor + 1;
            if next < end { Some(next) } else { None }
        }
    }

    /// Simulate swap_exact_in: given `amount_in` of input token, return output amount.
    /// Reads bins on demand from account data via `read_bin`.
    /// Returns (amount_out, total_fee)
    pub fn quote_exact_in(&self, accounts: &[crate::compat::AccountInfo], amount_in: u64, swap_for_y: bool) -> Option<(u64, u64)> {
        if amount_in == 0 {
            return Some((0, 0));
        }

        let (_, range_end) = self.bin_range(swap_for_y);
        debug_eprintln!(
            "[dlmm_sim::quote_exact_in] amount_in={} swap_for_y={} active_id={} bin_step={} range_end={}",
            amount_in, swap_for_y, self.active_id, self.bin_step, range_end
        );

        let mut cursor = self.start_cursor(swap_for_y)?;
        let mut amount_left = amount_in;
        let mut total_out: u64 = 0;
        let mut total_fee: u64 = 0;
        let mut vol_acc = self.initial_vol_acc();
        let mut first_bin = true;

        debug_eprintln!("[dlmm_sim::quote_exact_in] start_cursor={}", cursor);

        loop {
            if amount_left == 0 { break; }
            let bin = match self.read_bin(accounts, cursor, swap_for_y) {
                Some(b) => b,
                None => break,
            };

            // Skip empty bins before updating volatility — matches old behavior
            // where only non-empty bins were in the array.
            let max_amount_out = if swap_for_y { bin.amount_y } else { bin.amount_x };
            if max_amount_out == 0 {
                cursor = match self.advance_cursor(cursor, swap_for_y) { Some(c) => c, None => break };
                continue;
            }

            if !first_bin {
                vol_acc = self.update_volatility_accumulator(vol_acc, bin.id);
            }
            first_bin = false;

            let total_fee_rate = self.get_total_fee(vol_acc);
            let price = bin.price;

            debug_eprintln!(
                "[dlmm_sim::quote_exact_in] bin[{}] id={} amt_x={} amt_y={} price={} max_out={} amount_left={} fee_rate={}",
                cursor, bin.id, bin.amount_x, bin.amount_y, bin.price, max_amount_out, amount_left, total_fee_rate
            );

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
    /// Reads bins on demand from account data via `read_bin`.
    /// Returns (amount_in_with_fees, total_fee)
    pub fn quote_exact_out(&self, accounts: &[crate::compat::AccountInfo], amount_out: u64, swap_for_y: bool) -> Option<(u64, u64)> {
        if amount_out == 0 {
            return Some((0, 0));
        }

        let mut cursor = self.start_cursor(swap_for_y)?;
        let mut remaining_out = amount_out;
        let mut total_in: u64 = 0;
        let mut total_fee: u64 = 0;
        let mut vol_acc = self.initial_vol_acc();
        let mut first_bin = true;

        loop {
            if remaining_out == 0 { break; }
            let bin = match self.read_bin(accounts, cursor, swap_for_y) {
                Some(b) => b,
                None => break,
            };

            let max_amount_out = if swap_for_y { bin.amount_y } else { bin.amount_x };
            if max_amount_out == 0 {
                cursor = match self.advance_cursor(cursor, swap_for_y) { Some(c) => c, None => break };
                continue;
            }

            if !first_bin {
                vol_acc = self.update_volatility_accumulator(vol_acc, bin.id);
            }
            first_bin = false;

            let total_fee_rate = self.get_total_fee(vol_acc);
            let price = bin.price;

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
    // offset 64: bump_seed (1 byte)
    // offset 65: bin_step_seed (2 bytes)
    // offset 67: pair_type (1 byte)
    // offset 68: active_id (4 bytes, i32)
    // offset 72: bin_step (2 bytes, u16)
    // ...

    // StaticParameters (at offset 0):
    // base_factor: u16 (offset 0)
    // filter_period: u16 (offset 2)
    // decay_period: u16 (offset 4)
    // reduction_factor: u16 (offset 6)
    // variable_fee_control: u32 (offset 8)
    // max_volatility_accumulator: u32 (offset 12)
    // ...
    // base_fee_power_factor: u8 (offset 26)
    // ...
    // protocol_share: u16 (offset 16)

    let base_factor = u16::from_le_bytes(d[0..2].try_into().ok()?);
    let variable_fee_control = u32::from_le_bytes(d[8..12].try_into().ok()?);
    let max_volatility_accumulator = u32::from_le_bytes(d[12..16].try_into().ok()?);
    let base_fee_power_factor = d[26];

    // VariableParameters (at offset 32):
    // volatility_accumulator: u32 (offset 0)
    // volatility_reference: u32 (offset 4)
    // index_reference: i32 (offset 8)
    // ...
    let volatility_accumulator = u32::from_le_bytes(d[32..36].try_into().ok()?);
    let volatility_reference = u32::from_le_bytes(d[36..40].try_into().ok()?);
    let index_reference = i32::from_le_bytes(d[40..44].try_into().ok()?);

    // active_id at offset 69, bin_step at offset 73
    let active_id = i32::from_le_bytes(d[68..72].try_into().ok()?);
    let bin_step = u16::from_le_bytes(d[72..74].try_into().ok()?);

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
    pub fn from_params(params: DlmmPoolParams) -> Self {
        Self::new_config(
            params.active_id,
            params.bin_step,
            params.base_factor,
            params.base_fee_power_factor,
            params.variable_fee_control,
            params.volatility_accumulator,
            params.volatility_reference,
            params.index_reference,
            params.max_volatility_accumulator,
        )
    }
}

/// Build raw bin array account data from a slice of DlmmBin (for tests).
/// Returns the byte buffer (8-byte discriminator + 48-byte header + 70 * 144-byte bins).
#[cfg(test)]
pub fn make_test_bin_array_data(bins: &[DlmmBin], bin_array_index: i64) -> Vec<u8> {
    let lower_bin_id = (bin_array_index as i32) * MAX_BIN_PER_ARRAY;
    let mut data = vec![0u8; 8 + 48 + 70 * 144];
    // bin_array_index at inner offset 0..8
    data[8..16].copy_from_slice(&bin_array_index.to_le_bytes());
    for bin in bins {
        let idx = (bin.id - lower_bin_id) as usize;
        if idx >= 70 { continue; }
        let off = 8 + 48 + idx * 144;
        data[off..off + 8].copy_from_slice(&bin.amount_x.to_le_bytes());
        data[off + 8..off + 16].copy_from_slice(&bin.amount_y.to_le_bytes());
        data[off + 16..off + 32].copy_from_slice(&bin.price.to_le_bytes());
    }
    data
}

/// Create a DlmmPool with bin sources set up for testing.
/// Both sfy and sfx point to the same account indices (0 for sfy, 1 for sfx).
/// Range covers the bins provided (around active_id, up to 10 in each direction).
#[cfg(test)]
pub fn make_test_dlmm_pool(
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
    bin_array_index: i64,
) -> DlmmPool {
    let lower_bin_id = (bin_array_index as i32) * MAX_BIN_PER_ARRAY;
    let active_pos = (active_id - lower_bin_id).max(0).min(69) as u8;
    // sfy (walk down): range from active_pos-9 to active_pos+1
    let sfy_end = (active_pos + 1).min(70);
    let sfy_start = sfy_end.saturating_sub(10);
    // sfx (walk up): range from active_pos to active_pos+10
    let sfx_start = active_pos;
    let sfx_end = (active_pos + 10).min(70);

    let mut pool = DlmmPool::new_config(
        active_id, bin_step, base_factor, base_fee_power_factor,
        variable_fee_control, volatility_accumulator, volatility_reference,
        index_reference, max_volatility_accumulator,
    );
    pool.set_bin_source(true, 0, lower_bin_id, sfy_start, sfy_end);
    pool.set_bin_source(false, 1, lower_bin_id, sfx_start, sfx_end);
    pool
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

    /// Helper: create AccountInfo array from bin data for testing.
    fn make_test_accounts(bins: &[DlmmBin], bin_array_index: i64) -> (Vec<u8>, Vec<u8>) {
        let d1 = super::make_test_bin_array_data(bins, bin_array_index);
        let d2 = super::make_test_bin_array_data(bins, bin_array_index);
        (d1, d2)
    }

    #[test]
    fn test_simple_swap_single_bin() {
        // Create a pool with a single bin at id=0, bin_step=100 (1%)
        // Bin has 1000 Y tokens, price=1.0 (Q64.64 = ONE)
        let bins = [DlmmBin {
            id: 0,
            amount_x: 0,
            amount_y: 1_000_000_000,
            price: ONE,
        }];
        let (mut d1, mut d2) = make_test_accounts(&bins, 0);
        let key1 = solana_program::pubkey::Pubkey::default();
        let key2 = solana_program::pubkey::Pubkey::default();
        let owner = solana_program::pubkey::Pubkey::default();
        let mut l1 = 0u64;
        let mut l2 = 0u64;
        let acc1 = solana_program::account_info::AccountInfo::new(&key1, false, false, &mut l1, &mut d1, &owner, false, 0);
        let acc2 = solana_program::account_info::AccountInfo::new(&key2, false, false, &mut l2, &mut d2, &owner, false, 0);
        let accounts = [acc1, acc2];

        let pool = super::make_test_dlmm_pool(0, 100, 10, 0, 0, 0, 0, 0, 0, &bins, 0);

        // Swap 100M X for Y (swap_for_y=true)
        let (out, fee) = pool.quote_exact_in(&accounts, 100_000_000, true).unwrap();
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
        let bins = [
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
        ];
        // bin_array_index = 100 / 70 = 1, lower_bin_id = 70
        let bin_array_index = 1i64;
        let (mut d1, mut d2) = make_test_accounts(&bins, bin_array_index);
        let key1 = solana_program::pubkey::Pubkey::default();
        let key2 = solana_program::pubkey::Pubkey::default();
        let owner = solana_program::pubkey::Pubkey::default();
        let mut l1 = 0u64;
        let mut l2 = 0u64;
        let acc1 = solana_program::account_info::AccountInfo::new(&key1, false, false, &mut l1, &mut d1, &owner, false, 0);
        let acc2 = solana_program::account_info::AccountInfo::new(&key2, false, false, &mut l2, &mut d2, &owner, false, 0);
        let accounts = [acc1, acc2];

        let pool = super::make_test_dlmm_pool(
            100, 20, 50, 0, 40_000, 5_000, 2_000, 100, 350_000,
            &bins, bin_array_index,
        );

        // swap_for_y: X→Y, walks down through bins 100, 99
        let (out_y, fee_y) = pool.quote_exact_in(&accounts, 250_000_000, true).unwrap();
        // swap_for_x: Y→X, walks up through bins 100, 101
        let (out_x, fee_x) = pool.quote_exact_in(&accounts, 250_000_000, false).unwrap();

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
