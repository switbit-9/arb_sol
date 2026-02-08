use crate::programs::meteora_damm_v2::MeteoraDammV2;
use crate::programs::meteora_dlmm::MeteoraDlmm;
use crate::programs::programs::ProgramInstance;
use crate::programs::pump_amm::PumpAmm;
use crate::programs::SolarBError;
use anchor_lang::prelude::*;

/// Helper to extract MeteoraDlmm from ProgramInstance
pub(crate) fn extract_dlmm<'a, 'info>(
    instance: &'a ProgramInstance<'info>,
) -> Result<&'a MeteoraDlmm<'info>> {
    match instance {
        ProgramInstance::MeteoraDlmm(dlmm) => Ok(dlmm),
        _ => Err(error!(SolarBError::InvalidProgramType)),
    }
}

/// Helper to extract MeteoraDammV2 from ProgramInstance
pub(crate) fn extract_damm2<'a, 'info>(
    instance: &'a ProgramInstance<'info>,
) -> Result<&'a MeteoraDammV2<'info>> {
    match instance {
        ProgramInstance::MeteoraDammV2(amm) => Ok(amm),
        _ => Err(error!(SolarBError::InvalidProgramType)),
    }
}

/// Helper to extract PumpAmm from ProgramInstance
pub(crate) fn extract_pump<'a, 'info>(
    instance: &'a ProgramInstance<'info>,
) -> Result<&'a PumpAmm<'info>> {
    match instance {
        ProgramInstance::PumpAmm(pump) => Ok(pump),
        _ => Err(error!(SolarBError::InvalidProgramType)),
    }
}
