use crate::programs::{ProgramInstance, ProgramMeta};
use crate::utils::bot_config::BotConfig;
use crate::utils::token::{get_transfer_fees, MintFee};
use pinocchio::account_info::AccountInfo;
use pinocchio::program_error::ProgramError;
use pinocchio::pubkey::Pubkey;
use pinocchio::sysvars::clock::Clock;

type Result<T> = core::result::Result<T, ProgramError>;

const WSOL: Pubkey = five8_const::decode_32_const("So11111111111111111111111111111111111111112");

/// Maximum golden-section iterations. 50 iterations on a 1 SOL range narrows
/// well below 1 lamport — fully exhaustive.
const MAX_ITERATIONS: usize = 50;

/// Minimum input amount to evaluate (avoids division-by-zero edge cases).
const MIN_AMOUNT: u64 = 1_000;

/// Integer golden-ratio division: x * 0.6180339... ≈ x * 6180 / 10000.
#[inline(always)]
fn golden_div(x: u64) -> u64 {
    ((x as u128) * 6180 / 10000) as u64
}

/// Simulate a 2-hop swap: buy on pool `buy_idx`, sell on pool `sell_idx`.
/// Returns profit = final_output - amount_in.
#[inline]
fn simulate_2hop(
    accounts: &[AccountInfo],
    instances: &mut [ProgramInstance],
    buy_idx: usize,
    sell_idx: usize,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    amount_in: u64,
    clock: &Clock,
    mint_fees: &[(Pubkey, MintFee)],
) -> Result<i128> {
    let (base1, quote1) = instances[buy_idx].get_mints();
    let (in_fee_1, out_fee_1) = get_transfer_fees(input_mint, base1, quote1, mint_fees);
    let middle_amount = instances[buy_idx].swap_base_in(
        accounts, input_mint, amount_in, in_fee_1, out_fee_1, clock,
    )?;
    let (base2, quote2) = instances[sell_idx].get_mints();
    let (in_fee_2, out_fee_2) = get_transfer_fees(middle_mint, base2, quote2, mint_fees);
    let final_amount = instances[sell_idx].swap_base_in(
        accounts, middle_mint, middle_amount, in_fee_2, out_fee_2, clock,
    )?;
    Ok(final_amount as i128 - amount_in as i128)
}

/// Simulate a 3-hop swap and return profit.
#[inline]
fn simulate_3hop(
    accounts: &[AccountInfo],
    instances: &mut [ProgramInstance],
    indices: &[usize; 3],
    mints: &[Pubkey; 4],
    amount_in: u64,
    clock: &Clock,
    mint_fees: &[(Pubkey, MintFee)],
) -> Result<i128> {
    let mut current = amount_in;
    for hop in 0..3 {
        let (base, quote) = instances[indices[hop]].get_mints();
        let (in_fee, out_fee) = get_transfer_fees(mints[hop], base, quote, mint_fees);
        current = instances[indices[hop]].swap_base_in(
            accounts, mints[hop], current, in_fee, out_fee, clock,
        )?;
    }
    Ok(current as i128 - amount_in as i128)
}

/// Result of a golden-section search.
#[derive(Debug, Clone)]
pub struct GoldenSearchResult {
    pub buy_idx: usize,
    pub sell_idx: usize,
    pub optimal_amount: u64,
    pub profit: i128,
}

/// Golden-section search over all 2-hop pool pairs using real `swap_base_in`.
///
/// For every (buy, sell) pair it runs a full golden-section search to find the
/// input amount that maximises profit, then returns the best pair + amount.
/// This is the ground-truth reference — accurate but expensive (many swap calls).
pub fn golden_search_2hop(
    accounts: &[AccountInfo],
    instances: &mut [ProgramInstance],
    config: &BotConfig,
    mint_fees: &[(Pubkey, MintFee)],
) -> Option<GoldenSearchResult> {
    let start_token = config.start_token.unwrap_or(WSOL);
    let (base_mint, quote_mint) = instances.first()?.get_mints();
    let middle_mint = if *base_mint == start_token { *quote_mint } else { *base_mint };
    let max_amount = config.max_amount_in;

    let n = instances.len();
    let mut best: Option<GoldenSearchResult> = None;

    for bi in 0..n {
        for si in 0..n {
            if bi == si { continue; }

            let result = golden_section_on_pair(
                accounts, instances, bi, si,
                start_token, middle_mint, max_amount,
                &config.clock, mint_fees,
            );

            if let Some((amount, profit)) = result {
                if profit > 0 && best.as_ref().map_or(true, |b| profit > b.profit) {
                    best = Some(GoldenSearchResult {
                        buy_idx: bi,
                        sell_idx: si,
                        optimal_amount: amount,
                        profit,
                    });
                }
            }
        }
    }

    best
}

