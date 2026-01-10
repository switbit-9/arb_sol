use anchor_lang::prelude::*;

pub mod arbitrage;
pub mod math;
pub mod programs;
pub mod utils;

use arbitrage::algo_2::{check_arbitrage, ArbitragePath};
use arbitrage::base::{Edge, EdgeSide, Pool};
use programs::{MeteoraDammV1, MeteoraDammV2, MeteoraDlmm, ProgramMeta, PumpAmm, SolarBError};
use utils::utils::parse_token_account;

declare_id!("Ckgi61iKuKeVLfCgAuqaURw18e52D7SvqVj9TUw6NftF");

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InstructionData {
    pub accounts_length: [u32; 5],
    pub epoch: u16,
}

#[derive(Accounts)]
pub struct Initialize {}

#[program]
pub mod solar_b {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>, data: InstructionData) -> Result<()> {
        // ctx.remaining_accounts
        // msg!("Greetings from: {:?}", ctx.program_id);
        // msg!("Reamingin accounts {:?}", ctx.remaining_accounts);
        // for item in ctx.remaining_accounts {
        //     msg!("Remaining account {:?}", &item);
        // }
        // Ok(())
        // let payload = get_full_payload(ctx);
        // let market_data: Vec<Market> = payload
        //     .programs
        //     .iter()
        //     .map(|p| {
        //         return execute_program(p.program_id, p.accounts);
        //     })
        //     .collect();
        // let paths = get_paths("SOL", &market_data);
        // if paths.len() == 0 {
        //     /// exec first path
        // }
        // msg!("Context {:?}", ctx);
        // msg!(
        //     "Instruction data {:?} {:?}",
        //     ctx.remaining_accounts.len(),
        //     &data.accounts_length
        // );
        // msg!("Remaining accounts {:?}", ctx.remaining_accounts);

        // Work directly with remaining_accounts slice - don't clone AccountInfo
        require!(
            ctx.remaining_accounts.len() >= 7,
            SolarBError::InsufficientAccounts
        );
        let first_accounts = &ctx.remaining_accounts[..7];

        let payer = &first_accounts[0];
        if payer.lamports() == 0 {
            return Err(error!(SolarBError::InsufficientFunds));
        }
        let rest = &ctx.remaining_accounts[7..];

        let mut instances = parse_accounts(rest, &data)?;
        // for instance in instances {
        //     instance.as_ref().log_accounts()?;
        // }
        // Run arbitrage with default start amount (1 SOL = 1e9 lamports)
        // TODO: Get start token from context or parameters
        let arbitrage_path = run_arbitrage(&mut instances, 1_000_000_000, None).unwrap();
        execute_arbitrage_path(
            &arbitrage_path,
            &mut instances,
            payer,
            &first_accounts[1], // mint_1
            &first_accounts[2], // mint_1_token_program
            &first_accounts[3], // user_mint_1_token_account
            &first_accounts[4], // mint_2
            &first_accounts[5], // mint_2_token_program
            &first_accounts[6], // user_mint_2_token_account
        )?;
        Ok(())
    }
}

fn parse_accounts<'info>(
    accounts: &[AccountInfo<'info>],
    data: &InstructionData,
) -> Result<Vec<Box<dyn ProgramMeta + 'info>>> {
    let mut index: usize = 0;

    // Pre-allocate capacity: count non-zero spans to estimate instance count
    let estimated_capacity = data.accounts_length.iter().filter(|&&len| len > 0).count();
    let mut instances = Vec::with_capacity(estimated_capacity);

    for &raw_span in data.accounts_length.iter() {
        let span = usize::try_from(raw_span).map_err(|_| SolarBError::InvalidAccountsLength)?;
        if span == 0 {
            continue;
        }
        require!(
            accounts.len() >= index + span,
            SolarBError::InsufficientAccounts
        );

        let segment = &accounts[index..index + span];
        // Avoid cloning AccountInfo - just pass the reference's key
        let program_key = segment[0].key;
        let instance: Box<dyn ProgramMeta> = find_program_instance(program_key, segment)?;
        // TODO: Implement find_program_instance to create ProgramMeta instances
        instances.push(instance);
        // instance.log_accounts()?;
        index += span;
    }

    require!(index == accounts.len(), SolarBError::TrailingAccounts);

    Ok(instances)
}

