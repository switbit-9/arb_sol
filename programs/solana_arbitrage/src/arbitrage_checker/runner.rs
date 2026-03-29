use anchor_lang::prelude::*;
use anchor_spl::token::spl_token::native_mint::ID as WSOL;

use crate::programs::{self, ProgramInstance, ProgramMeta, PoolKind, SolarBError};
use crate::utils::bot_config::BotConfig;
use crate::MAX_POOLS;
#[cfg(feature = "benchmark")]
use crate::sol_remaining_cu;
use super::{ArbitrageResult, Hop, amm_sim, dlmm_sim, whirlpool_sim, clmm_sim};
use super::dlmm_checker::{check_dlmm_dlmm, check_dlmm_to_amm, check_amm_to_dlmm};
use super::amm_checker::check_amm_amm;
use super::whirlpool_checker::{
    check_whirlpool_to_amm, check_amm_to_whirlpool, check_whirlpool_whirlpool,
    check_whirlpool_to_dlmm, check_dlmm_to_whirlpool,
};
use super::clmm_checker::{
    check_clmm_to_amm, check_amm_to_clmm, check_clmm_clmm,
    check_clmm_to_dlmm, check_dlmm_to_clmm,
    check_clmm_to_whirlpool, check_whirlpool_to_clmm,
};
use programs::orca::{D_TICK_ARRAY_0, D_TICK_ARRAY_1, D_TICK_ARRAY_2};
/// Pre-extracted checker pool: AMM (stack), DLMM/Whirlpool/CLMM (stack, account-backed lazy ticks).
enum CheckerPool {
    Amm(amm_sim::AmmPool),
    Dlmm(dlmm_sim::DlmmPool),
    Whirlpool(whirlpool_sim::WhirlpoolPool),
    Clmm(clmm_sim::ClmmPool),
}
const ONE_Q64: u128 = 1u128 << 64;
const Q32: f64 = (1u64 << 32) as f64; // 4294967296.0
const MAX_CANDIDATES: usize = MAX_POOLS; // * (MAX_POOLS - 1);

struct Candidate {
    bi: u8,
    si: u8,
    excess: u128,
    score: u128, // depth >= max_in: excess; depth < max_in: excess * depth / max_in
}

/// Pre-extracted per-pool info cached once during the price scan.
/// Avoids redundant `get_mints()` / `pool_kind()` calls in the N^2 pair loop.
/// Prices are Q32 fixed-point (value * 2^32) to keep the N^2 filter integer-only.
struct PoolInfo {
    base: Pubkey,
    quote: Pubkey,
    buy_price_q32: u64,
    sell_price_q32: u64,
    kind: PoolKind,
    /// Depth proxy for buy direction (start_token → mid_token): max input in start_token lamports.
    buy_depth: u64,
    /// Depth proxy for sell direction (mid_token → start_token): max input in mid_token lamports.
    sell_depth: u64,
}

