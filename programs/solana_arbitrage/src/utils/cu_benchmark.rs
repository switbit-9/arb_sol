use anchor_lang::prelude::*;
use crate::programs::{ProgramInstance, ProgramMeta};
use crate::dex_type;
use crate::utils::bot_config::BotConfig;
use crate::pool_vec::PoolVec;
use crate::{parse_accounts, InstructionData, arb_mode, WSOL};

#[cfg(target_os = "solana")]
fn sol_log_compute_units() {
    unsafe { solana_msg::syscalls::sol_log_compute_units_() }
}
#[cfg(not(target_os = "solana"))]
fn sol_log_compute_units() {}

/// Public wrapper so other modules can log remaining CU.
pub fn log_cu() {
    sol_log_compute_units();
}

#[cfg(target_os = "solana")]
fn sol_remaining_compute_units() -> u64 {
    extern "C" { fn sol_remaining_compute_units() -> u64; }
    unsafe { sol_remaining_compute_units() }
}
#[cfg(not(target_os = "solana"))]
fn sol_remaining_compute_units() -> u64 { 0 }

#[inline(never)]
pub fn run_cu_benchmark() {
    use crate::arbitrage_checker::dlmm_sim::{DlmmPool, DlmmBin};
    use crate::arbitrage_checker::amm_sim::AmmPool;
    use crate::arbitrage_checker::dlmm_checker::{check_amm_to_dlmm, check_dlmm_to_amm};
    use crate::arbitrage::analytical_algo::{PoolModel, analytical_optimal_2pool};

    let one: u128 = 1u128 << 64;

    // ── Pump pool: cheap base (price ~0.00001 SOL/token) ──
    let pump = AmmPool::from_pump(1_000_000_000_000, 10_000_000);

    // ── DLMM: single bin at price=1.0 ──
    let dlmm_1bin = DlmmPool::new(
        0, 100, 10, 0, 0, 0, 0, 0, 0,
        &[DlmmBin { id: 0, amount_x: 0, amount_y: 10_000_000_000, price: one }],
    );

    // ── DLMM: 10 bins (realistic multi-bin) ──
    let bins_10: [DlmmBin; 10] = core::array::from_fn(|i| DlmmBin {
        id: i as i32,
        amount_x: 0,
        amount_y: 1_000_000_000,
        price: one,
    });
    let dlmm_10bin = DlmmPool::new(0, 100, 10, 0, 0, 0, 0, 0, 0, &bins_10);

    let fee_f64 = (1_000_000.0 - pump.buy_input_fee as f64) / 1_000_000.0;

    // ── Analytical PoolModels ──
    // Pump buy side (CP fee-on-input): quote → base
    let pump_buy_model = PoolModel::CpFeeOnInput {
        reserve_in: pump.quote_vault as f64,
        reserve_out: pump.base_vault as f64,
        fee: fee_f64,
        marginal_price: pump.base_vault as f64 * fee_f64 / pump.quote_vault as f64,
    };
    // DLMM sell side (Linear): base → quote
    let dlmm_sell_model = PoolModel::Linear {
        price: 1.0,
        fee: 0.999, // ~0.1% base fee for bin_step=100, base_factor=10
        max_in: 10_000_000_000,
        bin_step_frac: 0.01,
        marginal_price: 0.999,
    };
    // DLMM buy side (Linear): quote → base
    let dlmm_buy_model = PoolModel::Linear {
        price: 1.0,
        fee: 0.999,
        max_in: 10_000_000_000,
        bin_step_frac: 0.01,
        marginal_price: 0.999,
    };
    // Pump sell side (CP fee-on-output): base → quote
    let pump_sell_model = PoolModel::CpFeeOnOutput {
        reserve_in: pump.base_vault as f64,
        reserve_out: pump.quote_vault as f64,
        fee: fee_f64,
        marginal_price: pump.quote_vault as f64 * fee_f64 / pump.base_vault as f64,
    };

    // ════════════════════════════════════════════════════
    // PUMP → DLMM
    // ════════════════════════════════════════════════════

    msg!("=== CHECKER: pump_to_dlmm (1 bin) ===");
    sol_log_compute_units();
    let r = check_amm_to_dlmm(&pump, true, &dlmm_1bin, true, u64::MAX);
    sol_log_compute_units();
    msg!("  amt={} profit={}", r.amount_in, r.profit);

    msg!("=== CHECKER: pump_to_dlmm (10 bins) ===");
    sol_log_compute_units();
    let r = check_amm_to_dlmm(&pump, true, &dlmm_10bin, true, u64::MAX);
    sol_log_compute_units();
    msg!("  amt={} profit={}", r.amount_in, r.profit);

    msg!("=== ANALYTICAL: pump_to_dlmm (CP_FI + Linear) ===");
    sol_log_compute_units();
    let r = analytical_optimal_2pool(&pump_buy_model, &dlmm_sell_model, u64::MAX);
    sol_log_compute_units();
    msg!("  result={:?}", r.map(|r| r.optimal_amount));

    // ════════════════════════════════════════════════════
    // DLMM → PUMP
    // ════════════════════════════════════════════════════

    msg!("=== CHECKER: dlmm_to_pump (1 bin) ===");
    sol_log_compute_units();
    let r = check_dlmm_to_amm(&dlmm_1bin, false, &pump, true, u64::MAX);
    sol_log_compute_units();
    msg!("  amt={} profit={}", r.amount_in, r.profit);

    msg!("=== CHECKER: dlmm_to_pump (10 bins) ===");
    sol_log_compute_units();
    let r = check_dlmm_to_amm(&dlmm_10bin, false, &pump, true, u64::MAX);
    sol_log_compute_units();
    msg!("  amt={} profit={}", r.amount_in, r.profit);

    msg!("=== ANALYTICAL: dlmm_to_pump (Linear + CP_FO) ===");
    sol_log_compute_units();
    let r = analytical_optimal_2pool(&dlmm_buy_model, &pump_sell_model, u64::MAX);
    sol_log_compute_units();
    msg!("  result={:?}", r.map(|r| r.optimal_amount));
}

