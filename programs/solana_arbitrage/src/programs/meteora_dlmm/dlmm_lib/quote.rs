use super::*;
use crate::compat::*;
use crate::utils::token::MintFee;
use std::result::Result::Ok;


#[derive(Debug)]
pub struct SwapExactInQuote {
    pub amount_out: u64,
    pub fee: u64,
}

#[derive(Debug)]
pub struct SwapExactOutQuote {
    pub amount_in: u64,
    pub fee: u64,
}

/// Slim copy of LbPair with only the fields needed for swap simulation (~28 bytes vs 896).
/// Created once in MeteoraDlmm::new(), used by quote_exact_in/out.
#[derive(Clone, Copy, Debug)]
pub struct LbPairSlim {
    pub active_id: i32,
    pub bin_step: u16,
    pub volatility_accumulator: u32,
    pub volatility_reference: u32,
    pub index_reference: i32,
    pub max_vol_acc: u32,
    pub variable_fee_control: u32,
}

/// Pre-computed values cached at instance level to avoid redundant work across swap calls.
/// All fields are constant for a given pool within a single transaction.
#[derive(Clone, Debug)]
pub struct SwapCache {
    /// get_base_fee() result — depends only on constant pool params
    pub base_fee: u128,
    /// ONE + (bin_step << 64) / BASIS_POINT_MAX — constant multiplier between adjacent bins
    pub price_base: u128,
    /// Bin array index for the single bin array in this direction
    pub bin_array_index: i64,
    /// Pre-computed volatility_accumulator after update_references (initial state)
    pub initial_vol_acc: u32,
    /// Whether variable_fee_control > 0 (skip variable fee computation entirely if false)
    pub has_variable_fee: bool,
}

#[inline(always)]
fn apply_transfer_fee_cached(fee: MintFee, amount: u64) -> u64 {
    if fee.bps == 0 { return 0; }
    let calculated = ((amount as u128) * (fee.bps as u128) / 10_000) as u64;
    if fee.max > 0 && calculated > fee.max { fee.max } else { calculated }
}

#[inline(always)]
fn calculate_transfer_fee_included_amount_cached(
    fee: MintFee,
    transfer_fee_excluded_amount: u64,
) -> anyhow::Result<u64> {
    if fee.bps == 0 || transfer_fee_excluded_amount == 0 {
        return Ok(transfer_fee_excluded_amount);
    }
    let denom = 10_000u64.saturating_sub(fee.bps as u64);
    if denom == 0 { return Ok(transfer_fee_excluded_amount); }
    let numer = (transfer_fee_excluded_amount as u128) * (fee.bps as u128);
    let transfer_fee = ((numer + denom as u128 - 1) / denom as u128) as u64;
    let capped = if fee.max > 0 && transfer_fee > fee.max { fee.max } else { transfer_fee };
    Ok(transfer_fee_excluded_amount.checked_add(capped).context("Overflow")?)
}