/// Extract arbitrage_checker pools from ProgramInstance and run the checker.
/// Pre-extracts all pools once to avoid repeated heap allocations (BPF bump allocator never frees).
#[inline(never)]
pub fn run_arb_checker<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance],
    config: &BotConfig,
) -> Option<ArbitrageResult> {
    let start_token = config.start_token.unwrap_or(WSOL);
    let max_in = config.max_amount_in;
    let mut best = ArbitrageResult::none();

    let n = instances.len().min(MAX_POOLS);
    if n < 2 { return None; }

    // Pre-extract prices, fees, mints, and pool kind once per pool — no heap alloc.
    let first = &instances[0];
    let (base_mint, quote_mint) = first.get_mints();
    let middle_mint = if *base_mint == start_token { *quote_mint } else { *base_mint };

    #[cfg(feature = "benchmark")]
    let cu_price_start = sol_remaining_cu();
    let mut info: Box<[PoolInfo; MAX_POOLS]> = Box::new([const { PoolInfo {
        base: Pubkey::new_from_array([0; 32]),
        quote: Pubkey::new_from_array([0; 32]),
        buy_price_q32: 0,
        sell_price_q32: 0,
        kind: PoolKind::PumpAmm,
        buy_depth: 0,
        sell_depth: 0,
    } }; MAX_POOLS]);
    for i in 0..n {
        let inst = &instances[i];
        let ((price_btq, price_qtb), (fee_btq, fee_qtb), (base, quote), kind) =
            match inst.get_checker_info() {
                Ok(info) => info,
                Err(_) => continue,
            };
        // Convert effective prices to Q32 fixed-point once per pool (cold O(N) path).
        // The N^2 pre-filter loop then uses pure integer math.
        let eff_btq_q32 = (price_btq * fee_btq * Q32) as u64;
        let eff_qtb_q32 = (price_qtb * fee_qtb * Q32) as u64;
        let buy = if start_token == *base { eff_btq_q32 } else { eff_qtb_q32 };
        let sell = if middle_mint == *base { eff_btq_q32 } else { eff_qtb_q32 };
        let (buy_depth, sell_depth) = if kind == PoolKind::MeteoraDlmm {
            let bd = inst.get_active_bin_max_in(start_token).unwrap_or(0);
            let sd = inst.get_active_bin_max_in(middle_mint).unwrap_or(0);
            (bd, sd)
        } else if let Ok((base_v, quote_v)) = inst.get_vault_amounts() {
            // start_token reserve = buy_depth, mid_token reserve = sell_depth
            if start_token == *base {
                (base_v, quote_v)
            } else {
                (quote_v, base_v)
            }
        } else {
            (0, 0)
        };
        info[i] = PoolInfo {
            base: *base,
            quote: *quote,
            buy_price_q32: buy,
            sell_price_q32: sell,
            kind,
            buy_depth,
            sell_depth,
        };
    }
    #[cfg(feature = "benchmark")]
    debug_msg!("  [CU] price_extract: {}", cu_price_start.saturating_sub(sol_remaining_cu()));

    // ---------- Phase 1: Rank all candidates by excess (descending) ----------
    // N^2 pre-filter is cheap (integer multiply). Collect all passing pairs ranked by excess.
    // Max possible candidates = MAX_POOLS * (MAX_POOLS - 1)
    #[cfg(feature = "benchmark")]
    let cu_n2_start = sol_remaining_cu();
    let mut candidates: [Candidate; MAX_CANDIDATES] = [const { Candidate { bi: 0, si: 0, excess: 0, score: 0 } }; MAX_CANDIDATES];
    let mut cand_count: usize = 0;

    for bi in 0..n {
        for si in 0..n {
            if bi == si { continue; }

            let bi_info = &info[bi];
            let si_info = &info[si];

            let product_q64 = (bi_info.buy_price_q32 as u128) * (si_info.sell_price_q32 as u128);
            if product_q64 <= ONE_Q64 { continue; }
            let excess = product_q64 - ONE_Q64;

            #[cfg(any(test, feature = "debug"))]
            {
                let (buy_price_btq, buy_price_qtb) = instances[bi].get_prices().unwrap_or((0.0, 0.0));
                let (sell_price_btq, sell_price_qtb) = instances[si].get_prices().unwrap_or((0.0, 0.0));
                let (buy_fee_btq, buy_fee_qtb) = instances[bi].get_fee_factor().unwrap_or((1.0, 1.0));
                let (sell_fee_btq, sell_fee_qtb) = instances[si].get_fee_factor().unwrap_or((1.0, 1.0));
                let (buy_price, buy_price_inv, buy_fee) = if start_token == *instances[bi].get_mints().0 {
                    (buy_price_btq, buy_price_qtb, buy_fee_btq)
                } else {
                    (buy_price_qtb, buy_price_btq, buy_fee_qtb)
                };
                let (sell_price, sell_price_inv, sell_fee) = if middle_mint == *instances[si].get_mints().0 {
                    (sell_price_btq, sell_price_qtb, sell_fee_btq)
                } else {
                    (sell_price_qtb, sell_price_btq, sell_fee_qtb)
                };
                let buy_price_display = buy_price.min(buy_price_inv);
                let sell_price_display = sell_price.min(sell_price_inv);
                let profit_pct = (excess as f64) / (ONE_Q64 as f64) * 100.0;
                let buy_fee_pct = (1.0 - buy_fee) * 100.0;
                let sell_fee_pct = (1.0 - sell_fee) * 100.0;
                debug_eprintln!(
                    "iteration bi={} si={}: {:?} -> {:?}, buy_price={:.6} -> sell_price={:.6}, profit={:.2}% buy_fee={:.4}% sell_fee={:.4}%",
                    bi, si, bi_info.kind, si_info.kind,
                    buy_price_display, sell_price_display,
                    profit_pct, buy_fee_pct, sell_fee_pct
                );
            }

            if (max_in as u128).saturating_mul(excess) <= 5000u128 << 64 { continue; }

            // Depth-weighted score: approximate absolute profit.
            // buy_depth is in start_token lamports (max start_token the buy pool absorbs).
            // sell_depth is in mid_token lamports (max mid_token the sell pool absorbs).
            // Convert sell_depth → start_token via buy price:
            //   sell_depth_start = sell_depth * 2^32 / buy_price_q32
            let sell_depth_start = if bi_info.buy_price_q32 > 0 {
                ((si_info.sell_depth as u128) << 32) / (bi_info.buy_price_q32 as u128)
            } else { 0 } as u64;
            let raw_depth = bi_info.buy_depth.min(sell_depth_start);
            // depth >= max_in: rank by price*fee margin alone.
            // depth < max_in: scale excess down by depth/max_in fraction.
            let score = if raw_depth >= max_in {
                excess
            } else {
                excess.saturating_mul(raw_depth as u128) / (max_in as u128)
            };

            if cand_count < MAX_CANDIDATES {
                candidates[cand_count] = Candidate { bi: bi as u8, si: si as u8, excess, score };
                cand_count += 1;
            }
        }
    }


    if cand_count == 0 {
        return None;
    }

    // Sort candidates descending by score (insertion sort — tiny array).
    // Score = excess * min_depth approximates absolute profit.
    for i in 1..cand_count {
        let mut j = i;
        while j > 0 && candidates[j].score > candidates[j - 1].score {
            candidates.swap(j, j - 1);
            j -= 1;
        }
    }

    debug_eprintln!("checker: {} candidates ranked by score", cand_count);
    #[cfg(feature = "benchmark")]
    debug_msg!("  [CU] n2_filter+sort: {}", cu_n2_start.saturating_sub(sol_remaining_cu()));

    // ---------- Phase 2: Check top-2, then fallback ----------
    // Run checker on #1 and #2. If either profits → take best and return immediately.
    // If neither profits → continue down the ranked list, return first profitable.
    let mut pools: Box<[Option<Option<CheckerPool>>; MAX_POOLS]> = Box::new([const { None }; MAX_POOLS]);
    let mut buy_flags: [bool; MAX_POOLS] = [false; MAX_POOLS];
    let mut sell_flags: [bool; MAX_POOLS] = [false; MAX_POOLS];

    for ci in 0..cand_count {
        let bi = candidates[ci].bi as usize;
        let si = candidates[ci].si as usize;
        let bi_info = &info[bi];
        let si_info = &info[si];

        // Lazy pool extraction
        #[cfg(feature = "benchmark")]
        let cu_extract_start = sol_remaining_cu();
        for idx in [bi, si] {
            if pools[idx].is_none() {
                let inst = &instances[idx];
                let kind = info[idx].kind;
                let is_amm = matches!(kind, PoolKind::PumpAmm | PoolKind::RaydiumAmm | PoolKind::MeteoraDammV1 | PoolKind::MeteoraDammV2 | PoolKind::RaydiumCPMM);
                let is_dlmm = kind == PoolKind::MeteoraDlmm;

                if is_amm {
                    pools[idx] = Some(extract_amm(inst).map(CheckerPool::Amm));
                } else if is_dlmm {
                    let sfy = match inst {
                        ProgramInstance::MeteoraDlmm(d) => start_token == d.base_token_pk,
                        _ => false,
                    };
                    pools[idx] = Some(extract_dlmm(inst, accounts, sfy).map(CheckerPool::Dlmm));
                    buy_flags[idx] = sfy;
                    sell_flags[idx] = !sfy;
                } else if kind == PoolKind::OrcaWhirlpool {
                    pools[idx] = Some(extract_whirlpool(inst, accounts).map(CheckerPool::Whirlpool));
                    let atb = match inst {
                        ProgramInstance::OrcaWhirlpool(w) => start_token == w.base_token_pk,
                        _ => false,
                    };
                    buy_flags[idx] = atb;
                    sell_flags[idx] = !atb;
                } else if kind == PoolKind::RaydiumCLMM {
                    pools[idx] = Some(extract_clmm(inst, accounts).map(CheckerPool::Clmm));
                    let zfo = match inst {
                        ProgramInstance::RaydiumCLMM(c) => start_token == c.base_token_pk,
                        _ => false,
                    };
                    buy_flags[idx] = zfo;
                    sell_flags[idx] = !zfo;
                } else {
                    pools[idx] = Some(None);
                }
            }
        }

        #[cfg(feature = "benchmark")]
        debug_msg!("  [CU] cand#{} extract({:?}+{:?}): {}", ci, bi_info.kind, si_info.kind, cu_extract_start.saturating_sub(sol_remaining_cu()));

        let (buy_pool, sell_pool) = match (&pools[bi], &pools[si]) {
            (Some(Some(b)), Some(Some(s))) => (b, s),
            _ => continue,
        };

        let buy_mid_is_base = middle_mint == bi_info.base;
        let sell_mid_is_base = middle_mint == si_info.base;

        #[cfg(feature = "benchmark")]
        let cu_checker_start = sol_remaining_cu();
        let result = match (buy_pool, sell_pool) {
            (CheckerPool::Amm(a), CheckerPool::Amm(b)) => {
                let a_oriented = if buy_mid_is_base { a.clone() } else { a.flipped() };
                let b_oriented = if sell_mid_is_base { b.clone() } else { b.flipped() };
                check_amm_amm(&a_oriented, &b_oriented, max_in)
            }
            (CheckerPool::Amm(amm), CheckerPool::Dlmm(dlmm)) => {
                check_amm_to_dlmm(amm, buy_mid_is_base, dlmm, sell_flags[si], max_in, accounts)
            }
            (CheckerPool::Dlmm(dlmm), CheckerPool::Amm(amm)) => {
                check_dlmm_to_amm(dlmm, buy_flags[bi], amm, sell_mid_is_base, max_in, accounts)
            }
            (CheckerPool::Dlmm(da), CheckerPool::Dlmm(db)) => {
                check_dlmm_dlmm(da, buy_flags[bi], db, sell_flags[si], max_in, accounts)
            }
            (CheckerPool::Whirlpool(wp), CheckerPool::Amm(amm)) => {
                check_whirlpool_to_amm(wp, buy_flags[bi], amm, sell_mid_is_base, max_in, accounts)
            }
            (CheckerPool::Amm(amm), CheckerPool::Whirlpool(wp)) => {
                check_amm_to_whirlpool(amm, buy_mid_is_base, wp, sell_flags[si], max_in, accounts)
            }
            (CheckerPool::Whirlpool(wa), CheckerPool::Whirlpool(wb)) => {
                check_whirlpool_whirlpool(wa, buy_flags[bi], wb, sell_flags[si], max_in, accounts)
            }
            (CheckerPool::Whirlpool(wp), CheckerPool::Dlmm(dlmm)) => {
                check_whirlpool_to_dlmm(wp, buy_flags[bi], dlmm, sell_flags[si], max_in, accounts)
            }
            (CheckerPool::Dlmm(dlmm), CheckerPool::Whirlpool(wp)) => {
                check_dlmm_to_whirlpool(dlmm, buy_flags[bi], wp, sell_flags[si], max_in, accounts)
            }
            (CheckerPool::Clmm(clmm), CheckerPool::Amm(amm)) => {
                check_clmm_to_amm(clmm, buy_flags[bi], amm, sell_mid_is_base, max_in, accounts)
            }
            (CheckerPool::Amm(amm), CheckerPool::Clmm(clmm)) => {
                check_amm_to_clmm(amm, buy_mid_is_base, clmm, sell_flags[si], max_in, accounts)
            }
            (CheckerPool::Clmm(ca), CheckerPool::Clmm(cb)) => {
                check_clmm_clmm(ca, buy_flags[bi], cb, sell_flags[si], max_in, accounts)
            }
            (CheckerPool::Clmm(clmm), CheckerPool::Dlmm(dlmm)) => {
                check_clmm_to_dlmm(clmm, buy_flags[bi], dlmm, sell_flags[si], max_in, accounts)
            }
            (CheckerPool::Dlmm(dlmm), CheckerPool::Clmm(clmm)) => {
                check_dlmm_to_clmm(dlmm, buy_flags[bi], clmm, sell_flags[si], max_in, accounts)
            }
            (CheckerPool::Clmm(clmm), CheckerPool::Whirlpool(wp)) => {
                check_clmm_to_whirlpool(clmm, buy_flags[bi], wp, sell_flags[si], max_in, accounts)
            }
            (CheckerPool::Whirlpool(wp), CheckerPool::Clmm(clmm)) => {
                check_whirlpool_to_clmm(wp, buy_flags[bi], clmm, sell_flags[si], max_in, accounts)
            }
        };

        #[cfg(feature = "benchmark")]
        debug_msg!("  [CU] cand#{} checker: {} (profit={})", ci, cu_checker_start.saturating_sub(sol_remaining_cu()), result.profit);

        if result.profit > best.profit {
            best = result;
            let buy_ltr = start_token == bi_info.base;
            let sell_ltr = middle_mint == si_info.base;
            best.hops[0] = Hop { instance_idx: bi as u8, left_to_right: buy_ltr };
            best.hops[1] = Hop { instance_idx: si as u8, left_to_right: sell_ltr };
            best.hop_count = 2;
        }

        // Early exit: after checking top-2 candidates, if we found profit → return.
        // Beyond top-2, return immediately on first profitable result.
        if ci == 1 && best.profit > config.min_profit {
            debug_eprintln!("checker: early exit after top-2 (profit={})", best.profit);
            return Some(best);
        }
        if ci >= 2 && best.profit > config.min_profit {
            debug_eprintln!("checker: fallback exit at cand#{} (profit={})", ci, best.profit);
            return Some(best);
        }

    }

    Some(best)
}