/// CU benchmark using real accounts: measures parse, analytical, and checker separately.
#[inline(never)]
pub fn run_cu_benchmark_accounts<'info>(
    accounts: &[AccountInfo<'info>],
    data: InstructionData,
    clock: Clock,
) -> Result<()> {
    let num_mints = data.mints as usize;
    let shared_statics_start = 4 + num_mints * 2;
    let pool_start = shared_statics_start + data.shared_statics_len as usize;
    let max_amount_in: u64 = 3_000_000_000;

    let total_pools = data.pool_types.iter().filter(|&&t| dex_type::dynamic_account_count(t) > 0).count() as u8;

    // ── Parse accounts ──
    msg!("=== PARSE ACCOUNTS ===");
    sol_log_compute_units();
    let mut instances = parse_accounts(
        accounts, shared_statics_start, pool_start, &data, &clock, 0, total_pools as usize,
    )?;
    sol_log_compute_units();
    msg!("  pools_parsed={}", instances.len());

    let mut group_config = BotConfig::new(
        Some(WSOL), max_amount_in, 5_500, 2,
        arb_mode::SINGLE_PAIR_MULTI_MARKET, clock.clone(), false,
    );

    // Each benchmark in its own #[inline(never)] fn so stack frames don't accumulate.
    bench_checker_subset(accounts, &instances, &group_config, max_amount_in);
    bench_checker_single_pair(accounts, &instances, max_amount_in);

    Ok(())
}

#[inline(never)]
fn bench_checker_subset<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance],
    group_config: &BotConfig,
    _max_amount_in: u64,
) {
    let subset: Vec<ProgramInstance> = instances.iter().take(2).cloned().collect();
    msg!("=== CHECKER: run_arb_checker (2 pools) ===");
    let cu_before = sol_remaining_compute_units();
    let checker_result = crate::arbitrage_checker::runner::run_arb_checker(accounts, &subset, group_config);
    let cu_after = sol_remaining_compute_units();
    let cu_used = cu_before.saturating_sub(cu_after);
    msg!("  profit={} amount={} cu_used={}", checker_result.profit, checker_result.amount_in, cu_used);
}


#[inline(never)]
fn bench_checker_single_pair<'info>(
    accounts: &[AccountInfo<'info>],
    instances: &[ProgramInstance],
    max_amount_in: u64,
) {
    use crate::programs::PoolKind;
    let start_token = WSOL;

    let mut amm_idx: Option<usize> = None;
    let mut dlmm_idx: Option<usize> = None;
    for i in 0..instances.len() {
        let kind = instances[i].pool_kind();
        if amm_idx.is_none() && matches!(kind, PoolKind::PumpAmm | PoolKind::RaydiumAmm) {
            amm_idx = Some(i);
        }
        if dlmm_idx.is_none() && kind == PoolKind::MeteoraDlmm {
            dlmm_idx = Some(i);
        }
    }
    if let (Some(ai), Some(di)) = (amm_idx, dlmm_idx) {
        let amm = crate::arbitrage_checker::runner::extract_amm(&instances[ai]);
        let buy_sfy = match &instances[di] {
            ProgramInstance::MeteoraDlmm(d) => start_token != d.base_token_pk,
            _ => false,
        };
        let dlmm = crate::arbitrage_checker::runner::extract_dlmm(&instances[di], accounts, buy_sfy);
        if let (Some(amm), Some(dlmm)) = (amm, dlmm) {
            let sfy = match &instances[di] {
                ProgramInstance::MeteoraDlmm(d) => start_token != d.base_token_pk,
                _ => false,
            };

            msg!("=== CHECKER: amm_to_dlmm (single pair) ===");
            sol_log_compute_units();
            let r = crate::arbitrage_checker::dlmm_checker::check_amm_to_dlmm(&amm, true, &dlmm, sfy, max_amount_in);
            sol_log_compute_units();
            msg!("  profit={} amount={}", r.profit, r.amount_in);

            msg!("=== CHECKER: dlmm_to_amm (single pair) ===");
            sol_log_compute_units();
            let r = crate::arbitrage_checker::dlmm_checker::check_dlmm_to_amm(&dlmm, !sfy, &amm, true, max_amount_in);
            sol_log_compute_units();
            msg!("  profit={} amount={}", r.profit, r.amount_in);
        }
    }
}
