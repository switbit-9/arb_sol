use crate::programs::ProgramMeta;
use crate::programs::SolarBError;

use anchor_lang::prelude::*;

/// Find a ProgramInstance by matching pool_id
/// Each pool has a unique pool_id, so we search through all instances to find the matching one
pub fn find_instance_by_pool_id<'a, 'info>(
    instances: &'a [Box<dyn ProgramMeta + 'info>],
    pool_id: &Pubkey,
) -> Result<&'a Box<dyn ProgramMeta + 'info>> {
    instances
        .iter()
        .find(|instance| instance.get_pool_id() == pool_id)
        .ok_or_else(|| error!(SolarBError::InvalidProgramType))
}