pub fn find_program_instance<'info>(
    program_id: &Pubkey,
    payload_accounts: &[AccountInfo<'info>],
) -> Result<Box<dyn ProgramMeta + 'info>> {
    // msg!(
    //     "Creating program for program_id: {}, accounts.len(): {}",
    //     program_id,
    //     payload_accounts.len()
    // );
    // if program_id == &RaydiumCPMM::PROGRAM_ID {
    //     msg!(
    //         "Initializing RaydiumCPMM with {} accounts",
    //         payload_accounts.len()
    //     );
    //     let pr = RaydiumCPMM::new(payload_accounts)?;
    //     return Ok(Box::new(pr));
    // }
    // if program_id == &RaydiumAmm::PROGRAM_ID {
    //     msg!(
    //         "Initializing RaydiumAmm with {} accounts",
    //         payload_accounts.len()
    //     );
    //     let pr = RaydiumAmm::new(payload_accounts)?;
    //     return Ok(Box::new(pr));
    // }
    // if program_id == &RaydiumClmm::PROGRAM_ID {
    //     msg!(
    //         "Initializing RaydiumClmm with {} accounts",
    //         payload_accounts.len()
    //     );
    //     let pr = RaydiumClmm::new(payload_accounts)?;
    //     return Ok(Box::new(pr));
    // }
    if program_id == &PumpAmm::PROGRAM_ID {
        let pr = PumpAmm::new(payload_accounts)?;
        return Ok(Box::new(pr));
    }
    // if program_id == &Whirlpools::PROGRAM_ID {
    //     msg!(
    //         "Initializing Whirlpools with {} accounts",
    //         payload_accounts.len()
    //     );
    //     let pr = Whirlpools::new(payload_accounts)?;
    //     return Ok(Box::new(pr));
    // }
    if program_id == &MeteoraDammV2::PROGRAM_ID {
        let pr = MeteoraDammV2::new(payload_accounts)?;
        return Ok(Box::new(pr));
    }
    if program_id == &MeteoraDammV1::PROGRAM_ID {
        let pr = MeteoraDammV1::new(payload_accounts)?;
        return Ok(Box::new(pr));
    }
    if program_id == &MeteoraDlmm::PROGRAM_ID {
        require!(
            payload_accounts.len() >= 13,
            SolarBError::InsufficientAccounts
        );
        let pr = MeteoraDlmm::new(payload_accounts)?;
        return Ok(Box::new(pr));
    }
    Err(error!(SolarBError::UnknownProgram))
}

pub fn generate_edges<'info>(program: &'info (dyn ProgramMeta + 'info)) -> Result<Vec<Edge>> {
    let (base_vault_info, quote_vault_info) = program.get_vaults();
    let base_vault = parse_token_account(base_vault_info)?;
    let quote_vault = parse_token_account(quote_vault_info)?;
    let base_amount = base_vault.amount as u128;
    let quote_amount = quote_vault.amount as u128;
    let price_base_in = program.compute_price_swap_base_in(base_amount, quote_amount)?;
    let price_base_out = program.compute_price_swap_base_out(base_amount, quote_amount)?;

    // Extract mints directly from the deserialized token accounts
    // Pool struct is small (40 bytes: Pubkey 32 + u128 16), but avoid unnecessary clones
    let base_pool = Pool::new(&base_vault.mint, base_amount);
    let quote_pool = Pool::new(&quote_vault.mint, quote_amount);
    let program_id = *program.get_id();
    Ok(vec![
        Edge::new(
            program_id,
            EdgeSide::LeftToRight,
            price_base_in,
            base_pool.clone(),
            quote_pool.clone(),
        ),
        Edge::new(
            program_id,
            EdgeSide::RightToLeft,
            price_base_out,
            quote_pool, // Move instead of clone
            base_pool,  // Move instead of clone
        ),
    ])
}

pub fn get_edges<'info>(instances: &'info [Box<dyn ProgramMeta + 'info>]) -> Result<Vec<Edge>> {
    // Pre-allocate capacity: each instance generates 2 edges
    let mut edges = Vec::with_capacity(instances.len() * 2);
    for instance in instances {
        let instance_edges = generate_edges(instance.as_ref())?;
        edges.extend(instance_edges);
    }
    Ok(edges)
}