/// Extract a checker AmmPool from a ProgramInstance.
#[inline(never)]
pub(crate) fn extract_amm(inst: &ProgramInstance) -> Option<amm_sim::AmmPool> {
    match inst {
        ProgramInstance::PumpAmm(p) => Some(amm_sim::AmmPool::from_pump_with_fee(
            p.base_vault_amount,
            p.quote_vault_amount,
            p.fee_numerator,
        )),
        ProgramInstance::RaydiumAmm(p) => Some(amm_sim::AmmPool::from_raydium_amm(
            p.base_vault_amount,
            p.quote_vault_amount,
            p.fee_millionths,
        )),
        ProgramInstance::MeteoraDammV1(p) => Some(amm_sim::AmmPool::from_damm_v1(
            p.base_vault_amount,
            p.quote_vault_amount,
            p.trade_fee_numerator,
            p.trade_fee_denominator,
        )),
        ProgramInstance::RaydiumCPMM(p) => {
            let (base_fees, quote_fees) = if p.base_is_token_0 {
                (p.fees_token_0, p.fees_token_1)
            } else {
                (p.fees_token_1, p.fees_token_0)
            };
            Some(amm_sim::AmmPool::from_cpmm(
                p.base_vault_amount.saturating_sub(base_fees),
                p.quote_vault_amount.saturating_sub(quote_fees),
                p.trade_fee_rate,
                p.adjusted_creator_fee_rate,
                p.buy_creator_fee_on_input,
                p.sell_creator_fee_on_input,
            ))
        }
        ProgramInstance::MeteoraDammV2(p) => Some(amm_sim::AmmPool::from_damm_v2(
            p.sqrt_price,
            p.liquidity,
            p.fee_rate_a_to_b,
            p.collect_fee_mode,
        )),
        _ => None,
    }
}

