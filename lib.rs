use anchor_lang::prelude::*;
use anchor_lang::system_program;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Burn};

declare_id!("BonkrTokenLaunchpad1111111111111111111111111");

#[program]
pub mod bonkr {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>, platform_fee_bps: u16, creator_fee_bps: u16, burn_fee_bps: u16, graduation_threshold_sol: u64) -> Result<()> {
        let factory = &mut ctx.accounts.factory_config;
        factory.authority = ctx.accounts.authority.key();
        factory.platform_fee_recipient = ctx.accounts.authority.key();
        factory.platform_fee_bps = platform_fee_bps;
        factory.creator_fee_bps = creator_fee_bps;
        factory.burn_fee_bps = burn_fee_bps;
        factory.graduation_threshold_sol = graduation_threshold_sol;
        factory.total_tokens_created = 0;
        factory.paused = false;
        factory.bump = ctx.bumps.factory_config;
        msg!("Bonkr Factory initialized");
        Ok(())
    }

    pub fn create_token(ctx: Context<CreateToken>, name: String, symbol: String, description: String, initial_virtual_sol: u64, initial_virtual_tokens: u64) -> Result<()> {
        require!(!ctx.accounts.factory_config.paused, BonkrError::ContractPaused);
        require!(name.len() <= 32, BonkrError::NameTooLong);
        require!(symbol.len() <= 10, BonkrError::SymbolTooLong);

        let token_state = &mut ctx.accounts.token_state;
        token_state.mint = ctx.accounts.token_mint.key();
        token_state.creator = ctx.accounts.creator.key();
        token_state.name = name.clone();
        token_state.symbol = symbol.clone();
        token_state.description = description;
        token_state.virtual_sol_reserve = initial_virtual_sol;
        token_state.virtual_token_reserve = initial_virtual_tokens;
        token_state.actual_sol_deposited = 0;
        token_state.total_supply = initial_virtual_tokens;
        token_state.circulating_supply = 0;
        token_state.burned_amount = 0;
        token_state.creator_unclaimed_fees = 0;
        token_state.is_graduated = false;
        token_state.trading_enabled = true;
        token_state.created_at = Clock::get()?.unix_timestamp;
        token_state.bump = ctx.bumps.token_state;
        token_state.vault_bump = ctx.bumps.sol_vault;

        ctx.accounts.factory_config.total_tokens_created += 1;

        emit!(TokenCreated { mint: token_state.mint, creator: token_state.creator, name, symbol, total_supply: token_state.total_supply, timestamp: token_state.created_at });
        Ok(())
    }

    pub fn buy(ctx: Context<Trade>, sol_amount: u64, min_tokens_out: u64) -> Result<()> {
        let factory = &ctx.accounts.factory_config;
        let token_state = &mut ctx.accounts.token_state;
        
        require!(!factory.paused, BonkrError::ContractPaused);
        require!(token_state.trading_enabled, BonkrError::TradingDisabled);
        require!(!token_state.is_graduated, BonkrError::TokenGraduated);
        require!(sol_amount > 0, BonkrError::InvalidAmount);

        let platform_fee = sol_amount * (factory.platform_fee_bps as u64) / 10000;
        let creator_fee = sol_amount * (factory.creator_fee_bps as u64) / 10000;
        let sol_after_fees = sol_amount - platform_fee - creator_fee;

        let k = token_state.virtual_sol_reserve.checked_mul(token_state.virtual_token_reserve).ok_or(BonkrError::MathOverflow)?;
        let new_virtual_sol = token_state.virtual_sol_reserve.checked_add(sol_after_fees).ok_or(BonkrError::MathOverflow)?;
        let new_virtual_tokens = k.checked_div(new_virtual_sol).ok_or(BonkrError::MathOverflow)?;
        let tokens_out = token_state.virtual_token_reserve.checked_sub(new_virtual_tokens).ok_or(BonkrError::MathOverflow)?;

        let burn_amount = tokens_out * (factory.burn_fee_bps as u64) / 10000;
        let tokens_to_user = tokens_out - burn_amount;

        require!(tokens_to_user >= min_tokens_out, BonkrError::SlippageExceeded);

        system_program::transfer(CpiContext::new(ctx.accounts.system_program.to_account_info(), system_program::Transfer { from: ctx.accounts.trader.to_account_info(), to: ctx.accounts.sol_vault.to_account_info() }), sol_after_fees + creator_fee)?;
        system_program::transfer(CpiContext::new(ctx.accounts.system_program.to_account_info(), system_program::Transfer { from: ctx.accounts.trader.to_account_info(), to: ctx.accounts.platform_fee_recipient.to_account_info() }), platform_fee)?;

        token_state.creator_unclaimed_fees += creator_fee;
        token_state.virtual_sol_reserve = new_virtual_sol;
        token_state.virtual_token_reserve = new_virtual_tokens;
        token_state.actual_sol_deposited += sol_after_fees;
        token_state.circulating_supply += tokens_to_user;
        token_state.burned_amount += burn_amount;

        let seeds = &[b"token_state", token_state.mint.as_ref(), &[token_state.bump]];
        token::mint_to(CpiContext::new_with_signer(ctx.accounts.token_program.to_account_info(), token::MintTo { mint: ctx.accounts.token_mint.to_account_info(), to: ctx.accounts.trader_token_account.to_account_info(), authority: ctx.accounts.token_state.to_account_info() }, &[&seeds[..]]), tokens_to_user)?;

        if token_state.actual_sol_deposited >= factory.graduation_threshold_sol && !token_state.is_graduated {
            emit!(ReadyToGraduate { mint: token_state.mint, sol_raised: token_state.actual_sol_deposited, timestamp: Clock::get()?.unix_timestamp });
        }
        Ok(())
    }

    pub fn sell(ctx: Context<Trade>, token_amount: u64, min_sol_out: u64) -> Result<()> {
        let factory = &ctx.accounts.factory_config;
        let token_state = &mut ctx.accounts.token_state;
        
        require!(!factory.paused, BonkrError::ContractPaused);
        require!(token_state.trading_enabled, BonkrError::TradingDisabled);
        require!(!token_state.is_graduated, BonkrError::TokenGraduated);
        require!(token_amount > 0, BonkrError::InvalidAmount);

        let burn_amount = token_amount * (factory.burn_fee_bps as u64) / 10000;
        let tokens_after_burn = token_amount - burn_amount;

        let k = token_state.virtual_sol_reserve.checked_mul(token_state.virtual_token_reserve).ok_or(BonkrError::MathOverflow)?;
        let new_virtual_tokens = token_state.virtual_token_reserve.checked_add(tokens_after_burn).ok_or(BonkrError::MathOverflow)?;
        let new_virtual_sol = k.checked_div(new_virtual_tokens).ok_or(BonkrError::MathOverflow)?;
        let sol_out_before_fees = token_state.virtual_sol_reserve.checked_sub(new_virtual_sol).ok_or(BonkrError::MathOverflow)?;

        let platform_fee = sol_out_before_fees * (factory.platform_fee_bps as u64) / 10000;
        let creator_fee = sol_out_before_fees * (factory.creator_fee_bps as u64) / 10000;
        let sol_to_user = sol_out_before_fees - platform_fee - creator_fee;

        require!(sol_to_user >= min_sol_out, BonkrError::SlippageExceeded);
        require!(sol_out_before_fees <= token_state.actual_sol_deposited, BonkrError::InsufficientLiquidity);

        token::burn(CpiContext::new(ctx.accounts.token_program.to_account_info(), Burn { mint: ctx.accounts.token_mint.to_account_info(), from: ctx.accounts.trader_token_account.to_account_info(), authority: ctx.accounts.trader.to_account_info() }), token_amount)?;

        **ctx.accounts.sol_vault.to_account_info().try_borrow_mut_lamports()? -= sol_to_user;
        **ctx.accounts.trader.to_account_info().try_borrow_mut_lamports()? += sol_to_user;
        **ctx.accounts.sol_vault.to_account_info().try_borrow_mut_lamports()? -= platform_fee;
        **ctx.accounts.platform_fee_recipient.to_account_info().try_borrow_mut_lamports()? += platform_fee;

        token_state.creator_unclaimed_fees += creator_fee;
        token_state.virtual_sol_reserve = new_virtual_sol;
        token_state.virtual_token_reserve = new_virtual_tokens;
        token_state.actual_sol_deposited -= sol_out_before_fees;
        token_state.circulating_supply -= tokens_after_burn;
        token_state.burned_amount += burn_amount;
        Ok(())
    }

    pub fn claim_creator_fees(ctx: Context<ClaimCreatorFees>) -> Result<()> {
        let token_state = &mut ctx.accounts.token_state;
        require!(ctx.accounts.creator.key() == token_state.creator, BonkrError::UnauthorizedCreator);
        require!(token_state.creator_unclaimed_fees > 0, BonkrError::NoFeesToClaim);
        let amount = token_state.creator_unclaimed_fees;
        token_state.creator_unclaimed_fees = 0;
        **ctx.accounts.sol_vault.to_account_info().try_borrow_mut_lamports()? -= amount;
        **ctx.accounts.creator.to_account_info().try_borrow_mut_lamports()? += amount;
        Ok(())
    }

    pub fn admin_recovery(ctx: Context<AdminRecovery>) -> Result<()> {
        let factory = &ctx.accounts.factory_config;
        let token_state = &mut ctx.accounts.token_state;
        require!(ctx.accounts.authority.key() == factory.authority, BonkrError::UnauthorizedAdmin);
        require!(!token_state.is_graduated, BonkrError::TokenGraduated);

        let vault_balance = ctx.accounts.sol_vault.lamports();
        let rent_exempt = Rent::get()?.minimum_balance(0);
        let amount = vault_balance.saturating_sub(rent_exempt);
        require!(amount > 0, BonkrError::NoFundsToRecover);

        token_state.actual_sol_deposited = 0;
        token_state.creator_unclaimed_fees = 0;
        token_state.trading_enabled = false;
        token_state.virtual_sol_reserve = 0;

        **ctx.accounts.sol_vault.to_account_info().try_borrow_mut_lamports()? -= amount;
        **ctx.accounts.authority.to_account_info().try_borrow_mut_lamports()? += amount;
        msg!("ADMIN RECOVERY: {} lamports withdrawn", amount);
        Ok(())
    }

    pub fn graduate(ctx: Context<Graduate>) -> Result<()> {
        let factory = &ctx.accounts.factory_config;
        let token_state = &mut ctx.accounts.token_state;
        require!(!token_state.is_graduated, BonkrError::TokenGraduated);
        require!(token_state.actual_sol_deposited >= factory.graduation_threshold_sol, BonkrError::NotReadyToGraduate);
        token_state.is_graduated = true;
        token_state.trading_enabled = false;
        emit!(TokenGraduated { mint: token_state.mint, sol_raised: token_state.actual_sol_deposited, circulating_supply: token_state.circulating_supply, timestamp: Clock::get()?.unix_timestamp });
        Ok(())
    }

    pub fn pause(ctx: Context<AdminAction>) -> Result<()> {
        require!(ctx.accounts.authority.key() == ctx.accounts.factory_config.authority, BonkrError::UnauthorizedAdmin);
        ctx.accounts.factory_config.paused = true;
        Ok(())
    }

    pub fn unpause(ctx: Context<AdminAction>) -> Result<()> {
        require!(ctx.accounts.authority.key() == ctx.accounts.factory_config.authority, BonkrError::UnauthorizedAdmin);
        ctx.accounts.factory_config.paused = false;
        Ok(())
    }

    pub fn transfer_authority(ctx: Context<TransferAuthority>) -> Result<()> {
        require!(ctx.accounts.authority.key() == ctx.accounts.factory_config.authority, BonkrError::UnauthorizedAdmin);
        ctx.accounts.factory_config.authority = ctx.accounts.new_authority.key();
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(init, payer = authority, space = 8 + FactoryConfig::INIT_SPACE, seeds = [b"factory_config"], bump)]
    pub factory_config: Account<'info, FactoryConfig>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(name: String, symbol: String)]
