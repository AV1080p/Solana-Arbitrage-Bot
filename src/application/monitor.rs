use borsh::from_slice;
use maplit::hashmap;
use anchor_client::solana_sdk::signature::Signer;
use anchor_client::solana_sdk::{hash::Hash, pubkey::Pubkey, signature::Signature};
use spl_token::solana_program::native_token::{lamports_to_sol, LAMPORTS_PER_SOL};
use tokio::process::Command;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::{collections::HashSet, time::Duration};
use base64;

use super::swap::{SwapDirection, SwapInType};
use crate::common::config::{
    JUPITER_PROGRAM,
    OKX_DEX_PROGRAM,
    LOG_INSTRUCTION,
    PUMP_SWAP_BUY_LOG_INSTRUCTION,
    PUMP_SWAP_BUY_PROGRAM_DATA_PREFIX,
    PUMP_SWAP_SELL_LOG_INSTRUCTION,
    PUMP_SWAP_SELL_PROGRAM_DATA_PREFIX,
    RAYDIUM_LAUNCHPAD_BUY_LOG_INSTRUCTION,
    RAYDIUM_LAUNCHPAD_SELL_LOG_INSTRUCTION,
    RAYDIUM_LAUNCHPAD_LOG_INSTRUCTION,
};
use crate::common::{    
    config::{AppState, LiquidityPool, Status, SwapConfig},
    logger::Logger,
};
use crate::core::tx;
use crate::dex::dex_registry::{DEXRegistry, identify_dex_from_pool};
use anyhow::{anyhow, Result};
use chrono::{Utc, Local};
use colored::Colorize;
use futures_util::stream::StreamExt;
use futures_util::{SinkExt, Sink};
use tokio::{
    sync::mpsc,
    task,
    time::{self, Instant},
};
use yellowstone_grpc_client::{ClientTlsConfig, GeyserGrpcClient};
// Import from crate::error instead
use crate::error::{ClientError, ClientResult};
use yellowstone_grpc_proto::geyser::{
    subscribe_update::UpdateOneof, CommitmentLevel, SubscribeRequest, SubscribeRequestPing,
    SubscribeRequestFilterTransactions, SubscribeUpdateTransaction, SubscribeUpdate,
};
use std::str::FromStr;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use serde_json;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstructionType {
    SwapBuy,
    SwapSell,
    ArbitrageSwap
}

#[derive(Clone, Debug)]
pub struct BondingCurveInfo {
    pub bonding_curve: Pubkey,
    pub new_virtual_sol_reserve: u64,
    pub new_virtual_token_reserve: u64,
}

#[derive(Clone, Debug)]
pub struct PoolInfo {
    pub pool_id: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub pool_base_token_account: Pubkey,
    pub pool_quote_token_account: Pubkey,
    pub base_reserve: u64,
    pub quote_reserve: u64,
}

#[derive(Clone, Debug)]
pub struct TradeInfoFromToken {
    pub instruction_type: InstructionType,
    pub slot: u64,
    pub recent_blockhash: Hash,
    pub signature: String,
    pub target: String,
    pub mint: String,
    pub pool_info: Option<PoolInfo>, // Pool information for swap operations
    pub token_amount: f64,
    pub amount: Option<u64>,
    pub base_amount_in: Option<u64>, // For sell operations
    pub min_quote_amount_out: Option<u64>, // For sell operations
    pub base_amount_out: Option<u64>, // For buy operations
    pub max_quote_amount_in: Option<u64>, // For buy operations
    // New fields for arbitrage
    pub source_dex: Option<String>,
    pub target_dex: Option<String>,
    pub price_difference: Option<f64>,
    pub expected_profit: Option<f64>,
}

pub struct FilterConfig {
    program_ids: Vec<String>,
    dex_program_ids: Vec<String>,
    arbitrage_threshold_pct: f64,
    min_liquidity: u64,
}

#[derive(Clone, Debug)]
pub struct TokenTrackingInfo {
    pub top_pnl: f64,
    pub last_price_check: Instant,
    pub price_history: Vec<(f64, Instant)>,  // Store price history with timestamps
}

#[derive(Clone, Debug)]
pub struct CopyTradeInfo {
    pub slot: u64,
    pub recent_blockhash: Hash,
    pub signature: String,
    pub target: String,
    pub mint: String,
    pub bonding_curve: String,
    pub volume_change: i64,
    pub bonding_curve_info: Option<BondingCurveInfo>,
}

lazy_static::lazy_static! {
    static ref COUNTER: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
    static ref SOLD: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
    static ref BOUGHTS: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
    static ref LAST_BUY_PAUSE_TIME: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    static ref BUYING_ENABLED: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));
    static ref TOKEN_TRACKING: Arc<Mutex<HashMap<String, TokenTrackingInfo>>> = Arc::new(Mutex::new(HashMap::new()));
    
    // Cache for THRESHOLD_BUY loaded from .env
    static ref THRESHOLD_BUY: Arc<Mutex<u64>> = Arc::new(Mutex::new(
        std::env::var("THRESHOLD_BUY")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1_000_000_000) // Default to 1 SOL if not specified
    ));
    
    // Cache for THRESHOLD_SELL loaded from .env
    static ref THRESHOLD_SELL: Arc<Mutex<u64>> = Arc::new(Mutex::new(
        std::env::var("THRESHOLD_SELL")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1_000_000_000) // Default to 1 SOL if not specified
    ));
    
    // Cache for MAX_WAIT_TIME loaded from .env
    static ref MAX_WAIT_TIME: Arc<Mutex<u64>> = Arc::new(Mutex::new(
        std::env::var("MAX_WAIT_TIME")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60000) // Default to 60 seconds if not specified
    ));
    
    // For tracking last received message time
    static ref LAST_MESSAGE_TIME: Arc<Mutex<Instant>> = Arc::new(Mutex::new(Instant::now()));
    
    // For arbitrage settings
    static ref ARBITRAGE_THRESHOLD: Arc<Mutex<f64>> = Arc::new(Mutex::new(
        std::env::var("ARBITRAGE_THRESHOLD")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(1.5) // Default to 1.5% if not specified
    ));
    
    // For minimum liquidity required for arbitrage
    static ref MIN_LIQUIDITY: Arc<Mutex<u64>> = Arc::new(Mutex::new(
        std::env::var("MIN_LIQUIDITY")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(10_000_000) // Default to 10 SOL if not specified
    ));
    
    // For tracking price differences across DEXes
    static ref PRICE_DIFFERENCES: Arc<Mutex<HashMap<String, HashMap<(String, String), f64>>>> = 
        Arc::new(Mutex::new(HashMap::new()));
}

// Add this function to update the last message time
fn update_last_message_time() {
    let mut last_time = LAST_MESSAGE_TIME.lock().unwrap();
    *last_time = Instant::now();
}

// Add this function to check connection health based on message reception
async fn check_connection_health(logger: &Logger) {
    let last_time = {
        let time = LAST_MESSAGE_TIME.lock().unwrap();
        *time
    };
    
    let now = Instant::now();
    let elapsed = now.duration_since(last_time);
    
    // If we haven't received a message in 5 minutes, log a warning
    if elapsed > Duration::from_secs(300) { // 5 minutes
        logger.log(format!(
            "[CONNECTION WARNING] => No messages received in {:?}. Connection may be stale.",
            elapsed
        ).yellow().to_string());
    }
}

