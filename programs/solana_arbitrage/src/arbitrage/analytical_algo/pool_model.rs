use crate::programs::{PoolKind, ProgramMeta};
use crate::compat::Pubkey;

/// Mathematical model of a pool for one specific swap direction.
/// Extracted from a ProgramInstance + input_mint at analysis time.
#[derive(Debug, Clone, Copy)]
pub enum PoolModel {
    /// Standard constant-product AMM with fee deducted from input BEFORE swap.
    /// out = reserve_out * dx * fee / (reserve_in + dx * fee)
    /// Applies to: RaydiumAmm, RaydiumCPMM, MeteoraDammV1, PumpAmm buy direction
    CpFeeOnInput { reserve_in: f64, reserve_out: f64, fee: f64, marginal_price: f64 },

    /// Constant-product AMM with fee deducted from output AFTER swap.
    /// out = reserve_out * dx / (reserve_in + dx) * fee
    /// Applies to: PumpAmm sell direction, MeteoraDammV2 (BothToken mode)
    CpFeeOnOutput { reserve_in: f64, reserve_out: f64, fee: f64, marginal_price: f64 },

    /// Linear pricing within active bin (DLMM single-bin approximation).
    /// out = dx * price * fee
    /// Applies to: MeteoraDLMM
    Linear { price: f64, fee: f64, max_in: u64, bin_step_frac: f64, marginal_price: f64 },

    /// Concentrated-liquidity (sqrt-price) pool — CP within active tick range.
    /// Uses virtual reserves: out = reserve_out * dx * fee / (reserve_in + dx * fee)
    /// Mathematically identical to CpFeeOnInput but with virtual reserves from L/sqrt_P.
    /// max_in = gross capacity of the active tick range.
    /// Applies to: RaydiumCLMM, OrcaWhirlpool
    Clmm { reserve_in: f64, reserve_out: f64, fee: f64, max_in: u64, marginal_price: f64 },

    /// No closed-form available. Use fast_quote + golden section fallback.
    Opaque { marginal_price: f64 },
}

impl PoolModel {
    /// Short label for logging.
    pub fn label(&self) -> &'static str {
        match self {
            PoolModel::CpFeeOnInput { .. } => "CP_FI",
            PoolModel::CpFeeOnOutput { .. } => "CP_FO",
            PoolModel::Linear { .. } => "DLMM",
            PoolModel::Clmm { .. } => "CLMM",
            PoolModel::Opaque { .. } => "Opaque",
        }
    }

    /// Fee factor for this pool model (1.0 = no fee).
    #[inline]
    pub fn fee(&self) -> f64 {
        match self {
            PoolModel::CpFeeOnInput { fee, .. } => *fee,
            PoolModel::CpFeeOnOutput { fee, .. } => *fee,
            PoolModel::Linear { fee, .. } => *fee,
            PoolModel::Clmm { fee, .. } => *fee,
            PoolModel::Opaque { .. } => 1.0,
        }
    }

    /// Marginal effective price (output per unit input at zero trade size),
    /// including fees. Sourced from each program's get_prices() * fee_factor.
    #[inline]
    pub fn marginal_price(&self) -> f64 {
        match self {
            PoolModel::CpFeeOnInput { marginal_price, .. }
            | PoolModel::CpFeeOnOutput { marginal_price, .. }
            | PoolModel::Linear { marginal_price, .. }
            | PoolModel::Clmm { marginal_price, .. }
            | PoolModel::Opaque { marginal_price } => *marginal_price,
        }
    }
}

