use anchor_lang::prelude::*;
use anchor_spl::token::{self,Mint,Token,TokenAccount,Transfer}

declare_id!("BN1n4CKZ57cfzH9X4s8kMQ94XuxnRg51LnhStEijGJ9k");

#[program]
pub mod staking_build {
    use super::*;

    pub fn initialize(ctx: Context<InitializePool>, reward_rate_per_sec: u64) -> Result<()> {
        let pool = &mut ctx.accounts.stake_pool;

        pool.reward_mint = ctx.accounts.reward_mint.key();
        pool.reward_vault = ctx.accounts.reward_vault.key();
        pool.reward_rate_per_sec = ctx.accounts.reward_rate_per_sec;
        pool.authority = ctx.accounts.initializer.key();

        msg!("Staking Pool Initialised");
        msg!("Reward Mint: {}",pool.reward_mint);
        msg!("Reward Rate: {}",pool.reward_rate_per_sec);
        
        Ok(())
    }

    pub fn stake_sol(ctx: Context<StakeSol>,amount:u64) -> Result<()> {
        let stake_entry = &mut ctx.accounts.stake_entry;
        let pool = &ctx.accounts.stake_pool;
        let clock = Clock::get()?;

        let (pending_rewards,new_last_staked) = calculate_rewards(
            stake_entry.staked_amount,
            stake_entry.last_staked_at,
            pool.reward_rate_per_sec,
            clock.unix_timestamp as u64
        );

        if pending_rewards > 0 {
            let cpi_accounts = token::MintTo {
                mint: ctx.accounts.reward_mint.to_account_info(),
                to: ctx.accounts.user_reward_account.to_account_info(),
                authority: ctx.accounts.reward_vault.to_account_info()
            };
            let cpi_program = ctx.accounts.token_program.to_account_info();

            let bump = *ctx.bumps.get("stake_entry").ok_or(ErrorCode::BumpNotFound);
            let signer_seeds: &[&[&[u8]]] = &[&[b"stake_entry",ctx.accounts.user.key().as_ref(),&[bump]]];

            toke::mint_to(
                CpiContext::new_with_signer(cpi_program,cpi_accounts,signer_seeds),pending_rewards
            )?;

            msg!("Claimed {} pending rewards before new stake." , pending_rewards);
        }

        anchor_lang::solana_program::program::invoke(
            &anchor_lang::solana_program::system_instruction::transfer(
                ctx.accounts.user.key,
                stake_entry.to_account_info().key,
                amount
            ),
            &[
                ctx.accounts.user.to_account_info(),
                stake_entry.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ]
        )?;

        stake_entry.staked_amount = stake_entry.staked_amount.checked_add(amount).unwrap();
        stake_entry.last_staked_at = new_last_staked;
        stake_entry.user_wallet = ctx.accounts.user.key();

        msg!("Staked {} SOL. Total staked: {}.", amount, stake_entry.staked_amount);

        Ok(())
    }