impl TradeInfoFromToken {
    pub fn from_json(txn: SubscribeUpdateTransaction, log_messages: Vec<String>) -> Result<Self> {
        let slot = txn.slot;
        println!("==== BEGIN TRANSACTION PARSING ====");
        println!("Transaction slot: {}", slot);
        println!("Log messages count: {}", log_messages.len());
        
        for (i, log) in log_messages.iter().enumerate() {
            println!("LOG[{}]: {}", i, log);
        }
        
        // Print the full transaction object in detail for debugging
        println!("=== DETAILED TRANSACTION OBJECT ===");
        println!("{:#?}", txn);
        
        let mut instruction_type = InstructionType::SwapBuy;
        let mut encoded_data = String::new();
        let mut amount: Option<u64> = None;
        let mut base_amount_in: Option<u64> = None;
        let mut min_quote_amount_out: Option<u64> = None;
        let mut base_amount_out: Option<u64> = None;
        let mut max_quote_amount_in: Option<u64> = None;
        let mut source_dex: Option<String> = None;
        let mut target_dex: Option<String> = None;
        let mut price_difference: Option<f64> = None;
        let mut expected_profit: Option<f64> = None;
            
        println!("Searching for instruction type in logs...");
        
        // First detect instruction type from logs
        for log in log_messages.iter() {
            println!("Checking log: {}", log);
            
            if log.contains(PUMP_SWAP_BUY_LOG_INSTRUCTION) && log_messages.iter().any(|l| l.contains(PUMP_SWAP_BUY_PROGRAM_DATA_PREFIX)) {
                instruction_type = InstructionType::SwapBuy;
                println!("DETECTED SwapBuy instruction: {}", log);
                break;
            } else if log.contains(PUMP_SWAP_SELL_LOG_INSTRUCTION) && log_messages.iter().any(|l| l.contains(PUMP_SWAP_SELL_PROGRAM_DATA_PREFIX)) {
                instruction_type = InstructionType::SwapSell;
                println!("DETECTED SwapSell instruction: {}", log);
                break;
            } else if log.contains("Program pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA") {
                // This is a fallback check for PumpSwap program
                for other_log in log_messages.iter() {
                    if other_log.contains("BuyEvent") {
                        instruction_type = InstructionType::SwapBuy;
                        println!("DETECTED SwapBuy instruction via fallback: {}", other_log);
                        break;
                    } else if other_log.contains("SellEvent") {
                        instruction_type = InstructionType::SwapSell;
                        println!("DETECTED SwapSell instruction via fallback: {}", other_log);
                        break;
                    } else if other_log.contains("ArbitrageEvent") {
                        instruction_type = InstructionType::ArbitrageSwap;
                        println!("DETECTED ArbitrageSwap instruction via fallback: {}", other_log);
                        break;
                    }
                }
                
                if matches!(instruction_type, InstructionType::SwapBuy | InstructionType::SwapSell | InstructionType::ArbitrageSwap) {
                    break;
                }
            }
        }
        
        println!("Instruction type detected: {:?}", instruction_type);

        // Process based on instruction type
        match instruction_type {
            InstructionType::SwapBuy => {
                println!("Processing SwapBuy instruction");
                // Extract swap buy parameters
                for log in log_messages.iter() {
                    if log.contains("base_amount_out:") {
                        if let Some(value_str) = log.split("base_amount_out:").nth(1).map(|s| s.trim()) {
                            if let Ok(value) = value_str.parse::<u64>() {
                                base_amount_out = Some(value);
                                println!("Extracted base_amount_out: {}", value);
                            }
                        }
                    }
                    if log.contains("max_quote_amount_in:") {
                        if let Some(value_str) = log.split("max_quote_amount_in:").nth(1).map(|s| s.trim()) {
                            if let Ok(value) = value_str.parse::<u64>() {
                                max_quote_amount_in = Some(value);
                                println!("Extracted max_quote_amount_in: {}", value);
                            }
                        }
                    }
                }
                
                // Extract transaction data
                if let Some(transaction) = txn.transaction.clone() {
                    let signature = match Signature::try_from(transaction.signature.clone()) {
                        Ok(signature) => {
                            let sig_str = format!("{:?}", signature);
                            println!("Parsed signature: {}", sig_str);
                            sig_str
                        },
                        Err(_) => "".to_string(),
                    };
                    
                    let recent_blockhash_slice = match transaction.transaction.as_ref()
                        .and_then(|t| t.message.as_ref())
                        .map(|m| &m.recent_blockhash) {
                        Some(hash) => {
                            println!("Found blockhash");
                            hash
                        },
                        None => {
                            println!("Failed to get blockhash");
                            return Err(anyhow::anyhow!("Failed to get recent blockhash"));
                        }
                    };
                    
                    let recent_blockhash = Hash::new(recent_blockhash_slice);
                    
                    // Extract pool information
                    let pool_info = extract_pool_info_from_transaction(&transaction, &log_messages)?;
                    
                    // Extract target address
                    let target = extract_target_address_from_transaction(&transaction)?;
                    
                    // Extract token amount
                    let token_amount = if let Some(meta) = &transaction.meta {
                        meta.post_token_balances
                            .iter()
                            .filter_map(|token_balance| {
                                if token_balance.owner == target {
                                    token_balance
                                        .ui_token_amount
                                        .as_ref()
                                        .map(|ui| ui.ui_amount)
                                } else {
                                    None
                                }
                            })
                            .next()
                            .unwrap_or(0_f64)
                    } else {
                        0_f64
                    };
                    
                    // Get mint from pool info
                    let mint = if let Some(pool) = &pool_info {
                        pool.base_mint.to_string()
                    } else {
                        "".to_string()
                    };
                    
                    return Ok(Self {
                        instruction_type,
                        slot,
                        recent_blockhash,
                        signature,
                        target,
                        mint,
                        pool_info,
                        token_amount,
                        amount,
                        base_amount_in: None,
                        min_quote_amount_out: None,
                        base_amount_out,
                        max_quote_amount_in,
                        source_dex,
                        target_dex,
                        price_difference,
                        expected_profit,
                    });
                } else {
                    println!("Transaction is None, cannot proceed");
                    return Err(anyhow::anyhow!("Transaction is None"));
                }
            },
            
            InstructionType::SwapSell => {
                println!("Processing SwapSell instruction");
                // Extract swap sell parameters
                for log in log_messages.iter() {
                    if log.contains("base_amount_in:") {
                        if let Some(value_str) = log.split("base_amount_in:").nth(1).map(|s| s.trim()) {
                            if let Ok(value) = value_str.parse::<u64>() {
                                base_amount_in = Some(value);
                                println!("Extracted base_amount_in: {}", value);
                            }
                        }
                    }
                    if log.contains("min_quote_amount_out:") {
                        if let Some(value_str) = log.split("min_quote_amount_out:").nth(1).map(|s| s.trim()) {
                            if let Ok(value) = value_str.parse::<u64>() {
                                min_quote_amount_out = Some(value);
                                println!("Extracted min_quote_amount_out: {}", value);
                            }
                        }
                    }
                }
                
                // Extract transaction data
                if let Some(transaction) = txn.transaction.clone() {
                    let signature = match Signature::try_from(transaction.signature.clone()) {
                        Ok(signature) => {
                            let sig_str = format!("{:?}", signature);
                            println!("Parsed signature: {}", sig_str);
                            sig_str
                        },
                        Err(_) => "".to_string(),
                    };
                    
                    let recent_blockhash_slice = match transaction.transaction.as_ref()
                        .and_then(|t| t.message.as_ref())
                        .map(|m| &m.recent_blockhash) {
                        Some(hash) => {
                            println!("Found blockhash");
                            hash
                        },
                        None => {
                            println!("Failed to get blockhash");
                            return Err(anyhow::anyhow!("Failed to get recent blockhash"));
                        }
                    };
                    
                    let recent_blockhash = Hash::new(recent_blockhash_slice);
                    
                    // Extract pool information
                    let pool_info = extract_pool_info_from_transaction(&transaction, &log_messages)?;
                    
                    // Extract target address
                    let target = extract_target_address_from_transaction(&transaction)?;
                    
                    // Extract token amount
                    let token_amount = if let Some(meta) = &transaction.meta {
                        meta.post_token_balances
                            .iter()
                            .filter_map(|token_balance| {
                                if token_balance.owner == target {
                                    token_balance
                                        .ui_token_amount
                                        .as_ref()
                                        .map(|ui| ui.ui_amount)
                                } else {
                                    None
                                }
                            })
                            .next()
                            .unwrap_or(0_f64)
                    } else {
                        0_f64
                    };
                    
                    // Get mint from pool info
                    let mint = if let Some(pool) = &pool_info {
                        pool.base_mint.to_string()
                    } else {
                        "".to_string()
                    };
                    
                    return Ok(Self {
                        instruction_type,
                        slot,
                        recent_blockhash,
                        signature,
                        target,
                        mint,
                        pool_info,
                        token_amount,
                        amount,
                        base_amount_in,
                        min_quote_amount_out,
                        base_amount_out: None,
                        max_quote_amount_in: None,
                        source_dex,
                        target_dex,
                        price_difference,
                        expected_profit,
                    });
                } else {
                    println!("Transaction is None, cannot proceed");
                    return Err(anyhow::anyhow!("Transaction is None"));
                }
            },
            
            InstructionType::ArbitrageSwap => {
                println!("Processing ArbitrageSwap instruction");
                
                // Extract arbitrage parameters
                for log in log_messages.iter() {
                    if log.contains("source_dex:") {
                        if let Some(value_str) = log.split("source_dex:").nth(1).map(|s| s.trim()) {
                            source_dex = Some(value_str.to_string());
                            println!("Extracted source_dex: {}", value_str);
                        }
                    }
                    if log.contains("target_dex:") {
                        if let Some(value_str) = log.split("target_dex:").nth(1).map(|s| s.trim()) {
                            target_dex = Some(value_str.to_string());
                            println!("Extracted target_dex: {}", value_str);
                        }
                    }
                    if log.contains("price_difference:") {
                        if let Some(value_str) = log.split("price_difference:").nth(1).map(|s| s.trim()) {
                            if let Ok(value) = value_str.parse::<f64>() {
                                price_difference = Some(value);
                                println!("Extracted price_difference: {}", value);
                            }
                        }
                    }
                    if log.contains("expected_profit:") {
                        if let Some(value_str) = log.split("expected_profit:").nth(1).map(|s| s.trim()) {
                            if let Ok(value) = value_str.parse::<f64>() {
                                expected_profit = Some(value);
                                println!("Extracted expected_profit: {}", value);
                            }
                        }
                    }
                }
                
                // Extract transaction data
                if let Some(transaction) = txn.transaction.clone() {
                    let signature = match Signature::try_from(transaction.signature.clone()) {
                        Ok(signature) => {
                            let sig_str = format!("{:?}", signature);
                            println!("Parsed signature: {}", sig_str);
                            sig_str
                        },
                        Err(_) => "".to_string(),
                    };
                    
                    let recent_blockhash_slice = match transaction.transaction.as_ref()
                        .and_then(|t| t.message.as_ref())
                        .map(|m| &m.recent_blockhash) {
                        Some(hash) => {
                            println!("Found blockhash");
                            hash
                        },
                        None => {
                            println!("Failed to get blockhash");
                            return Err(anyhow::anyhow!("Failed to get recent blockhash"));
                        }
                    };
                    
                    let recent_blockhash = Hash::new(recent_blockhash_slice);
                    
                    // Extract target address
                    let target = extract_target_address_from_transaction(&transaction)?;
                    
                    // Extract mint from logs
                    let mut mint = String::new();
                    for log in log_messages.iter() {
                        if log.contains("token_mint:") {
                            if let Some(value_str) = log.split("token_mint:").nth(1).map(|s| s.trim()) {
                                mint = value_str.to_string();
                                println!("Extracted token_mint: {}", value_str);
                                break;
                            }
                        }
                    }
                    
                    return Ok(Self {
                        instruction_type,
                        slot,
                        recent_blockhash,
                        signature,
                        target,
                        mint,
                        pool_info: None,
                        token_amount: 0.0,
                        amount: None,
                        base_amount_in: None,
                        min_quote_amount_out: None,
                        base_amount_out: None,
                        max_quote_amount_in: None,
                        source_dex,
                        target_dex,
                        price_difference,
                        expected_profit,
                    });
                } else {
                    println!("Transaction is None, cannot proceed");
                    return Err(anyhow::anyhow!("Transaction is None"));
                }
            }
        }
        
        // If we reach here, we failed to parse the transaction
        println!("Failed to parse transaction");
        Err(anyhow::anyhow!("Failed to parse transaction"))
    }
}

/// Helper function to extract pool information from a transaction
fn extract_pool_info_from_transaction(
    transaction: &yellowstone_grpc_proto::geyser::ConfirmedTransaction,
    log_messages: &[String],
) -> Result<Option<PoolInfo>> {
    if let Some(message) = transaction.transaction.as_ref().and_then(|t| t.message.as_ref()) {
        let account_keys = &message.account_keys;
        
        // Extract pool, base_mint, and quote_mint information
        let mut pool_id = Pubkey::default();
        let mut base_mint = Pubkey::default();
        let mut quote_mint = Pubkey::default();
        let mut pool_base_token_account = Pubkey::default();
        let mut pool_quote_token_account = Pubkey::default();
        let mut base_reserve = 0u64;
        let mut quote_reserve = 0u64;
        
        // Find DEX program instructions
        for instruction in &message.instructions {
            let program_idx = instruction.program_id_index as usize;
            if let Some(program_key) = account_keys.get(program_idx) {
                if let Ok(program_key_pubkey) = Pubkey::try_from(program_key.clone()) {
                    // Check if this is a DEX program
                    let dex_registry = DEXRegistry::new();
                    if dex_registry.find_dex_by_program_id(&program_key_pubkey).is_some() {
                        // Get accounts from instruction
                        let accounts = &instruction.accounts;
                        
                        // Pool ID is typically the first account
                        if accounts.len() > 0 {
                            if let Some(pool_account_key) = account_keys.get(accounts[0] as usize) {
                                if let Ok(pubkey) = Pubkey::try_from(pool_account_key.clone()) {
                                    pool_id = pubkey;
                                    println!("Pool ID: {}", pool_id);
                                }
                            }
                        }
                        
                        // Base mint is typically the 4th account
                        if accounts.len() > 3 {
                            if let Some(base_mint_key) = account_keys.get(accounts[3] as usize) {
                                if let Ok(pubkey) = Pubkey::try_from(base_mint_key.clone()) {
                                    base_mint = pubkey;
                                    println!("Base mint: {}", base_mint);
                                }
                            }
                        }
                        
                        // Quote mint is typically the 5th account
                        if accounts.len() > 4 {
                            if let Some(quote_mint_key) = account_keys.get(accounts[4] as usize) {
                                if let Ok(pubkey) = Pubkey::try_from(quote_mint_key.clone()) {
                                    quote_mint = pubkey;
                                    println!("Quote mint: {}", quote_mint);
                                }
                            }
                        }
                        
                        // Pool token accounts are typically after that
                        if accounts.len() > 7 {
                            if let Some(pool_base_key) = account_keys.get(accounts[7] as usize) {
                                if let Ok(pubkey) = Pubkey::try_from(pool_base_key.clone()) {
                                    pool_base_token_account = pubkey;
                                    println!("Pool base token account: {}", pool_base_token_account);
                                }
                            }
                        }
                        
                        if accounts.len() > 8 {
                            if let Some(pool_quote_key) = account_keys.get(accounts[8] as usize) {
                                if let Ok(pubkey) = Pubkey::try_from(pool_quote_key.clone()) {
                                    pool_quote_token_account = pubkey;
                                    println!("Pool quote token account: {}", pool_quote_token_account);
                                }
                            }
                        }
                        
                        break;
                    }
                }
            }
        }
        
        // Extract pool reserves from logs
        for log in log_messages {
            if log.contains("pool_base_token_reserves:") {
                if let Some(value_str) = log.split("pool_base_token_reserves:").nth(1).map(|s| s.trim()) {
                    if let Ok(value) = value_str.parse::<u64>() {
                        base_reserve = value;
                        println!("Extracted pool_base_token_reserves: {}", value);
                    }
                }
            }
            if log.contains("pool_quote_token_reserves:") {
                if let Some(value_str) = log.split("pool_quote_token_reserves:").nth(1).map(|s| s.trim()) {
                    if let Ok(value) = value_str.parse::<u64>() {
                        quote_reserve = value;
                        println!("Extracted pool_quote_token_reserves: {}", value);
                    }
                }
            }
        }
        
        // Only return pool info if we have valid data
        if pool_id != Pubkey::default() && base_mint != Pubkey::default() {
            return Ok(Some(PoolInfo {
                pool_id,
                base_mint,
                quote_mint,
                pool_base_token_account,
                pool_quote_token_account,
                base_reserve,
                quote_reserve,
            }));
        }
    }
    
    Ok(None)
}

