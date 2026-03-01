use ruint::aliases::U256;
use static_assertions::const_assert_eq;

use anchor_lang::prelude::*;
use num_enum::{IntoPrimitive, TryFromPrimitive};

use super::super::{
    constants::{
        fee::get_max_fee_numerator,
        NUM_REWARDS,
    },
    curve::{
        get_delta_amount_a_unsigned, get_delta_amount_a_unsigned_unchecked,
        get_delta_amount_b_unsigned, get_next_sqrt_price_from_input,
        get_next_sqrt_price_from_output,
    },
    error::PoolError,
    math::{
        safe_math::SafeMath,
        u128x128_math::Rounding,
    },
    params::swap::TradeDirection,
};
use super::fee::{FeeOnAmountResult, PoolFeesStruct, SplitFees};

use super::fee::FeeMode;

/// collect fee mode
#[repr(u8)]
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    IntoPrimitive,
    TryFromPrimitive,
    AnchorDeserialize,
    AnchorSerialize,
)]
pub enum CollectFeeMode {
    /// Both token, in this mode only out token is collected
    BothToken,
    /// Only token B, we just need token B, because if user want to collect fee in token A, they just need to flip order of tokens
    OnlyB,
}

/// pool status
#[repr(u8)]
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    IntoPrimitive,
    TryFromPrimitive,
    AnchorDeserialize,
    AnchorSerialize,
)]
pub enum PoolStatus {
    Enable,
    Disable,
}

#[repr(u8)]
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    IntoPrimitive,
    TryFromPrimitive,
    AnchorDeserialize,
    AnchorSerialize,
)]
pub enum PoolType {
    Permissionless,
    Customizable,
}

#[repr(u8)]
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    IntoPrimitive,
    TryFromPrimitive,
    AnchorDeserialize,
    AnchorSerialize,
)]
pub enum PoolVersion {
    V0, // 0
    V1, // 1
}

#[account(zero_copy)]
#[derive(InitSpace, Debug, Default)]
pub struct Pool {
    /// Pool fee
    pub pool_fees: PoolFeesStruct,
    /// token a mint
    pub token_a_mint: Pubkey,
    /// token b mint
    pub token_b_mint: Pubkey,
    /// token a vault
    pub token_a_vault: Pubkey,
    /// token b vault
    pub token_b_vault: Pubkey,
    /// Whitelisted vault to be able to buy pool before activation_point
    pub whitelisted_vault: Pubkey,
    /// partner
    pub partner: Pubkey,
    /// liquidity share
    pub liquidity: u128,
    /// padding, previous reserve amount, be careful to use that field
    pub _padding: u128,
    /// protocol a fee
    pub protocol_a_fee: u64,
    /// protocol b fee
    pub protocol_b_fee: u64,
    /// partner a fee
    pub partner_a_fee: u64,
    /// partner b fee
    pub partner_b_fee: u64,
    /// min price
    pub sqrt_min_price: u128,
    /// max price
    pub sqrt_max_price: u128,
    /// current price
    pub sqrt_price: u128,
    /// Activation point, can be slot or timestamp
    pub activation_point: u64,
    /// Activation type, 0 means by slot, 1 means by timestamp
    pub activation_type: u8,
    /// pool status, 0: enable, 1 disable
    pub pool_status: u8,
    /// token a flag
    pub token_a_flag: u8,
    /// token b flag
    pub token_b_flag: u8,
    /// 0 is collect fee in both token, 1 only collect fee in token a, 2 only collect fee in token b
    pub collect_fee_mode: u8,
    /// pool type
    pub pool_type: u8,
    /// pool version, 0: max_fee is still capped at 50%, 1: max_fee is capped at 99%
    pub version: u8,
    /// padding
    pub _padding_0: u8,
    /// cumulative
    pub fee_a_per_liquidity: [u8; 32], // U256
    /// cumulative
    pub fee_b_per_liquidity: [u8; 32], // U256
    // TODO: Is this large enough?
    pub permanent_lock_liquidity: u128,
    /// metrics
    pub metrics: PoolMetrics,
    /// pool creator
    pub creator: Pubkey,
    /// Padding for further use
    pub _padding_1: [u64; 6],
    /// Farming reward information
    pub reward_infos: [RewardInfo; NUM_REWARDS],
}