/// Run golden-section search on a single (buy, sell) pair.
/// Returns `Some((optimal_amount, profit))` or `None` if the pair errors out.
fn golden_section_on_pair(
    accounts: &[AccountInfo],
    instances: &mut [ProgramInstance],
    buy_idx: usize,
    sell_idx: usize,
    input_mint: Pubkey,
    middle_mint: Pubkey,
    max_amount: u64,
    clock: &Clock,
    mint_fees: &[(Pubkey, MintFee)],
) -> Option<(u64, i128)> {
    let mut a = MIN_AMOUNT;
    let mut b = max_amount;
    if b <= a { return None; }

    macro_rules! eval {
        ($amt:expr) => {
            simulate_2hop(
                accounts, instances, buy_idx, sell_idx,
                input_mint, middle_mint, $amt, clock, mint_fees,
            ).unwrap_or(i128::MIN)
        };
    }

    let mut c = b - golden_div(b - a);
    let mut d = a + golden_div(b - a);
    let mut fc = eval!(c);
    let mut fd = eval!(d);

    for _ in 0..MAX_ITERATIONS {
        if b <= a + 1 { break; }
        if fc > fd {
            b = d; d = c; fd = fc;
            c = b.saturating_sub(golden_div(b - a));
            fc = eval!(c);
        } else {
            a = c; c = d; fc = fd;
            d = a + golden_div(b.saturating_sub(a));
            fd = eval!(d);
        }
    }

    let optimal = (a + b) / 2;
    let profit = eval!(optimal);
    Some((optimal, profit))
}

/// Golden-section search result for a 3-hop path.
#[derive(Debug, Clone)]
pub struct GoldenSearch3HopResult {
    pub indices: [usize; 3],
    pub mints: [Pubkey; 4],
    pub optimal_amount: u64,
    pub profit: i128,
}

/// Golden-section search for a specific 3-hop path using real `swap_base_in`.
pub fn golden_search_3hop_path(
    accounts: &[AccountInfo],
    instances: &mut [ProgramInstance],
    indices: &[usize; 3],
    mints: &[Pubkey; 4],
    max_amount: u64,
    clock: &Clock,
    mint_fees: &[(Pubkey, MintFee)],
) -> Option<GoldenSearch3HopResult> {
    let mut a = MIN_AMOUNT;
    let mut b = max_amount;
    if b <= a { return None; }

    macro_rules! eval {
        ($amt:expr) => {
            simulate_3hop(
                accounts, instances, indices, mints, $amt, clock, mint_fees,
            ).unwrap_or(i128::MIN)
        };
    }

    let mut c = b - golden_div(b - a);
    let mut d = a + golden_div(b - a);
    let mut fc = eval!(c);
    let mut fd = eval!(d);

    for _ in 0..MAX_ITERATIONS {
        if b <= a + 1 { break; }
        if fc > fd {
            b = d; d = c; fd = fc;
            c = b.saturating_sub(golden_div(b - a));
            fc = eval!(c);
        } else {
            a = c; c = d; fc = fd;
            d = a + golden_div(b.saturating_sub(a));
            fd = eval!(d);
        }
    }

    let optimal = (a + b) / 2;
    let profit = eval!(optimal);

    Some(GoldenSearch3HopResult {
        indices: *indices,
        mints: *mints,
        optimal_amount: optimal,
        profit,
    })
}
