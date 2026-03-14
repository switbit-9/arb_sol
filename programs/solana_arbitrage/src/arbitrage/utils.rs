use crate::programs::{ProgramInstance, ProgramMeta};
use crate::programs::SolarBError;

use anchor_lang::prelude::*;

/// Find a ProgramInstance by matching pool_id
/// Each pool has a unique pool_id, so we search through all instances to find the matching one
pub fn find_instance_by_pool_id<'a>(
    instances: &'a [ProgramInstance],
    pool_id: &Pubkey,
) -> Result<&'a ProgramInstance<'a>> {
    instances
        .iter()
        .find(|instance| instance.get_pool_id() == pool_id)
        .ok_or_else(|| error!(SolarBError::InvalidProgramType))
}

/// Find the index of a ProgramInstance by matching pool_id
pub fn find_instance_index_by_pool_id(
    instances: &[ProgramInstance],
    pool_id: &Pubkey,
) -> Result<usize> {
    instances
        .iter()
        .position(|instance| instance.get_pool_id() == pool_id)
        .ok_or_else(|| error!(SolarBError::InvalidProgramType))
}