/// Extract a checker DlmmPool from a MeteoraDlmm ProgramInstance.
/// Records account indices and bin ranges — bins are read on demand by the checker.
/// D_BIN_BUY → swap_for_y=true, D_BIN_SELL → swap_for_y=false (independent of arb direction).
#[inline(never)]
pub(crate) fn extract_dlmm<'info>(
    inst: &ProgramInstance,
    accounts: &[AccountInfo<'info>],
    _buy_sfy: bool,
) -> Option<dlmm_sim::DlmmPool> {
    use dlmm_sim::DlmmPool;
    use programs::meteora_dlmm::{D_BIN_BUY, D_BIN_SELL};

    let dlmm = match inst {
        ProgramInstance::MeteoraDlmm(d) => d,
        _ => return None,
    };

    let slim = &dlmm.lb_pair_slim;

    // Reconstruct base_factor and base_fee_power_factor from the pool account.
    let pool_acc = &accounts[dlmm.dyn_start]; // D_POOL = 0
    let (base_factor, base_fee_power_factor) = {
        let d = match pool_acc.try_borrow_data() {
            Ok(d) => d,
            Err(_) => return None,
        };
        if d.len() < 8 + 30 { return None; }
        let inner = &d[8..];
        let bf = u16::from_le_bytes([inner[0], inner[1]]);
        let bfp = inner[26];
        (bf, bfp)
    };

    let mut pool = DlmmPool::new_config(
        slim.active_id,
        slim.bin_step,
        base_factor,
        base_fee_power_factor,
        slim.variable_fee_control,
        slim.volatility_accumulator,
        slim.volatility_reference,
        slim.index_reference,
        slim.max_vol_acc,
    );

    // Record account indices and compute bin ranges — no bin data is read here.
    for (bin_arr_offset, sfy_dir) in [(D_BIN_BUY, true), (D_BIN_SELL, false)] {
        let acc_idx = dlmm.dyn_start + bin_arr_offset;
        if acc_idx >= accounts.len() { continue; }
        let acc = &accounts[acc_idx];
        let data = match acc.try_borrow_data() {
            Ok(d) => d,
            Err(_) => continue,
        };
        if data.len() < 8 + 48 { continue; }
        let inner = &data[8..]; // skip discriminator
        let bin_array_index = i64::from_le_bytes(inner[0..8].try_into().ok()?);
        let lower_bin_id = (bin_array_index as i32) * 70;

        let raw_active_idx = slim.active_id - lower_bin_id;
        if raw_active_idx < 0 || raw_active_idx >= 70 {
            // Active bin is not in this bin array — skip this direction.
            continue;
        }
        let active_idx_in_array = raw_active_idx as usize;
        let (start, end) = if sfy_dir {
            (0, (active_idx_in_array + 1).min(70))
        } else {
            (active_idx_in_array, 70)
        };
        pool.set_bin_source(sfy_dir, acc_idx, lower_bin_id, start as u8, end as u8);
    }

    Some(pool)
}