/// Extract PoolModels for both swap directions using pre-fetched prices and fees.
///
/// Skips `get_prices()` and `get_fee_factor()` — the caller supplies those values
/// (typically from a Phase 1 price scan). Only vault/CLMM reads are new here.
///
/// Returns (buy_model, sell_model) where buy = start_token→middle, sell = middle→start_token.
pub fn extract_pool_model_both_cached(
    instance: &dyn ProgramMeta,
    start_token: Pubkey,
    middle_mint: Pubkey,
    price_base_to_quote: f64,
    price_quote_to_base: f64,
    fee_base_to_quote: f64,
    fee_quote_to_base: f64,
) -> (PoolModel, PoolModel) {
    let kind = instance.pool_kind();
    let (base_mint, _quote_mint) = instance.get_mints();

    let buy_marginal_price = if start_token == *base_mint {
        price_base_to_quote * fee_base_to_quote
    } else {
        price_quote_to_base * fee_quote_to_base
    };
    let sell_marginal_price = if middle_mint == *base_mint {
        price_base_to_quote * fee_base_to_quote
    } else {
        price_quote_to_base * fee_quote_to_base
    };

    let buy_opaque = PoolModel::Opaque { marginal_price: buy_marginal_price };
    let sell_opaque = PoolModel::Opaque { marginal_price: sell_marginal_price };

    match kind {
        PoolKind::RaydiumAmm | PoolKind::RaydiumCPMM | PoolKind::MeteoraDammV1 => {
            let (base_reserve, quote_reserve) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return (buy_opaque, sell_opaque),
            };
            let base_reserve_f = base_reserve as f64;
            let quote_reserve_f = quote_reserve as f64;
            if base_reserve_f <= 0.0 || quote_reserve_f <= 0.0 {
                return (buy_opaque, sell_opaque);
            }

            let buy_model = if start_token == *base_mint {
                PoolModel::CpFeeOnInput { reserve_in: base_reserve_f, reserve_out: quote_reserve_f, fee: fee_base_to_quote, marginal_price: buy_marginal_price }
            } else {
                PoolModel::CpFeeOnInput { reserve_in: quote_reserve_f, reserve_out: base_reserve_f, fee: fee_quote_to_base, marginal_price: buy_marginal_price }
            };
            let sell_model = if middle_mint == *base_mint {
                PoolModel::CpFeeOnInput { reserve_in: base_reserve_f, reserve_out: quote_reserve_f, fee: fee_base_to_quote, marginal_price: sell_marginal_price }
            } else {
                PoolModel::CpFeeOnInput { reserve_in: quote_reserve_f, reserve_out: base_reserve_f, fee: fee_quote_to_base, marginal_price: sell_marginal_price }
            };
            (buy_model, sell_model)
        }

        PoolKind::PumpAmm => {
            let (base_reserve, quote_reserve) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return (buy_opaque, sell_opaque),
            };
            let fee_factor = fee_base_to_quote; // PumpAmm symmetric fee
            let base_reserve_f = base_reserve as f64;
            let quote_reserve_f = quote_reserve as f64;
            if base_reserve_f <= 0.0 || quote_reserve_f <= 0.0 {
                return (buy_opaque, sell_opaque);
            }

            let build_model = |input_mint: Pubkey, marginal_price: f64| -> PoolModel {
                if input_mint == *base_mint {
                    PoolModel::CpFeeOnOutput { reserve_in: base_reserve_f, reserve_out: quote_reserve_f, fee: fee_factor, marginal_price }
                } else {
                    PoolModel::CpFeeOnInput { reserve_in: quote_reserve_f, reserve_out: base_reserve_f, fee: fee_factor, marginal_price }
                }
            };
            (build_model(start_token, buy_marginal_price), build_model(middle_mint, sell_marginal_price))
        }

        PoolKind::MeteoraDlmm => {
            let build_dlmm = |input_mint: Pubkey, marginal_price: f64| -> PoolModel {
                let price = if input_mint == *base_mint { price_base_to_quote } else { price_quote_to_base };
                if price <= 0.0 || !price.is_finite() {
                    return PoolModel::Opaque { marginal_price };
                }
                let active_bin_capacity = instance.get_active_bin_max_in(input_mint).unwrap_or(0);
                // eprintln!("Input Mint {}, {}", input_mint, active_bin_capacity);
                let bin_step_frac = instance.get_bin_step_frac();
                let max_in = if active_bin_capacity > 0 {
                    active_bin_capacity
                } else {
                    // Active bin depleted — multibin walker will skip to next bin.
                    // Use a conservative 1-bin estimate from total so candidate isn't discarded.
                    let (total_max_in, _) = instance.get_cached_max_amounts(input_mint);
                    if total_max_in == 0 {
                        return PoolModel::Opaque { marginal_price };
                    }
                    // Cap to ~1 bin worth so single-bin fallback doesn't wildly overestimate
                    let one_bin_est = (total_max_in as f64 * bin_step_frac).max(1.0) as u64;
                    one_bin_est.min(total_max_in)
                };

                PoolModel::Linear { price, fee: fee_base_to_quote, max_in, bin_step_frac, marginal_price }
            };
            (build_dlmm(start_token, buy_marginal_price), build_dlmm(middle_mint, sell_marginal_price))
        }

        PoolKind::MeteoraDammV2 => {
            let (base_reserve, quote_reserve) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return (buy_opaque, sell_opaque),
            };
            let base_reserve_f = base_reserve as f64;
            let quote_reserve_f = quote_reserve as f64;
            if base_reserve_f <= 0.0 || quote_reserve_f <= 0.0 {
                return (buy_opaque, sell_opaque);
            }

            let build_model = |input_mint: Pubkey, marginal_price: f64| -> PoolModel {
                let (reserve_in, reserve_out, fee) = if input_mint == *base_mint {
                    (base_reserve_f, quote_reserve_f, fee_base_to_quote)
                } else {
                    (quote_reserve_f, base_reserve_f, fee_quote_to_base)
                };
                if instance.is_fee_on_input(input_mint) {
                    PoolModel::CpFeeOnInput { reserve_in, reserve_out, fee, marginal_price }
                } else {
                    PoolModel::CpFeeOnOutput { reserve_in, reserve_out, fee, marginal_price }
                }
            };
            (build_model(start_token, buy_marginal_price), build_model(middle_mint, sell_marginal_price))
        }

        // OrcaWhirlpool & RaydiumCLMM: sqrt-price concentrated liquidity.
        // Model as Clmm with virtual reserves from L/sqrt_P (exact CP within tick range).
        // max_in may be 0 if price is exactly at a tick boundary — the multi-tick
        // walker handles this by walking to the next tick with capacity.
        PoolKind::OrcaWhirlpool | PoolKind::RaydiumCLMM => {
            let build_model = |input_mint: Pubkey, marginal_price: f64| -> PoolModel {
                let (vr_in, vr_out) = match instance.get_clmm_virtual_reserves(input_mint) {
                    Some(v) if v.0 > 0.0 && v.1 > 0.0 => v,
                    _ => return PoolModel::Opaque { marginal_price },
                };
                let max_in = instance.get_active_bin_max_in(input_mint).unwrap_or(0);
                let fee = if input_mint == *base_mint { fee_base_to_quote } else { fee_quote_to_base };
                PoolModel::Clmm { reserve_in: vr_in, reserve_out: vr_out, fee, max_in, marginal_price }
            };
            (build_model(start_token, buy_marginal_price), build_model(middle_mint, sell_marginal_price))
        }
    }
}

