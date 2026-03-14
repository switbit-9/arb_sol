use crate::programs::ProgramMeta;
use anchor_lang::prelude::Pubkey;

/// Mathematical model of a pool for one specific swap direction.
/// Extracted from a ProgramInstance + input_mint at analysis time.
#[derive(Debug, Clone, Copy)]
pub enum PoolModel {
    /// Standard constant-product AMM with fee deducted from input BEFORE swap.
    /// out = r_out * dx * fee / (r_in + dx * fee)
    /// Applies to: RaydiumAmm, RaydiumCPMM, MeteoraDammV1, PumpAmm buy direction
    CpFeeOnInput { reserve_in: f64, reserve_out: f64, fee: f64, marginal_price: f64 },

    /// Constant-product AMM with fee deducted from output AFTER swap.
    /// out = r_out * dx / (r_in + dx) * fee
    /// Applies to: PumpAmm sell direction, MeteoraDammV2 (BothToken mode)
    CpFeeOnOutput { reserve_in: f64, reserve_out: f64, fee: f64, marginal_price: f64 },

    /// Linear pricing within active bin (DLMM single-bin approximation).
    /// out = dx * price * fee
    /// Applies to: MeteoraDLMM
    Linear { price: f64, fee: f64, max_in: u64, bin_step_frac: f64, marginal_price: f64 },

    /// No closed-form available. Use fast_quote + golden section fallback.
    /// Applies to: OrcaWhirlpool, RaydiumCLMM
    Opaque { marginal_price: f64 },
}

impl PoolModel {
    /// Short label for logging.
    pub fn label(&self) -> &'static str {
        match self {
            PoolModel::CpFeeOnInput { .. } => "CP_FI",
            PoolModel::CpFeeOnOutput { .. } => "CP_FO",
            PoolModel::Linear { .. } => "DLMM",
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
            | PoolModel::Opaque { marginal_price } => *marginal_price,
        }
    }
}

