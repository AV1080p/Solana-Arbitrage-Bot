use std::{str::FromStr, sync::Arc, time::Duration};
use anyhow::{anyhow, Result};
use colored::Colorize;
use std::cmp;
use std::env;

use anchor_client::solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_program,
};
use spl_associated_token_account::{
    get_associated_token_address, instruction::create_associated_token_account_idempotent,
};
use spl_token::{amount_to_ui_amount, ui_amount_to_amount};
use spl_token_client::token::TokenError;
use tokio::time::{Instant, sleep};

use crate::{
    common::{config::SwapConfig, logger::Logger},
    core::token,
    engine::swap::{SwapDirection, SwapInType},
};

// PumpSwap Constants
pub const TEN_THOUSAND: u64 = 10000;
pub const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
pub const ASSOCIATED_TOKEN_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
pub const PUMP_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
pub const PUMP_GLOBAL_CONFIG: &str = "ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw";
pub const PUMP_FEE_RECIPIENT: &str = "62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV";
pub const PUMP_EVENT_AUTHORITY: &str = "GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR";
pub const BUY_DISCRIMINATOR: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
pub const SELL_DISCRIMINATOR: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];
pub const SOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// A struct to represent the PumpSwap pool which uses constant product AMM
#[derive(Debug, Clone)]
pub struct PumpSwapPool {
    pub pool_id: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub lp_mint: Pubkey,
    pub pool_base_account: Pubkey,
    pub pool_quote_account: Pubkey,
    pub base_reserve: u64,
    pub quote_reserve: u64,
}

pub struct PumpSwap {
    pub keypair: Arc<Keypair>,
    pub rpc_client: Option<Arc<anchor_client::solana_client::rpc_client::RpcClient>>,
    pub rpc_nonblocking_client: Option<Arc<anchor_client::solana_client::nonblocking::rpc_client::RpcClient>>,
}

impl PumpSwap {
    pub fn new(
        keypair: Arc<Keypair>,
        rpc_client: Option<Arc<anchor_client::solana_client::rpc_client::RpcClient>>,
        rpc_nonblocking_client: Option<Arc<anchor_client::solana_client::nonblocking::rpc_client::RpcClient>>,
    ) -> Self {
        Self {
            keypair,
            rpc_client,
            rpc_nonblocking_client,
        }
    }