pub struct CreateToken<'info> {
    #[account(mut, seeds = [b"factory_config"], bump = factory_config.bump)]
    pub factory_config: Account<'info, FactoryConfig>,
    #[account(init, payer = creator, space = 8 + TokenState::INIT_SPACE, seeds = [b"token_state", token_mint.key().as_ref()], bump)]
    pub token_state: Account<'info, TokenState>,
    #[account(init, payer = creator, seeds = [b"sol_vault", token_mint.key().as_ref()], bump, space = 0)]
    /// CHECK: PDA vault
    pub sol_vault: AccountInfo<'info>,
    #[account(init, payer = creator, mint::decimals = 9, mint::authority = token_state)]
    pub token_mint: Account<'info, Mint>,
    #[account(mut)]
    pub creator: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct Trade<'info> {
    #[account(seeds = [b"factory_config"], bump = factory_config.bump)]
    pub factory_config: Account<'info, FactoryConfig>,
    #[account(mut, seeds = [b"token_state", token_mint.key().as_ref()], bump = token_state.bump)]
    pub token_state: Account<'info, TokenState>,
    #[account(mut, seeds = [b"sol_vault", token_mint.key().as_ref()], bump = token_state.vault_bump)]
    /// CHECK: PDA vault
    pub sol_vault: AccountInfo<'info>,
    #[account(mut)]
    pub token_mint: Account<'info, Mint>,
    #[account(mut)]
    pub trader: Signer<'info>,
    #[account(mut, associated_token::mint = token_mint, associated_token::authority = trader)]
    pub trader_token_account: Account<'info, TokenAccount>,
    /// CHECK: Platform fee recipient
    #[account(mut, address = factory_config.platform_fee_recipient)]
    pub platform_fee_recipient: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ClaimCreatorFees<'info> {
    #[account(mut, seeds = [b"token_state", token_state.mint.as_ref()], bump = token_state.bump)]
    pub token_state: Account<'info, TokenState>,
    #[account(mut, seeds = [b"sol_vault", token_state.mint.as_ref()], bump = token_state.vault_bump)]
    /// CHECK: PDA vault
    pub sol_vault: AccountInfo<'info>,
    #[account(mut)]
    pub creator: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct AdminRecovery<'info> {
    #[account(seeds = [b"factory_config"], bump = factory_config.bump)]
    pub factory_config: Account<'info, FactoryConfig>,
    #[account(mut, seeds = [b"token_state", token_state.mint.as_ref()], bump = token_state.bump)]
    pub token_state: Account<'info, TokenState>,
    #[account(mut, seeds = [b"sol_vault", token_state.mint.as_ref()], bump = token_state.vault_bump)]
    /// CHECK: PDA vault
    pub sol_vault: AccountInfo<'info>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Graduate<'info> {
    #[account(seeds = [b"factory_config"], bump = factory_config.bump)]
    pub factory_config: Account<'info, FactoryConfig>,
    #[account(mut, seeds = [b"token_state", token_state.mint.as_ref()], bump = token_state.bump)]
    pub token_state: Account<'info, TokenState>,
    pub caller: Signer<'info>,
}