const_assert_eq!(Pool::INIT_SPACE, 1104);

#[zero_copy]
#[derive(Debug, InitSpace, Default)]
pub struct PoolMetrics {
    pub total_lp_a_fee: u128,
    pub total_lp_b_fee: u128,
    pub total_protocol_a_fee: u64,
    pub total_protocol_b_fee: u64,
    pub total_partner_a_fee: u64,
    pub total_partner_b_fee: u64,
    pub total_position: u64,
    pub padding: u64,
}

const_assert_eq!(PoolMetrics::INIT_SPACE, 80);

/// Stores the state relevant for tracking liquidity mining rewards
#[zero_copy]
#[derive(InitSpace, Default, Debug, PartialEq)]
pub struct RewardInfo {
    /// Indicates if the reward has been initialized
    pub initialized: u8,
    /// reward token flag
    pub reward_token_flag: u8,
    /// padding
    pub _padding_0: [u8; 6],
    /// Padding to ensure `reward_rate: u128` is 16-byte aligned
    pub _padding_1: [u8; 8], // 8 bytes
    /// Reward token mint.
    pub mint: Pubkey,
    /// Reward vault token account.
    pub vault: Pubkey,
    /// Authority account that allows to fund rewards
    pub funder: Pubkey,
    /// reward duration
    pub reward_duration: u64,
    /// reward duration end
    pub reward_duration_end: u64,
    /// reward rate
    pub reward_rate: u128,
    /// Reward per token stored
    pub reward_per_token_stored: [u8; 32], // U256
    /// The last time reward states were updated.
    pub last_update_time: u64,
    /// Accumulated seconds when the farm distributed rewards but the bin was empty.
    /// These rewards will be carried over to the next reward time window.
    pub cumulative_seconds_with_empty_liquidity_reward: u64,
}

const_assert_eq!(RewardInfo::INIT_SPACE, 192);

impl Pool {
    pub fn has_partner(&self) -> bool {
        self.partner != Pubkey::default()
    }

    pub fn get_swap_result_from_exact_output(
        &self,
        amount_out: u64,
        fee_mode: &FeeMode,
        trade_direction: TradeDirection,
        current_point: u64,
    ) -> Result<SwapResult2> {
        let mut actual_protocol_fee = 0;
        let mut actual_trading_fee = 0;
        let mut actual_referral_fee = 0;
        let mut actual_partner_fee = 0;

        let max_fee_numerator = get_max_fee_numerator(self.version)?;

        let included_fee_amount_out = if fee_mode.fees_on_input {
            amount_out
        } else {
            let trade_fee_numerator = self
                .pool_fees
                .get_total_trading_fee_from_excluded_fee_amount(
                    current_point,
                    self.activation_point,
                    amount_out,
                    trade_direction,
                    max_fee_numerator,
                )?;

            let (included_fee_amount_out, fee_amount) =
                PoolFeesStruct::get_included_fee_amount(trade_fee_numerator, amount_out)?;

            let SplitFees {
                trading_fee,
                protocol_fee,
                referral_fee,
                partner_fee,
            } = self
                .pool_fees
                .split_fees(fee_amount, fee_mode.has_referral, self.has_partner())?;

            actual_protocol_fee = protocol_fee;
            actual_trading_fee = trading_fee;
            actual_referral_fee = referral_fee;
            actual_partner_fee = partner_fee;

            included_fee_amount_out
        };

        let SwapAmountFromOutput {
            input_amount,
            next_sqrt_price,
        } = match trade_direction {
            TradeDirection::AtoB => self.calculate_a_to_b_from_amount_out(included_fee_amount_out),
            TradeDirection::BtoA => self.calculate_b_to_a_from_amount_out(included_fee_amount_out),
        }?;

        let included_fee_input_amount = if fee_mode.fees_on_input {
            let trade_fee_numerator = self
                .pool_fees
                .get_total_trading_fee_from_excluded_fee_amount(
                    current_point,
                    self.activation_point,
                    input_amount,
                    trade_direction,
                    max_fee_numerator,
                )?;

            let (included_fee_input_amount, fee_amount) =
                PoolFeesStruct::get_included_fee_amount(trade_fee_numerator, input_amount)?;

            let SplitFees {
                trading_fee,
                protocol_fee,
                referral_fee,
                partner_fee,
            } = self
                .pool_fees
                .split_fees(fee_amount, fee_mode.has_referral, self.has_partner())?;

            actual_protocol_fee = protocol_fee;
            actual_trading_fee = trading_fee;
            actual_referral_fee = referral_fee;
            actual_partner_fee = partner_fee;

            included_fee_input_amount
        } else {
            input_amount
        };

        Ok(SwapResult2 {
            amount_left: 0,
            included_fee_input_amount,
            excluded_fee_input_amount: input_amount,
            output_amount: amount_out,
            next_sqrt_price,
            trading_fee: actual_trading_fee,
            protocol_fee: actual_protocol_fee,
            partner_fee: actual_partner_fee,
            referral_fee: actual_referral_fee,
        })
    }