/// Helper function to extract target address from a transaction
fn extract_target_address_from_transaction(
    transaction: &yellowstone_grpc_proto::geyser::ConfirmedTransaction,
) -> Result<String> {
    if let Some(message) = transaction.transaction.as_ref().and_then(|t| t.message.as_ref()) {
        // The signer (first account) is typically the target/user
        if let Some(signer_key) = message.account_keys.first() {
            if let Ok(pubkey) = Pubkey::try_from(signer_key.clone()) {
                return Ok(pubkey.to_string());
            }
        }
    }
    
    Ok("".to_string())
}

/**
 * The following functions implement a ping-pong mechanism to keep the gRPC connection alive:
 * 
 * - process_stream_message: Handles incoming messages, responding to pings and logging pongs
 * - handle_ping_message: Sends a pong response when a ping is received
 * - send_heartbeat_ping: Proactively sends pings every 30 seconds
 * 
 * This ensures the connection stays active even during periods of inactivity,
 * preventing timeouts from the server or network infrastructure.
 */

/// Send a ping response when we receive a ping
async fn handle_ping_message(
    subscribe_tx: &Arc<tokio::sync::Mutex<impl Sink<SubscribeRequest, Error = impl std::fmt::Debug> + Unpin>>,
    logger: &Logger,
) -> Result<(), String> {
    let ping_request = SubscribeRequest {
        ping: Some(SubscribeRequestPing { id: 1 }),
        ..Default::default()
    };

    // Get a lock on the mutex
    let mut locked_tx = subscribe_tx.lock().await;
    
    // Send the ping response
    match locked_tx.send(ping_request).await {
        Ok(_) => {
            Ok(())
        },
        Err(e) => {
            Err(format!("Failed to send ping response: {:?}", e))
        }
    }
}

/// Process stream messages including ping-pong for keepalive
async fn process_stream_message(
    msg: &SubscribeUpdate,
    subscribe_tx: &Arc<tokio::sync::Mutex<impl Sink<SubscribeRequest, Error = impl std::fmt::Debug> + Unpin>>,
    logger: &Logger,
) -> Result<(), String> {
    update_last_message_time();
    match &msg.update_oneof {
        Some(UpdateOneof::Ping(_)) => {
            handle_ping_message(subscribe_tx, logger).await?;
        }
        Some(UpdateOneof::Pong(_)) => {
        }
        _ => {
        }
    }
    Ok(())
}

// Heartbeat function to periodically send pings
async fn send_heartbeat_ping(
    subscribe_tx: &Arc<tokio::sync::Mutex<impl Sink<SubscribeRequest, Error = impl std::fmt::Debug> + Unpin>>,
    logger: &Logger
) -> Result<(), String> {
    let ping_request = SubscribeRequest {
        ping: Some(SubscribeRequestPing { id: 1 }),
        ..Default::default()
    };
    
    // Get a lock on the mutex
    let mut locked_tx = subscribe_tx.lock().await;
    
    // Send the ping heartbeat
    match locked_tx.send(ping_request).await {
        Ok(_) => {
            Ok(())
        },
        Err(e) => {
            Err(format!("Failed to send heartbeat ping: {:?}", e))
        }
    }
}

/// Function to ensure record directories exist
fn ensure_record_dirs() -> Result<(), String> {
    let dirs = [
        crate::common::config::RECORD_BASE_DIR,
        crate::common::config::RECORD_PUMPFUN_DIR,
        crate::common::config::RECORD_PUMPSWAP_DIR,
        crate::common::config::RECORD_RAYDIUM_DIR,
    ];
    
    for dir in dirs.iter() {
        if !Path::new(dir).exists() {
            fs::create_dir_all(dir).map_err(|e| format!("Failed to create directory {}: {}", dir, e))?;
        }
    }
    
    Ok(())
}

/// Save transaction data to a file
fn save_transaction_record(protocol: &str, signature: &str, data: &str, extension: &str) -> Result<(), String> {
    let base_dir = match protocol {
        "pumpfun" => crate::common::config::RECORD_PUMPFUN_DIR,
        "pumpswap" => crate::common::config::RECORD_PUMPSWAP_DIR,
        "raydium" => crate::common::config::RECORD_RAYDIUM_DIR,
        _ => crate::common::config::RECORD_BASE_DIR,
    };
    
    let timestamp = Utc::now().format("%Y%m%d%H%M%S");
    let filename = format!("{}/{}_{}.{}", base_dir, signature, timestamp, extension);
    
    let mut file = File::create(&filename)
        .map_err(|e| format!("Failed to create file {}: {}", filename, e))?;
    
    file.write_all(data.as_bytes())
        .map_err(|e| format!("Failed to write to file {}: {}", filename, e))?;
    
    Ok(())
}

/// Determine protocol from transaction logs
fn determine_protocol(log_messages: &[String]) -> Option<&'static str> {
    use crate::common::config::*;
    
    for log in log_messages {
        // Check for PumpSwap
        if log.contains(PUMP_SWAP_BUY_LOG_INSTRUCTION) || 
           log.contains(PUMP_SWAP_SELL_LOG_INSTRUCTION) || 
           log.contains("Program pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA") {
            return Some("pumpswap");
        }
        
        // Check for Raydium
        if log.contains(RAYDIUM_LAUNCHPAD_BUY_LOG_INSTRUCTION) || 
           log.contains(RAYDIUM_LAUNCHPAD_SELL_LOG_INSTRUCTION) || 
           log.contains(RAYDIUM_LAUNCHPAD_LOG_INSTRUCTION) ||
           log.contains("Program 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8") {
            return Some("raydium");
        }
        
        // Check for Orca/Whirlpool
        if log.contains("Program whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc") {
            return Some("whirlpool");
        }
        
        // Check for Meteora
        if log.contains("Program M2mx93ekt1fmXSVkTrUL9xVFHkmME8HTUi5Cyc5aF7K") {
            return Some("meteora");
        }
    }
    
    None
}

/// Extract transaction type from logs
fn extract_transaction_type(log_messages: &[String]) -> &'static str {
    for log in log_messages {
        if log.contains("buy") || log.contains("Buy") || log.contains("BUY") {
            return "buy";
        }
        
        if log.contains("sell") || log.contains("Sell") || log.contains("SELL") {
            return "sell";
        }
        
        if log.contains("swap") || log.contains("Swap") || log.contains("SWAP") {
            return "swap";
        }
        
        if log.contains("mint") || log.contains("Mint") || log.contains("MINT") {
            return "mint";
        }
        
        if log.contains("arbitrage") || log.contains("Arbitrage") || log.contains("ARBITRAGE") {
            return "arbitrage";
        }
    }
    
    "unknown"
}