    pub async fn build_swap_ixn_by_mint(
        &self,
        mint_str: &str,
        pool: Option<PumpSwapPool>,
        swap_config: SwapConfig,
        start_time: Instant,
    ) -> Result<(Arc<Keypair>, Vec<Instruction>, f64)> {
        let logger = Logger::new("[PUMPSWAP-SWAP-BY-MINT] => ".blue().to_string());
        let slippage_bps = swap_config.slippage * 100;
        let owner = self.keypair.pubkey();
        let mint = Pubkey::from_str(mint_str).map_err(|_| anyhow!("Invalid mint address"))?;
        let sol_mint = Pubkey::from_str(SOL_MINT)?;
        
        // Determine input and output tokens based on swap direction
        let (token_in, token_out, discriminator) = match swap_config.swap_direction {
            SwapDirection::Buy => (sol_mint, mint, BUY_DISCRIMINATOR),
            SwapDirection::Sell => (mint, sol_mint, SELL_DISCRIMINATOR),
        };
        
        let pump_program = Pubkey::from_str(PUMP_PROGRAM)?;
        let mut create_instruction = None;
        let mut close_instruction = None;

        // Get or fetch pool information
        let pool_info = if let Some(pool) = pool {
            pool
        } else {
            get_pool_info(self.rpc_client.clone().unwrap(), mint).await?
        };
        
        // Calculate reserves based on the pool
        let base_reserve = pool_info.base_reserve;
        let quote_reserve = pool_info.quote_reserve;

        // Use SPL token's get_associated_token_address directly to ensure consistency
        let in_ata = get_associated_token_address(&owner, &token_in);
        let out_ata = get_associated_token_address(&owner, &token_out);
        
        let (amount_specified, _amount_ui_pretty) = match swap_config.swap_direction {
            SwapDirection::Buy => {
                // Create base ATA if it doesn't exist.
                let out_ata_exists = async {
                    let max_retries = 3;
                    let mut retry_count = 0;
                    
                    while retry_count < max_retries {
                        match token::get_account_info(
                            self.rpc_nonblocking_client.clone().expect("RPC nonblocking client not initialized"),
                            token_out,
                            out_ata,
                        ).await {
                            Ok(_) => return true,
                            Err(TokenError::AccountNotFound) | Err(TokenError::AccountInvalidOwner) => return false,
                            Err(_) => {
                                retry_count += 1;
                                if retry_count < max_retries {
                                    sleep(Duration::from_millis(200)).await;
                                }
                            }
                        }
                    }
                    false
                }.await;
                
                if !out_ata_exists {
                    create_instruction = Some(create_associated_token_account_idempotent(
                        &owner,
                        &owner,
                        &token_out,
                        &Pubkey::from_str(TOKEN_PROGRAM)?,
                    ));
                }
                
                (
                    ui_amount_to_amount(swap_config.amount_in, 9), // SOL decimals
                    (swap_config.amount_in, 9),
                )
            }
            SwapDirection::Sell => {
                // Check if the input ATA exists
                let in_ata_exists = async {
                    let max_retries = 6;
                    let mut retry_count = 0;
                    
                    while retry_count < max_retries {
                        match token::get_account_info(
                            self.rpc_nonblocking_client.clone().expect("RPC nonblocking client not initialized"),
                            token_in,
                            in_ata,
                        ).await {
                            Ok(_) => return true,
                            Err(TokenError::AccountNotFound) | Err(TokenError::AccountInvalidOwner) => return false,
                            Err(_) => {
                                retry_count += 1;
                                if retry_count < max_retries {
                                    sleep(Duration::from_millis(200)).await;
                                }
                            }
                        }
                    }
                    false
                }.await;
                
                if !in_ata_exists {
                    logger.log(format!("ATA for token {} does not exist, cannot sell", token_in));
                    return Err(anyhow!("Token ATA does not exist, cannot sell"));
                }
                
                // Get account and mint info
                let in_account = token::get_account_info(
                    self.rpc_nonblocking_client.clone().expect("RPC nonblocking client not initialized"),
                    token_in,
                    in_ata,
                ).await?;
                
                let in_mint = token::get_mint_info(
                    self.rpc_nonblocking_client.clone().expect("RPC nonblocking client not initialized"),
                    self.keypair.clone(),
                    token_in,
                ).await?;
                
                let amount = match swap_config.in_type {
                    SwapInType::Qty => {
                        ui_amount_to_amount(swap_config.amount_in, in_mint.base.decimals)
                    }
                    SwapInType::Pct => {
                        let amount_in_pct = swap_config.amount_in.min(1.0);
                        if amount_in_pct == 1.0 {
                            // Sell all. will close ATA for mint {token_in}
                            close_instruction = Some(spl_token::instruction::close_account(
                                &Pubkey::from_str(TOKEN_PROGRAM)?,
                                &in_ata,
                                &owner,
                                &owner,
                                &[&owner],
                            )?);
                            in_account.base.amount
                        } else {
                            (amount_in_pct * 100.0) as u64 * in_account.base.amount / 100
                        }
                    }
                };
                
                // Validate the amount is not zero and doesn't exceed account balance
                if amount == 0 {
                    return Err(anyhow!("Amount is zero, cannot sell"));
                }
                
                if amount > in_account.base.amount {
                    return Err(anyhow!("Sell amount exceeds account balance"));
                }
                
                (
                    amount,
                    (
                        amount_to_ui_amount(amount, in_mint.base.decimals),
                        in_mint.base.decimals,
                    ),
                )
            }
        };

        // Calculate token price from reserves
        let token_price: f64 = (quote_reserve as f64) / (base_reserve as f64);

        // Prepare swap instruction parameters based on direction
        let (base_amount, quote_amount, accounts) = match swap_config.swap_direction {
            SwapDirection::Buy => {
                // For buy: base_amount_out and max_quote_amount_in
                let base_amount_out = calculate_buy_base_amount(amount_specified, quote_reserve, base_reserve);
                let max_quote_amount_in = max_amount_with_slippage(amount_specified, slippage_bps);
                
                // Check if buy amount exceeds pool reserves
                if base_amount_out > base_reserve {
                    return Err(anyhow!("Cannot buy more base tokens than the pool reserves"));
                }
                
                // Create buy accounts vector
                (
                    base_amount_out,
                    max_quote_amount_in,
                    create_buy_accounts(
                        pool_info.pool_id,
                        owner,
                        mint,
                        sol_mint,
                        out_ata,
                        in_ata,
                        pool_info.pool_base_account,
                        pool_info.pool_quote_account,
                    )?,
                )
            }
            SwapDirection::Sell => {
                // For sell: base_amount_in and min_quote_amount_out
                let base_amount_in = amount_specified;
                let quote_amount_out = calculate_sell_quote_amount(base_amount_in, base_reserve, quote_reserve);
                let min_quote_amount_out = min_amount_with_slippage(quote_amount_out, slippage_bps);
                
                // Create sell accounts vector
                (
                    base_amount_in,
                    min_quote_amount_out,
                    create_sell_accounts(
                        pool_info.pool_id,
                        owner,
                        mint,
                        sol_mint,
                        in_ata,
                        out_ata,
                        pool_info.pool_base_account,
                        pool_info.pool_quote_account,
                    )?,
                )
            }
        };

        // Create the swap instruction
        let swap_instruction = create_swap_instruction(
            pump_program,
            discriminator,
            base_amount,
            quote_amount,
            accounts,
        );

        // Build the final transaction instructions
        let mut instructions = vec![];
        if let Some(create_instruction) = create_instruction {
            instructions.push(create_instruction);
        }
        if amount_specified > 0 {
            instructions.push(swap_instruction);
        }
        if let Some(close_instruction) = close_instruction {
            instructions.push(close_instruction);
        }
        
        // Validate instructions are not empty
        if instructions.is_empty() {
            return Err(anyhow!("Instructions is empty, no txn required."
                .red()
                .italic()
                .to_string()));
        }
        
        // Time-based expiration check
        if swap_config.swap_direction == SwapDirection::Buy
            && start_time.elapsed() > Duration::from_millis(get_expire_condition())
        {
            return Err(anyhow!("RPC connection is too busy. Expire this txn."
                .red()
                .italic()
                .to_string()));
        }

        // Return instructions with token price
        Ok((self.keypair.clone(), instructions, token_price))
    }