/// Extract a checker WhirlpoolPool from an OrcaWhirlpool ProgramInstance.
/// Records tick array account indices — ticks are read lazily by the checker.
#[inline(never)]
pub(crate) fn extract_whirlpool<'info>(
    inst: &ProgramInstance,
    accounts: &[AccountInfo<'info>],
) -> Option<whirlpool_sim::WhirlpoolPool> {
    use programs::orca::states::tick::{TICK_ARRAY_DISCRIMINATOR, DYNAMIC_TICK_ARRAY_DISCRIMINATOR};

    let wp = match inst {
        ProgramInstance::OrcaWhirlpool(w) => w,
        _ => return None,
    };

    let mut pool = whirlpool_sim::WhirlpoolPool::new_lazy(
        wp.sqrt_price,
        wp.liquidity,
        wp.tick_current_index,
        wp.tick_spacing,
        wp.fee_rate,
    );

    // Register tick array accounts — no tick data is read here.
    // Both formats store start_tick_index as i32 at offset 8.
    const START_TICK_OFF: usize = 8;
    const BITMAP_OFF: usize = 8 + 4 + 32; // 44: disc + start + whirlpool pubkey

    for offset in [D_TICK_ARRAY_0, D_TICK_ARRAY_1, D_TICK_ARRAY_2] {
        let acc_idx = wp.dyn_start + offset;
        if acc_idx >= accounts.len() { continue; }
        let data = match accounts[acc_idx].try_borrow_data() {
            Ok(d) => d,
            Err(_) => continue,
        };
        if data.len() < START_TICK_OFF + 4 { continue; }

        let start_tick = i32::from_le_bytes(
            data[START_TICK_OFF..START_TICK_OFF + 4].try_into().ok()?,
        );

        if data.len() >= 8 && data[0..8] == TICK_ARRAY_DISCRIMINATOR {
            pool.add_simple_source(acc_idx, start_tick);
        } else if data.len() >= BITMAP_OFF + 16 && data[0..8] == DYNAMIC_TICK_ARRAY_DISCRIMINATOR {
            let bitmap = u128::from_le_bytes(
                data[BITMAP_OFF..BITMAP_OFF + 16].try_into().ok()?,
            );
            pool.add_dynamic_source(acc_idx, start_tick, bitmap);
        }
    }

    Some(pool)
}