pub async fn new_token_trader_pumpfun(
    yellowstone_grpc_http: String,
    yellowstone_grpc_token: String,
    app_state: AppState,
    swap_config: SwapConfig,
    time_exceed: u64,
    counter_limit: u64,
    min_dev_buy: u64,
    max_dev_buy: u64,
) -> Result<(), String> {
    // Log the copy trading configuration
    let logger = Logger::new("[PUMPFUN-MONITOR] => ".blue().bold().to_string());

    // INITIAL SETTING FOR SUBSCIBE
    // -----------------------------------------------------------------------------------------------------------------------------
    let mut client = GeyserGrpcClient::build_from_shared(yellowstone_grpc_http.clone())
        .map_err(|e| format!("Failed to build client: {}", e))?
        .x_token::<String>(Some(yellowstone_grpc_token.clone()))
        .map_err(|e| format!("Failed to set x_token: {}", e))?
        .tls_config(ClientTlsConfig::new().with_native_roots())
        .map_err(|e| format!("Failed to set tls config: {}", e))?
        .connect()
        .await
        .map_err(|e| format!("Failed to connect: {}", e))?;

    // Create additional clones for later use in tasks
    let yellowstone_grpc_http = Arc::new(yellowstone_grpc_http);
    let yellowstone_grpc_token = Arc::new(yellowstone_grpc_token);
    let app_state = Arc::new(app_state);
    let swap_config = Arc::new(swap_config);

    // Log the copy trading configuration
    let logger = Logger::new("[PUMPFUN-MONITOR] => ".blue().bold().to_string());

    let mut retry_count = 0;
    const MAX_RETRIES: u32 = 3;
    let (subscribe_tx, mut stream) = loop {
        match client.subscribe().await {
            Ok(pair) => break pair,
            Err(e) => {
                retry_count += 1;
                if retry_count >= MAX_RETRIES {
                    return Err(format!("Failed to subscribe after {} attempts: {}", MAX_RETRIES, e));
                }
                logger.log(format!(
                    "[CONNECTION ERROR] => Failed to subscribe (attempt {}/{}): {}. Retrying in 5 seconds...",
                    retry_count, MAX_RETRIES, e
                ).red().to_string());
                time::sleep(Duration::from_secs(5)).await;
            }
        }
    };

    // Convert to Arc to allow cloning across tasks
    let subscribe_tx = Arc::new(tokio::sync::Mutex::new(subscribe_tx));

    // Get copy trading configuration from environment
    let copy_trading_target_address = std::env::var("COPY_TRADING_TARGET_ADDRESS").ok();
    let is_multi_copy_trading = std::env::var("IS_MULTI_COPY_TRADING")
        .ok()
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(false);
    
    // Prepare program IDs for monitoring - include all protocols
    let mut program_ids = vec![
        PUMP_PROGRAM.to_string(),                    // PumpFun
        "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA".to_string(), // PumpSwap
        "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(), // Raydium
    ];
    
    // Initialize the vector for copy trading target addresses
    let mut copy_trading_target_addresses: Vec<String> = Vec::new();
    // Handle multiple copy trading targets if enabled
    if is_multi_copy_trading {
        if let Some(address_str) = copy_trading_target_address {
            // Parse comma-separated addresses
            for addr in address_str.split(',') {
                let trimmed_addr = addr.trim();
                if !trimmed_addr.is_empty() {
                    program_ids.push(trimmed_addr.to_string());
                    copy_trading_target_addresses.push(trimmed_addr.to_string());
                }
            }
        }
    } else if let Some(address) = copy_trading_target_address {
        // Single address mode
        if !address.is_empty() {
            program_ids.push(address.clone());
            copy_trading_target_addresses.push(address);
        }
    }

    let filter_config = FilterConfig {
        program_ids: program_ids.clone(),
        dex_program_ids: vec![],
        arbitrage_threshold_pct: 0.0,
        min_liquidity: 0,
    };

    // Log the copy trading configuration
    if !filter_config.copy_trading_target_addresses.is_empty() {
        logger.log(format!(
            "[COPY TRADING] => Monitoring {} address(es)",
            filter_config.copy_trading_target_addresses.len(),
        ).green().to_string());
        
        for (i, addr) in filter_config.copy_trading_target_addresses.iter().enumerate() {
            logger.log(format!(
                "\t * [TARGET {}] => {}",
                i + 1, addr
            ).green().to_string());
        }
    }

    subscribe_tx
        .lock()
        .await
        .send(SubscribeRequest {
            slots: HashMap::new(),
            accounts: HashMap::new(),
            transactions: hashmap! {
                "All".to_owned() => SubscribeRequestFilterTransactions {
                    vote: None,
                    failed: Some(false),
                    signature: None,
                    account_include: vec![
                        PUMP_PROGRAM.to_string(),                      // PumpFun
                        "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA".to_string(), // PumpSwap
                        "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(), // Raydium
                    ],
                    account_exclude: vec![JUPITER_PROGRAM.to_string(), OKX_DEX_PROGRAM.to_string()],
                    account_required: Vec::<String>::new()
                }
            },
            transactions_status: HashMap::new(),
            entry: HashMap::new(),
            blocks: HashMap::new(),
            blocks_meta: HashMap::new(),
            commitment: Some(CommitmentLevel::Processed as i32),
            accounts_data_slice: vec![],
            ping: None,
            from_slot: None,
        })
        .await
        .map_err(|e| format!("Failed to send subscribe request: {}", e))?;

    let existing_liquidity_pools = Arc::new(Mutex::new(HashSet::<LiquidityPool>::new()));

    let rpc_nonblocking_client = app_state.clone().rpc_nonblocking_client.clone();
    let rpc_client = app_state.clone().rpc_client.clone();
    let wallet = app_state.clone().wallet.clone();
    let swapx = Pump::new(
        rpc_nonblocking_client.clone(),
        rpc_client.clone(),
        wallet.clone(),
    );

    logger.log("[STARTED. MONITORING]...".blue().bold().to_string());
    
    // Set buying enabled to true at start
    {
        let mut buying_enabled = BUYING_ENABLED.lock().unwrap();
        *buying_enabled = true;
    }

    // After all setup and before the main loop, add a heartbeat ping task
    let subscribe_tx_clone = subscribe_tx.clone();
    let logger_clone = logger.clone();
    
    tokio::spawn(async move {
        let ping_logger = logger_clone.clone();
        let mut interval = time::interval(Duration::from_secs(30));
        
        loop {
            interval.tick().await;
            
            if let Err(e) = send_heartbeat_ping(&subscribe_tx_clone, &ping_logger).await {
                ping_logger.log(format!("[CONNECTION ERROR] => {}", e).red().to_string());
                break;
            }
        }
    });

    // Start a background task to check the status of tokens periodically
    let existing_liquidity_pools_clone = Arc::clone(&existing_liquidity_pools);
    let logger_clone = logger.clone();
    let app_state_for_background = Arc::clone(&app_state);
    let swap_config_for_background = Arc::clone(&swap_config);
    
    tokio::spawn(async move {
        let pools_clone = Arc::clone(&existing_liquidity_pools_clone);
        let check_logger = logger_clone.clone();
        let app_state_clone = Arc::clone(&app_state_for_background);
        let swap_config_clone = Arc::clone(&swap_config_for_background);
        
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            
            // Check if there are any bought tokens and if any have exceeded MAX_WAIT_TIME
            let now = Instant::now();
            let max_wait_time_millis = *MAX_WAIT_TIME.lock().unwrap();
            let max_wait_duration = Duration::from_millis(max_wait_time_millis);
            
            let (has_bought_tokens, tokens_to_sell) = {
                let pools = pools_clone.lock().unwrap();
                let bought_tokens: Vec<String> = pools.iter()
                    .filter(|pool| pool.status == Status::Bought)
                    .map(|pool| pool.mint.clone())
                    .collect();
                
                let timed_out_tokens: Vec<(String, Instant)> = pools.iter()
                    .filter(|pool| pool.status == Status::Bought && 
                           pool.timestamp.map_or(false, |ts| now.duration_since(ts) > max_wait_duration))
                    .map(|pool| (pool.mint.clone(), pool.timestamp.unwrap()))
                    .collect();
                
                // Log bought tokens that are waiting to be sold
                if !bought_tokens.is_empty() {
                    check_logger.log(format!(
                        "\n\t * [BUYING PAUSED] => Waiting for tokens to be sold: {:?}",
                        bought_tokens
                    ).yellow().to_string());
                }
                
                // Log tokens that have timed out and will be force-sold
                if !timed_out_tokens.is_empty() {
                    check_logger.log(format!(
                        "\n\t * [TIMEOUT DETECTED] => Will force-sell tokens that exceeded {} ms wait time: {:?}",
                        max_wait_time_millis,
                        timed_out_tokens.iter().map(|(mint, _)| mint).collect::<Vec<_>>()
                    ).red().bold().to_string());
                }
                
                (bought_tokens.len() > 0, timed_out_tokens)
            };
            
            // Update buying status
            {
                let mut buying_enabled = BUYING_ENABLED.lock().unwrap();
                *buying_enabled = !has_bought_tokens;
            }
            
            // Force-sell tokens that have exceeded MAX_WAIT_TIME
            for (mint, timestamp) in tokens_to_sell {
                // Clone the necessary state for this token
                let logger_for_selling = check_logger.clone();
                let pools_clone_for_selling = Arc::clone(&pools_clone);
                let app_state_for_selling = app_state_clone.clone();
                let swap_config_for_selling = swap_config_clone.clone();
                
                check_logger.log(format!(
                    "\n\t * [FORCE SELLING] => Token {} exceeded wait time (elapsed: {:?})",
                    mint, now.duration_since(timestamp)
                ).red().to_string());
                
                tokio::spawn(async move {
                    // Get the existing pool for this mint
                    let existing_pool = {
                        let pools = pools_clone_for_selling.lock().unwrap();
                        pools.iter()
                            .find(|pool| pool.mint == mint)
                            .cloned()
                            .unwrap_or(LiquidityPool {
                                mint: mint.clone(),
                                buy_price: 0_f64,
                                sell_price: 0_f64,
                                status: Status::Bought,
                                timestamp: Some(timestamp),
                            })
                    };
                    
                    // Set up sell config
                    let sell_config = SwapConfig {
                        swap_direction: SwapDirection::Sell,
                        in_type: SwapInType::Pct,
                        amount_in: 1_f64,  // Sell 100%
                        slippage: 100_u64, // Use full slippage
                        use_jito: swap_config_for_selling.clone().use_jito,
                    };
                    
                    // Create Pump instance for selling
                    let app_state_for_task = app_state_for_selling.clone();
                    let rpc_nonblocking_client = app_state_for_task.rpc_nonblocking_client.clone();
                    let rpc_client = app_state_for_task.rpc_client.clone();
                    let wallet = app_state_for_task.wallet.clone();
                    let swapx = Pump::new(rpc_nonblocking_client.clone(), rpc_client.clone(), wallet.clone());
                    
                    // Execute the sell operation
                    let start_time = Instant::now();
                    match swapx.build_swap_ixn_by_mint(&mint, None, sell_config, start_time).await {
                        Ok(result) => {
                            // Send instructions and confirm
                            let (keypair, instructions, token_price) = (result.0, result.1, result.2);
                            let recent_blockhash = match rpc_nonblocking_client.get_latest_blockhash().await {
                                Ok(hash) => hash,
                                Err(e) => {
                                    logger_for_selling.log(format!(
                                        "Error getting blockhash for force-selling {}: {}", mint, e
                                    ).red().to_string());
                                    return;
                                }
                            };
                            
                            match tx::new_signed_and_send_zeroslot(
                                recent_blockhash,
                                &keypair,
                                instructions,
                                &logger_for_selling,
                            ).await {
                                Ok(res) => {
                                    let sold_pool = LiquidityPool {
                                        mint: mint.clone(),
                                        buy_price: existing_pool.buy_price,
                                        sell_price: token_price,
                                        status: Status::Sold,
                                        timestamp: Some(Instant::now()),
                                    };
                                    
                                    // Update pool status to sold
                                    {
                                        let mut pools = pools_clone_for_selling.lock().unwrap();
                                        pools.retain(|pool| pool.mint != mint);
                                        pools.insert(sold_pool.clone());
                                    }
                                    
                                    logger_for_selling.log(format!(
                                        "\n\t * [SUCCESSFUL FORCE-SELL] => TX_HASH: (https://solscan.io/tx/{}) \n\t * [POOL] => ({}) \n\t * [SOLD] => {} :: ({:?}).",
                                        &res[0], mint, Utc::now(), start_time.elapsed()
                                    ).green().to_string());
                                    
                                    // Check if all tokens are sold
                                    let all_sold = {
                                        let pools = pools_clone_for_selling.lock().unwrap();
                                        !pools.iter().any(|pool| pool.status == Status::Bought)
                                    };
                                    
                                    if all_sold {
                                        // If all tokens are sold, enable buying
                                        let mut buying_enabled = BUYING_ENABLED.lock().unwrap();
                                        *buying_enabled = true;
                                        
                                        logger_for_selling.log(
                                            "\n\t * [BUYING ENABLED] => All tokens sold, can buy new tokens now"
                                            .green()
                                            .to_string(),
                                        );
                                    }
                                },
                                Err(e) => {
                                    logger_for_selling.log(format!(
                                        "Force-sell failed for {}: {}", mint, e
                                    ).red().to_string());
                                }
                            }
                        },
                        Err(e) => {
                            logger_for_selling.log(format!(
                                "Error building swap instruction for force-selling {}: {}", mint, e
                            ).red().to_string());
                        }
                    }
                });
            }
        }
    });

    // In new_token_trader_pumpfun after the heartbeat task
    // Add a connection health check task
    let logger_health = logger.clone(); 
    tokio::spawn(async move {
        let health_logger = logger_health.clone();
        let mut interval = time::interval(Duration::from_secs(300)); // 5 minutes
        
        loop {
            interval.tick().await;
            health_logger.log("[CONNECTION HEALTH] => gRPC subscription still active".green().to_string());
        }
    });

    // In new_token_trader_pumpfun after the health check task:
    // Add a connection watchdog task
    let logger_watchdog = logger.clone();
    tokio::spawn(async move {
        let watchdog_logger = logger_watchdog;
        let mut interval = time::interval(Duration::from_secs(120)); // Check every 2 minutes
        
        loop {
            interval.tick().await;
            check_connection_health(&watchdog_logger).await;
        }
    });

    // Ensure record directories exist
    ensure_record_dirs()?;

    while let Some(message) = stream.next().await {
        match message {
            Ok(msg) => {
                // Process ping/pong messages
                if let Err(e) = process_stream_message(&msg, &subscribe_tx, &logger).await {
                    logger.log(format!("Error handling stream message: {}", e).red().to_string());
                    continue;
                }
                
                // Process transaction messages
                if let Some(UpdateOneof::Transaction(txn)) = msg.update_oneof {
                    let start_time = Instant::now();
                    if let Some(log_messages) = txn
                        .clone()
                        .transaction
                        .and_then(|txn1| txn1.meta)
                        .map(|meta| meta.log_messages)
                    {
                        // Determine protocol and transaction type
                        let protocol = determine_protocol(&log_messages);
                        let tx_type = extract_transaction_type(&log_messages);
                        
                        // Get transaction signature
                        let signature = txn.transaction
                            .as_ref()
                            .and_then(|tx| tx.signature.first())
                            .map(|sig| bs58::encode(&[*sig]).into_string())
                            .unwrap_or_else(|| "unknown".to_string());
                        
                        // Save transaction data if protocol is recognized
                        if let Some(protocol_name) = protocol {
                            // Create a simplified JSON representation since SubscribeUpdateTransaction doesn't implement Serialize
                            let json_data = format!(
                                "{{\"signature\":\"{}\",\"slot\":{},\"transaction_type\":\"{}\",\"protocol\":\"{}\"}}",
                                signature,
                                txn.slot,
                                tx_type,
                                protocol_name
                            );
                            
                            if let Err(e) = save_transaction_record(
                                protocol_name, 
                                &signature, 
                                &json_data, 
                                "json"
                            ) {
                                logger.log(format!("Failed to save transaction JSON: {}", e).red().to_string());
                            }
                            
                            // Save logs
                            let logs_text = log_messages.join("\n");
                            if let Err(e) = save_transaction_record(
                                protocol_name, 
                                &signature, 
                                &logs_text, 
                                "log"
                            ) {
                                logger.log(format!("Failed to save transaction logs: {}", e).red().to_string());
                            }
                            
                            // Log the transaction
                            logger.log(format!(
                                "\n\t * [RECORDED TRANSACTION] => Protocol: {}, Type: {}, Signature: {}",
                                protocol_name.to_uppercase(),
                                tx_type.to_uppercase(),
                                signature
                            ).green().to_string());
                        }
                        
                        // Continue with existing processing
                        // ... rest of your transaction processing code ...
                    }
                }
            }
            Err(error) => {
                logger.log(
                    format!("Yellowstone gRpc Error: {:?}", error)
                        .red()
                        .to_string(),
                );
                break;
            }
        }
    }
    Ok(())
}

