use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};

// Define the program ID as specified in Anchor.toml
declare_id!("Fg6PaGSDgDxqD8M8qB5wX86L3F95xU5Nq7Uj5f2T15U1");

/// A simple staking program where users stake native SOL
/// and earn a custom SPL token (RewardToken) as reward.
#[program]
pub mod sol_staking {
    use super::*;
    
    /// Initializes the global state for the staking pool.
    /// Creates the Reward Token Mint and the Program's Reward Vault.
    pub fn initialize_pool(ctx: Context<InitializePool>, reward_rate_per_sec: u64) -> Result<()> {
        let pool = &mut ctx.accounts.stake_pool;
        
        // Use a seed for the pool PDA to allow for multiple pools if needed.
        // The default seed is 'stake_pool'.
        pool.reward_mint = ctx.accounts.reward_mint.key();
        pool.reward_vault = ctx.accounts.reward_vault.key();
        pool.reward_rate_per_sec = reward_rate_per_sec; // E.g., 1000 = 0.001 token per staked SOL per second
        pool.authority = ctx.accounts.initializer.key();
        
        msg!("Staking Pool Initialized.");
        msg!("Reward Mint: {}", pool.reward_mint);
        msg!("Reward Rate (per sec/SOL): {}", pool.reward_rate_per_sec);

        Ok(())
    }