pub fn run_arbitrage<'info>(
    instances: &mut Vec<Box<dyn ProgramMeta + 'info>>,
    start_amount: u128,
    start_token: Option<Pubkey>,
) -> Result<ArbitragePath> {
    // Note: We don't actually use epoch, so avoid creating full Clock struct
    // If epoch is needed later, get it separately: Clock::get()?.epoch

    // Extract edges - Vec<Edge> is on heap, only Vec metadata (24 bytes) on stack
    let edges = get_edges(instances.as_slice())?;

    // Check for arbitrage opportunities
    // Pre-allocate Vec<&Edge> with known capacity to avoid reallocations
    let mut edge_refs = Vec::with_capacity(edges.len());
    for edge in &edges {
        edge_refs.push(edge);
    }
    let arbitrage_path = check_arbitrage(&edge_refs, start_amount, start_token, None)?;

    // Explicitly drop to free Vec metadata (24 bytes) from stack immediately
    // edges Vec is on heap, but Vec struct metadata (ptr+len+cap) is on stack
    drop(edge_refs);
    drop(edges);

    if arbitrage_path.profit < 0 {
        return Err(error!(SolarBError::NoProfitFound));
    }

    msg!("= {:?}", arbitrage_path.profit);

    Ok(arbitrage_path)
}

