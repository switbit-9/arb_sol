use anchor_lang::prelude::*;

pub const AMM_CONFIG_SEED: &str = "amm_config";

/// Holds the current owner of the factory
#[account]
#[derive(Default, Debug)]
pub struct AmmConfig {
    /// Bump to identify PDA
    pub bump: u8,
    /// Status to control if new pool can be create
    pub disable_create_pool: bool,
    /// Config index
    pub index: u16,
    /// The trade fee, denominated in hundredths of a bip (10^-6)
    pub trade_fee_rate: u64,
    /// The protocol fee
    pub protocol_fee_rate: u64,
    /// The fund fee, denominated in hundredths of a bip (10^-6)
    pub fund_fee_rate: u64,
    /// Fee for create a new pool
    pub create_pool_fee: u64,
    /// Address of the protocol fee owner
    pub protocol_owner: Pubkey,
    /// Address of the fund fee owner
    pub fund_owner: Pubkey,
    /// The pool creator fee, denominated in hundredths of a bip (10^-6)
    pub creator_fee_rate: u64,
    /// padding
    pub padding: [u64; 15],
}

impl AmmConfig {
    pub const LEN: usize = 8 + 1 + 1 + 2 + 4 * 8 + 32 * 2 + 8 + 8 * 15;

    /// Manually deserialize AmmConfig from account data, skipping the discriminator.
    /// This is needed when reading account data from on-chain accounts that may have
    /// a different discriminator than our local Anchor struct.
    pub fn try_from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() < 8 + (Self::LEN - 8) {
            return Err(anchor_lang::error::ErrorCode::AccountDiscriminatorNotFound.into());
        }

        let offset = 8; // Skip discriminator
        let mut cursor = offset;

        let bump = data[cursor];
        cursor += 1;

        let disable_create_pool = data[cursor] != 0;
        cursor += 1;

        let index = u16::from_le_bytes([data[cursor], data[cursor + 1]]);
        cursor += 2;

        let trade_fee_rate = u64::from_le_bytes(
            data[cursor..cursor + 8]
                .try_into()
                .map_err(|_| anchor_lang::error::ErrorCode::AccountDiscriminatorNotFound)?,
        );
        cursor += 8;

        let protocol_fee_rate = u64::from_le_bytes(
            data[cursor..cursor + 8]
                .try_into()
                .map_err(|_| anchor_lang::error::ErrorCode::AccountDiscriminatorNotFound)?,
        );
        cursor += 8;

        let fund_fee_rate = u64::from_le_bytes(
            data[cursor..cursor + 8]
                .try_into()
                .map_err(|_| anchor_lang::error::ErrorCode::AccountDiscriminatorNotFound)?,
        );
        cursor += 8;

        let create_pool_fee = u64::from_le_bytes(
            data[cursor..cursor + 8]
                .try_into()
                .map_err(|_| anchor_lang::error::ErrorCode::AccountDiscriminatorNotFound)?,
        );
        cursor += 8;

        let protocol_owner = Pubkey::try_from(&data[cursor..cursor + 32])
            .map_err(|_| anchor_lang::error::ErrorCode::AccountDiscriminatorNotFound)?;
        cursor += 32;

        let fund_owner = Pubkey::try_from(&data[cursor..cursor + 32])
            .map_err(|_| anchor_lang::error::ErrorCode::AccountDiscriminatorNotFound)?;
        cursor += 32;

        let creator_fee_rate = u64::from_le_bytes(
            data[cursor..cursor + 8]
                .try_into()
                .map_err(|_| anchor_lang::error::ErrorCode::AccountDiscriminatorNotFound)?,
        );
        cursor += 8;

        let mut padding = [0u64; 15];
        for i in 0..15 {
            padding[i] = u64::from_le_bytes(
                data[cursor..cursor + 8]
                    .try_into()
                    .map_err(|_| anchor_lang::error::ErrorCode::AccountDiscriminatorNotFound)?,
            );
            cursor += 8;
        }

        Ok(Self {
            bump,
            disable_create_pool,
            index,
            trade_fee_rate,
            protocol_fee_rate,
            fund_fee_rate,
            create_pool_fee,
            protocol_owner,
            fund_owner,
            creator_fee_rate,
            padding,
        })
    }
}