/// Extract PoolModels for both swap directions at once, sharing expensive lookups
/// (get_vault_amounts, get_fee_factor, get_prices) between buy and sell.
///
/// Returns (buy_model, sell_model) where buy = start_token→middle, sell = middle→start_token.
pub fn extract_pool_model_both(
    instance: &dyn ProgramMeta,
    start_token: Pubkey,
    middle_mint: Pubkey,
) -> (PoolModel, PoolModel) {
    let name = instance.name();
    let (base_mint, _quote_mint) = instance.get_mints();

    // Shared lookups — done once instead of twice
    let (price_a_to_b, price_b_to_a) = instance.get_prices().unwrap_or((0.0, 0.0));
    let (fee_a_to_b, fee_b_to_a) = instance.get_fee_factor().unwrap_or((1.0, 1.0));

    let buy_marginal = if start_token == *base_mint {
        price_a_to_b * fee_a_to_b
    } else {
        price_b_to_a * fee_b_to_a
    };
    let sell_marginal = if middle_mint == *base_mint {
        price_a_to_b * fee_a_to_b
    } else {
        price_b_to_a * fee_b_to_a
    };

    let buy_opaque = PoolModel::Opaque { marginal_price: buy_marginal };
    let sell_opaque = PoolModel::Opaque { marginal_price: sell_marginal };

    match name {
        "RaydiumAmm" | "RaydiumCPMM" | "MeteoraDammV1" => {
            let (base_vault, quote_vault) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return (buy_opaque, sell_opaque),
            };
            let bv = base_vault as f64;
            let qv = quote_vault as f64;
            if bv <= 0.0 || qv <= 0.0 {
                return (buy_opaque, sell_opaque);
            }

            let buy = if start_token == *base_mint {
                PoolModel::CpFeeOnInput { reserve_in: bv, reserve_out: qv, fee: fee_a_to_b, marginal_price: buy_marginal }
            } else {
                PoolModel::CpFeeOnInput { reserve_in: qv, reserve_out: bv, fee: fee_b_to_a, marginal_price: buy_marginal }
            };
            let sell = if middle_mint == *base_mint {
                PoolModel::CpFeeOnInput { reserve_in: bv, reserve_out: qv, fee: fee_a_to_b, marginal_price: sell_marginal }
            } else {
                PoolModel::CpFeeOnInput { reserve_in: qv, reserve_out: bv, fee: fee_b_to_a, marginal_price: sell_marginal }
            };
            (buy, sell)
        }

        "PumpAmm" => {
            let (base_vault, quote_vault) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return (buy_opaque, sell_opaque),
            };
            let fee_factor = fee_a_to_b; // PumpAmm symmetric fee
            let bv = base_vault as f64;
            let qv = quote_vault as f64;
            if bv <= 0.0 || qv <= 0.0 {
                return (buy_opaque, sell_opaque);
            }

            let make_model = |input_mint: Pubkey, mp: f64| -> PoolModel {
                if input_mint == *base_mint {
                    PoolModel::CpFeeOnOutput { reserve_in: bv, reserve_out: qv, fee: fee_factor, marginal_price: mp }
                } else {
                    PoolModel::CpFeeOnInput { reserve_in: qv, reserve_out: bv, fee: fee_factor, marginal_price: mp }
                }
            };
            (make_model(start_token, buy_marginal), make_model(middle_mint, sell_marginal))
        }

        "MeteoraDLMM" => {
            let make_dlmm = |input_mint: Pubkey, mp: f64| -> PoolModel {
                let price = if input_mint == *base_mint { price_a_to_b } else { price_b_to_a };
                if price <= 0.0 || !price.is_finite() {
                    return PoolModel::Opaque { marginal_price: mp };
                }
                let max_in_active = instance.get_active_bin_max_in(input_mint).unwrap_or(u64::MAX);
                let bin_step_frac = instance.get_bin_step_frac();
                let (max_in, price) = if max_in_active == 0 {
                    let (total_max_in, _) = instance.get_cached_max_amounts(input_mint);
                    if total_max_in == 0 {
                        return PoolModel::Opaque { marginal_price: mp };
                    }
                    (total_max_in, price / (1.0 + bin_step_frac))
                } else {
                    (max_in_active, price)
                };
                PoolModel::Linear { price, fee: fee_a_to_b, max_in, bin_step_frac, marginal_price: mp }
            };
            (make_dlmm(start_token, buy_marginal), make_dlmm(middle_mint, sell_marginal))
        }

        "MeteoraDammV2" => {
            let (base_vault, quote_vault) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return (buy_opaque, sell_opaque),
            };
            let bv = base_vault as f64;
            let qv = quote_vault as f64;
            if bv <= 0.0 || qv <= 0.0 {
                return (buy_opaque, sell_opaque);
            }

            let make_model = |input_mint: Pubkey, mp: f64| -> PoolModel {
                let (r_in, r_out, fee) = if input_mint == *base_mint {
                    (bv, qv, fee_a_to_b)
                } else {
                    (qv, bv, fee_b_to_a)
                };
                if instance.is_fee_on_input(input_mint) {
                    PoolModel::CpFeeOnInput { reserve_in: r_in, reserve_out: r_out, fee, marginal_price: mp }
                } else {
                    PoolModel::CpFeeOnOutput { reserve_in: r_in, reserve_out: r_out, fee, marginal_price: mp }
                }
            };
            (make_model(start_token, buy_marginal), make_model(middle_mint, sell_marginal))
        }

        "OrcaWhirlpool" | "RaydiumCLMM" => {
            let (base_vault, quote_vault) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return (buy_opaque, sell_opaque),
            };
            let bv = base_vault as f64;
            let qv = quote_vault as f64;
            if bv <= 0.0 || qv <= 0.0 {
                return (buy_opaque, sell_opaque);
            }

            let make_model = |input_mint: Pubkey, mp: f64| -> PoolModel {
                let (r_in, r_out) = if input_mint == *base_mint { (bv, qv) } else { (qv, bv) };
                PoolModel::CpFeeOnInput { reserve_in: r_in, reserve_out: r_out, fee: fee_a_to_b, marginal_price: mp }
            };
            (make_model(start_token, buy_marginal), make_model(middle_mint, sell_marginal))
        }

        _ => (buy_opaque, sell_opaque),
    }
}