pub async fn copy_trader_pumpfun(
    yellowstone_grpc_http: String,
    yellowstone_grpc_token: String,
    app_state: AppState,
    swap_config: SwapConfig,
    time_exceed: u64,
    counter_limit: u64,
    min_dev_buy: u64,
    max_dev_buy: u64,
) -> Result<(), String> {
    // Log the copy trading configuration
    let logger = Logger::new("[COPY-TRADER] => ".blue().bold().to_string());
    
    // INITIAL SETTING FOR SUBSCRIBE
    // -----------------------------------------------------------------------------------------------------------------------------
    let mut client = GeyserGrpcClient::build_from_shared(yellowstone_grpc_http.clone())
        .map_err(|e| format!("Failed to build client: {}", e))?
        .x_token::<String>(Some(yellowstone_grpc_token.clone()))
        .map_err(|e| format!("Failed to set x_token: {}", e))?
        .tls_config(ClientTlsConfig::new().with_native_roots())
        .map_err(|e| format!("Failed to set tls config: {}", e))?
        .connect()
        .await
        .map_err(|e| format!("Failed to connect: {}", e))?;

    // Create additional clones for later use in tasks
    let yellowstone_grpc_http = Arc::new(yellowstone_grpc_http);
    let yellowstone_grpc_token = Arc::new(yellowstone_grpc_token);
    let app_state = Arc::new(app_state);
    let swap_config = Arc::new(swap_config);

    // Log the copy trading configuration
    let logger = Logger::new("[COPY-TRADER] => ".blue().bold().to_string());

    let mut retry_count = 0;
    const MAX_RETRIES: u32 = 3;
    let (subscribe_tx, mut stream) = loop {
        match client.subscribe().await {
            Ok(pair) => break pair,
            Err(e) => {
                retry_count += 1;
                if retry_count >= MAX_RETRIES {
                    return Err(format!("Failed to subscribe after {} attempts: {}", MAX_RETRIES, e));
                }
                logger.log(format!(
                    "[CONNECTION ERROR] => Failed to subscribe (attempt {}/{}): {}. Retrying in 5 seconds...",
                    retry_count, MAX_RETRIES, e
                ).red().to_string());
                time::sleep(Duration::from_secs(5)).await;
            }
        }
    };

    // Convert to Arc to allow cloning across tasks
    let subscribe_tx = Arc::new(tokio::sync::Mutex::new(subscribe_tx));

    // Get copy trading configuration from environment
    let copy_trading_target_address = std::env::var("COPY_TRADING_TARGET_ADDRESS").ok();
    let is_multi_copy_trading = std::env::var("IS_MULTI_COPY_TRADING")
        .ok()
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(false);
    
    // Prepare target addresses for monitoring
    let mut program_ids = vec![];
    let mut copy_trading_target_addresses: Vec<String> = Vec::new();
    
    // Handle multiple copy trading targets if enabled
    if is_multi_copy_trading {
        if let Some(address_str) = copy_trading_target_address {
            // Parse comma-separated addresses
            for addr in address_str.split(',') {
                let trimmed_addr = addr.trim();
                if !trimmed_addr.is_empty() {
                    program_ids.push(trimmed_addr.to_string());
                    copy_trading_target_addresses.push(trimmed_addr.to_string());
                }
            }
        }
    } else if let Some(address) = copy_trading_target_address {
        // Single address mode
        if !address.is_empty() {
            program_ids.push(address.clone());
            copy_trading_target_addresses.push(address);
        }
    }

    // Ensure we have at least one target address
    if copy_trading_target_addresses.is_empty() {
        return Err("No COPY_TRADING_TARGET_ADDRESS specified. Please set this environment variable.".to_string());
    }

    let filter_config = FilterConfig {
        program_ids: program_ids.clone(),
        dex_program_ids: vec![],
        arbitrage_threshold_pct: 0.0,
        min_liquidity: 0,
    };

    // Log the copy trading configuration starts here
    logger.log(format!(
        "[COPY TRADING] => Monitoring {} address(es)",
        filter_config.copy_trading_target_addresses.len()
    ).green().to_string());
    
    for (i, addr) in filter_config.copy_trading_target_addresses.iter().enumerate() {
        logger.log(format!(
            "\t * [TARGET {}] => {}",
            i + 1, addr
        ).green().to_string());
    }

    subscribe_tx
        .lock()
        .await
        .send(SubscribeRequest {
            slots: HashMap::new(),
            accounts: HashMap::new(),
            transactions: hashmap! {
                "All".to_owned() => SubscribeRequestFilterTransactions {
                    vote: None,
                    failed: Some(false),
                    signature: None,
                    account_include: vec![
                        PUMP_PROGRAM.to_string(),                      // PumpFun
                        "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA".to_string(), // PumpSwap
                        "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(), // Raydium
                    ],
                    account_exclude: vec![JUPITER_PROGRAM.to_string(), OKX_DEX_PROGRAM.to_string()],
                    account_required: Vec::<String>::new()
                }
            },
            transactions_status: HashMap::new(),
            entry: HashMap::new(),
            blocks: HashMap::new(),
            blocks_meta: HashMap::new(),
            commitment: Some(CommitmentLevel::Processed as i32),
            accounts_data_slice: vec![],
            ping: None,
            from_slot: None,
        })
        .await
        .map_err(|e| format!("Failed to send subscribe request: {}", e))?;

    let existing_liquidity_pools = Arc::new(Mutex::new(HashSet::<LiquidityPool>::new()));

    let rpc_nonblocking_client = app_state.clone().rpc_nonblocking_client.clone();
    let rpc_client = app_state.clone().rpc_client.clone();
    let wallet = app_state.clone().wallet.clone();
    let swapx = Pump::new(
        rpc_nonblocking_client.clone(),
        rpc_client.clone(),
        wallet.clone(),
    );

    logger.log("[STARTED. MONITORING COPY TARGETS]...".blue().bold().to_string());
    
    // Set buying enabled to true at start
    {
        let mut buying_enabled = BUYING_ENABLED.lock().unwrap();
        *buying_enabled = true;
    }

    // After all setup and before the main loop, add a heartbeat ping task
    let subscribe_tx_clone = subscribe_tx.clone();
    let logger_clone = logger.clone();
    
    tokio::spawn(async move {
        let ping_logger = logger_clone.clone();
        let mut interval = time::interval(Duration::from_secs(30));
        
        loop {
            interval.tick().await;
            
            if let Err(e) = send_heartbeat_ping(&subscribe_tx_clone, &ping_logger).await {
                ping_logger.log(format!("[CONNECTION ERROR] => {}", e).red().to_string());
                break;
            }
        }
    });

    // Start a background task to check the status of tokens periodically
    let existing_liquidity_pools_clone = Arc::clone(&existing_liquidity_pools);
    let logger_clone = logger.clone();
    let app_state_for_background = Arc::clone(&app_state);
    let swap_config_for_background = Arc::clone(&swap_config);
    
    tokio::spawn(async move {
        let pools_clone = Arc::clone(&existing_liquidity_pools_clone);
        let check_logger = logger_clone.clone();
        let app_state_clone = Arc::clone(&app_state_for_background);
        let swap_config_clone = Arc::clone(&swap_config_for_background);
        
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            
            // Check if there are any bought tokens and if any have exceeded MAX_WAIT_TIME
            let now = Instant::now();
            let max_wait_time_millis = *MAX_WAIT_TIME.lock().unwrap();
            let max_wait_duration = Duration::from_millis(max_wait_time_millis);
            
            let (has_bought_tokens, tokens_to_sell) = {
                let pools = pools_clone.lock().unwrap();
                let bought_tokens: Vec<String> = pools.iter()
                    .filter(|pool| pool.status == Status::Bought)
                    .map(|pool| pool.mint.clone())
                    .collect();
                
                let timed_out_tokens: Vec<(String, Instant)> = pools.iter()
                    .filter(|pool| pool.status == Status::Bought && 
                           pool.timestamp.map_or(false, |ts| now.duration_since(ts) > max_wait_duration))
                    .map(|pool| (pool.mint.clone(), pool.timestamp.unwrap()))
                    .collect();
                
                // Log bought tokens that are waiting to be sold
                if !bought_tokens.is_empty() {
                    check_logger.log(format!(
                        "\n\t * [BUYING PAUSED] => Waiting for tokens to be sold: {:?}",
                        bought_tokens
                    ).yellow().to_string());
                }
                
                // Log tokens that have timed out and will be force-sold
                if !timed_out_tokens.is_empty() {
                    check_logger.log(format!(
                        "\n\t * [TIMEOUT DETECTED] => Will force-sell tokens that exceeded {} ms wait time: {:?}",
                        max_wait_time_millis,
                        timed_out_tokens.iter().map(|(mint, _)| mint).collect::<Vec<_>>()
                    ).red().bold().to_string());
                }
                
                (bought_tokens.len() > 0, timed_out_tokens)
            };
            
            // Update buying status
            {
                let mut buying_enabled = BUYING_ENABLED.lock().unwrap();
                *buying_enabled = !has_bought_tokens;
                
            }
            
            // Force-sell tokens that have exceeded MAX_WAIT_TIME
            for (mint, timestamp) in tokens_to_sell {
                // Clone the necessary state for this token
                let logger_for_selling = check_logger.clone();
                let pools_clone_for_selling = Arc::clone(&pools_clone);
                let app_state_for_selling = app_state_clone.clone();
                let swap_config_for_selling = swap_config_clone.clone();
                
                check_logger.log(format!(
                    "\n\t * [FORCE SELLING] => Token {} exceeded wait time (elapsed: {:?})",
                    mint, now.duration_since(timestamp)
                ).red().to_string());
                
                tokio::spawn(async move {
                    // Get the existing pool for this mint
                    let existing_pool = {
                        let pools = pools_clone_for_selling.lock().unwrap();
                        pools.iter()
                            .find(|pool| pool.mint == mint)
                            .cloned()
                            .unwrap_or(LiquidityPool {
                                mint: mint.clone(),
                                buy_price: 0_f64,
                                sell_price: 0_f64,
                                status: Status::Bought,
                                timestamp: Some(timestamp),
                            })
                    };
                    
                    // Set up sell config
                    let sell_config = SwapConfig {
                        swap_direction: SwapDirection::Sell,
                        in_type: SwapInType::Pct,
                        amount_in: 1_f64,  // Sell 100%
                        slippage: 100_u64, // Use full slippage
                        use_jito: swap_config_for_selling.clone().use_jito,
                    };
                    
                    // Create Pump instance for selling
                    let app_state_for_task = app_state_for_selling.clone();
                    let rpc_nonblocking_client = app_state_for_task.rpc_nonblocking_client.clone();
                    let rpc_client = app_state_for_task.rpc_client.clone();
                    let wallet = app_state_for_task.wallet.clone();
                    let swapx = Pump::new(rpc_nonblocking_client.clone(), rpc_client.clone(), wallet.clone());
                    
                    // Execute the sell operation
                    let start_time = Instant::now();
                    match swapx.build_swap_ixn_by_mint(&mint, None, sell_config, start_time).await {
                        Ok(result) => {
                            // Send instructions and confirm
                            let (keypair, instructions, token_price) = (result.0, result.1, result.2);
                            let recent_blockhash = match rpc_nonblocking_client.get_latest_blockhash().await {
                                Ok(hash) => hash,
                                Err(e) => {
                                    logger_for_selling.log(format!(
                                        "Error getting blockhash for force-selling {}: {}", mint, e
                                    ).red().to_string());
                                    return;
                                }
                            };
                            
                            match tx::new_signed_and_send_zeroslot(
                                recent_blockhash,
                                &keypair,
                                instructions,
                                &logger_for_selling,
                            ).await {
                                Ok(res) => {
                                    let sold_pool = LiquidityPool {
                                        mint: mint.clone(),
                                        buy_price: existing_pool.buy_price,
                                        sell_price: token_price,
                                        status: Status::Sold,
                                        timestamp: Some(Instant::now()),
                                    };
                                    
                                    // Update pool status to sold
                                    {
                                        let mut pools = pools_clone_for_selling.lock().unwrap();
                                        pools.retain(|pool| pool.mint != mint);
                                        pools.insert(sold_pool.clone());
                                    }
                                    
                                    logger_for_selling.log(format!(
                                        "\n\t * [SUCCESSFUL FORCE-SELL] => TX_HASH: (https://solscan.io/tx/{}) \n\t * [POOL] => ({}) \n\t * [SOLD] => {} :: ({:?}).",
                                        &res[0], mint, Utc::now(), start_time.elapsed()
                                    ).green().to_string());
                                    
                                    // Check if all tokens are sold
                                    let all_sold = {
                                        let pools = pools_clone_for_selling.lock().unwrap();
                                        !pools.iter().any(|pool| pool.status == Status::Bought)
                                    };
                                    
                                    if all_sold {
                                        // If all tokens are sold, enable buying
                                        let mut buying_enabled = BUYING_ENABLED.lock().unwrap();
                                        *buying_enabled = true;
                                        
                                        logger_for_selling.log(
                                            "\n\t * [BUYING ENABLED] => All tokens sold, can buy new tokens now"
                                            .green()
                                            .to_string(),
                                        );
                                    }
                                },
                                Err(e) => {
                                    logger_for_selling.log(format!(
                                        "Force-sell failed for {}: {}", mint, e
                                    ).red().to_string());
                                }
                            }
                        },
                        Err(e) => {
                            logger_for_selling.log(format!(
                                "Error building swap instruction for force-selling {}: {}", mint, e
                            ).red().to_string());
                        }
                    }
                });
            }
        }
    });

    // In copy_trader_pumpfun after the heartbeat task
    // Add a connection health check task
    let logger_health = logger.clone();
    tokio::spawn(async move {
        let health_logger = logger_health.clone();
        let mut interval = time::interval(Duration::from_secs(300)); // 5 minutes
        loop {
            interval.tick().await;
        }
    });

    // In copy_trader_pumpfun after the health check task:
    // Add a connection watchdog task
    let logger_watchdog = logger.clone();
    tokio::spawn(async move {
        let watchdog_logger = logger_watchdog;
        let mut interval = time::interval(Duration::from_secs(120)); // Check every 2 minutes
        
        loop {
            interval.tick().await;
            check_connection_health(&watchdog_logger).await;
        }
    });

    // In copy_trader_pumpfun after setting up the initial subscription and before the main event loop
    // Replace the PNL monitoring and auto-sell task with a pure price monitoring task
    let price_monitoring_pools_clone = Arc::clone(&existing_liquidity_pools);
    let price_monitoring_logger_clone = logger.clone();
    let price_monitoring_app_state_clone = Arc::clone(&app_state);
    let price_monitoring_token_tracking = Arc::clone(&TOKEN_TRACKING);

    tokio::spawn(async move {
        let pools_clone = Arc::clone(&price_monitoring_pools_clone);
        let monitor_logger = price_monitoring_logger_clone.clone();
        let app_state_clone = Arc::clone(&price_monitoring_app_state_clone);
        let token_tracking = Arc::clone(&price_monitoring_token_tracking);
        
        // Create price monitoring interval - check every 5 seconds
        let mut interval = time::interval(Duration::from_secs(5));
        
        loop {
            interval.tick().await;
            
            // Get current pools to check
            let tokens_to_check = {
                let pools = pools_clone.lock().unwrap();
                pools.iter()
                    .filter(|pool| pool.status == Status::Bought)
                    .map(|pool| pool.clone())
                    .collect::<Vec<LiquidityPool>>()
            };
            
            if tokens_to_check.is_empty() {
                continue;
            }
            
            monitor_logger.log(format!(
                "\n[PRICE MONITOR] => Checking prices for {} tokens",
                tokens_to_check.len()
            ).blue().to_string());
            
            // Check each token's current price
            for pool in tokens_to_check {
                let mint = pool.mint.clone();
                let buy_price = pool.buy_price;
                let bought_time = pool.timestamp.unwrap_or(Instant::now());
                let time_elapsed = Instant::now().duration_since(bought_time);
                
                // Clone necessary variables
                let logger_for_price = monitor_logger.clone();
                let token_tracking_clone = Arc::clone(&token_tracking);
                let app_state_for_price = app_state_clone.clone();
                
                // Create Pump instance for price checking
                let rpc_nonblocking_client = app_state_for_price.rpc_nonblocking_client.clone();
                let rpc_client = app_state_for_price.rpc_client.clone();
                let wallet = app_state_for_price.wallet.clone();
                let swapx = Pump::new(rpc_nonblocking_client.clone(), rpc_client.clone(), wallet.clone());
                
                // Execute as a separate task to avoid blocking price check loop
                tokio::spawn(async move {
                    // Get current price estimate
                    let current_price = match swapx.get_token_price(&mint).await {
                        Ok(price) => price,
                        Err(e) => {
                            logger_for_price.log(format!(
                                "[PRICE ERROR] => Failed to get current price for {}: {}",
                                mint, e
                            ).red().to_string());
                            return;
                        }
                    };
                    
                    // Calculate PNL for informational purposes
                    let pnl = if buy_price > 0.0 {
                        ((current_price - buy_price) / buy_price) * 100.0
                    } else {
                        0.0
                    };
                    
                    // Get or create token tracking info
                    let mut tracking_info = {
                        let mut tracking = token_tracking_clone.lock().unwrap();
                        tracking.entry(mint.clone()).or_insert_with(|| TokenTrackingInfo {
                            top_pnl: pnl,
                            last_price_check: Instant::now(),
                            price_history: Vec::new(),
                        }).clone()
                    };
                    
                    // Update top PNL if current PNL is higher (for informational purposes)
                    if pnl > tracking_info.top_pnl {
                        let mut tracking = token_tracking_clone.lock().unwrap();
                        if let Some(info) = tracking.get_mut(&mint) {
                            info.top_pnl = pnl;
                            // Add price to history
                            info.price_history.push((current_price, Instant::now()));
                            // Keep only the last 100 price points
                            if info.price_history.len() > 100 {
                                info.price_history.remove(0);
                            }
                        }
                        tracking_info.top_pnl = pnl;
                        
                        logger_for_price.log(format!(
                            "\n[PNL PEAK] => Token {} reached new peak PNL: {:.2}%",
                            mint, pnl
                        ).green().bold().to_string());
                    }
                    
                    // Log current price status
                    logger_for_price.log(format!(
                        "[PRICE STATUS] => Token: {} | Buy: ${:.6} | Current: ${:.6} | PNL: {:.2}% | Peak PNL: {:.2}% | Time: {:?}",
                        mint, buy_price, current_price, pnl, tracking_info.top_pnl, time_elapsed
                    ).cyan().to_string());
                    
                    // Update last price check time
                    {
                        let mut tracking = token_tracking_clone.lock().unwrap();
                        if let Some(info) = tracking.get_mut(&mint) {
                            info.last_price_check = Instant::now();
                            // Add price to history
                            info.price_history.push((current_price, Instant::now()));
                            // Keep only the last 100 price points
                            if info.price_history.len() > 100 {
                                info.price_history.remove(0);
                            }
                        }
                    }
                    
                    // Calculate price change rate over the last few data points
                    let price_change_rate = {
                        let tracking = token_tracking_clone.lock().unwrap();
                        if let Some(info) = tracking.get(&mint) {
                            if info.price_history.len() >= 2 {
                                let newest = &info.price_history[info.price_history.len() - 1];
                                let oldest = &info.price_history[0];
                                let time_diff = newest.1.duration_since(oldest.1).as_secs_f64();
                                if time_diff > 0.0 {
                                    (newest.0 - oldest.0) / time_diff
                                } else {
                                    0.0
                                }
                            } else {
                                0.0
                            }
                        } else {
                            0.0
                        }
                    };
                    
                    // Log price change rate
                    if price_change_rate != 0.0 {
                        logger_for_price.log(format!(
                            "[PRICE CHANGE RATE] => Token: {} | Rate: ${:.6}/sec",
                            mint, price_change_rate
                        ).yellow().to_string());
                    }
                });
            }
        }
    });

    while let Some(message) = stream.next().await {
        match message {
            Ok(msg) => {
                // Process ping/pong messages
                if let Err(e) = process_stream_message(&msg, &subscribe_tx, &logger).await {
                    logger.log(format!("Error handling stream message: {}", e).red().to_string());
                    continue;
                }
                
                // Process transaction messages
                if let Some(UpdateOneof::Transaction(txn)) = msg.update_oneof {
                    let start_time = Instant::now();
                    if let Some(log_messages) = txn
                        .clone()
                        .transaction
                        .and_then(|txn1| txn1.meta)
                        .map(|meta| meta.log_messages)
                    {
                        // Process transaction to extract trade information
                        let trade_info = match TradeInfoFromToken::from_json(txn.clone(), log_messages.clone()) {
                            Ok(info) => info,
                            Err(e) => {
                                logger.log(
                                    format!("Error in parsing txn: {}", e)
                                        .red()
                                        .italic()
                                        .to_string(),
                                );
                                continue;
                            }
                        };

                        // Check if this transaction is from one of our copy trading addresses
                        let is_copy_trading_tx = filter_config.copy_trading_target_addresses.iter()
                            .any(|addr| trade_info.target == *addr);
                        
                        if !is_copy_trading_tx {
                            // Skip transactions not from our copy targets
                            continue;
                        }

                        // Process the buy transaction from target addresses only
                        logger.log(format!(
                            "\n\t * [COPY TARGET ACTION] => (https://solscan.io/tx/{}) - SLOT:({}) \n\t * [TARGET] => ({}) \n\t * [TOKEN] => ({}) \n\t * [BUY AMOUNT] => ({}) SOL \n\t * [TIMESTAMP] => {} :: ({:?}).",
                            trade_info.signature,
                            trade_info.slot,
                            trade_info.target,
                            trade_info.mint,
                            lamports_to_sol(trade_info.volume_change.abs() as u64),
                            Utc::now(),
                            start_time.elapsed(),
                        ).blue().to_string());

                        // Apply copy rate decision - always copy
                        let should_copy = true;
                        
                        // Check buy amount limits
                        let buy_amount = lamports_to_sol(trade_info.volume_change.abs() as u64);
                        if buy_amount > max_dev_buy as f64 {
                            logger.log(format!(
                                "\n\t * [BUY AMOUNT EXCEEDS MAX] => {} > {}",
                                buy_amount, max_dev_buy
                            ).yellow().to_string());
                            continue;
                        }
                        if buy_amount < min_dev_buy as f64 {
                            logger.log(format!(
                                "\n\t * [BUY AMOUNT BELOW MIN] => {} < {}",
                                buy_amount, min_dev_buy
                            ).yellow().to_string());
                            continue;
                        }

                        // Check if this token is already in our pools
                        let is_duplicate = {
                            let pools = existing_liquidity_pools.lock().unwrap();
                            pools.iter().any(|pool| pool.mint == trade_info.mint)
                        };
                        
                        if is_duplicate {
                            logger.log(format!(
                                "\n\t * [DUPLICATE TOKEN] => Token already in our pools: {}",
                                trade_info.mint
                            ).yellow().to_string());
                            continue;
                        }

                        // Check if buying is enabled
                        let buying_enabled = {
                            let enabled = BUYING_ENABLED.lock().unwrap();
                            *enabled
                        };
                        
                        if !buying_enabled {
                            logger.log(format!(
                                "\n\t * [SKIPPING BUY] => Waiting for all tokens to be sold first"
                            ).yellow().to_string());
                            continue;
                        }

                        // Temporarily disable buying while we're processing this buy
                        {
                            let mut buying_enabled = BUYING_ENABLED.lock().unwrap();
                            *buying_enabled = false;
                        }

                        // Clone the shared variables for this task
                        let swapx_clone = swapx.clone();
                        let logger_clone = logger.clone();
                        let mut swap_config_clone = (*Arc::clone(&swap_config)).clone();
                        let app_state_clone = Arc::clone(&app_state).clone();
                        
                        let mint_str = trade_info.mint.clone();
                        let bonding_curve_info = trade_info.bonding_curve_info.clone();
                        let existing_liquidity_pools_clone = Arc::clone(&existing_liquidity_pools);
                        let recent_blockhash = trade_info.clone().recent_blockhash;

                        // Determine trading amount based on comparing SOL amount and TOKEN_AMOUNT
                        let sol_amount = lamports_to_sol(trade_info.volume_change.abs() as u64);
                        let token_amount = trade_info.token_amount;
                        
                        // If token amount is smaller than SOL amount, use token amount for trading
                        if token_amount > 0.0 && token_amount < sol_amount {
                            // Modify swap_config to use the detected token amount
                            swap_config_clone.amount_in = token_amount;
                            logger.log(format!(
                                "\n\t * [USING TOKEN AMOUNT] => {}, SOL Amount: {}",
                                token_amount, sol_amount
                            ).green().to_string());
                        }

                        logger.log(format!(
                            "\n\t * [COPYING BUY] => Token: {}, Amount: {}",
                            mint_str, swap_config_clone.amount_in
                        ).green().to_string());

                        let task = tokio::spawn(async move {
                            match swapx_clone
                                .build_swap_ixn_by_mint(
                                    &mint_str,
                                    bonding_curve_info,
                                    swap_config_clone.clone(),
                                    start_time,
                                )
                                .await
                            {
                                Ok(result) => {
                                    let (keypair, instructions, token_price) =
                                        (result.0, result.1, result.2);
                                    
                                    match tx::new_signed_and_send_zeroslot(
                                        recent_blockhash,
                                        &keypair,
                                        instructions,
                                        &logger_clone,
                                    ).await {
                                        Ok(res) => {
                                            let bought_pool = LiquidityPool {
                                                mint: mint_str.clone(),
                                                buy_price: token_price,
                                                sell_price: 0_f64,
                                                status: Status::Bought,
                                                timestamp: Some(Instant::now()),
                                            };
                                            
                                            // Create a local copy before modifying
                                            {
                                                let mut existing_pools =
                                                    existing_liquidity_pools_clone.lock().unwrap();
                                                existing_pools.retain(|pool| pool.mint != mint_str);
                                                existing_pools.insert(bought_pool.clone());
                                                
                                                // Log after modification within the lock scope
                                                logger_clone.log(format!(
                                                    "\n\t * [SUCCESSFUL-COPY-BUY] => TX_HASH: (https://solscan.io/tx/{}) \n\t * [TOKEN] => ({}) \n\t * [DONE] => {} :: ({:?}) \n\t * [TOTAL TOKENS] => {}",
                                                    &res[0], mint_str, Utc::now(), start_time.elapsed(), existing_pools.len()
                                                ).green().to_string());
                                            }
                                        },
                                        Err(e) => {
                                            logger_clone.log(
                                                format!("Failed to copy buy for {}: {}", mint_str.clone(), e)
                                                    .red()
                                                    .italic()
                                                    .to_string(),
                                            );
                                            
                                            // Re-enable buying since this one failed
                                            let mut buying_enabled = BUYING_ENABLED.lock().unwrap();
                                            *buying_enabled = true;
                                            
                                            let failed_pool = LiquidityPool {
                                                mint: mint_str.clone(),
                                                buy_price: 0_f64,
                                                sell_price: 0_f64,
                                                status: Status::Failure,
                                                timestamp: None,
                                            };
                                            
                                            // Use a local scope for the mutex lock
                                            {
                                                let mut update_pools =
                                                    existing_liquidity_pools_clone.lock().unwrap();
                                                update_pools.retain(|pool| pool.mint != mint_str);
                                                update_pools.insert(failed_pool.clone());
                                            }
                                        }
                                    }
                                },
                                Err(error) => {
                                    logger_clone.log(
                                        format!("Error building swap instruction: {}", error)
                                            .red()
                                            .italic()
                                            .to_string(),
                                    );
                                    
                                    // Re-enable buying since this one failed
                                    let mut buying_enabled = BUYING_ENABLED.lock().unwrap();
                                    *buying_enabled = true;
                                    
                                    let failed_pool = LiquidityPool {
                                        mint: mint_str.clone(),
                                        buy_price: 0_f64,
                                        sell_price: 0_f64,
                                        status: Status::Failure,
                                        timestamp: None,
                                    };
                                    
                                    // Use a local scope for the mutex lock
                                    {
                                        let mut update_pools =
                                            existing_liquidity_pools_clone.lock().unwrap();
                                        update_pools.retain(|pool| pool.mint != mint_str);
                                        update_pools.insert(failed_pool.clone());
                                    }
                                }
                            }
                        });
                    }
                }
            }
            Err(error) => {
                logger.log(
                    format!("Yellowstone gRpc Error: {:?}", error)
                        .red()
                        .to_string(),
                );
                break;
            }
        }
    }
    Ok(())
}

