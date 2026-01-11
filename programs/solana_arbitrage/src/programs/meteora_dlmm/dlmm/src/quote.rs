use crate::extensions::LbPairExtension;
use crate::*;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::pubkey::Pubkey;
use std::collections::HashMap;
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

fn validate_swap_activation(
    lb_pair: &LbPair,
    current_timestamp: u64,
    current_slot: u64,
) -> anyhow::Result<()> {
    ensure!(
        lb_pair.status()?.eq(&PairStatus::Enabled),
        "Pair is disabled"
    );

    let pair_type = lb_pair.pair_type()?;
    if pair_type.eq(&PairType::Permission) {
        let activation_type = lb_pair.activation_type()?;
        let current_point = match activation_type {
            ActivationType::Slot => current_slot,
            ActivationType::Timestamp => current_timestamp,
        };

        ensure!(
            current_point >= lb_pair.activation_point,
            "Pair is disabled"
        );
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn quote_exact_out<'a>(
    lb_pair_pubkey: Pubkey,
    lb_pair: &LbPair,
    mut amount_out: u64,
    swap_for_y: bool,
    bin_arrays: &HashMap<Pubkey, BinArray>,
    bitmap_extension: Option<&BinArrayBitmapExtension>,
    clock: &Clock,
    mint_x_account: &InterfaceAccount<'a, anchor_spl::token_interface::Mint>,
    mint_y_account: &InterfaceAccount<'a, anchor_spl::token_interface::Mint>,
) -> anyhow::Result<SwapExactOutQuote> {
    let current_timestamp = clock.unix_timestamp as u64;
    let current_slot = clock.slot;
    let epoch = clock.epoch;
    msg!("11: {:?}", amount_out);

    validate_swap_activation(lb_pair, current_timestamp, current_slot)?;

    let mut lb_pair = *lb_pair;
    lb_pair.update_references(current_timestamp as i64)?;

    let mut total_amount_in: u64 = 0;
    let mut total_fee: u64 = 0;

    let (in_mint_account, out_mint_account) = if swap_for_y {
        (mint_x_account, mint_y_account)
    } else {
        (mint_y_account, mint_x_account)
    };

    amount_out =
        calculate_transfer_fee_included_amount(out_mint_account, amount_out, epoch)?.amount;

    while amount_out > 0 {
        let active_bin_array_pubkey = get_bin_array_pubkeys_for_swap(
            lb_pair_pubkey,
            &lb_pair,
            bitmap_extension,
            swap_for_y,
            1,
        )?
        .pop()
        .context("Pool out of liquidity")?;

        let active_bin_array = bin_arrays
            .get(&active_bin_array_pubkey)
            .context("Active bin array not found")?;

        // Use only the index for range checking (stack-safe, 8 bytes)
        let bin_array_index = active_bin_array.index as i32;

        // Cache range calculation once per bin array (doesn't change within inner loop)
        let (lower_bin_id, upper_bin_id) =
            BinArray::get_bin_array_lower_upper_bin_id(bin_array_index)?;

        loop {
            // Early exit checks
            if amount_out == 0 {
                break;
            }

            if lb_pair.active_id < lower_bin_id || lb_pair.active_id > upper_bin_id {
                break;
            }

            lb_pair.update_volatility_accumulator()?;

            // Calculate bin index within array
            let bin_index_in_array: i32 = lb_pair
                .active_id
                .checked_sub(lower_bin_id)
                .context("MathOverflow")?;
            let bin_index_usize = bin_index_in_array as usize;

            ensure!(
                bin_index_usize < MAX_BIN_PER_ARRAY,
                "Bin index out of bounds"
            );

            // Clone only the specific bin we need (~144 bytes, acceptable on stack)
            // This avoids cloning the entire BinArray (~10KB)
            let mut active_bin = active_bin_array.bins[bin_index_usize];
            let price = active_bin.get_or_store_bin_price(lb_pair.active_id, lb_pair.bin_step)?;

            if !active_bin.is_empty(!swap_for_y) {
                let bin_max_amount_out = active_bin.get_max_amount_out(swap_for_y);
                if amount_out >= bin_max_amount_out {
                    let max_amount_in = active_bin.get_max_amount_in(price, swap_for_y)?;
                    let max_fee = lb_pair.compute_fee(max_amount_in)?;

                    total_amount_in = total_amount_in
                        .checked_add(max_amount_in)
                        .context("MathOverflow")?;

                    total_fee = total_fee.checked_add(max_fee).context("MathOverflow")?;

                    amount_out = amount_out
                        .checked_sub(bin_max_amount_out)
                        .context("MathOverflow")?;
                } else {
                    let amount_in = Bin::get_amount_in(amount_out, price, swap_for_y)?;
                    let fee = lb_pair.compute_fee(amount_in)?;

                    total_amount_in = total_amount_in
                        .checked_add(amount_in)
                        .context("MathOverflow")?;

                    total_fee = total_fee.checked_add(fee).context("MathOverflow")?;

                    amount_out = 0;
                }
            }

            if amount_out > 0 {
                lb_pair.advance_active_bin(swap_for_y)?;
            }
        }
    }

    total_amount_in = total_amount_in
        .checked_add(total_fee)
        .context("MathOverflow")?;

    total_amount_in =
        calculate_transfer_fee_included_amount(in_mint_account, total_amount_in, epoch)?.amount;

    Ok(SwapExactOutQuote {
        amount_in: total_amount_in,
        fee: total_fee,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn quote_exact_in<'a>(
    lb_pair_pubkey: Pubkey,
    lb_pair: &LbPair,
    amount_in: u64,
    swap_for_y: bool,
    bin_arrays: Vec<AccountInfo<'a>>,
    bitmap_extension: Option<&BinArrayBitmapExtension>,
    clock: &Clock,
    mint_x_account: &InterfaceAccount<'a, anchor_spl::token_interface::Mint>,
    mint_y_account: &InterfaceAccount<'a, anchor_spl::token_interface::Mint>,
) -> anyhow::Result<SwapExactInQuote> {
    msg!("11: {:?}", amount_in);
    let current_timestamp: u64 = clock.unix_timestamp as u64;
    let current_slot = clock.slot;
    let epoch = clock.epoch;

    let mut lb_pair = *lb_pair;
    lb_pair.update_references(current_timestamp as i64)?;
    let mut total_amount_out: u64 = 0;
    let mut total_fee: u64 = 0;

    let (in_mint_account, out_mint_account) = if swap_for_y {
        (mint_x_account, mint_y_account)
    } else {
        (mint_y_account, mint_x_account)
    };
    let transfer_fee_excluded_amount_in =
        calculate_transfer_fee_excluded_amount(in_mint_account, amount_in, epoch)?.amount;
    let mut amount_left = transfer_fee_excluded_amount_in;

    // Constants moved outside loop for better performance
    const BIN_ARRAY_HEADER_SIZE: usize = 56;
    const BIN_SIZE: usize = 144;

    // Create HashMap once for O(1) account lookup instead of O(n) find() each iteration
    let bin_arrays_map: HashMap<Pubkey, &AccountInfo> =
        bin_arrays.iter().map(|acc| (*acc.key, acc)).collect();

    while amount_left > 0 {
        let active_bin_array_pubkey = get_bin_array_pubkeys_for_swap(
            lb_pair_pubkey,
            &lb_pair,
            bitmap_extension,
            swap_for_y,
            1,
        )?
        .pop()
        .context("Pool out of liquidity")?;
        msg!("12: {}", active_bin_array_pubkey);
        let active_bin_array_account = match bin_arrays_map.get(&active_bin_array_pubkey) {
            Some(account) => *account,
            None => {
                msg!("12: ERROR - Required bin array {} not found in provided accounts, insufficient liquidity", active_bin_array_pubkey);
                msg!(
                    "12: Current amount_left: {}, total_amount_out so far: {}",
                    amount_left,
                    total_amount_out
                );
                // We don't have the required bin array account - stop the swap
                // This means we've exhausted the available bin arrays
                // Return partial result if we made some progress, otherwise it's an error
                if total_amount_out == 0 {
                    return Err(anyhow::anyhow!(
                        "Insufficient liquidity: required bin array not available"
                    ));
                }
                break;
            }
        };

        let bin_array_data = active_bin_array_account.try_borrow_data()?;
        // Read only the index field (offset 8, size 8) to avoid deserializing entire BinArray
        let bin_array_index: i64 = bytemuck::pod_read_unaligned(&bin_array_data[8..16]);
        msg!("13");
        // Cache range calculation once per bin array (doesn't change within inner loop)
        let (lower_bin_id, upper_bin_id) =
            BinArray::get_bin_array_lower_upper_bin_id(bin_array_index as i32)?;

        loop {
            // Early exit checks
            if amount_left == 0 {
                break;
            }

            if lb_pair.active_id < lower_bin_id || lb_pair.active_id > upper_bin_id {
                break;
            }
            msg!("14");
            lb_pair.update_volatility_accumulator()?;

            // Calculate bin index within array
            let bin_index_in_array: i32 = lb_pair
                .active_id
                .checked_sub(lower_bin_id)
                .context("MathOverflow")?;
            let bin_index_usize = bin_index_in_array as usize;
            msg!("15");
            // Calculate bin offset (bounds check removed - validated by range check above)
            let bin_offset = BIN_ARRAY_HEADER_SIZE + (bin_index_usize * BIN_SIZE);

            // Read single bin from account data (only ~144 bytes on stack)
            let mut active_bin: Bin =
                bytemuck::pod_read_unaligned(&bin_array_data[bin_offset..bin_offset + BIN_SIZE]);

            let price = active_bin.get_or_store_bin_price(lb_pair.active_id, lb_pair.bin_step)?;

            if !active_bin.is_empty(!swap_for_y) {
                let SwapResult {
                    amount_in_with_fees,
                    amount_out,
                    fee,
                    ..
                } = active_bin.swap(amount_left, price, swap_for_y, &lb_pair, None)?;

                amount_left = amount_left
                    .checked_sub(amount_in_with_fees)
                    .context("MathOverflow")?;

                total_amount_out = total_amount_out
                    .checked_add(amount_out)
                    .context("MathOverflow")?;
                total_fee = total_fee.checked_add(fee).context("MathOverflow")?;

                // Only advance if we still have amount left to swap
                if amount_left > 0 {
                    lb_pair.advance_active_bin(swap_for_y)?;
                }
            } else {
                // Bin is empty, advance to next bin immediately
                msg!("16: empty bin, advancing");
                let old_active_id = lb_pair.active_id;
                lb_pair.advance_active_bin(swap_for_y)?;
                // Safety check: if we didn't actually advance (shouldn't happen), break to avoid infinite loop
                if lb_pair.active_id == old_active_id {
                    msg!("16: ERROR - active_id did not change, breaking to avoid infinite loop");
                    break;
                }
                // Check if we've moved outside the current bin array range - if so, break to get new bin array
                if lb_pair.active_id < lower_bin_id || lb_pair.active_id > upper_bin_id {
                    msg!("16: active_id moved outside bin array range, breaking to get new array");
                    break;
                }
            }
            msg!("16");
        }
    }
    msg!("17");
    let transfer_fee_excluded_amount_out =
        calculate_transfer_fee_excluded_amount(out_mint_account, total_amount_out, epoch)?.amount;
    msg!("18");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::load_mint;
    use anchor_client::solana_sdk::clock::Clock;
    use anchor_client::Cluster;
    use anchor_lang::prelude::*;
    use anchor_lang::solana_program::account_info::AccountInfo;
    use anchor_lang::solana_program::program_pack::Pack;
    use anchor_spl::token_interface::Mint;
    use solana_client::nonblocking::rpc_client::RpcClient;
    use solana_sdk::pubkey::Pubkey;

    /// Get on chain clock
    async fn get_clock(rpc_client: RpcClient) -> anyhow::Result<Clock> {
        let clock_account = rpc_client
            .get_account(&anchor_client::solana_sdk::sysvar::clock::ID)
            .await?;

        let clock_state: Clock = bincode::deserialize(clock_account.data.as_ref())?;

        Ok(clock_state)
    }

    /// Convert raw RPC account to InterfaceAccount<Mint>
    fn account_to_interface_mint(
        account: solana_sdk::account::Account,
        pubkey: Pubkey,
    ) -> InterfaceAccount<'static, Mint> {
        let data = Box::leak(Box::new(account.data));
        let lamports = Box::leak(Box::new(account.lamports));
        let owner = Box::leak(Box::new(account.owner));
        let key = Box::leak(Box::new(pubkey));

        // Create AccountInfo with 'static lifetime
        let account_info: &'static AccountInfo<'static> = Box::leak(Box::new(AccountInfo::new(
            key, false, false, lamports, data, owner, false, 0,
        )));

        // Create InterfaceAccount from AccountInfo
        // Since AccountInfo is 'static, InterfaceAccount will also be 'static
        InterfaceAccount::<Mint>::try_from(account_info).expect("Failed to create InterfaceAccount")
    }

    /// Convert solana_sdk::account::Account to AccountInfo
    fn account_to_account_info(
        key: Pubkey,
        account: solana_sdk::account::Account,
    ) -> AccountInfo<'static> {
        let data = Box::leak(Box::new(account.data));
        let lamports = Box::leak(Box::new(account.lamports));
        let owner_bytes: [u8; 32] = account.owner.to_bytes();
        let owner = Pubkey::try_from(owner_bytes.as_ref()).unwrap();
        let owner_static = Box::leak(Box::new(owner));
        let key_static = Box::leak(Box::new(key));
        AccountInfo::new(
            key_static,
            false, // is_signer
            false, // is_writable
            lamports,
            data,
            owner_static,
            account.executable,
            account.rent_epoch,
        )
    }

    #[tokio::test]
    async fn test_swap_quote_exact_out() {
        // RPC client. No gPA is required.
        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        let sol_usdc = Pubkey::from_str_const("HTvjzsfX3yU6BUodCjZ5vZkUrAxMDTrBs3CJaq43ashR");

        let lb_pair_account = rpc_client.get_account(&sol_usdc).await.unwrap();

        let lb_pair: LbPair = bytemuck::pod_read_unaligned(&lb_pair_account.data[8..]);

        let mut mint_accounts = rpc_client
            .get_multiple_accounts(&[lb_pair.token_x_mint, lb_pair.token_y_mint])
            .await
            .unwrap();

        let mint_x_account =
            account_to_interface_mint(mint_accounts[0].take().unwrap(), lb_pair.token_x_mint);
        let mint_y_account =
            account_to_interface_mint(mint_accounts[1].take().unwrap(), lb_pair.token_y_mint);

        // 3 bin arrays to left, and right is enough to cover most of the swap, and stay under 1.4m CU constraint.
        // Get 3 bin arrays to the left from the active bin
        let left_bin_array_pubkeys =
            get_bin_array_pubkeys_for_swap(sol_usdc, &lb_pair, None, true, 3).unwrap();

        // Get 3 bin arrays to the right the from active bin
        let right_bin_array_pubkeys =
            get_bin_array_pubkeys_for_swap(sol_usdc, &lb_pair, None, false, 3).unwrap();

        // Fetch bin arrays
        let bin_array_pubkeys = left_bin_array_pubkeys
            .into_iter()
            .chain(right_bin_array_pubkeys.into_iter())
            .collect::<Vec<Pubkey>>();

        let accounts = rpc_client
            .get_multiple_accounts(&bin_array_pubkeys)
            .await
            .unwrap();

        // Create HashMap for quote_exact_out (which still uses HashMap)
        let bin_arrays = accounts
            .iter()
            .zip(bin_array_pubkeys.iter())
            .filter_map(|(account_opt, key)| {
                account_opt
                    .as_ref()
                    .map(|account| (*key, bytemuck::pod_read_unaligned(&account.data[8..])))
            })
            .collect::<HashMap<_, _>>();

        // Create Vec<AccountInfo> for quote_exact_in (stack-safe approach)
        let bin_array_account_infos: Vec<AccountInfo> = accounts
            .into_iter()
            .zip(bin_array_pubkeys.into_iter())
            .filter_map(|(account_opt, key)| {
                account_opt.map(|account| account_to_account_info(key, account))
            })
            .collect();

        let usdc_token_multiplier = 1_000_000.0;
        let sol_token_multiplier = 1_000_000_000.0;

        let out_sol_amount = 1_000_000_000;
        let clock = get_clock(rpc_client).await.unwrap();

        let quote_result = quote_exact_out(
            sol_usdc,
            &lb_pair,
            out_sol_amount,
            false,
            &bin_arrays,
            None,
            &clock,
            &mint_x_account,
            &mint_y_account,
        )
        .unwrap();

        let in_amount = quote_result.amount_in + quote_result.fee;

        println!(
            "{} USDC -> exact 1 SOL",
            in_amount as f64 / usdc_token_multiplier
        );

        let quote_result = quote_exact_in(
            sol_usdc,
            &lb_pair,
            in_amount,
            false,
            bin_array_account_infos.clone(),
            None,
            &clock,
            &mint_x_account,
            &mint_y_account,
        )
        .unwrap();

        println!(
            "{} USDC -> {} SOL",
            in_amount as f64 / usdc_token_multiplier,
            quote_result.amount_out as f64 / sol_token_multiplier
        );

        let out_usdc_amount = 200_000_000;

        let quote_result = quote_exact_out(
            sol_usdc,
            &lb_pair,
            out_usdc_amount,
            true,
            &bin_arrays,
            None,
            &clock,
            &mint_x_account,
            &mint_y_account,
        )
        .unwrap();

        let in_amount = quote_result.amount_in + quote_result.fee;

        println!(
            "{} SOL -> exact 200 USDC",
            in_amount as f64 / sol_token_multiplier
        );

        let quote_result = quote_exact_in(
            sol_usdc,
            &lb_pair,
            in_amount,
            true,
            bin_array_account_infos,
            None,
            &clock,
            &mint_x_account,
            &mint_y_account,
        )
        .unwrap();

        println!(
            "{} SOL -> {} USDC",
            in_amount as f64 / sol_token_multiplier,
            quote_result.amount_out as f64 / usdc_token_multiplier
        );
    }

    #[tokio::test]
    async fn test_swap_quote_exact_in() {
        // RPC client. No gPA is required.
        let rpc_client = RpcClient::new(Cluster::Mainnet.url().to_string());

        let sol_usdc = Pubkey::from_str_const("8ztFxjFPfVUtEf4SLSapcFj8GW2dxyUA9no2bLPq7H7V");

        let lb_pair_account = rpc_client.get_account(&sol_usdc).await.unwrap();

        let lb_pair: LbPair = bytemuck::pod_read_unaligned(&lb_pair_account.data[8..]);

        let mut mint_accounts = rpc_client
            .get_multiple_accounts(&[lb_pair.token_x_mint, lb_pair.token_y_mint])
            .await
            .unwrap();

        let mint_x_account =
            account_to_interface_mint(mint_accounts[0].take().unwrap(), lb_pair.token_x_mint);
        let mint_y_account =
            account_to_interface_mint(mint_accounts[1].take().unwrap(), lb_pair.token_y_mint);

        // 3 bin arrays to left, and right is enough to cover most of the swap, and stay under 1.4m CU constraint.
        // Get 3 bin arrays to the left from the active bin
        let left_bin_array_pubkeys =
            get_bin_array_pubkeys_for_swap(sol_usdc, &lb_pair, None, true, 3).unwrap();

        // Get 3 bin arrays to the right the from active bin
        let right_bin_array_pubkeys =
            get_bin_array_pubkeys_for_swap(sol_usdc, &lb_pair, None, false, 3).unwrap();

        // Fetch bin arrays
        let bin_array_pubkeys = left_bin_array_pubkeys
            .into_iter()
            .chain(right_bin_array_pubkeys.into_iter())
            .collect::<Vec<Pubkey>>();

        let accounts = rpc_client
            .get_multiple_accounts(&bin_array_pubkeys)
            .await
            .unwrap();

        // Create Vec<AccountInfo> for quote_exact_in (stack-safe approach)
        let bin_array_account_infos: Vec<AccountInfo> = accounts
            .into_iter()
            .zip(bin_array_pubkeys.into_iter())
            .filter_map(|(account_opt, key)| {
                account_opt.map(|account| account_to_account_info(key, account))
            })
            .collect();

        // 1 SOL -> USDC
        let in_sol_amount = 1_000_000_000;

        let clock = get_clock(rpc_client).await.unwrap();

        let quote_result = quote_exact_in(
            sol_usdc,
            &lb_pair,
            in_sol_amount,
            true,
            bin_array_account_infos.clone(),
            None,
            &clock,
            &mint_x_account,
            &mint_y_account,
        )
        .unwrap();

        println!(
            "1 SOL -> {:?} USDC",
            quote_result.amount_out as f64 / 1_000_000.0
        );

        // 100 USDC -> SOL
        let in_usdc_amount = 100_000_000;

        let quote_result = quote_exact_in(
            sol_usdc,
            &lb_pair,
            in_usdc_amount,
            false,
            bin_array_account_infos,
            None,
            &clock,
            &mint_x_account,
            &mint_y_account,
        )
        .unwrap();

        println!(
            "100 USDC -> {:?} SOL",
            quote_result.amount_out as f64 / 1_000_000_000.0
        );
    }
}