    pub fn get_swap_result_from_partial_input(
        &self,
        amount_in: u64,
        fee_mode: &FeeMode,
        trade_direction: TradeDirection,
        current_point: u64,
    ) -> Result<SwapResult2> {
        let mut actual_protocol_fee = 0;
        let mut actual_trading_fee = 0;
        let mut actual_referral_fee = 0;
        let mut actual_partner_fee = 0;

        let max_fee_numerator = get_max_fee_numerator(self.version)?;

        let trade_fee_numerator = self
            .pool_fees
            .get_total_trading_fee_from_included_fee_amount(
                current_point,
                self.activation_point,
                amount_in,
                trade_direction,
                max_fee_numerator,
            )?;

        let mut actual_amount_in = if fee_mode.fees_on_input {
            let FeeOnAmountResult {
                amount,
                trading_fee,
                protocol_fee,
                partner_fee,
                referral_fee,
            } = self.pool_fees.get_fee_on_amount(
                amount_in,
                trade_fee_numerator,
                fee_mode.has_referral,
                self.has_partner(),
            )?;

            actual_protocol_fee = protocol_fee;
            actual_trading_fee = trading_fee;
            actual_referral_fee = referral_fee;
            actual_partner_fee = partner_fee;

            amount
        } else {
            amount_in
        };

        let SwapAmountFromInput {
            amount_left,
            output_amount,
            next_sqrt_price,
        } = match trade_direction {
            TradeDirection::AtoB => self.calculate_a_to_b_from_partial_amount_in(actual_amount_in),
            TradeDirection::BtoA => self.calculate_b_to_a_from_partial_amount_in(actual_amount_in),
        }?;

        let included_fee_input_amount = if amount_left > 0 {
            actual_amount_in = actual_amount_in.safe_sub(amount_left)?;

            if fee_mode.fees_on_input {
                let trade_fee_numerator = self
                    .pool_fees
                    .get_total_trading_fee_from_excluded_fee_amount(
                        current_point,
                        self.activation_point,
                        actual_amount_in,
                        trade_direction,
                        max_fee_numerator,
                    )?;

                let (included_fee_amount_in, fee_amount) =
                    PoolFeesStruct::get_included_fee_amount(trade_fee_numerator, actual_amount_in)?;

                let SplitFees {
                    trading_fee,
                    protocol_fee,
                    referral_fee,
                    partner_fee,
                } = self.pool_fees.split_fees(
                    fee_amount,
                    fee_mode.has_referral,
                    self.has_partner(),
                )?;

                actual_protocol_fee = protocol_fee;
                actual_trading_fee = trading_fee;
                actual_referral_fee = referral_fee;
                actual_partner_fee = partner_fee;

                included_fee_amount_in
            } else {
                actual_amount_in
            }
        } else {
            amount_in
        };

        let actual_amount_out = if fee_mode.fees_on_input {
            output_amount
        } else {
            let FeeOnAmountResult {
                amount,
                trading_fee,
                protocol_fee,
                partner_fee,
                referral_fee,
            } = self.pool_fees.get_fee_on_amount(
                output_amount,
                trade_fee_numerator,
                fee_mode.has_referral,
                self.has_partner(),
            )?;

            actual_protocol_fee = protocol_fee;
            actual_trading_fee = trading_fee;
            actual_referral_fee = referral_fee;
            actual_partner_fee = partner_fee;

            amount
        };

        Ok(SwapResult2 {
            included_fee_input_amount,
            excluded_fee_input_amount: actual_amount_in,
            amount_left,
            output_amount: actual_amount_out,
            next_sqrt_price,
            trading_fee: actual_trading_fee,
            protocol_fee: actual_protocol_fee,
            partner_fee: actual_partner_fee,
            referral_fee: actual_referral_fee,
        })
    }