/// Extract a PoolModel for a specific swap direction from a ProgramInstance.
///
/// `instance`: the pool's ProgramMeta implementation
/// `input_mint`: which token is being input (determines directional reserves and fee type)
pub fn extract_pool_model(instance: &dyn ProgramMeta, input_mint: Pubkey) -> PoolModel {
    let name = instance.name();
    let (base_mint, _quote_mint) = instance.get_mints();

    let (price_a_to_b, price_b_to_a) = instance.get_prices().unwrap_or((0.0, 0.0));
    let (fee_a_to_b, fee_b_to_a) = instance.get_fee_factor().unwrap_or((1.0, 1.0));

    // Compute marginal price from the program's own price calculation, fee-adjusted.
    let marginal_price = if input_mint == *base_mint {
        price_a_to_b * fee_a_to_b
    } else {
        price_b_to_a * fee_b_to_a
    };

    let opaque = PoolModel::Opaque { marginal_price };

    match name {
        "RaydiumAmm" | "RaydiumCPMM" | "MeteoraDammV1" => {
            let (base_vault, quote_vault) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return opaque,
            };

            let (r_in, r_out, fee) = if input_mint == *base_mint {
                (base_vault as f64, quote_vault as f64, fee_a_to_b)
            } else {
                (quote_vault as f64, base_vault as f64, fee_b_to_a)
            };

            if r_in <= 0.0 || r_out <= 0.0 {
                return opaque;
            }

            PoolModel::CpFeeOnInput { reserve_in: r_in, reserve_out: r_out, fee, marginal_price }
        }

        "PumpAmm" => {
            let (base_vault, quote_vault) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return opaque,
            };
            // PumpAmm returns symmetric fee_factor, but application differs by direction
            let fee_factor = fee_a_to_b;

            if input_mint == *base_mint {
                // Selling base: input = base token, output = quote (SOL)
                // Fee is on OUTPUT (after swap)
                let r_in = base_vault as f64;
                let r_out = quote_vault as f64;
                if r_in <= 0.0 || r_out <= 0.0 {
                    return opaque;
                }
                PoolModel::CpFeeOnOutput {
                    reserve_in: r_in,
                    reserve_out: r_out,
                    fee: fee_factor,
                    marginal_price,
                }
            } else {
                // Buying base: input = quote (SOL), output = base token
                // Fee is on INPUT (before swap)
                let r_in = quote_vault as f64;
                let r_out = base_vault as f64;
                if r_in <= 0.0 || r_out <= 0.0 {
                    return opaque;
                }
                PoolModel::CpFeeOnInput {
                    reserve_in: r_in,
                    reserve_out: r_out,
                    fee: fee_factor,
                    marginal_price,
                }
            }
        }

        "MeteoraDLMM" => {
            let fee_factor = fee_a_to_b;

            let price = if input_mint == *base_mint {
                price_a_to_b
            } else {
                price_b_to_a
            };

            if price <= 0.0 || !price.is_finite() {
                return opaque;
            }

            let max_in_active = instance.get_active_bin_max_in(input_mint).unwrap_or(u64::MAX);
            let bin_step_frac = instance.get_bin_step_frac();

            // When the active bin is empty but the pool has liquidity in adjacent
            // bins, fall back to the total capacity with a conservative price
            // (shifted by one bin step). The analytical formula will set
            // dlmm_capped=true, triggering multibin/golden-section refinement.
            let (max_in, price) = if max_in_active == 0 {
                let (total_max_in, _) = instance.get_cached_max_amounts(input_mint);
                if total_max_in == 0 {
                    return opaque;
                }
                // Next bin price is worse by one bin step
                let conservative_price = price / (1.0 + bin_step_frac);
                (total_max_in, conservative_price)
            } else {
                (max_in_active, price)
            };

            PoolModel::Linear {
                price,
                fee: fee_factor,
                max_in,
                bin_step_frac,
                marginal_price,
            }
        }

        "MeteoraDammV2" => {
            // CLAMM: virtual reserves from liquidity + sqrt_price behave as constant-product
            let (base_vault, quote_vault) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return opaque,
            };

            let (r_in, r_out, fee) = if input_mint == *base_mint {
                (base_vault as f64, quote_vault as f64, fee_a_to_b)
            } else {
                (quote_vault as f64, base_vault as f64, fee_b_to_a)
            };

            if r_in <= 0.0 || r_out <= 0.0 {
                return opaque;
            }

            if instance.is_fee_on_input(input_mint) {
                PoolModel::CpFeeOnInput { reserve_in: r_in, reserve_out: r_out, fee, marginal_price }
            } else {
                PoolModel::CpFeeOnOutput { reserve_in: r_in, reserve_out: r_out, fee, marginal_price }
            }
        }

        // Concentrated liquidity pools modelled as constant-product within the
        // active tick range using virtual reserves: v_a = L/√P, v_b = L×√P
        "OrcaWhirlpool" | "RaydiumCLMM" => {
            let (base_vault, quote_vault) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return opaque,
            };
            let fee_factor = fee_a_to_b;

            let (r_in, r_out) = if input_mint == *base_mint {
                (base_vault as f64, quote_vault as f64)
            } else {
                (quote_vault as f64, base_vault as f64)
            };

            if r_in <= 0.0 || r_out <= 0.0 {
                return opaque;
            }

            PoolModel::CpFeeOnInput { reserve_in: r_in, reserve_out: r_out, fee: fee_factor, marginal_price }
        }

        _ => opaque,
    }
}