    pub async fn get_token_price(&self, mint_str: &str) -> Result<f64> {
        let mint = Pubkey::from_str(mint_str).map_err(|_| anyhow!("Invalid mint address"))?;
        
        // Get the pool info
        let pool_info = get_pool_info(self.rpc_client.clone().unwrap(), mint).await?;
        
        // Calculate price from reserves (quote/base)
        let price = pool_info.quote_reserve as f64 / pool_info.base_reserve as f64;
        
        Ok(price)
    }
}

/// Get the PumpSwap pool information for a specific token mint
async fn get_pool_info(
    rpc_client: Arc<anchor_client::solana_client::rpc_client::RpcClient>,
    mint: Pubkey,
) -> Result<PumpSwapPool> {
    let sol_mint = Pubkey::from_str(SOL_MINT)?;
    let pump_program = Pubkey::from_str(PUMP_PROGRAM)?;
    
    // Find the pool address for the base mint and quote mint (SOL)
    let (pool_id, _) = Pubkey::find_program_address(
        &[
            b"pool",
            &[0u8, 0u8], // index as u16 (little-endian)
            &Pubkey::default().to_bytes(), // creator (we don't know this, using default)
            &mint.to_bytes(),
            &sol_mint.to_bytes(),
        ],
        &pump_program,
    );
    
    // Find the LP mint address
    let (lp_mint, _) = Pubkey::find_program_address(
        &[
            b"pool_lp_mint",
            &pool_id.to_bytes(),
        ],
        &pump_program,
    );
    
    // Get the pool token accounts
    let pool_base_account = get_associated_token_address(&pool_id, &mint);
    let pool_quote_account = get_associated_token_address(&pool_id, &sol_mint);
    
    // Get token balances (reserves)
    let base_balance = match rpc_client.get_token_account_balance(&pool_base_account) {
        Ok(balance) => {
            match balance.ui_amount {
                Some(amount) => (amount * (10f64.powf(balance.decimals as f64))) as u64,
                None => 0,
            }
        },
        Err(_) => 0,
    };
    
    let quote_balance = match rpc_client.get_token_account_balance(&pool_quote_account) {
        Ok(balance) => {
            match balance.ui_amount {
                Some(amount) => (amount * (10f64.powf(balance.decimals as f64))) as u64,
                None => 0,
            }
        },
        Err(_) => 0,
    };
    
    // Return the pool info
    Ok(PumpSwapPool {
        pool_id,
        base_mint: mint,
        quote_mint: sol_mint,
        lp_mint,
        pool_base_account,
        pool_quote_account,
        base_reserve: base_balance,
        quote_reserve: quote_balance,
    })
}