pub fn execute_arbitrage_path<'info>(
    arbitrage_path: &ArbitragePath,
    instances: &mut Vec<Box<dyn ProgramMeta + 'info>>,
    payer: &AccountInfo<'info>,
    mint_1: &AccountInfo<'info>,
    mint_1_token_program: &AccountInfo<'info>,
    user_mint_1_token_account: &AccountInfo<'info>,
    mint_2: &AccountInfo<'info>,
    mint_2_token_program: &AccountInfo<'info>,
    user_mint_2_token_account: &AccountInfo<'info>,
) -> Result<()> {
    let mut current_amount = arbitrage_path.start_amount;

    // Clock is now fetched inside the loop block scope for each iteration
    // This ensures it's dropped immediately after each swap operation

    for (i, edge) in arbitrage_path.edges.iter().enumerate() {
        msg!("Edge {:?} -> {:?}", edge.program, edge.side);

        // Find the index of the program instance first, so we can remove it after execution
        let instance_index = instances
            .iter()
            .position(|instance| instance.get_id() == &edge.program)
            .ok_or(SolarBError::UnknownProgram)?;

        // Wrap swap operations in a block scope so program_instance and clock are dropped immediately
        // This frees stack space (8 bytes for program_instance reference + ~40 bytes for clock) after execution
        let amount_out = {
            // Get program instance by index - scoped to this block
            let program_instance = instances[instance_index].as_ref();

            // Get Clock for this swap (may change between swaps) - scoped to this block
            let clock = Clock::get()?;

            match edge.side {
                EdgeSide::LeftToRight => {
                    let amount = program_instance.swap_base_out(current_amount as u64, clock)?;
                    msg!(
                        "Invoking swap base out for program {:?} with amount_in={}, amount_out={}",
                        program_instance.get_id(),
                        current_amount,
                        amount
                    );
                    program_instance.invoke_swap_base_out(
                        current_amount as u64,
                        Some(amount),
                        payer.clone(),
                        user_mint_1_token_account.clone(),
                        user_mint_2_token_account.clone(),
                        mint_1.clone(),
                        mint_2.clone(),
                        mint_1_token_program.clone(),
                        mint_2_token_program.clone(),
                    )?;
                    amount
                }
                EdgeSide::RightToLeft => {
                    let amount = program_instance.swap_base_in(current_amount as u64, clock)?;
                    msg!(
                        "Invoking swap base in for program {:?} with amount_in={}, amount_out={}",
                        program_instance.get_id(),
                        current_amount,
                        amount
                    );
                    program_instance.invoke_swap_base_in(
                        current_amount as u64,
                        Some(amount),
                        payer.clone(),
                        user_mint_1_token_account.clone(),
                        user_mint_2_token_account.clone(),
                        mint_1.clone(),
                        mint_2.clone(),
                        mint_1_token_program.clone(),
                        mint_2_token_program.clone(),
                    )?;
                    amount
                }
            }
            // program_instance and clock are dropped here when this block ends
        };

        // Remove the program instance from the vector after it's been used
        // Use swap_remove instead of remove: O(1) instead of O(n), and doesn't shift elements
        // Order doesn't matter since we're removing after use and finding by program_id
        instances.swap_remove(instance_index);

        current_amount = amount_out as u128;
        msg!(
            "Edge {} completed, new current_amount={}",
            i,
            current_amount
        );
    }

    let final_profit = current_amount as i128 - arbitrage_path.start_amount as i128;
    msg!(
        "Completed. Final amount: {}, Profit: {}",
        current_amount,
        final_profit
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::{account_info::AccountInfo, pubkey::Pubkey, system_program};

    // Helper function to create a mock AccountInfo
    fn create_mock_account_info(
        key: Pubkey,
        owner: Pubkey,
        lamports: u64,
        data: Option<Vec<u8>>,
    ) -> AccountInfo<'static> {
        let data_vec = if let Some(provided_data) = data {
            Box::leak(Box::new(provided_data))
        } else {
            Box::leak(Box::new(Vec::new()))
        };
        let lamports_static = Box::leak(Box::new(lamports));
        let owner_static = Box::leak(Box::new(owner));
        let key_static = Box::leak(Box::new(key));

        AccountInfo::new(
            key_static,
            false, // is_signer
            false, // is_writable
            lamports_static,
            data_vec,
            owner_static,
            false, // executable
            0,     // rent_epoch
        )
    }

    // Helper to create multiple mock accounts
    fn create_mock_accounts(count: usize, owner: Pubkey) -> Vec<AccountInfo<'static>> {
        (0..count)
            .map(|_| {
                let key = Pubkey::new_unique();
                create_mock_account_info(key, owner, 1000, None)
            })
            .collect()
    }

    #[test]
    fn test_parse_accounts_success_single_program() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // Create MeteoraDammV2 program accounts (9 accounts: program_id + 8 payload)
        let program_id = MeteoraDammV2::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id, owner, 0, None));
        // Add 8 more accounts for MeteoraDammV2
        for _ in 0..8 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        let data = InstructionData {
            accounts_length: [9, 0, 0, 0, 0],
            epoch: 0,
        };

        let result = parse_accounts(&accounts, &data);
        assert!(result.is_ok());
        let instances = result.unwrap();
        assert!(instances.len() == 1);
        assert!(*instances[0].get_id() == program_id);
    }

    #[test]
    fn test_parse_accounts_success_multiple_programs() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // First program: MeteoraDammV2 (9 accounts)
        let program_id_1 = MeteoraDammV2::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id_1, owner, 0, None));
        for _ in 0..8 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        // Second program: MeteoraDlmm (13 accounts)
        let program_id_2 = MeteoraDlmm::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id_2, owner, 0, None));
        for _ in 0..12 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        let data = InstructionData {
            accounts_length: [9, 13, 0, 0, 0],
            epoch: 0,
        };

        let result = parse_accounts(&accounts, &data);
        assert!(result.is_ok());
        let instances = result.unwrap();
        assert!(instances.len() == 2);
        assert!(*instances[0].get_id() == program_id_1);
        assert!(*instances[1].get_id() == program_id_2);
    }

    #[test]
    fn test_parse_accounts_skips_zero_span() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // Create one program
        let program_id = MeteoraDammV2::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id, owner, 0, None));
        for _ in 0..8 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        // Zero spans should be skipped
        let data = InstructionData {
            accounts_length: [9, 0, 0, 0, 0],
            epoch: 0,
        };

        let result = parse_accounts(&accounts, &data);
        assert!(result.is_ok());
        let instances = result.unwrap();
        assert!(instances.len() == 1);
    }

    #[test]
    fn test_parse_accounts_insufficient_accounts() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // Only provide 5 accounts when 9 are needed
        let program_id = MeteoraDammV2::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id, owner, 0, None));
        for _ in 0..4 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        let data = InstructionData {
            accounts_length: [9, 0, 0, 0, 0],
            epoch: 0,
        };

        let result = parse_accounts(&accounts, &data);
        assert!(result.is_err());
        // Just verify it's an error - Anchor error types are complex to match
    }

    #[test]
    fn test_parse_accounts_trailing_accounts() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // Create program with 9 accounts
        let program_id = MeteoraDammV2::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id, owner, 0, None));
        for _ in 0..8 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        // Add an extra account that shouldn't be there
        accounts.push(create_mock_account_info(
            Pubkey::new_unique(),
            owner,
            0,
            None,
        ));

        let data = InstructionData {
            accounts_length: [9, 0, 0, 0, 0],
            epoch: 0,
        };

        let result = parse_accounts(&accounts, &data);
        assert!(result.is_err());
        // Just verify it's an error - Anchor error types are complex to match
    }

    #[test]
    fn test_parse_accounts_unknown_program() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // Use an unknown program ID
        let unknown_program_id = Pubkey::new_unique();
        accounts.push(create_mock_account_info(unknown_program_id, owner, 0, None));
        for _ in 0..8 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        let data = InstructionData {
            accounts_length: [9, 0, 0, 0, 0],
            epoch: 0,
        };

        let result = parse_accounts(&accounts, &data);
        assert!(result.is_err());
        // Just verify it's an error - Anchor error types are complex to match
    }

    #[test]
    fn test_parse_accounts_invalid_accounts_length() {
        let accounts = create_mock_accounts(5, system_program::id());

        // Use a span that's too large to convert from u32 to usize
        // On most platforms this won't happen, but we test the error path
        let data = InstructionData {
            accounts_length: [u32::MAX, 0, 0, 0, 0],
            epoch: 0,
        };

        let result = parse_accounts(&accounts, &data);
        // This should either error on conversion or on insufficient accounts
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_accounts_empty_segment() {
        let accounts = Vec::new();

        let data = InstructionData {
            accounts_length: [0, 0, 0, 0, 0],
            epoch: 0,
        };

        let result = parse_accounts(&accounts, &data);
        assert!(result.is_ok());
        let instances = result.unwrap();
        assert!(instances.len() == 0);
    }

    #[test]
    fn test_parse_accounts_meteora_damm_v1() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // MeteoraDammV1 needs 10 accounts (no program_id in payload, starts with pool_id)
        let program_id = MeteoraDammV1::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id, owner, 0, None));
        for _ in 0..9 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        let data = InstructionData {
            accounts_length: [10, 0, 0, 0, 0],
            epoch: 0,
        };

        let result = parse_accounts(&accounts, &data);
        assert!(result.is_ok());
        let instances = result.unwrap();
        assert!(instances.len() == 1);
        assert!(*instances[0].get_id() == program_id);
    }

    #[test]
    fn test_parse_accounts_meteora_dlmm() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // MeteoraDlmm needs 13 accounts
        let program_id = MeteoraDlmm::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id, owner, 0, None));
        for _ in 0..12 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        let data = InstructionData {
            accounts_length: [13, 0, 0, 0, 0],
            epoch: 0,
        };

        let result = parse_accounts(&accounts, &data);
        assert!(result.is_ok());
        let instances = result.unwrap();
        assert!(instances.len() == 1);
        assert!(*instances[0].get_id() == program_id);
    }

    #[test]
    fn test_parse_accounts_insufficient_accounts_for_program() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // MeteoraDlmm needs 13 accounts, but only provide 10
        let program_id = MeteoraDlmm::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id, owner, 0, None));
        for _ in 0..9 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        let data = InstructionData {
            accounts_length: [10, 0, 0, 0, 0],
            epoch: 0,
        };

        let result = parse_accounts(&accounts, &data);
        assert!(result.is_err());
        // Just verify it's an error - Anchor error types are complex to match
    }

    #[test]
    fn test_parse_accounts_multiple_programs_with_zero_spans() {
        let owner = system_program::id();
        let mut accounts = Vec::new();

        // First program
        let program_id_1 = MeteoraDammV2::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id_1, owner, 0, None));
        for _ in 0..8 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        // Second program
        let program_id_2 = MeteoraDlmm::PROGRAM_ID;
        accounts.push(create_mock_account_info(program_id_2, owner, 0, None));
        for _ in 0..12 {
            accounts.push(create_mock_account_info(
                Pubkey::new_unique(),
                owner,
                0,
                None,
            ));
        }

        // Mix of zero and non-zero spans
        let data = InstructionData {
            accounts_length: [9, 0, 13, 0, 0],
            epoch: 0,
        };

        let result = parse_accounts(&accounts, &data);
        assert!(result.is_ok());
        let instances = result.unwrap();
        assert!(instances.len() == 2);
        assert!(*instances[0].get_id() == program_id_1);
        assert!(*instances[1].get_id() == program_id_2);
    }
}