    /// User stakes native SOL.
    /// The SOL is transferred directly to the StakeEntry PDA, which holds the balance.
    /// This function also creates or updates the user's StakeEntry account.
    pub fn stake_sol(ctx: Context<StakeSol>, amount: u64) -> Result<()> {
        let stake_entry = &mut ctx.accounts.stake_entry;
        let pool = &ctx.accounts.stake_pool;
        let clock = Clock::get()?;

        // Calculate and claim any pending rewards first, before modifying the stake.
        // This prevents users from gaming the reward calculation by staking/unstaking quickly.
        let (pending_rewards, new_last_staked) = calculate_rewards(
            stake_entry.staked_amount,
            stake_entry.last_staked_at,
            pool.reward_rate_per_sec,
            clock.unix_timestamp as u64,
        );
        
        // 1. Claim pending rewards if any
        if pending_rewards > 0 {
            // CPI to mint rewards to the user
            let cpi_accounts = token::MintTo {
                mint: ctx.accounts.reward_mint.to_account_info(),
                to: ctx.accounts.user_reward_account.to_account_info(),
                authority: ctx.accounts.reward_vault.to_account_info(), // The Reward Vault PDA is the Mint Authority
            };
            let cpi_program = ctx.accounts.token_program.to_account_info();
            
            // Generate seeds for signing the CPI
            let bump = *ctx.bumps.get("stake_entry").ok_or(ErrorCode::BumpNotFound)?;
            let signer_seeds: &[&[&[u8]]] = &[&[
                b"stake_entry", 
                ctx.accounts.user.key().as_ref(), 
                &[bump]
            ]];
            
            token::mint_to(
                CpiContext::new_with_signer(cpi_program, cpi_accounts, signer_seeds), 
                pending_rewards
            )?;
            
            msg!("Claimed {} pending rewards before new stake.", pending_rewards);
        }

        // 2. Transfer SOL to the StakeEntry PDA
        anchor_lang::solana_program::program::invoke(
            &anchor_lang::solana_program::system_instruction::transfer(
                ctx.accounts.user.key,
                stake_entry.to_account_info().key,
                amount,
            ),
            &[
                ctx.accounts.user.to_account_info(),
                stake_entry.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
        )?;

        // 3. Update the StakeEntry state
        stake_entry.staked_amount = stake_entry.staked_amount.checked_add(amount).unwrap();
        stake_entry.last_staked_at = new_last_staked;
        stake_entry.user_wallet = ctx.accounts.user.key();
        
        msg!("Staked {} SOL. Total staked: {}.", amount, stake_entry.staked_amount);

        Ok(())
    }

    /// Claims accrued reward tokens.
    pub fn claim_rewards(ctx: Context<ClaimRewards>) -> Result<()> {
        let stake_entry = &mut ctx.accounts.stake_entry;
        let pool = &ctx.accounts.stake_pool;
        let clock = Clock::get()?;

        require!(stake_entry.staked_amount > 0, ErrorCode::NoStakedBalance);

        // 1. Calculate pending rewards
        let (pending_rewards, new_last_staked) = calculate_rewards(
            stake_entry.staked_amount,
            stake_entry.last_staked_at,
            pool.reward_rate_per_sec,
            clock.unix_timestamp as u64,
        );
        
        if pending_rewards == 0 {
            msg!("No new rewards to claim.");
            return Ok(());
        }

        // 2. CPI to mint rewards
        let cpi_accounts = token::MintTo {
            mint: ctx.accounts.reward_mint.to_account_info(),
            to: ctx.accounts.user_reward_account.to_account_info(),
            authority: ctx.accounts.reward_vault.to_account_info(), // The Reward Vault is the Mint Authority
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();

        // Generate seeds for signing the CPI
        // We use the StakePool PDA's seeds to sign the mint instruction
        let bump = *ctx.bumps.get("stake_pool").ok_or(ErrorCode::BumpNotFound)?;
        let signer_seeds: &[&[&[u8]]] = &[&[
            b"stake_pool", 
            &[bump]
        ]];

        token::mint_to(
            CpiContext::new_with_signer(cpi_program, cpi_accounts, signer_seeds), 
            pending_rewards
        )?;

        // 3. Update StakeEntry state
        stake_entry.last_staked_at = new_last_staked;
        
        msg!("Successfully claimed {} reward tokens.", pending_rewards);

        Ok(())
    }

    /// User unstakes their native SOL and optionally closes their StakeEntry account.
    pub fn unstake_sol(ctx: Context<UnstakeSol>) -> Result<()> {
        let stake_entry = &mut ctx.accounts.stake_entry;
        let user = &ctx.accounts.user;
        
        require!(stake_entry.staked_amount > 0, ErrorCode::NoStakedBalance);

        // NOTE: Claiming rewards MUST be done in a separate transaction for security
        // and atomicity, to prevent state updates from being out of sync.
        
        let amount_to_unstake = stake_entry.staked_amount;
        
        // 1. Transfer SOL back to the user
        // The StakeEntry PDA holds the staked SOL. We transfer lamports.
        let stake_entry_info = stake_entry.to_account_info();

        // Ensure the stake_entry account has enough lamports to cover the staked amount
        if stake_entry_info.lamports() < amount_to_unstake {
            return Err(ErrorCode::InsufficientLamports.into());
        }
        
        **// Transfer SOL (lamports) back to the user**
        **// We use a custom invocation for lamport transfers from a PDA**
        let to_transfer = amount_to_unstake;
        
        // Credit lamports to user
        **anchor_lang::solana_program::program::invoke(
            &anchor_lang::solana_program::system_instruction::transfer(
                stake_entry_info.key,
                user.key,
                to_transfer,
            ),
            &[
                stake_entry_info.clone(), 
                user.to_account_info().clone(), 
                ctx.accounts.system_program.to_account_info().clone()
            ],
        )?;**

        // 2. Update the StakeEntry state
        stake_entry.staked_amount = 0;
        stake_entry.last_staked_at = Clock::get()?.unix_timestamp as u64; // Reset timestamp

        // 3. Close the StakeEntry account to recover rent (optional, but good practice)
        // If the account is closed, the remaining rent SOL is returned to the user
        let current_lamports = stake_entry_info.lamports();
        let rent_exempt_amount = ctx.accounts.rent.minimum_balance(stake_entry_info.data_len());

        // Only close if all SOL has been unstaked and only rent remains
        if current_lamports <= rent_exempt_amount {
             **stake_entry_info.exit(&ctx.program_id)?;**
        }

        msg!("Unstaked {} SOL. StakeEntry account closed if empty.", amount_to_unstake);

        Ok(())
    }
}

/// Helper function to calculate earned rewards since the last stake/claim action.
/// The reward rate is calculated based on u64 units to avoid float math on-chain.
/// Reward = staked_amount * reward_rate * (current_time - last_staked_time)
pub fn calculate_rewards(
    staked_amount: u64,
    last_staked_at: u64,
    reward_rate_per_sec: u64,
    current_time: u64,
) -> (u64, u64) {
    if staked_amount == 0 || current_time <= last_staked_at {
        return (0, current_time);
    }

    let time_elapsed = current_time.checked_sub(last_staked_at).unwrap_or(0);
    
    // We use a large multiplier (e.g., 10^9 or 10^12) in real applications
    // for high precision. Here, we'll keep it simple by assuming the reward_rate_per_sec
    // is already scaled (e.g., 1 unit = 0.000001 token).
    
    // Total reward = staked_amount * time_elapsed * reward_rate_per_sec
    let total_reward = staked_amount
        .checked_mul(time_elapsed)
        .unwrap_or(0)
        .checked_mul(reward_rate_per_sec)
        .unwrap_or(0);
        
    // Assuming the reward_rate_per_sec is scaled by 10^9 (NINE_DECIMALS)
    // In a real application, you'd divide by the scaling factor here. 
    // For this example, we just return the total_reward as the raw amount.

    (total_reward, current_time)
}

// --- ACCOUNTS CONTEXTS ---

/// Accounts required to initialize the Staking Pool
#[derive(Accounts)]
pub struct InitializePool<'info> {
    // Account for the global pool state.
    // Seed: "stake_pool"
    #[account(
        init, 
        payer = initializer, 
        space = 8 + StakePool::LEN,
        seeds = [b"stake_pool"],
        bump
    )]
    pub stake_pool: Account<'info, StakePool>,
    
    // The mint for the reward token (SPL Token standard).
    // The pool PDA will be the authority to mint new tokens.
    #[account(
        init,
        payer = initializer,
        mint::decimals = 9,
        mint::authority = reward_vault, // Reward Vault PDA will be the mint authority
    )]
    pub reward_mint: Account<'info, Mint>,

    // The Reward Vault PDA. This account acts as the mint authority for the Reward Mint.
    // Seed: "reward_vault"
    #[account(
        seeds = [b"reward_vault"],
        bump
    )]
    /// CHECK: This account is simply a PDA used as the Mint Authority, no state storage needed.
    pub reward_vault: AccountInfo<'info>,

    #[account(mut)]
    pub initializer: Signer<'info>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