/// Function to monitor for arbitrage opportunities
pub async fn arbitrage_monitor(
    yellowstone_grpc_http: String,
    yellowstone_grpc_token: String,
    app_state: AppState,
    swap_config: SwapConfig,
    arbitrage_threshold_pct: f64,
    min_liquidity: u64,
) -> Result<(), String> {
    use crate::engine::pool_discovery::{PoolCacheManager, PoolInfo};
    use anchor_client::solana_client::nonblocking::rpc_client::RpcClient;
    use anchor_client::solana_sdk::commitment_config::CommitmentConfig;
    use std::time::Duration;
    use std::env;
    
    // Log the arbitrage configuration
    let logger = Logger::new("[ARBITRAGE-MONITOR] => ".blue().bold().to_string());

    // Initialize RPC client for initial pool discovery
    let rpc_url = env::var("RPC_URL").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    let rpc_client = RpcClient::new_with_commitment(rpc_url, CommitmentConfig::confirmed());
    
    // Initialize pool cache manager
    let cache_path = "pool_cache.json";
    let pool_cache_manager = match crate::engine::pool_discovery::PoolCacheManager::new(cache_path) {
        Ok(manager) => Arc::new(manager),
        Err(e) => return Err(format!("Failed to initialize pool cache: {}", e)),
    };
    
    // Get list of token mints to monitor from environment or use defaults
    let token_mints_str = env::var("MONITOR_TOKEN_MINTS").unwrap_or_else(|_| "".to_string());
    let mut token_mints = Vec::new();
    
    if !token_mints_str.is_empty() {
        for mint_str in token_mints_str.split(',') {
            if let Ok(pubkey) = Pubkey::from_str(mint_str.trim()) {
                token_mints.push(pubkey);
            } else {
                logger.log(format!("Invalid token mint: {}", mint_str).red().to_string());
            }
        }
    }
    
    // If no token mints specified, use some popular tokens as default
    if token_mints.is_empty() {
        // Add some default popular tokens like SOL, USDC, BONK, JUP, etc.
        let default_mints = [
            "So11111111111111111111111111111111111111112", // SOL
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", // USDC
            "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263", // BONK
            "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN", // JUP
        ];
        
        for mint_str in default_mints.iter() {
            if let Ok(pubkey) = Pubkey::from_str(mint_str) {
                token_mints.push(pubkey);
            }
        }
    }
    
    // Log the tokens we're monitoring
    logger.log(format!(
        "[TOKEN MONITORING] => Tracking {} tokens for arbitrage opportunities",
        token_mints.len()
    ).green().to_string());
    
    for token_mint in &token_mints {
        logger.log(format!("\t * [TOKEN] => {}", token_mint).green().to_string());
    }
    
    // Initialize pool cache with discovered pools
    logger.log("[POOL DISCOVERY] => Discovering pools for monitored tokens...".blue().to_string());
    
    match crate::engine::pool_discovery::initialize_pool_cache(
        &rpc_client, 
        &token_mints, 
        &pool_cache_manager
    ).await {
        Ok(_) => {
            if let Ok(cache) = pool_cache_manager.get_cache() {
                let total_pools = cache.pools.values().map(|v| v.len()).sum::<usize>();
                logger.log(format!(
                    "[POOL DISCOVERY] => Found {} pools for {} tokens",
                    total_pools,
                    cache.pools.len()
                ).green().to_string());
                
                // Log pools per token
                for (token_mint, pools) in &cache.pools {
                    logger.log(format!(
                        "\t * [TOKEN] => {} has {} pools",
                        token_mint,
                        pools.len()
                    ).green().to_string());
                    
                    for pool in pools {
                        logger.log(format!(
                            "\t\t - [POOL] => {} on {}",
                            pool.pool_id,
                            pool.dex_name
                        ).cyan().to_string());
                    }
                }
            }
        },
        Err(e) => {
            logger.log(format!(
                "[POOL DISCOVERY ERROR] => Failed to initialize pool cache: {}",
                e
            ).red().to_string());
            // Continue anyway, we might discover pools during monitoring
        }
    }

    // INITIAL SETTING FOR SUBSCRIBE
    // -----------------------------------------------------------------------------------------------------------------------------
    let mut client = GeyserGrpcClient::build_from_shared(yellowstone_grpc_http.clone())
        .map_err(|e| format!("Failed to build client: {}", e))?
        .x_token::<String>(Some(yellowstone_grpc_token.clone()))
        .map_err(|e| format!("Failed to set x_token: {}", e))?
        .tls_config(ClientTlsConfig::new().with_native_roots())
        .map_err(|e| format!("Failed to set tls config: {}", e))?
        .connect()
        .await
        .map_err(|e| format!("Failed to connect: {}", e))?;

    // Create additional clones for later use in tasks
    let yellowstone_grpc_http = Arc::new(yellowstone_grpc_http);
    let yellowstone_grpc_token = Arc::new(yellowstone_grpc_token);
    let app_state = Arc::new(app_state);
    let swap_config = Arc::new(swap_config);
    let pool_cache_manager = Arc::new(pool_cache_manager);

    let mut retry_count = 0;
    const MAX_RETRIES: u32 = 3;
    let (subscribe_tx, mut stream) = loop {
        match client.subscribe().await {
            Ok(pair) => break pair,
            Err(e) => {
                retry_count += 1;
                if retry_count >= MAX_RETRIES {
                    return Err(format!("Failed to subscribe after {} attempts: {}", MAX_RETRIES, e));
                }
                logger.log(format!(
                    "[CONNECTION ERROR] => Failed to subscribe (attempt {}/{}): {}. Retrying in 5 seconds...",
                    retry_count, MAX_RETRIES, e
                ).red().to_string());
                time::sleep(Duration::from_secs(5)).await;
            }
        }
    };

    // Convert to Arc to allow cloning across tasks
    let subscribe_tx = Arc::new(tokio::sync::Mutex::new(subscribe_tx));

    // Initialize DEX registry to get program IDs
    let dex_registry = DEXRegistry::new();
    
    // Prepare program IDs for monitoring - include all DEXes
    let mut program_ids = Vec::new();
    
    // Add all DEX program IDs to the monitoring list
    for dex in dex_registry.get_all_dexes() {
        program_ids.push(dex.program_id.to_string());
        logger.log(format!(
            "[MONITORING DEX] => {} ({})",
            dex.name, dex.program_id
        ).green().to_string());
    }

    // Create filter config
    let filter_config = FilterConfig {
        program_ids: program_ids.clone(),
        dex_program_ids: program_ids.clone(),
        arbitrage_threshold_pct,
        min_liquidity,
    };

    logger.log(format!(
        "[ARBITRAGE CONFIG] => Threshold: {}%, Min Liquidity: {} SOL",
        filter_config.arbitrage_threshold_pct,
        lamports_to_sol(filter_config.min_liquidity)
    ).green().to_string());

    subscribe_tx
        .lock()
        .await
        .send(SubscribeRequest {
            slots: HashMap::new(),
            accounts: HashMap::new(),
            transactions: hashmap! {
                "All".to_owned() => SubscribeRequestFilterTransactions {
                    vote: None,
                    failed: Some(false),
                    signature: None,
                    account_include: program_ids.clone(),
                    account_exclude: vec![JUPITER_PROGRAM.to_string(), OKX_DEX_PROGRAM.to_string()],
                    account_required: Vec::<String>::new()
                }
            },
            transactions_status: HashMap::new(),
            entry: HashMap::new(),
            blocks: HashMap::new(),
            blocks_meta: HashMap::new(),
            commitment: Some(CommitmentLevel::Processed as i32),
            accounts_data_slice: vec![],
            ping: None,
            from_slot: None,
        })
        .await
        .map_err(|e| format!("Failed to send subscribe request: {}", e))?;

    // Use a HashMap to track token prices across different DEXes
    let token_prices = Arc::new(Mutex::new(HashMap::<String, HashMap<String, (f64, u64)>>::new()));

    logger.log("[STARTED. MONITORING FOR ARBITRAGE OPPORTUNITIES]...".blue().bold().to_string());

    // After all setup and before the main loop, add a heartbeat ping task
    let subscribe_tx_clone = subscribe_tx.clone();
    let logger_clone = logger.clone();
    
    tokio::spawn(async move {
        let ping_logger = logger_clone.clone();
        let mut interval = time::interval(Duration::from_secs(30));
        
        loop {
            interval.tick().await;
            
            if let Err(e) = send_heartbeat_ping(&subscribe_tx_clone, &ping_logger).await {
                ping_logger.log(format!("[CONNECTION ERROR] => {}", e).red().to_string());
                break;
            }
        }
    });

    // Start a background task to check for arbitrage opportunities periodically
    let token_prices_clone = Arc::clone(&token_prices);
    let logger_clone = logger.clone();
    let pool_cache_manager_clone = Arc::clone(&pool_cache_manager);
    let arbitrage_threshold = filter_config.arbitrage_threshold_pct;
    let min_liquidity_value = filter_config.min_liquidity;
    
    tokio::spawn(async move {
        let prices_clone = Arc::clone(&token_prices_clone);
        let arb_logger = logger_clone.clone();
        let cache_manager = Arc::clone(&pool_cache_manager_clone);
        
        // Create arbitrage checking interval - check every 5 seconds
        let mut interval = time::interval(Duration::from_secs(5));
        
        loop {
            interval.tick().await;
            
            // Check for arbitrage opportunities
            let opportunities = {
                let prices = prices_clone.lock().unwrap();
                let mut arb_opportunities = Vec::new();
                
                // Get the current cache
                let cache = match cache_manager.get_cache() {
                    Ok(c) => c,
                    Err(e) => {
                        arb_logger.log(format!("[CACHE ERROR] => {}", e).red().to_string());
                        continue;
                    }
                };
                
                for (token_mint, dex_prices) in prices.iter() {
                    // Need at least 2 DEXes to compare
                    if dex_prices.len() < 2 {
                        continue;
                    }
                    
                    // Convert to a vector for easier comparison
                    let dex_price_vec: Vec<(&String, &(f64, u64))> = dex_prices.iter().collect();
                    
                    for i in 0..dex_price_vec.len() {
                        for j in i+1..dex_price_vec.len() {
                            let (dex1, (price1, liquidity1)) = dex_price_vec[i];
                            let (dex2, (price2, liquidity2)) = dex_price_vec[j];
                            
                            // Calculate price difference percentage
                            let price_diff_pct = ((price1 - price2).abs() / price2) * 100.0;
                            
                            // Check if price difference exceeds threshold and both have sufficient liquidity
                            if price_diff_pct > arbitrage_threshold && 
                               *liquidity1 >= min_liquidity_value && 
                               *liquidity2 >= min_liquidity_value {
                                
                                // Determine buy and sell DEXes based on price
                                let (buy_dex, buy_price, sell_dex, sell_price) = if price1 < price2 {
                                    (dex1, price1, dex2, price2)
                                } else {
                                    (dex2, price2, dex1, price1)
                                };
                                
                                // Calculate expected profit percentage
                                let expected_profit_pct = ((sell_price - buy_price) / buy_price) * 100.0;
                                
                                // Find the pool IDs from the cache
                                let mut buy_pool_id = "unknown";
                                let mut sell_pool_id = "unknown";
                                
                                if let Some(pools) = cache.pools.get(token_mint) {
                                    for pool in pools {
                                        if &pool.dex_name == *buy_dex {
                                            buy_pool_id = &pool.pool_id;
                                        } else if &pool.dex_name == *sell_dex {
                                            sell_pool_id = &pool.pool_id;
                                        }
                                    }
                                }
                                
                                arb_opportunities.push((
                                    token_mint.clone(),
                                    buy_dex.clone(),
                                    *buy_price,
                                    buy_pool_id.to_string(),
                                    sell_dex.clone(),
                                    *sell_price,
                                    sell_pool_id.to_string(),
                                    expected_profit_pct
                                ));
                            }
                        }
                    }
                }
                
                arb_opportunities
            };
            
            // Log arbitrage opportunities
            if !opportunities.is_empty() {
                arb_logger.log(format!(
                    "[ARBITRAGE OPPORTUNITIES] => Found {} potential arbitrage trades",
                    opportunities.len()
                ).green().bold().to_string());
                
                for (token, buy_dex, buy_price, buy_pool, sell_dex, sell_price, sell_pool, profit) in opportunities {
                    arb_logger.log(format!(
                        "\n\t * [ARBITRAGE] => Token: {} \n\t * [BUY] => {} at ${:.6} (Pool: {}) \n\t * [SELL] => {} at ${:.6} (Pool: {}) \n\t * [PROFIT] => {:.2}%",
                        token, buy_dex, buy_price, buy_pool, sell_dex, sell_price, sell_pool, profit
                    ).cyan().to_string());
                    
                    // Here you would implement the actual arbitrage execution
                    // This would involve:
                    // 1. Buy the token on the cheaper DEX
                    // 2. Sell the token on the more expensive DEX
                    // 3. Calculate actual profit after fees
                    
                    // For now, just log that we would execute the trade
                    arb_logger.log(format!(
                        "\n\t * [WOULD EXECUTE] => Arbitrage trade for token {} between {} and {}",
                        token, buy_dex, sell_dex
                    ).yellow().to_string());
                    
                    // Save arbitrage opportunity to a file for later analysis
                    let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S").to_string();
                    let record = serde_json::json!({
                        "timestamp": timestamp,
                        "token_mint": token,
                        "buy_dex": buy_dex,
                        "buy_price": buy_price,
                        "buy_pool": buy_pool,
                        "sell_dex": sell_dex,
                        "sell_price": sell_price,
                        "sell_pool": sell_pool,
                        "price_difference_pct": profit,
                        "min_liquidity": lamports_to_sol(min_liquidity_value)
                    });
                    
                    // Ensure the directory exists
                    let record_dir = "arbitrage_opportunities";
                    if !Path::new(record_dir).exists() {
                        if let Err(e) = fs::create_dir_all(record_dir) {
                            arb_logger.log(format!("[ERROR] => Failed to create directory: {}", e).red().to_string());
                        }
                    }
                    
                    // Write to file
                    let filename = format!("{}/arb_{}_{}.json", record_dir, token.split_at(8).0, timestamp);
                    if let Ok(mut file) = File::create(&filename) {
                        if let Err(e) = file.write_all(serde_json::to_string_pretty(&record).unwrap_or_default().as_bytes()) {
                            arb_logger.log(format!("[ERROR] => Failed to write to file: {}", e).red().to_string());
                        }
                    }
                }
            }
        }
    });

    // Add a connection health check task
    let logger_health = logger.clone(); 
    tokio::spawn(async move {
        let health_logger = logger_health.clone();
        let mut interval = time::interval(Duration::from_secs(300)); // 5 minutes
        
        loop {
            interval.tick().await;
            check_connection_health(&health_logger).await;
        }
    });

    // Ensure record directories exist
    ensure_record_dirs()?;

    while let Some(message) = stream.next().await {
        match message {
            Ok(msg) => {
                // Process ping/pong messages
                if let Err(e) = process_stream_message(&msg, &subscribe_tx, &logger).await {
                    logger.log(format!("Error handling stream message: {}", e).red().to_string());
                    continue;
                }
                
                // Process transaction messages
                if let Some(UpdateOneof::Transaction(txn)) = msg.update_oneof {
                    let start_time = Instant::now();
                    if let Some(log_messages) = txn
                        .clone()
                        .transaction
                        .and_then(|txn1| txn1.meta)
                        .map(|meta| meta.log_messages)
                    {
                        // Extract DEX program ID from transaction
                        if let Some(transaction) = txn.transaction.clone() {
                            if let Some(message) = transaction.transaction.and_then(|t| t.message) {
                                for instruction in message.instructions {
                                    let program_idx = instruction.program_id_index as usize;
                                    if let Some(program_id_bytes) = message.account_keys.get(program_idx) {
                                        if let Ok(program_id) = Pubkey::try_from(program_id_bytes.clone()) {
                                            // Check if this is a DEX program
                                            if let Some(dex) = dex_registry.find_dex_by_program_id(&program_id) {
                                                logger.log(format!(
                                                    "[TRANSACTION] => DEX: {}, Signature: {}",
                                                    dex.name,
                                                    bs58::encode(&transaction.signature).into_string()
                                                ).blue().to_string());
                                                
                                                // Extract pool information and token prices
                                                // This would involve parsing the transaction logs and data
                                                // For now, we'll just log that we detected a DEX transaction
                                                
                                                // In a real implementation, you would:
                                                // 1. Extract the token mint address
                                                // 2. Extract the pool information
                                                // 3. Calculate the token price based on the pool reserves
                                                // 4. Update the token_prices HashMap
                                                
                                                // Mock implementation for demonstration
                                                let mock_token_mint = "TokenMintAddress";
                                                let mock_price = 1.0 + (rand::random::<f64>() * 0.1); // Random price between 1.0 and 1.1
                                                let mock_liquidity = 1_000_000_000; // 1 SOL
                                                
                                                // Update token prices
                                                {
                                                    let mut prices = token_prices.lock().unwrap();
                                                    let dex_prices = prices
                                                        .entry(mock_token_mint.to_string())
                                                        .or_insert_with(HashMap::new);
                                                    
                                                    dex_prices.insert(dex.name.clone(), (mock_price, mock_liquidity));
                                                }
                                                
                                                logger.log(format!(
                                                    "[PRICE UPDATE] => Token: {}, DEX: {}, Price: ${:.6}, Liquidity: {} SOL",
                                                    mock_token_mint, dex.name, mock_price, lamports_to_sol(mock_liquidity)
                                                ).green().to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(error) => {
                logger.log(
                    format!("Yellowstone gRpc Error: {:?}", error)
                        .red()
                        .to_string(),
                );
                break;
            }
        }
    }
    Ok(())
}