    pub fn claim_rewards(ctx: Context<ClaimRewards>) -> Result<()> {
        let stake_entry = &mut ctx.accounts.stake_entry;
        let pool = &ctx.accounts.stake_pool;
        let clock = Clock::get()?;

        require!(stake_entry.staked_amount > 0, ErrorCode::NoStakedBalance);

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

        let cpi_accounts = token::MintTo {
            mint: ctx.accounts.reward_mint.to_account_info(),
            to: ctx.accounts.user_reward_account.to_account_info(),
            authority: ctx.accounts.reward_vault.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();

        let bump = *ctx.bumps.get("stake_pool").ok_or(ErrorCode::BumpNotFound)?;
        let signer_seeds: &[&[&[u8]]] = &[&[
            b"stake_pool", 
            &[bump]
        ]];

        token::mint_to(
            CpiContext::new_with_signer(cpi_program, cpi_accounts, signer_seeds), 
            pending_rewards
        )?;

        stake_entry.last_staked_at = new_last_staked;
        
        msg!("Successfully claimed {} reward tokens.", pending_rewards);

        Ok(())
    }

    pub fn unstake_sol(ctx: Context<UnstakeSol>) -> Result<()> {
        let stake_entry = &mut ctx.accounts.stake_entry;
        let user = &ctx.accounts.user;
        
        require!(stake_entry.staked_amount > 0, ErrorCode::NoStakedBalance);
        
        let amount_to_unstake = stake_entry.staked_amount;
        let stake_entry_info = stake_entry.to_account_info();

        if stake_entry_info.lamports() < amount_to_unstake {
            return Err(ErrorCode::InsufficientLamports.into());
        }
        
        let to_transfer = amount_to_unstake;
        
        anchor_lang::solana_program::program::invoke(
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
        )?;

        stake_entry.staked_amount = 0;
        stake_entry.last_staked_at = Clock::get()?.unix_timestamp as u64; 

        let current_lamports = stake_entry_info.lamports();
        let rent_exempt_amount = ctx.accounts.rent.minimum_balance(stake_entry_info.data_len());

        if current_lamports <= rent_exempt_amount {
            stake_entry_info.exit(&ctx.program_id)?;
        }

        msg!("Unstaked {} SOL. StakeEntry account closed if empty.", amount_to_unstake);

        Ok(())
    }
}

pub fn calculate_rewards(
    staked_amount: u64,
    last_staked_at: u64,
    reward_rate_per_sec: u64,
    current_time: u64
) -> (u64,u64) {
    if staked_amount == 0 || current_time <= last_staked_at {
        return (0,current_time);
    }

    let time_elapsed = current_time.checked_sub(last_staked_at).unwrap_or(0);

    let total_reward = staked_amount
                        .checked_mul(time_elapsed)
                        .unwrap_or(0)
                        .checked_mul(reward_rate_per_sec)
                        .unwrap_or(0);

    (total_reward,current_time)
}

#[account]
pub struct StakePool {
    pub authority: Pubkey,
    pub reward_mint: Pubkey,
    pub reward_vault: Pubkey,
    pub reward_rate_per_sec: u64
}

impl StakePool {
    pub const LEN: usize = 32 + 32 + 32 + 8;
}

#[account]
pub struct StakeEntry {
    pub user_wallet: Pubkey,
    pub staked_amount: u64,
    pub last_staked_at: u64
}

impl StakeEntry {
    pub const LEN: usize = 32 + 8 + 8;
}

#[derive(Accounts)]
pub struct InitializePool<'info> {
    #[account(
        init,
        payer = initializer,
        space = 8 + StakePool::LEN,
        seeds= [b"stake_pool"],
        bump
    )]
    pub stake_pool: Account<'info,StakePool>,

    #[account(
        init,
        payer = initializer,
        mint::decimals = 9,
        mint::authority = reward_vault
    )]
    pub reward_mint: Account<'info,Mint>,

    #[account(
        seeds= [b"reward_vault"],
        bump
    )]
    pub reward_vault: Account<'info>,

    #[account(mut)]
    pub initializer: Signer<'info>,
    pub system_program: Program<'info,System>,
    pub token_program: Program<'info,Token>,
    pub rent: Sysvar<'info,Rent>
}

#[derive(Accounts)]
pub struct StakeSol<'info> {
    #[account(mut)]
    pub user:Signer<'info>,

    #[account(
        seeds= [b"stake_pool"],
        bump,
        has_one = reward_mint
    )]
    pub stake_pool: Account<'info,StakePool>,

    #[account(
        init_if_needed,
        payer = user,
        space = 8 + StakeEntry::LEN,
        seeds= [b"stake_entry",user.key().as_ref()],
        bump
    )]
    pub stake_entry: Account<'info,StakeEntry>,

    #[account(mut)]
    pub reward_mint: Account<'info,Mint>,

    #[account(
        seeds = [b"reward_vault"]
        bump
    )]
    pub reward_vault: AccountInfo<'info>,

    #[account(mut,token::mint = reward_mint, token::authority = user)]
    pub user_reward_account: Account<'info,TokenAccount>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct UnstakeSol<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(seeds = [b"stake_pool"],bump)]
    pub stake_pool: Account<'info,StakePool>,

    #[account(
        mut,
        seeds = [b"stake_entry",user.key().as_ref()],
        bump,
        has_one = user_wallet
    )]
    pub stake_entry: Account<'info,StakeEntry>,
    pub system_program: Program<'info,System>,
    pub rent: Sysvar<'info,Rent>
}

#[derive(Accounts)]
pub struct ClaimRewards<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(
        seeds = [b"stake_pool"],
        bump,
        has_one = reward_mint
    )]
    pub stake_pool: Account<'info,StakePool>,

    #[account(
        mut,
        seeds = [b"stake_entry",user.key().as_ref()],
        bump,
        has_one = user_wallet
    )]
    pub stake_entry: Account<'info,StakeEntry>,

    #[account(mut)]
    pub reward_mint: Account<'info,Mint>,

    #[account(
        seeds = [b"reward_vault"],
        bump
    )]
    pub reward_vault: AccountInfo<'info>,

    #[account(mut,token::mint = reward_mint, token::authority = user)]
    pub user_reward_account: Account<'info,TokenAccount>,

    pub token_program: Program<'info,Token>
}

#[error_code]
pub enum ErrorCode {
    #[msg("The account is already initialised")]
    AlreadyInitialized,
    #[msg("The user does not have any staked balance to unstake or claim rewards")]
    NoStakedBalance,
    #[msg("Bump seed not found for PDA derivation")]
    BumpNotFound,
    #[msg("The stake Entry account does not have anough lamports to cover the staked amuount")]
    InsufficientLamports,
}