/// Calculate the amount of base tokens received for a given quote amount in buy operation
fn calculate_buy_base_amount(quote_amount_in: u64, quote_reserve: u64, base_reserve: u64) -> u64 {
    // For buys in constant product AMM:
    // quote_reserve * base_reserve = (quote_reserve + quote_amount_in) * (base_reserve - base_amount_out)
    // Solving for base_amount_out:
    // base_amount_out = base_reserve - (quote_reserve * base_reserve) / (quote_reserve + quote_amount_in)
    
    if quote_amount_in == 0 || base_reserve == 0 || quote_reserve == 0 {
        return 0;
    }
    
    let quote_reserve_after = quote_reserve.checked_add(quote_amount_in).unwrap_or(quote_reserve);
    let numerator = (quote_reserve as u128).checked_mul(base_reserve as u128).unwrap_or(0);
    let denominator = quote_reserve_after as u128;
    
    if denominator == 0 {
        return 0;
    }
    
    let base_reserve_after = numerator.checked_div(denominator).unwrap_or(0);
    let base_amount_out = base_reserve.checked_sub(base_reserve_after as u64).unwrap_or(0);
    
    base_amount_out
}

/// Calculate the amount of quote tokens received for a given base amount in sell operation
fn calculate_sell_quote_amount(base_amount_in: u64, base_reserve: u64, quote_reserve: u64) -> u64 {
    // For sells in constant product AMM:
    // quote_reserve * base_reserve = (quote_reserve - quote_amount_out) * (base_reserve + base_amount_in)
    // Solving for quote_amount_out:
    // quote_amount_out = quote_reserve - (quote_reserve * base_reserve) / (base_reserve + base_amount_in)
    
    if base_amount_in == 0 || base_reserve == 0 || quote_reserve == 0 {
        return 0;
    }
    
    let base_reserve_after = base_reserve.checked_add(base_amount_in).unwrap_or(base_reserve);
    let numerator = (quote_reserve as u128).checked_mul(base_reserve as u128).unwrap_or(0);
    let denominator = base_reserve_after as u128;
    
    if denominator == 0 {
        return 0;
    }
    
    let quote_reserve_after = numerator.checked_div(denominator).unwrap_or(0);
    let quote_amount_out = quote_reserve.checked_sub(quote_reserve_after as u64).unwrap_or(0);
    
    quote_amount_out
}

/// Calculate the minimum amount with slippage tolerance
fn min_amount_with_slippage(input_amount: u64, slippage_bps: u64) -> u64 {
    input_amount
        .checked_mul(TEN_THOUSAND.checked_sub(slippage_bps).unwrap_or(TEN_THOUSAND))
        .unwrap_or(input_amount)
        .checked_div(TEN_THOUSAND)
        .unwrap_or(input_amount)
}

/// Calculate the maximum amount with slippage tolerance
fn max_amount_with_slippage(input_amount: u64, slippage_bps: u64) -> u64 {
    input_amount
        .checked_mul(slippage_bps.checked_add(TEN_THOUSAND).unwrap_or(TEN_THOUSAND))
        .unwrap_or(input_amount)
        .checked_div(TEN_THOUSAND)
        .unwrap_or(input_amount)
}