    pub fn get_swap_result_from_exact_input(
        &self,
        amount_in: u64,
        fee_mode: &FeeMode,
        trade_direction: TradeDirection,
        current_point: u64,
    ) -> Result<SwapResult2> {
        let mut actual_protocol_fee = 0;
        let mut actual_trading_fee = 0;
        let mut actual_referral_fee = 0;
        let mut actual_partner_fee = 0;

        let max_fee_numerator = get_max_fee_numerator(self.version)?;

        // We can compute the trade_fee_numerator here. Instead of separately for amount_in, and amount_out.
        // This is because FeeRateLimiter (fee rate scale based on amount) only applied when fee_mode.fees_on_input
        // (a.k.a TradeDirection::QuoteToBase + CollectFeeMode::QuoteToken)
        // For the rest of the time, the fee rate is not dependent on amount.
        let trade_fee_numerator = self
            .pool_fees
            .get_total_trading_fee_from_included_fee_amount(
                current_point,
                self.activation_point,
                amount_in,
                trade_direction,
                max_fee_numerator,
            )?;

        let actual_amount_in = if fee_mode.fees_on_input {
            let FeeOnAmountResult {
                amount,
                trading_fee,
                protocol_fee,
                partner_fee,
                referral_fee,
            } = self.pool_fees.get_fee_on_amount(
                amount_in,
                trade_fee_numerator,
                fee_mode.has_referral,
                self.has_partner(),
            )?;

            actual_protocol_fee = protocol_fee;
            actual_trading_fee = trading_fee;
            actual_referral_fee = referral_fee;
            actual_partner_fee = partner_fee;

            amount
        } else {
            amount_in
        };

        let SwapAmountFromInput {
            output_amount,
            next_sqrt_price,
            amount_left,
        } = match trade_direction {
            TradeDirection::AtoB => self.calculate_a_to_b_from_amount_in(actual_amount_in),
            TradeDirection::BtoA => self.calculate_b_to_a_from_amount_in(actual_amount_in),
        }?;

        let actual_amount_out = if fee_mode.fees_on_input {
            output_amount
        } else {
            let FeeOnAmountResult {
                amount,
                trading_fee,
                protocol_fee,
                partner_fee,
                referral_fee,
            } = self.pool_fees.get_fee_on_amount(
                output_amount,
                trade_fee_numerator,
                fee_mode.has_referral,
                self.has_partner(),
            )?;

            actual_protocol_fee = protocol_fee;
            actual_trading_fee = trading_fee;
            actual_referral_fee = referral_fee;
            actual_partner_fee = partner_fee;

            amount
        };

        Ok(SwapResult2 {
            amount_left,
            included_fee_input_amount: amount_in,
            excluded_fee_input_amount: actual_amount_in,
            output_amount: actual_amount_out,
            next_sqrt_price,
            trading_fee: actual_trading_fee,
            protocol_fee: actual_protocol_fee,
            partner_fee: actual_partner_fee,
            referral_fee: actual_referral_fee,
        })
    }