#[allow(clippy::too_many_arguments)]
pub fn quote_exact_out<'a>(
    lb_pair: &LbPairSlim,
    mut amount_out: u64,
    swap_for_y: bool,
    bin_array: &AccountInfo<'a>,
    transfer_fee_x: MintFee,
    transfer_fee_y: MintFee,
    cache: &SwapCache,
) -> anyhow::Result<SwapExactOutQuote> {
    let mut slim = *lb_pair;

    let mut total_amount_in: u64 = 0;
    let mut total_fee: u64 = 0;

    let (in_fee, out_fee) = if swap_for_y {
        (transfer_fee_x, transfer_fee_y)
    } else {
        (transfer_fee_y, transfer_fee_x)
    };

    amount_out = calculate_transfer_fee_included_amount_cached(out_fee, amount_out)?;

    let mut prev_price: Option<u128> = None;
    slim.volatility_accumulator = cache.initial_vol_acc;
    let mut first_bin = true;

    const BIN_ARRAY_HEADER_SIZE: usize = 56;
    const BIN_SIZE: usize = 144;

    while amount_out > 0 {
        let needed_index = BinArray::bin_id_to_bin_array_index(slim.active_id)? as i64;
        if needed_index != cache.bin_array_index {
            if total_amount_in == 0 {
                return Err(anyhow::anyhow!(
                    "Insufficient liquidity: required bin array not available"
                ));
            }
            break;
        }

        let bin_array_data = bin_array.try_borrow_data()?;
        let bin_array_index: i64 = bytemuck::pod_read_unaligned(&bin_array_data[8..16]);
        let (lower_bin_id, upper_bin_id) =
            BinArray::get_bin_array_lower_upper_bin_id(bin_array_index as i32)?;

        let lb_pair_bin_array_index = BinArray::bin_id_to_bin_array_index(slim.active_id)?;
        if i64::from(lb_pair_bin_array_index) != bin_array_index {
            if swap_for_y {
                slim.active_id = upper_bin_id;
            } else {
                slim.active_id = lower_bin_id;
            }
            prev_price = None;
        }

        loop {
            if amount_out == 0 { break; }
            if slim.active_id < lower_bin_id || slim.active_id > upper_bin_id { break; }

            if first_bin {
                first_bin = false;
            } else {
                // Inlined update_volatility_accumulator
                let delta_id = (i64::from(slim.index_reference) - i64::from(slim.active_id)).unsigned_abs();
                let va = u64::from(slim.volatility_reference)
                    .checked_add(delta_id.checked_mul(BASIS_POINT_MAX as u64).context("overflow")?)
                    .context("overflow")?;
                slim.volatility_accumulator = std::cmp::min(va, slim.max_vol_acc.into())
                    .try_into().context("overflow")?;
            }

            // Compute total_fee_rate with cached base_fee
            let total_fee_rate = if cache.has_variable_fee {
                // Inlined compute_variable_fee
                let va: u128 = slim.volatility_accumulator.into();
                let bs: u128 = slim.bin_step.into();
                let vfc: u128 = slim.variable_fee_control.into();
                let sq = va.checked_mul(bs).context("overflow")?.checked_pow(2).context("overflow")?;
                let variable_fee = vfc.checked_mul(sq).context("overflow")?
                    .checked_add(99_999_999_999).context("overflow")?
                    .checked_div(100_000_000_000).context("overflow")?;
                std::cmp::min(cache.base_fee + variable_fee, MAX_FEE_RATE.into())
            } else {
                std::cmp::min(cache.base_fee, MAX_FEE_RATE.into())
            };

            let bin_index_in_array: i32 = slim.active_id
                .checked_sub(lower_bin_id)
                .context("MathOverflow")?;
            let bin_offset = BIN_ARRAY_HEADER_SIZE + (bin_index_in_array as usize * BIN_SIZE);

            let mut active_bin: Bin =
                bytemuck::pod_read_unaligned(&bin_array_data[bin_offset..bin_offset + BIN_SIZE]);

            // Incremental price computation
            let price = if active_bin.price != 0 {
                let p = active_bin.price;
                prev_price = Some(p);
                p
            } else if let Some(prev) = prev_price {
                let p = if swap_for_y {
                    shl_div(prev, cache.price_base, SCALE_OFFSET, Rounding::Down)
                        .unwrap_or_else(|| get_price_from_id(slim.active_id, slim.bin_step).unwrap_or(0))
                } else {
                    mul_shr(prev, cache.price_base, SCALE_OFFSET, Rounding::Down)
                        .unwrap_or_else(|| get_price_from_id(slim.active_id, slim.bin_step).unwrap_or(0))
                };
                active_bin.price = p;
                prev_price = Some(p);
                p
            } else {
                let p = get_price_from_id(slim.active_id, slim.bin_step)?;
                active_bin.price = p;
                prev_price = Some(p);
                p
            };

            if !active_bin.is_empty(!swap_for_y) {
                let bin_max_amount_out = active_bin.get_max_amount_out(swap_for_y);

                let denominator_fee = u128::from(FEE_PRECISION)
                    .checked_sub(total_fee_rate)
                    .context("MathOverflow")?;

                if amount_out >= bin_max_amount_out {
                    let max_amount_in = active_bin.get_max_amount_in(price, swap_for_y)?;
                    let max_fee = {
                        let f = u128::from(max_amount_in)
                            .checked_mul(total_fee_rate).context("MathOverflow")?
                            .checked_add(denominator_fee).context("MathOverflow")?
                            .checked_sub(1).context("MathOverflow")?;
                        u64::try_from(f.checked_div(denominator_fee).context("MathOverflow")?).context("MathOverflow")?
                    };

                    total_amount_in = total_amount_in.checked_add(max_amount_in).context("MathOverflow")?;
                    total_fee = total_fee.checked_add(max_fee).context("MathOverflow")?;
                    amount_out = amount_out.checked_sub(bin_max_amount_out).context("MathOverflow")?;
                } else {
                    let amount_in = Bin::get_amount_in(amount_out, price, swap_for_y)?;
                    let fee = {
                        let f = u128::from(amount_in)
                            .checked_mul(total_fee_rate).context("MathOverflow")?
                            .checked_add(denominator_fee).context("MathOverflow")?
                            .checked_sub(1).context("MathOverflow")?;
                        u64::try_from(f.checked_div(denominator_fee).context("MathOverflow")?).context("MathOverflow")?
                    };

                    total_amount_in = total_amount_in.checked_add(amount_in).context("MathOverflow")?;
                    total_fee = total_fee.checked_add(fee).context("MathOverflow")?;
                    amount_out = 0;
                }
            }

            if amount_out > 0 {
                // Inlined advance_active_bin
                slim.active_id = if swap_for_y {
                    slim.active_id.checked_sub(1)
                } else {
                    slim.active_id.checked_add(1)
                }.context("overflow")?;
                ensure!(slim.active_id >= MIN_BIN_ID && slim.active_id <= MAX_BIN_ID, "Insufficient liquidity");
            }
        }
    }

    total_amount_in = total_amount_in.checked_add(total_fee).context("MathOverflow")?;
    total_amount_in =
        calculate_transfer_fee_included_amount_cached(in_fee, total_amount_in)?;

    Ok(SwapExactOutQuote {
        amount_in: total_amount_in,
        fee: total_fee,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn quote_exact_in<'a>(
    lb_pair: &LbPairSlim,
    amount_in: u64,
    swap_for_y: bool,
    bin_array: &AccountInfo<'a>,
    transfer_fee_x: MintFee,
    transfer_fee_y: MintFee,
    cache: &SwapCache,
) -> anyhow::Result<SwapExactInQuote> {
    let mut slim = *lb_pair;
    let mut total_amount_out: u64 = 0;
    let mut total_fee: u64 = 0;

    let (in_fee, out_fee) = if swap_for_y {
        (transfer_fee_x, transfer_fee_y)
    } else {
        (transfer_fee_y, transfer_fee_x)
    };

    let fee = apply_transfer_fee_cached(in_fee, amount_in);
    let transfer_fee_excluded_amount_in = amount_in.checked_sub(fee).context("MathOverflow")?;
    let mut amount_left = transfer_fee_excluded_amount_in;

    debug_eprintln!(
        "[quote_exact_in] amount_in={} swap_for_y={} active_id={} bin_step={} transfer_fee={} amount_after_fee={}",
        amount_in, swap_for_y, lb_pair.active_id, lb_pair.bin_step, fee, amount_left
    );

    const BIN_ARRAY_HEADER_SIZE: usize = 56;
    const BIN_SIZE: usize = 144;

    let mut prev_price: Option<u128> = None;
    slim.volatility_accumulator = cache.initial_vol_acc;
    let mut first_bin = true;

    while amount_left > 0 {
        let needed_index = BinArray::bin_id_to_bin_array_index(slim.active_id)? as i64;
        if needed_index != cache.bin_array_index {
            if total_amount_out == 0 {
                return Err(anyhow::anyhow!(
                    "Insufficient liquidity: required bin array not available"
                ));
            }
            break;
        }

        let bin_array_data = bin_array.try_borrow_data()?;
        let bin_array_index: i64 = bytemuck::pod_read_unaligned(&bin_array_data[8..16]);
        let (lower_bin_id, upper_bin_id) =
            BinArray::get_bin_array_lower_upper_bin_id(bin_array_index as i32)?;

        let lb_pair_bin_array_index = BinArray::bin_id_to_bin_array_index(slim.active_id)?;
        if i64::from(lb_pair_bin_array_index) != bin_array_index {
            if swap_for_y {
                slim.active_id = upper_bin_id;
            } else {
                slim.active_id = lower_bin_id;
            }
            prev_price = None;
        }

        loop {
            if amount_left == 0 { break; }
            if slim.active_id < lower_bin_id || slim.active_id > upper_bin_id { break; }

            if first_bin {
                first_bin = false;
            } else {
                // Inlined update_volatility_accumulator
                let delta_id = (i64::from(slim.index_reference) - i64::from(slim.active_id)).unsigned_abs();
                let va = u64::from(slim.volatility_reference)
                    .checked_add(delta_id.checked_mul(BASIS_POINT_MAX as u64).context("overflow")?)
                    .context("overflow")?;
                slim.volatility_accumulator = std::cmp::min(va, slim.max_vol_acc.into())
                    .try_into().context("overflow")?;
            }

            // Compute total_fee_rate with cached base_fee
            let total_fee_rate = if cache.has_variable_fee {
                // Inlined compute_variable_fee
                let va: u128 = slim.volatility_accumulator.into();
                let bs: u128 = slim.bin_step.into();
                let vfc: u128 = slim.variable_fee_control.into();
                let sq = va.checked_mul(bs).context("overflow")?.checked_pow(2).context("overflow")?;
                let variable_fee = vfc.checked_mul(sq).context("overflow")?
                    .checked_add(99_999_999_999).context("overflow")?
                    .checked_div(100_000_000_000).context("overflow")?;
                std::cmp::min(cache.base_fee + variable_fee, MAX_FEE_RATE.into())
            } else {
                std::cmp::min(cache.base_fee, MAX_FEE_RATE.into())
            };

            let bin_index_in_array: i32 = slim.active_id
                .checked_sub(lower_bin_id)
                .context("MathOverflow")?;
            let bin_offset = BIN_ARRAY_HEADER_SIZE + (bin_index_in_array as usize * BIN_SIZE);

            let mut active_bin: Bin =
                bytemuck::pod_read_unaligned(&bin_array_data[bin_offset..bin_offset + BIN_SIZE]);

            // Incremental price computation
            let price = if active_bin.price != 0 {
                let p = active_bin.price;
                prev_price = Some(p);
                p
            } else if let Some(prev) = prev_price {
                let p = if swap_for_y {
                    shl_div(prev, cache.price_base, SCALE_OFFSET, Rounding::Down)
                        .unwrap_or_else(|| get_price_from_id(slim.active_id, slim.bin_step).unwrap_or(0))
                } else {
                    mul_shr(prev, cache.price_base, SCALE_OFFSET, Rounding::Down)
                        .unwrap_or_else(|| get_price_from_id(slim.active_id, slim.bin_step).unwrap_or(0))
                };
                active_bin.price = p;
                prev_price = Some(p);
                p
            } else {
                let p = get_price_from_id(slim.active_id, slim.bin_step)?;
                active_bin.price = p;
                prev_price = Some(p);
                p
            };

            debug_eprintln!(
                "[quote_exact_in] Bin {}: amount_x={} amount_y={} price={} empty={} amount_left={}",
                slim.active_id,
                active_bin.amount_x,
                active_bin.amount_y,
                price,
                active_bin.is_empty(!swap_for_y),
                amount_left,
            );

            if !active_bin.is_empty(!swap_for_y) {
                let max_amount_out = active_bin.get_max_amount_out(swap_for_y);
                let mut max_amount_in = active_bin.get_max_amount_in(price, swap_for_y)?;

                let denominator_fee = u128::from(FEE_PRECISION)
                    .checked_sub(total_fee_rate)
                    .context("MathOverflow")?;
                let max_fee = {
                    let f = u128::from(max_amount_in)
                        .checked_mul(total_fee_rate).context("MathOverflow")?
                        .checked_add(denominator_fee).context("MathOverflow")?
                        .checked_sub(1).context("MathOverflow")?;
                    u64::try_from(f.checked_div(denominator_fee).context("MathOverflow")?).context("MathOverflow")?
                };
                max_amount_in = max_amount_in.checked_add(max_fee).context("MathOverflow")?;

                let (amount_in_with_fees, amount_out, bin_fee) = if amount_left > max_amount_in {
                    (max_amount_in, max_amount_out, max_fee)
                } else {
                    let fee_amt = {
                        let f = u128::from(amount_left)
                            .checked_mul(total_fee_rate).context("MathOverflow")?
                            .checked_add((FEE_PRECISION - 1).into()).context("MathOverflow")?;
                        u64::try_from(f.checked_div(FEE_PRECISION.into()).context("MathOverflow")?).context("MathOverflow")?
                    };
                    let amount_in_after_fee = amount_left.checked_sub(fee_amt).context("MathOverflow")?;
                    let amt_out = Bin::get_amount_out(amount_in_after_fee, price, swap_for_y)?;
                    (amount_left, std::cmp::min(amt_out, max_amount_out), fee_amt)
                };

                debug_eprintln!(
                    "[quote_exact_in] Bin {}: in={} out={} fee={} | remaining={}",
                    slim.active_id,
                    amount_in_with_fees,
                    amount_out,
                    bin_fee,
                    amount_left.saturating_sub(amount_in_with_fees),
                );

                amount_left = amount_left.checked_sub(amount_in_with_fees).context("MathOverflow")?;
                total_amount_out = total_amount_out.checked_add(amount_out).context("MathOverflow")?;
                total_fee = total_fee.checked_add(bin_fee).context("MathOverflow")?;

                if amount_left > 0 {
                    // Inlined advance_active_bin
                    slim.active_id = if swap_for_y {
                        slim.active_id.checked_sub(1)
                    } else {
                        slim.active_id.checked_add(1)
                    }.context("overflow")?;
                    ensure!(slim.active_id >= MIN_BIN_ID && slim.active_id <= MAX_BIN_ID, "Insufficient liquidity");
                }
            } else {
                let old_active_id = slim.active_id;
                // Inlined advance_active_bin
                slim.active_id = if swap_for_y {
                    slim.active_id.checked_sub(1)
                } else {
                    slim.active_id.checked_add(1)
                }.context("overflow")?;
                ensure!(slim.active_id >= MIN_BIN_ID && slim.active_id <= MAX_BIN_ID, "Insufficient liquidity");
                if slim.active_id == old_active_id { break; }
                if slim.active_id < lower_bin_id || slim.active_id > upper_bin_id { break; }
            }
        }
    }

    let fee = apply_transfer_fee_cached(out_fee, total_amount_out);
    let transfer_fee_excluded_amount_out =
        total_amount_out.checked_sub(fee).context("MathOverflow")?;
    debug_eprintln!(
        "[quote_exact_in] RESULT: amount_in={} total_out={} out_transfer_fee={} final_out={} total_fee={}",
        amount_in, total_amount_out, fee, transfer_fee_excluded_amount_out, total_fee
    );
    Ok(SwapExactInQuote {
        amount_out: transfer_fee_excluded_amount_out,
        fee: total_fee,
    })
}

pub fn get_bin_array_pubkeys_for_swap(
    lb_pair_pubkey: Pubkey,
    lb_pair: &LbPair,
    bitmap_extension: Option<&BinArrayBitmapExtension>,
    swap_for_y: bool,
    take_count: u8,
) -> anyhow::Result<Vec<Pubkey>> {
    let mut start_bin_array_idx = BinArray::bin_id_to_bin_array_index(lb_pair.active_id)?;
    let mut bin_array_idx = vec![];
    let increment = if swap_for_y { -1 } else { 1 };

    loop {
        if bin_array_idx.len() == take_count as usize {
            break;
        }

        if lb_pair.is_overflow_default_bin_array_bitmap(start_bin_array_idx) {
            let Some(bitmap_extension) = bitmap_extension else {
                break;
            };
            match bitmap_extension
                .next_bin_array_index_with_liquidity(swap_for_y, start_bin_array_idx)
            {
                Ok((next_bin_array_idx, has_liquidity)) => {
                    if has_liquidity {
                        bin_array_idx.push(next_bin_array_idx);
                        start_bin_array_idx = next_bin_array_idx + increment;
                    } else {
                        // Switch to internal bitmap
                        start_bin_array_idx = next_bin_array_idx;
                    }
                }
                Err(_) => {
                    // Out of search range. No liquidity.
                    break;
                }
            }
        } else {
            match lb_pair
                .next_bin_array_index_with_liquidity_internal(swap_for_y, start_bin_array_idx)
            {
                Ok((next_bin_array_idx, has_liquidity)) => {
                    if has_liquidity {
                        bin_array_idx.push(next_bin_array_idx);
                        start_bin_array_idx = next_bin_array_idx + increment;
                    } else {
                        // Switch to external bitmap
                        start_bin_array_idx = next_bin_array_idx;
                    }
                }
                Err(_) => {
                    break;
                }
            }
        }
    }

    let bin_array_pubkeys = bin_array_idx
        .into_iter()
        .map(|idx| derive_bin_array_pda(lb_pair_pubkey, idx.into()).0)
        .collect();

    Ok(bin_array_pubkeys)
}

pub fn get_active_bin_array<'a>(
    lb_pair_pubkey: Pubkey,
    lb_pair: &LbPair,
    bitmap_extension: Option<&BinArrayBitmapExtension>,
    swap_for_y: bool,
    bin_arrays: &[AccountInfo<'a>],
) -> anyhow::Result<Bin> {
    const BIN_ARRAY_HEADER_SIZE: usize = 56;
    const BIN_SIZE: usize = 144;

    let active_bin_array_pubkey =
        get_bin_array_pubkeys_for_swap(lb_pair_pubkey, lb_pair, bitmap_extension, swap_for_y, 1)?
            .pop()
            .context("Pool out of liquidity")?;

    let active_bin_array_account = bin_arrays
        .iter()
        .find(|acc| *acc.key == active_bin_array_pubkey)
        .context("Active bin array not found in provided accounts")?;

    // Verify the active bin is in this bin array by reading the bin array index
    let bin_array_data = active_bin_array_account.try_borrow_data()?;
    let bin_array_index: i64 = bytemuck::pod_read_unaligned(&bin_array_data[8..16]);

    // Calculate the bin range for this bin array
    let (lower_bin_id, upper_bin_id) =
        BinArray::get_bin_array_lower_upper_bin_id(bin_array_index as i32)?;

    // Verify active_id is within this bin array's range
    ensure!(
        lb_pair.active_id >= lower_bin_id && lb_pair.active_id <= upper_bin_id,
        "Active bin ID is not within the active bin array range"
    );

    // Calculate bin index within array
    let bin_index_in_array: i32 = lb_pair
        .active_id
        .checked_sub(lower_bin_id)
        .context("MathOverflow")?;
    let bin_index_usize = bin_index_in_array as usize;

    // Calculate bin offset
    let bin_offset = BIN_ARRAY_HEADER_SIZE + (bin_index_usize * BIN_SIZE);

    // Verify we have enough data
    ensure!(
        bin_array_data.len() >= bin_offset + BIN_SIZE,
        "Bin array data too short"
    );

    // Read single bin from account data
    let mut active_bin: Bin =
        bytemuck::pod_read_unaligned(&bin_array_data[bin_offset..bin_offset + BIN_SIZE]);

    // Store the price in the bin if not already set
    let _price = active_bin.get_or_store_bin_price(lb_pair.active_id, lb_pair.bin_step)?;

    // Return the active bin
    Ok(active_bin)
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use crate::token::load_mint;
//     use anchor_client::solana_sdk::clock::Clock;
//     use anchor_client::Cluster;
//     use anchor_lang::prelude::*;
//     use anchor_lang::solana_program::account_info::AccountInfo;
//     use anchor_lang::solana_program::program_pack::Pack;
//     use anchor_spl::token_interface::Mint;
//     use solana_client::nonblocking::rpc_client::RpcClient;
//     use solana_sdk::pubkey::Pubkey;

//     /// Get on chain clock
//     async fn get_clock(rpc_client: RpcClient) -> anyhow::Result<Clock> {
//         let clock_account = rpc_client
//             .get_account(&anchor_client::solana_sdk::sysvar::clock::ID)
//             .await?;

//         let clock_state: Clock = bincode::deserialize(clock_account.data.as_ref())?;

//         Ok(clock_state)
//     }

//     /// Convert raw RPC account to InterfaceAccount<Mint>
//     fn account_to_interface_mint(
//         account: solana_sdk::account::Account,
//         pubkey: Pubkey,
//     ) -> InterfaceAccount<'static, Mint> {
//         let data = Box::leak(Box::new(account.data));
//         let lamports = Box::leak(Box::new(account.lamports));
//         let owner = Box::leak(Box::new(account.owner));
//         let key = Box::leak(Box::new(pubkey));

//         // Create AccountInfo with 'static lifetime
//         let account_info: &'static AccountInfo<'static> = Box::leak(Box::new(AccountInfo::new(
//             key, false, false, lamports, data, owner, false, 0,
//         )));

//         // Create InterfaceAccount from AccountInfo
//         // Since AccountInfo is 'static, InterfaceAccount will also be 'static
//         InterfaceAccount::<Mint>::try_from(account_info).expect("Failed to create InterfaceAccount")
//     }

//     /// Convert solana_sdk::account::Account to AccountInfo
//     fn account_to_account_info(
//         key: Pubkey,
//         account: solana_sdk::account::Account,
//     ) -> AccountInfo<'static> {
//         let data = Box::leak(Box::new(account.data));
//         let lamports = Box::leak(Box::new(account.lamports));
//         let owner_bytes: [u8; 32] = account.owner.to_bytes();
//         let owner = Pubkey::try_from(owner_bytes.as_ref()).unwrap();
//         let owner_static = Box::leak(Box::new(owner));
//         let key_static = Box::leak(Box::new(key));
//         AccountInfo::new(
//             key_static,
//             false, // is_signer
//             false, // is_writable
//             lamports,
//             data,
//             owner_static,
//             account.executable,
//             account.rent_epoch,
//         )
//     }

//     // #[tokio::test]
//     // async fn test_swap_quote_exact_out() {
//     //     // RPC client. No gPA is required.
//     //     let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

//     //     let sol_usdc = Pubkey::from_str_const("HTvjzsfX3yU6BUodCjZ5vZkUrAxMDTrBs3CJaq43ashR");

//     //     let lb_pair_account = rpc_client.get_account(&sol_usdc).await.unwrap();

//     //     let lb_pair: LbPair = bytemuck::pod_read_unaligned(&lb_pair_account.data[8..]);

//     //     let mut mint_accounts = rpc_client
//     //         .get_multiple_accounts(&[lb_pair.token_x_mint, lb_pair.token_y_mint])
//     //         .await
//     //         .unwrap();

//     //     let mint_x_account =
//     //         account_to_interface_mint(mint_accounts[0].take().unwrap(), lb_pair.token_x_mint);
//     //     let mint_y_account =
//     //         account_to_interface_mint(mint_accounts[1].take().unwrap(), lb_pair.token_y_mint);

//     //     // 3 bin arrays to left, and right is enough to cover most of the swap, and stay under 1.4m CU constraint.
//     //     // Get 3 bin arrays to the left from the active bin
//     //     let left_bin_array_pubkeys =
//     //         get_bin_array_pubkeys_for_swap(sol_usdc, &lb_pair, None, true, 3).unwrap();

//     //     // Get 3 bin arrays to the right the from active bin
//     //     let right_bin_array_pubkeys =
//     //         get_bin_array_pubkeys_for_swap(sol_usdc, &lb_pair, None, false, 3).unwrap();

//     //     // Fetch bin arrays
//     //     let bin_array_pubkeys = left_bin_array_pubkeys
//     //         .into_iter()
//     //         .chain(right_bin_array_pubkeys.into_iter())
//     //         .collect::<Vec<Pubkey>>();

//     //     let accounts = rpc_client
//     //         .get_multiple_accounts(&bin_array_pubkeys)
//     //         .await
//     //         .unwrap();

//     //     // Create HashMap for quote_exact_out (which still uses HashMap)
//     //     let bin_arrays = accounts
//     //         .iter()
//     //         .zip(bin_array_pubkeys.iter())
//     //         .filter_map(|(account_opt, key)| {
//     //             account_opt
//     //                 .as_ref()
//     //                 .map(|account| (*key, bytemuck::pod_read_unaligned(&account.data[8..])))
//     //         })
//     //         .collect::<HashMap<_, _>>();

//     //     // Create Vec<AccountInfo> for quote_exact_in (stack-safe approach)
//     //     let bin_array_account_infos: Vec<AccountInfo> = accounts
//     //         .into_iter()
//     //         .zip(bin_array_pubkeys.into_iter())
//     //         .filter_map(|(account_opt, key)| {
//     //             account_opt.map(|account| account_to_account_info(key, account))
//     //         })
//     //         .collect();

//     //     let usdc_token_multiplier = 1_000_000.0;
//     //     let sol_token_multiplier = 1_000_000_000.0;

//     //     let out_sol_amount = 1_000_000_000;
//     //     let clock = get_clock(rpc_client).await.unwrap();

//     //     let quote_result = quote_exact_out(
//     //         sol_usdc,
//     //         &lb_pair,
//     //         out_sol_amount,
//     //         false,
//     //         &bin_arrays,
//     //         None,
//     //         &clock,
//     //         &mint_x_account,
//     //         &mint_y_account,
//     //     )
//     //     .unwrap();

//     //     let in_amount = quote_result.amount_in + quote_result.fee;

//     //     println!(
//     //         "{} USDC -> exact 1 SOL",
//     //         in_amount as f64 / usdc_token_multiplier
//     //     );

//     //     let quote_result = quote_exact_in(
//     //         sol_usdc,
//     //         &lb_pair,
//     //         in_amount,
//     //         false,
//     //         bin_array_account_infos.clone(),
//     //         None,
//     //         &clock,
//     //         &mint_x_account,
//     //         &mint_y_account,
//     //     )
//     //     .unwrap();

//     //     println!(
//     //         "{} USDC -> {} SOL",
//     //         in_amount as f64 / usdc_token_multiplier,
//     //         quote_result.amount_out as f64 / sol_token_multiplier
//     //     );

//     //     let out_usdc_amount = 200_000_000;

//     //     let quote_result = quote_exact_out(
//     //         sol_usdc,
//     //         &lb_pair,
//     //         out_usdc_amount,
//     //         true,
//     //         &bin_arrays,
//     //         None,
//     //         &clock,
//     //         &mint_x_account,
//     //         &mint_y_account,
//     //     )
//     //     .unwrap();

//     //     let in_amount = quote_result.amount_in + quote_result.fee;

//     //     println!(
//     //         "{} SOL -> exact 200 USDC",
//     //         in_amount as f64 / sol_token_multiplier
//     //     );

//     //     let quote_result = quote_exact_in(
//     //         sol_usdc,
//     //         &lb_pair,
//     //         in_amount,
//     //         true,
//     //         bin_array_account_infos,
//     //         None,
//     //         &clock,
//     //         &mint_x_account,
//     //         &mint_y_account,
//     //     )
//     //     .unwrap();

//     //     println!(
//     //         "{} SOL -> {} USDC",
//     //         in_amount as f64 / sol_token_multiplier,
//     //         quote_result.amount_out as f64 / usdc_token_multiplier
//     //     );
//     // }

//     // #[tokio::test]
//     // async fn test_swap_quote_exact_in() {
//     //     // RPC client. No gPA is required.
//     //     let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

//     //     let sol_usdc = Pubkey::from_str_const("8ztFxjFPfVUtEf4SLSapcFj8GW2dxyUA9no2bLPq7H7V");

//     //     let lb_pair_account = rpc_client.get_account(&sol_usdc).await.unwrap();

//     //     let lb_pair: LbPair = bytemuck::pod_read_unaligned(&lb_pair_account.data[8..]);

//     //     let mut mint_accounts = rpc_client
//     //         .get_multiple_accounts(&[lb_pair.token_x_mint, lb_pair.token_y_mint])
//     //         .await
//     //         .unwrap();

//     //     let mint_x_account =
//     //         account_to_interface_mint(mint_accounts[0].take().unwrap(), lb_pair.token_x_mint);
//     //     let mint_y_account =
//     //         account_to_interface_mint(mint_accounts[1].take().unwrap(), lb_pair.token_y_mint);

//     //     // 3 bin arrays to left, and right is enough to cover most of the swap, and stay under 1.4m CU constraint.
//     //     // Get 3 bin arrays to the left from the active bin
//     //     let left_bin_array_pubkeys =
//     //         get_bin_array_pubkeys_for_swap(sol_usdc, &lb_pair, None, true, 3).unwrap();

//     //     // Get 3 bin arrays to the right the from active bin
//     //     let right_bin_array_pubkeys =
//     //         get_bin_array_pubkeys_for_swap(sol_usdc, &lb_pair, None, false, 3).unwrap();

//     //     // Fetch bin arrays
//     //     let bin_array_pubkeys = left_bin_array_pubkeys
//     //         .into_iter()
//     //         .chain(right_bin_array_pubkeys.into_iter())
//     //         .collect::<Vec<Pubkey>>();

//     //     let accounts = rpc_client
//     //         .get_multiple_accounts(&bin_array_pubkeys)
//     //         .await
//     //         .unwrap();

//     //     // Create Vec<AccountInfo> for quote_exact_in (stack-safe approach)
//     //     let bin_array_account_infos: Vec<AccountInfo> = accounts
//     //         .into_iter()
//     //         .zip(bin_array_pubkeys.into_iter())
//     //         .filter_map(|(account_opt, key)| {
//     //             account_opt.map(|account| account_to_account_info(key, account))
//     //         })
//     //         .collect();

//     //     // 1 SOL -> USDC
//     //     let in_sol_amount = 1_000_000_000;

//     //     let clock = get_clock(rpc_client).await.unwrap();

//     //     let quote_result = quote_exact_in(
//     //         sol_usdc,
//     //         &lb_pair,
//     //         in_sol_amount,
//     //         true,
//     //         bin_array_account_infos.clone(),
//     //         None,
//     //         &clock,
//     //         &mint_x_account,
//     //         &mint_y_account,
//     //     )
//     //     .unwrap();

//     //     println!(
//     //         "1 SOL -> {:?} USDC",
//     //         quote_result.amount_out as f64 / 1_000_000.0
//     //     );

//     //     // 100 USDC -> SOL
//     //     let in_usdc_amount = 100_000_000;

//     //     let quote_result = quote_exact_in(
//     //         sol_usdc,
//     //         &lb_pair,
//     //         in_usdc_amount,
//     //         false,
//     //         bin_array_account_infos,
//     //         None,
//     //         &clock,
//     //         &mint_x_account,
//     //         &mint_y_account,
//     //     )
//     //     .unwrap();

//     //     println!(
//     //         "100 USDC -> {:?} SOL",
//     //         quote_result.amount_out as f64 / 1_000_000_000.0
//     //     );
//     // }
// }