/// Extract a checker ClmmPool from a RaydiumCLMM ProgramInstance.
/// Records tick array account indices — ticks are read lazily by the checker.
#[inline(never)]
pub(crate) fn extract_clmm<'info>(
    inst: &ProgramInstance,
    accounts: &[AccountInfo<'info>],
) -> Option<clmm_sim::ClmmPool> {
    use clmm_sim::ClmmPool;
    use programs::raydium_clmm::{D_TICK_BUY_0, D_TICK_BUY_1, D_TICK_SELL_0, D_TICK_SELL_1};

    let clmm = match inst {
        ProgramInstance::RaydiumCLMM(c) => c,
        _ => return None,
    };

    let mut pool = ClmmPool::new_lazy(
        clmm.sqrt_price_x64,
        clmm.liquidity,
        clmm.tick_current,
        clmm.tick_spacing,
        clmm.trade_fee_rate,
    );

    // Register tick array accounts — no tick data is read here.
    // start_tick_index is at offset 8 (disc) + 32 (pool_id) = 40.
    const TA_START_OFF: usize = 8 + 32;
    const MIN_HDR: usize = TA_START_OFF + 4;

    for offset in [D_TICK_BUY_0, D_TICK_BUY_1, D_TICK_SELL_0, D_TICK_SELL_1] {
        let acc_idx = clmm.dyn_start + offset;
        if acc_idx >= accounts.len() { continue; }
        let data = match accounts[acc_idx].try_borrow_data() {
            Ok(d) => d,
            Err(_) => continue,
        };
        if data.len() < MIN_HDR { continue; }
        let start_tick = i32::from_le_bytes(
            data[TA_START_OFF..TA_START_OFF + 4].try_into().ok()?,
        );
        pool.add_tick_source(acc_idx, start_tick);
    }

    Some(pool)
}