#[derive(Accounts)]
pub struct AdminAction<'info> {
    #[account(mut, seeds = [b"factory_config"], bump = factory_config.bump)]
    pub factory_config: Account<'info, FactoryConfig>,
    #[account(mut)]
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct TransferAuthority<'info> {
    #[account(mut, seeds = [b"factory_config"], bump = factory_config.bump)]
    pub factory_config: Account<'info, FactoryConfig>,
    #[account(mut)]
    pub authority: Signer<'info>,
    /// CHECK: New authority
    pub new_authority: AccountInfo<'info>,
}

#[account]
#[derive(InitSpace)]
pub struct FactoryConfig {
    pub authority: Pubkey,
    pub platform_fee_recipient: Pubkey,
    pub platform_fee_bps: u16,
    pub creator_fee_bps: u16,
    pub burn_fee_bps: u16,
    pub graduation_threshold_sol: u64,
    pub total_tokens_created: u64,
    pub paused: bool,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct TokenState {
    pub mint: Pubkey,
    pub creator: Pubkey,
    #[max_len(32)]
    pub name: String,
    #[max_len(10)]
    pub symbol: String,
    #[max_len(256)]
    pub description: String,
    pub virtual_sol_reserve: u64,
    pub virtual_token_reserve: u64,
    pub actual_sol_deposited: u64,
    pub total_supply: u64,
    pub circulating_supply: u64,
    pub burned_amount: u64,
    pub creator_unclaimed_fees: u64,
    pub is_graduated: bool,
    pub trading_enabled: bool,
    pub created_at: i64,
    pub bump: u8,
    pub vault_bump: u8,
}

#[event]
pub struct TokenCreated { pub mint: Pubkey, pub creator: Pubkey, pub name: String, pub symbol: String, pub total_supply: u64, pub timestamp: i64 }
#[event]
pub struct ReadyToGraduate { pub mint: Pubkey, pub sol_raised: u64, pub timestamp: i64 }
#[event]
pub struct TokenGraduated { pub mint: Pubkey, pub sol_raised: u64, pub circulating_supply: u64, pub timestamp: i64 }

#[error_code]
pub enum BonkrError {
    #[msg("Contract is paused")] ContractPaused,
    #[msg("Trading is disabled")] TradingDisabled,
    #[msg("Token graduated")] TokenGraduated,
    #[msg("Invalid amount")] InvalidAmount,
    #[msg("Math overflow")] MathOverflow,
    #[msg("Slippage exceeded")] SlippageExceeded,
    #[msg("Insufficient liquidity")] InsufficientLiquidity,
    #[msg("Unauthorized admin")] UnauthorizedAdmin,
    #[msg("Unauthorized creator")] UnauthorizedCreator,
    #[msg("No fees to claim")] NoFeesToClaim,
    #[msg("No funds to recover")] NoFundsToRecover,
    #[msg("Name too long")] NameTooLong,
    #[msg("Symbol too long")] SymbolTooLong,
    #[msg("Not ready to graduate")] NotReadyToGraduate,
}
