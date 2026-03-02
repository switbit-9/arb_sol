use crate::programs::ProgramMeta;
use anchor_lang::prelude::Pubkey;

/// Mathematical model of a pool for one specific swap direction.
/// Extracted from a ProgramInstance + input_mint at analysis time.
#[derive(Debug, Clone)]
pub enum PoolModel {
    /// Standard constant-product AMM with fee deducted from input BEFORE swap.
    /// out = r_out * dx * fee / (r_in + dx * fee)
    /// Applies to: RaydiumAmm, RaydiumCPMM, MeteoraDammV1, PumpAmm buy direction
    CpFeeOnInput { r_in: f64, r_out: f64, fee: f64 },

    /// Constant-product AMM with fee deducted from output AFTER swap.
    /// out = r_out * dx / (r_in + dx) * fee
    /// Applies to: PumpAmm sell direction, MeteoraDammV2 (BothToken mode)
    CpFeeOnOutput { r_in: f64, r_out: f64, fee: f64 },

    /// Linear pricing within active bin (DLMM single-bin approximation).
    /// out = dx * price * fee
    /// Applies to: MeteoraDLMM
    Linear { price: f64, fee: f64, max_in: u64 },

    /// No closed-form available. Use fast_quote + golden section fallback.
    /// Applies to: OrcaWhirlpool, RaydiumCLMM
    Opaque,
}

impl PoolModel {
    /// Short label for logging.
    pub fn label(&self) -> &'static str {
        match self {
            PoolModel::CpFeeOnInput { .. } => "CP_FI",
            PoolModel::CpFeeOnOutput { .. } => "CP_FO",
            PoolModel::Linear { .. } => "DLMM",
            PoolModel::Opaque => "Opaque",
        }
    }
}

/// Extract a PoolModel for a specific swap direction from a ProgramInstance.
///
/// `instance`: the pool's ProgramMeta implementation
/// `input_mint`: which token is being input (determines directional reserves and fee type)
pub fn extract_pool_model(instance: &dyn ProgramMeta, input_mint: Pubkey) -> PoolModel {
    let name = instance.name();
    let (base_mint, _quote_mint) = instance.get_mints();

    match name {
        "RaydiumAmm" | "RaydiumCPMM" | "MeteoraDammV1" => {
            let (base_vault, quote_vault) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return PoolModel::Opaque,
            };
            let (fee_a_to_b, fee_b_to_a) =
                instance.get_fee_factor().unwrap_or((1.0, 1.0));

            let (r_in, r_out, fee) = if input_mint == *base_mint {
                (base_vault as f64, quote_vault as f64, fee_a_to_b)
            } else {
                (quote_vault as f64, base_vault as f64, fee_b_to_a)
            };

            if r_in <= 0.0 || r_out <= 0.0 {
                return PoolModel::Opaque;
            }

            PoolModel::CpFeeOnInput { r_in, r_out, fee }
        }

        "PumpAmm" => {
            let (base_vault, quote_vault) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return PoolModel::Opaque,
            };
            // PumpAmm returns symmetric fee_factor, but application differs by direction
            let fee_factor = instance.get_fee_factor().unwrap_or((1.0, 1.0)).0;

            if input_mint == *base_mint {
                // Selling base: input = base token, output = quote (SOL)
                // Fee is on OUTPUT (after swap)
                let r_in = base_vault as f64;
                let r_out = quote_vault as f64;
                if r_in <= 0.0 || r_out <= 0.0 {
                    return PoolModel::Opaque;
                }
                PoolModel::CpFeeOnOutput {
                    r_in,
                    r_out,
                    fee: fee_factor,
                }
            } else {
                // Buying base: input = quote (SOL), output = base token
                // Fee is on INPUT (before swap)
                let r_in = quote_vault as f64;
                let r_out = base_vault as f64;
                if r_in <= 0.0 || r_out <= 0.0 {
                    return PoolModel::Opaque;
                }
                PoolModel::CpFeeOnInput {
                    r_in,
                    r_out,
                    fee: fee_factor,
                }
            }
        }

        "MeteoraDLMM" => {
            let (price_base_to_quote, price_quote_to_base) = match instance.get_prices() {
                Ok(p) => p,
                Err(_) => return PoolModel::Opaque,
            };
            let fee_factor = instance.get_fee_factor().unwrap_or((1.0, 1.0)).0;

            let price = if input_mint == *base_mint {
                price_base_to_quote
            } else {
                price_quote_to_base
            };

            if price <= 0.0 || !price.is_finite() {
                return PoolModel::Opaque;
            }

            let max_in = instance.get_active_bin_max_in(input_mint).unwrap_or(u64::MAX);

            PoolModel::Linear {
                price,
                fee: fee_factor,
                max_in,
            }
        }

        "MeteoraDammV2" => {
            // CLAMM: virtual reserves from liquidity + sqrt_price behave as constant-product
            let (base_vault, quote_vault) = match instance.get_vault_amounts() {
                Ok(v) => v,
                Err(_) => return PoolModel::Opaque,
            };
            let (fee_a_to_b, fee_b_to_a) =
                instance.get_fee_factor().unwrap_or((1.0, 1.0));

            let (r_in, r_out, fee) = if input_mint == *base_mint {
                (base_vault as f64, quote_vault as f64, fee_a_to_b)
            } else {
                (quote_vault as f64, base_vault as f64, fee_b_to_a)
            };

            if r_in <= 0.0 || r_out <= 0.0 {
                return PoolModel::Opaque;
            }

            if instance.is_fee_on_input(input_mint) {
                PoolModel::CpFeeOnInput { r_in, r_out, fee }
            } else {
                PoolModel::CpFeeOnOutput { r_in, r_out, fee }
            }
        }

        // OrcaWhirlpool, RaydiumCLMM — no closed form
        _ => PoolModel::Opaque,
    }
}
