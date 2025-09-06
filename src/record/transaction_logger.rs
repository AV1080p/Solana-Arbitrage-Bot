use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use chrono::Utc;
use yellowstone_grpc_proto::geyser::{SubscribeUpdateTransaction, SubscribeUpdate};

/// Program IDs for the protocols we're interested in
pub const RAYDIUM_CLMM_PROGRAM=CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK
pub const RAYDIUM_CPMM_PROGRAM=CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C
pub const RAYDIUM_AMM_PROGRAM=675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8
pub const WHIRLPOOLS_PROGRAM=whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc
pub const METEORA_DLMM_PROGRAM=LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo
pub const METEORA_POOLS_PROGRAM=Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB
pub const PUMP_SWAP_PROGRAM=pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA


/// Ensures all necessary record directories exist
pub fn ensure_record_dirs() -> Result<(), String> {
    let base_dir = "./record";
    if !Path::new(base_dir).exists() {
        fs::create_dir_all(base_dir).map_err(|e| format!("Failed to create directory {}: {}", base_dir, e))?;
    }
    
    // Create subdirectories for each protocol
    let protocols = ["pumpfun", "pumpswap", "raydium", "raydium_cpmm", "whirlpool", "stable_swap", "meteora_pools", "meteora_dlmm"];
    for protocol in protocols.iter() {
        let protocol_dir = format!("{}/{}", base_dir, protocol);
        if !Path::new(&protocol_dir).exists() {
            fs::create_dir_all(&protocol_dir).map_err(|e| format!("Failed to create directory {}: {}", protocol_dir, e))?;
        }
    }
    
    Ok(())
}

/// Logs raw transaction data to a file without parsing
pub fn log_raw_transaction(
    transaction: &SubscribeUpdateTransaction,
    log_messages: &[String],
) -> Result<(), String> {
    // Ensure the record directory exists
    ensure_record_dirs()?;

    // Create a timestamp for the log file
    let timestamp = Utc::now().timestamp_millis();
    let signature = match &transaction.transaction {
        Some(tx_info) => format!("{:?}", tx_info.signature.clone()),
        None => "unknown".to_string(),
    };
    let slot = transaction.slot;
    
    // Determine which protocol this transaction is for
    let protocol_prefix = if is_pumpfun_transaction(log_messages) {
        "pumpfun"
    } else if is_pumpswap_transaction(log_messages) {
        "pumpswap"
    } else if is_raydium_transaction(log_messages) {
        "raydium"
    } else if is_raydium_cpmm_transaction(log_messages) {
        "raydium_cpmm"
    } else if is_whirlpool_transaction(log_messages) {
        "whirlpool"
    } else if is_stable_swap_transaction(log_messages) {
        "stable_swap"
    } else if is_meteora_pools_transaction(log_messages) {
        "meteora_pools"
    } else if is_meteora_dlmm_transaction(log_messages) {
        "meteora_dlmm"
    } else {
        "unknown"
    };
    
    // Create the filename with protocol, timestamp and signature
    let filename = format!("./record/{}/{}_tx_{}.log", 
                          protocol_prefix, protocol_prefix, timestamp);

    // Build the log content with transaction data and log messages
    let mut log_content = String::new();
    log_content.push_str(&format!("==== BEGIN TRANSACTION LOG ====\n"));
    log_content.push_str(&format!("Protocol: {}\n", protocol_prefix));
    log_content.push_str(&format!("Transaction slot: {}\n", slot));
    log_content.push_str(&format!("Log messages count: {}\n", log_messages.len()));
    
    // Add all log messages
    for (i, log) in log_messages.iter().enumerate() {
        log_content.push_str(&format!("LOG[{}]: {}\n", i, log));
    }
    
    // Add more transaction details (signatures, accounts, etc.)
    if let Some(tx_info) = &transaction.transaction {
        log_content.push_str(&format!("Transaction Signature: {:?}\n", tx_info.signature));
        
        if let Some(meta) = &tx_info.meta {
            log_content.push_str(&format!("Transaction Success: {}\n", meta.err.is_none()));
            
            // Add account keys if available
            if let Some(tx_msg) = &tx_info.transaction {
                if let Some(message) = &tx_msg.message {
                    if !message.account_keys.is_empty() {
                        log_content.push_str("Account Keys:\n");
                        for (i, key) in message.account_keys.iter().enumerate() {
                            log_content.push_str(&format!("  Key[{}]: {:?}\n", i, key));
                        }
                    }
                }
            }
            
            // Add post token balances if present
            if !meta.post_token_balances.is_empty() {
                log_content.push_str("Post Token Balances:\n");
                for balance in &meta.post_token_balances {
                    log_content.push_str(&format!("  Account: {}, Mint: {}, Amount: {}\n", 
                    balance.owner, balance.mint, balance.ui_token_amount.as_ref().and_then(|amt| Some(amt.ui_amount)).unwrap_or(0.0)));
                }
            }
        }
    }
    
    log_content.push_str(&format!("==== END TRANSACTION LOG ====\n"));

    // Write to file
    let mut file = File::create(&filename)
        .map_err(|e| format!("Failed to create log file {}: {}", filename, e))?;
    
    file.write_all(log_content.as_bytes())
        .map_err(|e| format!("Failed to write to log file {}: {}", filename, e))?;

    Ok(())
}