    pub fn calculate_b_to_a_from_amount_out(
        &self,
        amount_out: u64,
    ) -> Result<SwapAmountFromOutput> {
        let next_sqrt_price =
            get_next_sqrt_price_from_output(self.sqrt_price, self.liquidity, amount_out, false)?;

        if next_sqrt_price > self.sqrt_max_price {
            return Err(PoolError::PriceRangeViolation.into());
        }

        let in_amount = get_delta_amount_b_unsigned(
            self.sqrt_price,
            next_sqrt_price,
            self.liquidity,
            Rounding::Up,
        )?;

        Ok(SwapAmountFromOutput {
            input_amount: in_amount,
            next_sqrt_price,
        })
    }

    pub fn calculate_a_to_b_from_amount_out(
        &self,
        amount_out: u64,
    ) -> Result<SwapAmountFromOutput> {
        let next_sqrt_price =
            get_next_sqrt_price_from_output(self.sqrt_price, self.liquidity, amount_out, true)?;

        if next_sqrt_price < self.sqrt_min_price {
            return Err(PoolError::PriceRangeViolation.into());
        }

        let in_amount = get_delta_amount_a_unsigned(
            next_sqrt_price,
            self.sqrt_price,
            self.liquidity,
            Rounding::Up,
        )?;

        Ok(SwapAmountFromOutput {
            input_amount: in_amount,
            next_sqrt_price,
        })
    }

    pub fn calculate_b_to_a_from_partial_amount_in(
        &self,
        amount_in: u64,
    ) -> Result<SwapAmountFromInput> {
        let max_amount_in = get_delta_amount_b_unsigned(
            self.sqrt_price,
            self.sqrt_max_price,
            self.liquidity,
            Rounding::Up,
        )?;

        let (consumed_in_amount, next_sqrt_price) = if amount_in >= max_amount_in {
            (max_amount_in, self.sqrt_max_price)
        } else {
            let next_sqrt_price =
                get_next_sqrt_price_from_input(self.sqrt_price, self.liquidity, amount_in, false)?;
            (amount_in, next_sqrt_price)
        };

        let output_amount = get_delta_amount_a_unsigned(
            self.sqrt_price,
            next_sqrt_price,
            self.liquidity,
            Rounding::Down,
        )?;

        let amount_left = amount_in.safe_sub(consumed_in_amount)?;

        Ok(SwapAmountFromInput {
            output_amount,
            next_sqrt_price,
            amount_left,
        })
    }

    pub fn calculate_a_to_b_from_partial_amount_in(
        &self,
        amount_in: u64,
    ) -> Result<SwapAmountFromInput> {
        let max_amount_in = get_delta_amount_a_unsigned(
            self.sqrt_min_price,
            self.sqrt_price,
            self.liquidity,
            Rounding::Up,
        )?;

        let (consumed_in_amount, next_sqrt_price) = if amount_in >= max_amount_in {
            (max_amount_in, self.sqrt_min_price)
        } else {
            let next_sqrt_price =
                get_next_sqrt_price_from_input(self.sqrt_price, self.liquidity, amount_in, true)?;
            (amount_in, next_sqrt_price)
        };

        let output_amount = get_delta_amount_b_unsigned(
            next_sqrt_price,
            self.sqrt_price,
            self.liquidity,
            Rounding::Down,
        )?;

        let amount_left = amount_in.safe_sub(consumed_in_amount)?;

        Ok(SwapAmountFromInput {
            output_amount,
            next_sqrt_price,
            amount_left,
        })
    }