/// Accounts required to stake SOL
#[derive(Accounts)]
pub struct StakeSol<'info> {
    // The user staking their SOL
    #[account(mut)]
    pub user: Signer<'info>,
    
    // The global pool state. Must be initialized.
    #[account(
        seeds = [b"stake_pool"], 
        bump,
        has_one = reward_mint // Constraint: Ensure the mint matches the pool's record
    )]
    pub stake_pool: Account<'info, StakePool>,

    // The user's stake entry account. Stores the staked amount and last staked time.
    // It's created/initialized on the first stake.
    // Seed: "stake_entry", user_wallet_pubkey
    #[account(
        init_if_needed,
        payer = user,
        space = 8 + StakeEntry::LEN,
        seeds = [b"stake_entry", user.key().as_ref()],
        bump
    )]
    pub stake_entry: Account<'info, StakeEntry>,

    // The mint for the reward token.
    #[account(mut)]
    pub reward_mint: Account<'info, Mint>,

    // The Reward Vault PDA which is the Mint Authority.
    #[account(
        seeds = [b"reward_vault"], 
        bump
    )]
    /// CHECK: This is the PDA that must match the mint authority
    pub reward_vault: AccountInfo<'info>,

    // The user's SPL Token account to receive rewards.
    // Must be initialized by the user beforehand.
    #[account(mut, token::mint = reward_mint, token::authority = user)]
    pub user_reward_account: Account<'info, TokenAccount>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