/// Check if a transaction is from PumpFun
pub fn is_pumpfun_transaction(log_messages: &[String]) -> bool {
    for log in log_messages {
        if log.contains(PUMP_FUN_PROGRAM) {
            return true;
        }
    }
    false
}

/// Check if a transaction is from PumpSwap
pub fn is_pumpswap_transaction(log_messages: &[String]) -> bool {
    for log in log_messages {
        if log.contains(PUMP_SWAP_PROGRAM) {
            return true;
        }
    }
    false
}

/// Check if a transaction is from Raydium
pub fn is_raydium_transaction(log_messages: &[String]) -> bool {
    for log in log_messages {
        if log.contains(RAYDIUM_PROGRAM) {
            return true;
        }
    }
    false
}

/// Check if a transaction is from Raydium CPMM
pub fn is_raydium_cpmm_transaction(log_messages: &[String]) -> bool {
    for log in log_messages {
        if log.contains(RAYDIUM_CPMM_PROGRAM) {
            return true;
        }
    }
    false
}

/// Check if a transaction is from Whirlpool
pub fn is_whirlpool_transaction(log_messages: &[String]) -> bool {
    for log in log_messages {
        if log.contains(WHIRLPOOL_PROGRAM) {
            return true;
        }
    }
    false
}

/// Check if a transaction is from Stable Swap
pub fn is_stable_swap_transaction(log_messages: &[String]) -> bool {
    for log in log_messages {
        if log.contains(STABBLE_STABLE_SWAP_PROGRAM) {
            return true;
        }
    }
    false
}

/// Check if a transaction is from Meteora Pools
pub fn is_meteora_pools_transaction(log_messages: &[String]) -> bool {
    for log in log_messages {
        if log.contains(METEORA_POOLS_PROGRAM) {
            return true;
        }
    }
    false
}

/// Check if a transaction is from Meteora DLMM
pub fn is_meteora_dlmm_transaction(log_messages: &[String]) -> bool {
    for log in log_messages {
        if log.contains(METEORA_DLMM_PROGRAM) {
            return true;
        }
    }
    false
}

/// Determine if a transaction is related to any of our target protocols
pub fn is_target_protocol_transaction(log_messages: &[String]) -> bool {
    is_pumpfun_transaction(log_messages) || 
    is_pumpswap_transaction(log_messages) || 
    is_raydium_transaction(log_messages) ||
    is_raydium_cpmm_transaction(log_messages) ||
    is_whirlpool_transaction(log_messages) ||
    is_stable_swap_transaction(log_messages) ||
    is_meteora_pools_transaction(log_messages) ||
    is_meteora_dlmm_transaction(log_messages)
} 