    fn calculate_a_to_b_from_amount_in(&self, amount_in: u64) -> Result<SwapAmountFromInput> {
        // finding new target price
        let next_sqrt_price =
            get_next_sqrt_price_from_input(self.sqrt_price, self.liquidity, amount_in, true)?;

        if next_sqrt_price < self.sqrt_min_price {
            return Err(PoolError::PriceRangeViolation.into());
        }

        // finding output amount
        let output_amount = get_delta_amount_b_unsigned(
            next_sqrt_price,
            self.sqrt_price,
            self.liquidity,
            Rounding::Down,
        )?;

        Ok(SwapAmountFromInput {
            output_amount,
            next_sqrt_price,
            amount_left: 0,
        })
    }

    pub fn calculate_a_to_b_from_amount_in_exact(&self, amount_in: u64) -> Result<(u64, u128)> {
        // finding new target price
        let next_sqrt_price =
            get_next_sqrt_price_from_input(self.sqrt_price, self.liquidity, amount_in, true)?;

        if next_sqrt_price < self.sqrt_min_price {
            return Err(PoolError::PriceRangeViolation.into());
        }

        // finding output amount
        let output_amount = get_delta_amount_b_unsigned(
            next_sqrt_price,
            self.sqrt_price,
            self.liquidity,
            Rounding::Down,
        )?;

        Ok((output_amount, next_sqrt_price))
    }

    pub fn calculate_b_to_a_from_amount_in_exact(&self, amount_in: u64) -> Result<(u64, u128)> {
        // finding new target price
        let next_sqrt_price =
            get_next_sqrt_price_from_input(self.sqrt_price, self.liquidity, amount_in, false)?;

        if next_sqrt_price > self.sqrt_max_price {
            return Err(PoolError::PriceRangeViolation.into());
        }

        // finding output amount
        let output_amount = get_delta_amount_a_unsigned(
            self.sqrt_price,
            next_sqrt_price,
            self.liquidity,
            Rounding::Down,
        )?;

        Ok((output_amount, next_sqrt_price))
    }

    fn calculate_b_to_a_from_amount_in(&self, amount_in: u64) -> Result<SwapAmountFromInput> {
        // finding new target price
        let next_sqrt_price =
            get_next_sqrt_price_from_input(self.sqrt_price, self.liquidity, amount_in, false)?;

        if next_sqrt_price > self.sqrt_max_price {
            return Err(PoolError::PriceRangeViolation.into());
        }
        // finding output amount
        let output_amount = get_delta_amount_a_unsigned(
            self.sqrt_price,
            next_sqrt_price,
            self.liquidity,
            Rounding::Down,
        )?;

        Ok(SwapAmountFromInput {
            output_amount,
            next_sqrt_price,
            amount_left: 0,
        })
    }

    pub fn get_max_amount_in(&self, trade_direction: TradeDirection) -> Result<u64> {
        let amount = match trade_direction {
            TradeDirection::AtoB => get_delta_amount_a_unsigned_unchecked(
                self.sqrt_min_price,
                self.sqrt_price,
                self.liquidity,
                Rounding::Down,
            )?,
            TradeDirection::BtoA => get_delta_amount_a_unsigned_unchecked(
                self.sqrt_price,
                self.sqrt_max_price,
                self.liquidity,
                Rounding::Down,
            )?,
        };
        if amount > U256::from(u64::MAX) {
            Ok(u64::MAX)
        } else {
            Ok(amount.try_into().unwrap())
        }
    }
}

#[derive(Debug, PartialEq, AnchorDeserialize, AnchorSerialize, Clone, Copy)]
pub struct SwapResult2 {
    // This is excluded_transfer_fee_amount_in
    pub included_fee_input_amount: u64,
    pub excluded_fee_input_amount: u64,
    pub amount_left: u64,
    pub output_amount: u64,
    pub next_sqrt_price: u128,
    pub trading_fee: u64,
    pub protocol_fee: u64,
    pub partner_fee: u64,
    pub referral_fee: u64,
}

pub struct SwapAmountFromInput {
    output_amount: u64,
    next_sqrt_price: u128,
    amount_left: u64,
}

pub struct SwapAmountFromOutput {
    input_amount: u64,
    next_sqrt_price: u128,
}