/// Accounts required to claim rewards
#[derive(Accounts)]
pub struct ClaimRewards<'info> {
    // The user claiming rewards
    #[account(mut)]
    pub user: Signer<'info>,
    
    // The global pool state
    #[account(
        seeds = [b"stake_pool"], 
        bump,
        has_one = reward_mint
    )]
    pub stake_pool: Account<'info, StakePool>,

    // The user's stake entry account.
    #[account(
        mut,
        seeds = [b"stake_entry", user.key().as_ref()],
        bump,
        has_one = user_wallet // Constraint: Ensure the user wallet matches the one recorded in the entry
    )]
    pub stake_entry: Account<'info, StakeEntry>,

    // The mint for the reward token.
    #[account(mut)]
    pub reward_mint: Account<'info, Mint>,

    // The Reward Vault PDA which is the Mint Authority.
    #[account(
        seeds = [b"reward_vault"], 
        bump
    )]
    /// CHECK: This is the PDA that must match the mint authority
    pub reward_vault: AccountInfo<'info>,

    // The user's SPL Token account to receive rewards.
    #[account(mut, token::mint = reward_mint, token::authority = user)]
    pub user_reward_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    // We need System Program only if we were doing SOL transfer, but not for minting.
}

/// Accounts required to unstake SOL
#[derive(Accounts)]
pub struct UnstakeSol<'info> {
    // The user unstaking their SOL
    #[account(mut)]
    pub user: Signer<'info>,

    // The global pool state. Read-only here.
    #[account(seeds = [b"stake_pool"], bump)]
    pub stake_pool: Account<'info, StakePool>,

    // The user's stake entry account. Mutated to transfer SOL and reset state.
    // Closing the account returns residual rent to the user.
    #[account(
        mut,
        seeds = [b"stake_entry", user.key().as_ref()],
        bump,
        has_one = user_wallet,
        // Close the account only if all SOL is unstaked.
        // Anchor's close constraint is tricky with native SOL deposits, so we handle it manually.
        // close = user // Use if we are sure the account is empty, but handling manually is safer.
    )]
    pub stake_entry: Account<'info, StakeEntry>,

    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}


// --- STATE ACCOUNTS ---

/// Global state for the Staking Pool. (PDA)
#[account]
pub struct StakePool {
    pub authority: Pubkey,          // The admin/owner who initialized the pool
    pub reward_mint: Pubkey,        // Pubkey of the custom reward token mint
    pub reward_vault: Pubkey,       // Pubkey of the Reward Vault PDA (Mint Authority)
    pub reward_rate_per_sec: u64,   // Reward rate per staked SOL per second (e.g., 1000 for 0.001)
    // Note: Total Staked SOL is often tracked but can be calculated by summing all StakeEntry accounts.
}

impl StakePool {
    pub const LEN: usize = 32 + 32 + 32 + 8; // 96 + 8 = 104 bytes
}

/// State for a single user's stake. (PDA)
/// This account also implicitly holds the staked SOL as its balance.
#[account]
pub struct StakeEntry {
    pub user_wallet: Pubkey,        // The user who owns this stake
    pub staked_amount: u64,         // Amount of SOL currently staked
    pub last_staked_at: u64,        // Unix timestamp of the last stake/claim action
}

impl StakeEntry {
    pub const LEN: usize = 32 + 8 + 8; // 48 bytes
}


// --- ERRORS ---

#[error_code]
pub enum ErrorCode {
    #[msg("The account is already initialized.")]
    AlreadyInitialized,
    #[msg("The user does not have any staked balance to unstake or claim rewards.")]
    NoStakedBalance,
    #[msg("Bump seed not found for PDA derivation.")]
    BumpNotFound,
    #[msg("The Stake Entry account does not have enough lamports to cover the staked amount.")]
    InsufficientLamports,
}