/// Extract PoolModels for both swap directions at once.
/// Fetches prices/fees then delegates to [`extract_pool_model_both_cached`].
pub fn extract_pool_model_both(
    instance: &dyn ProgramMeta,
    start_token: Pubkey,
    middle_mint: Pubkey,
) -> (PoolModel, PoolModel) {
    let (price_btq, price_qtb) = instance.get_prices().unwrap_or((0.0, 0.0));
    let (fee_btq, fee_qtb) = instance.get_fee_factor().unwrap_or((1.0, 1.0));
    extract_pool_model_both_cached(instance, start_token, middle_mint, price_btq, price_qtb, fee_btq, fee_qtb)
}

/// Extract a PoolModel for a specific swap direction from a ProgramInstance.
///
/// `instance`: the pool's ProgramMeta implementation
/// `input_mint`: which token is being input (determines directional reserves and fee type)
pub fn extract_pool_model(instance: &dyn ProgramMeta, input_mint: Pubkey) -> PoolModel {
    let kind = instance.pool_kind();
    let (base_mint, _quote_mint) = instance.get_mints();

    let (price_base_to_quote, price_quote_to_base) = instance.get_prices().unwrap_or((0.0, 0.0));
    let (fee_base_to_quote, fee_quote_to_base) = instance.get_fee_factor().unwrap_or((1.0, 1.0));

    let marginal_price = if input_mint == *base_mint {
        price_base_to_quote * fee_base_to_quote
    } else {
        price_quote_to_base * fee_quote_to_base
    };

    let opaque = PoolModel::Opaque { marginal_price };

    match kind {
        PoolKind::RaydiumAmm | PoolKind::RaydiumCPMM | PoolKind::MeteoraDammV1 => {
            let (base_reserve, quote_reserve) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return opaque,
            };

            let (reserve_in, reserve_out, fee) = if input_mint == *base_mint {
                (base_reserve as f64, quote_reserve as f64, fee_base_to_quote)
            } else {
                (quote_reserve as f64, base_reserve as f64, fee_quote_to_base)
            };

            if reserve_in <= 0.0 || reserve_out <= 0.0 {
                return opaque;
            }

            PoolModel::CpFeeOnInput { reserve_in, reserve_out, fee, marginal_price }
        }

        PoolKind::PumpAmm => {
            let (base_reserve, quote_reserve) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return opaque,
            };
            let fee_factor = fee_base_to_quote; // PumpAmm symmetric fee

            if input_mint == *base_mint {
                // Selling base: fee is on OUTPUT (after swap)
                let reserve_in = base_reserve as f64;
                let reserve_out = quote_reserve as f64;
                if reserve_in <= 0.0 || reserve_out <= 0.0 {
                    return opaque;
                }
                PoolModel::CpFeeOnOutput { reserve_in, reserve_out, fee: fee_factor, marginal_price }
            } else {
                // Buying base: fee is on INPUT (before swap)
                let reserve_in = quote_reserve as f64;
                let reserve_out = base_reserve as f64;
                if reserve_in <= 0.0 || reserve_out <= 0.0 {
                    return opaque;
                }
                PoolModel::CpFeeOnInput { reserve_in, reserve_out, fee: fee_factor, marginal_price }
            }
        }

        PoolKind::MeteoraDlmm => {
            let fee_factor = fee_base_to_quote;

            let price = if input_mint == *base_mint {
                price_base_to_quote
            } else {
                price_quote_to_base
            };

            if price <= 0.0 || !price.is_finite() {
                return opaque;
            }

            let active_bin_capacity = instance.get_active_bin_max_in(input_mint).unwrap_or(0);
            let bin_step_frac = instance.get_bin_step_frac();
            let max_in = if active_bin_capacity > 0 {
                active_bin_capacity
            } else {
                let (total_max_in, _) = instance.get_cached_max_amounts(input_mint);
                if total_max_in == 0 {
                    return opaque;
                }
                let one_bin_est = (total_max_in as f64 * bin_step_frac).max(1.0) as u64;
                one_bin_est.min(total_max_in)
            };

            PoolModel::Linear { price, fee: fee_factor, max_in, bin_step_frac, marginal_price }
        }

        PoolKind::MeteoraDammV2 => {
            let (base_reserve, quote_reserve) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return opaque,
            };

            let (reserve_in, reserve_out, fee) = if input_mint == *base_mint {
                (base_reserve as f64, quote_reserve as f64, fee_base_to_quote)
            } else {
                (quote_reserve as f64, base_reserve as f64, fee_quote_to_base)
            };

            if reserve_in <= 0.0 || reserve_out <= 0.0 {
                return opaque;
            }

            if instance.is_fee_on_input(input_mint) {
                PoolModel::CpFeeOnInput { reserve_in, reserve_out, fee, marginal_price }
            } else {
                PoolModel::CpFeeOnOutput { reserve_in, reserve_out, fee, marginal_price }
            }
        }

        // OrcaWhirlpool & RaydiumCLMM: sqrt-price concentrated liquidity.
        PoolKind::OrcaWhirlpool | PoolKind::RaydiumCLMM => {
            let (vr_in, vr_out) = match instance.get_clmm_virtual_reserves(input_mint) {
                Some(v) if v.0 > 0.0 && v.1 > 0.0 => v,
                _ => return opaque,
            };
            let max_in = instance.get_active_bin_max_in(input_mint).unwrap_or(0);
            let fee = if input_mint == *base_mint { fee_base_to_quote } else { fee_quote_to_base };
            PoolModel::Clmm { reserve_in: vr_in, reserve_out: vr_out, fee, max_in, marginal_price }
        }
    }
}