/// Create accounts for buy operation
fn create_buy_accounts(
    pool_id: Pubkey,
    user: Pubkey,
    base_mint: Pubkey,
    quote_mint: Pubkey,
    user_base_token_account: Pubkey,
    user_quote_token_account: Pubkey,
    pool_base_token_account: Pubkey,
    pool_quote_token_account: Pubkey,
) -> Result<Vec<AccountMeta>> {
    let global_config = Pubkey::from_str(PUMP_GLOBAL_CONFIG)?;
    let fee_recipient = Pubkey::from_str(PUMP_FEE_RECIPIENT)?;
    let fee_recipient_ata = get_associated_token_address(&fee_recipient, &quote_mint);
    let event_authority = Pubkey::from_str(PUMP_EVENT_AUTHORITY)?;
    let pump_program = Pubkey::from_str(PUMP_PROGRAM)?;
    let token_program = Pubkey::from_str(TOKEN_PROGRAM)?;
    let associated_token_program = Pubkey::from_str(ASSOCIATED_TOKEN_PROGRAM)?;
    
    Ok(vec![
        AccountMeta::new_readonly(pool_id, false),
        AccountMeta::new(user, true),
        AccountMeta::new_readonly(global_config, false),
        AccountMeta::new_readonly(base_mint, false),
        AccountMeta::new_readonly(quote_mint, false),
        AccountMeta::new(user_base_token_account, false),
        AccountMeta::new(user_quote_token_account, false),
        AccountMeta::new(pool_base_token_account, false),
        AccountMeta::new(pool_quote_token_account, false),
        AccountMeta::new_readonly(fee_recipient, false),
        AccountMeta::new(fee_recipient_ata, false),
        AccountMeta::new_readonly(token_program, false),
        AccountMeta::new_readonly(token_program, false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(associated_token_program, false),
        AccountMeta::new_readonly(event_authority, false),
        AccountMeta::new_readonly(pump_program, false),
    ])
}

/// Create accounts for sell operation
fn create_sell_accounts(
    pool_id: Pubkey,
    user: Pubkey,
    base_mint: Pubkey,
    quote_mint: Pubkey,
    user_base_token_account: Pubkey,
    user_quote_token_account: Pubkey,
    pool_base_token_account: Pubkey,
    pool_quote_token_account: Pubkey,
) -> Result<Vec<AccountMeta>> {
    let global_config = Pubkey::from_str(PUMP_GLOBAL_CONFIG)?;
    let fee_recipient = Pubkey::from_str(PUMP_FEE_RECIPIENT)?;
    let fee_recipient_ata = get_associated_token_address(&fee_recipient, &quote_mint);
    let event_authority = Pubkey::from_str(PUMP_EVENT_AUTHORITY)?;
    let pump_program = Pubkey::from_str(PUMP_PROGRAM)?;
    let token_program = Pubkey::from_str(TOKEN_PROGRAM)?;
    let associated_token_program = Pubkey::from_str(ASSOCIATED_TOKEN_PROGRAM)?;
    
    Ok(vec![
        AccountMeta::new_readonly(pool_id, false),
        AccountMeta::new(user, true),
        AccountMeta::new_readonly(global_config, false),
        AccountMeta::new_readonly(base_mint, false),
        AccountMeta::new_readonly(quote_mint, false),
        AccountMeta::new(user_base_token_account, false),
        AccountMeta::new(user_quote_token_account, false),
        AccountMeta::new(pool_base_token_account, false),
        AccountMeta::new(pool_quote_token_account, false),
        AccountMeta::new_readonly(fee_recipient, false),
        AccountMeta::new(fee_recipient_ata, false),
        AccountMeta::new_readonly(token_program, false),
        AccountMeta::new_readonly(token_program, false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(associated_token_program, false),
        AccountMeta::new_readonly(event_authority, false),
        AccountMeta::new_readonly(pump_program, false),
    ])
}

/// Create a swap instruction with the given parameters
fn create_swap_instruction(
    program_id: Pubkey,
    discriminator: [u8; 8],
    base_amount: u64,
    quote_amount: u64,
    accounts: Vec<AccountMeta>,
) -> Instruction {
    // Create the data buffer: discriminator + base_amount + quote_amount
    let mut data = Vec::with_capacity(24); // 8 + 8 + 8 bytes
    data.extend_from_slice(&discriminator);
    data.extend_from_slice(&base_amount.to_le_bytes());
    data.extend_from_slice(&quote_amount.to_le_bytes());
    
    Instruction {
        program_id,
        accounts,
        data,
    }
}

/// Get expiration time for transaction
fn get_expire_condition() -> u64 {
    env::var("EXPIRE_CONDITION")
        .ok()
        .and_then(|v| u64::from_str(&v).ok())
        .unwrap_or(10000) // Default 10 seconds
}
