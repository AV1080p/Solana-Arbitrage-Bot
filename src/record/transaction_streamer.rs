use colored::Colorize;
use futures_util::{stream::StreamExt, SinkExt};
use std::collections::HashMap;
use tokio::time::{self, Duration};
use yellowstone_grpc_client::{ClientTlsConfig, GeyserGrpcClient};
use yellowstone_grpc_proto::geyser::{
    subscribe_update::UpdateOneof,
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterTransactions,
};

use crate::common::logger::Logger;
use crate::record::transaction_logger::{
    log_raw_transaction, 
    is_target_protocol_transaction,
    is_pumpfun_transaction,
    is_pumpswap_transaction,
    is_raydium_transaction,
    is_raydium_cpmm_transaction,
    is_whirlpool_transaction,
    is_stable_swap_transaction,
    is_meteora_pools_transaction,
    is_meteora_dlmm_transaction,
    PUMP_FUN_PROGRAM,
    PUMP_SWAP_PROGRAM,
    RAYDIUM_PROGRAM,
    RAYDIUM_CPMM_PROGRAM,
    WHIRLPOOL_PROGRAM,
    STABBLE_STABLE_SWAP_PROGRAM,
    METEORA_POOLS_PROGRAM,
    METEORA_DLMM_PROGRAM
};

/// Stream and log transactions from pumpfun, pumpswap, and raydium
pub async fn stream_protocol_transactions(
    yellowstone_grpc_http: String,
    yellowstone_grpc_token: String,
) -> Result<(), String> {
    // Create logger
    let logger = Logger::new("[TX-STREAMER] => ".blue().bold().to_string());
    logger.log("Starting transaction streaming...".green().to_string());

    // Initialize gRPC client
    let mut client = GeyserGrpcClient::build_from_shared(yellowstone_grpc_http)
        .map_err(|e| format!("Failed to build client: {}", e))?
        .x_token::<String>(Some(yellowstone_grpc_token))
        .map_err(|e| format!("Failed to set x_token: {}", e))?
        .tls_config(ClientTlsConfig::new().with_native_roots())
        .map_err(|e| format!("Failed to set tls config: {}", e))?
        .connect()
        .await
        .map_err(|e| format!("Failed to connect: {}", e))?;

    // Set up subscription
    let (mut subscribe_tx, mut stream) = client.subscribe().await
        .map_err(|e| format!("Failed to subscribe: {}", e))?;

    // Program IDs for the protocols we're interested in
    let program_ids = vec![
        PUMP_FUN_PROGRAM.to_string(),
        PUMP_SWAP_PROGRAM.to_string(),
        RAYDIUM_PROGRAM.to_string(),
        RAYDIUM_CPMM_PROGRAM.to_string(),
        WHIRLPOOL_PROGRAM.to_string(),
        STABBLE_STABLE_SWAP_PROGRAM.to_string(),
        METEORA_POOLS_PROGRAM.to_string(),
        METEORA_DLMM_PROGRAM.to_string(),
    ];

    // Create transaction filter
    let tx_filter = SubscribeRequestFilterTransactions {
        vote: Some(false),
        failed: Some(false),
        signature: None,
        account_include: program_ids,
        account_exclude: vec![],
        account_required: vec![],
    };

    // Create a HashMap for transactions filter
    let mut transactions_filter = HashMap::new();
    transactions_filter.insert("txFilter".to_string(), tx_filter);

    // Set up transaction filter for the specified programs
    let subscribe_request = SubscribeRequest {
        slots: HashMap::new(),
        accounts: HashMap::new(),
        transactions: transactions_filter,
        entry: HashMap::new(),
        blocks: HashMap::new(),
        blocks_meta: HashMap::new(),
        commitment: Some(CommitmentLevel::Confirmed as i32),
        accounts_data_slice: vec![],
        ping: None,
        // Removed from_slot because it's not supported by the server
        from_slot: None,
        transactions_status: HashMap::new(),
    };

    // Submit subscription request
    if let Err(e) = subscribe_tx.send(subscribe_request).await {
        return Err(format!("Failed to send subscription request: {:?}", e));
    }

    logger.log("Subscription active. Waiting for transactions...".green().to_string());

    // Stats counters
    let mut pumpfun_count = 0;
    let mut pumpswap_count = 0;
    let mut raydium_count = 0;
    let mut raydium_cpmm_count = 0;
    let mut whirlpool_count = 0;
    let mut stable_swap_count = 0;
    let mut meteora_pools_count = 0;
    let mut meteora_dlmm_count = 0;

    // Process incoming messages
    while let Some(message) = stream.next().await {
        match message {
            Ok(msg) => {
                if let Some(UpdateOneof::Transaction(txn)) = msg.update_oneof {
                    // Extract log messages from the transaction
                    let log_messages: Vec<String> = if let Some(tx_info) = &txn.transaction {
                        if let Some(meta) = &tx_info.meta {
                            meta.log_messages.clone()
                        } else {
                            vec![]
                        }
                    } else {
                        vec![]
                    };
                    
                    if is_pumpfun_transaction(&log_messages) {
                        pumpfun_count += 1;
                        if let Err(e) = log_raw_transaction(&txn, &log_messages) {
                            logger.log(format!("[ERROR] Failed to log PumpFun transaction: {}", e).red().to_string());
                        } else {
                            logger.log(format!("[PUMPFUN] Logged transaction #{}: Slot {}", 
                                pumpfun_count, txn.slot).green().to_string());
                        }
                    } else if is_pumpswap_transaction(&log_messages) {
                        pumpswap_count += 1;
                        if let Err(e) = log_raw_transaction(&txn, &log_messages) {
                            logger.log(format!("[ERROR] Failed to log PumpSwap transaction: {}", e).red().to_string());
                        } else {
                            logger.log(format!("[PUMPSWAP] Logged transaction #{}: Slot {}", 
                                pumpswap_count, txn.slot).green().to_string());
                        }
                    } else if is_raydium_transaction(&log_messages) {
                        raydium_count += 1;
                        if let Err(e) = log_raw_transaction(&txn, &log_messages) {
                            logger.log(format!("[ERROR] Failed to log Raydium transaction: {}", e).red().to_string());
                        } else {
                            logger.log(format!("[RAYDIUM] Logged transaction #{}: Slot {}", 
                                raydium_count, txn.slot).green().to_string());
                        }
                    } else if is_raydium_cpmm_transaction(&log_messages) {
                        raydium_cpmm_count += 1;
                        if let Err(e) = log_raw_transaction(&txn, &log_messages) {
                            logger.log(format!("[ERROR] Failed to log Raydium CPMM transaction: {}", e).red().to_string());
                        } else {
                            logger.log(format!("[RAYDIUM-CPMM] Logged transaction #{}: Slot {}", 
                                raydium_cpmm_count, txn.slot).green().to_string());
                        }
                    } else if is_whirlpool_transaction(&log_messages) {
                        whirlpool_count += 1;
                        if let Err(e) = log_raw_transaction(&txn, &log_messages) {
                            logger.log(format!("[ERROR] Failed to log Whirlpool transaction: {}", e).red().to_string());
                        } else {
                            logger.log(format!("[WHIRLPOOL] Logged transaction #{}: Slot {}", 
                                whirlpool_count, txn.slot).green().to_string());
                        }
                    } else if is_stable_swap_transaction(&log_messages) {
                        stable_swap_count += 1;
                        if let Err(e) = log_raw_transaction(&txn, &log_messages) {
                            logger.log(format!("[ERROR] Failed to log Stable Swap transaction: {}", e).red().to_string());
                        } else {
                            logger.log(format!("[STABLE-SWAP] Logged transaction #{}: Slot {}", 
                                stable_swap_count, txn.slot).green().to_string());
                        }
                    } else if is_meteora_pools_transaction(&log_messages) {
                        meteora_pools_count += 1;
                        if let Err(e) = log_raw_transaction(&txn, &log_messages) {
                            logger.log(format!("[ERROR] Failed to log Meteora Pools transaction: {}", e).red().to_string());
                        } else {
                            logger.log(format!("[METEORA-POOLS] Logged transaction #{}: Slot {}", 
                                meteora_pools_count, txn.slot).green().to_string());
                        }
                    } else if is_meteora_dlmm_transaction(&log_messages) {
                        meteora_dlmm_count += 1;
                        if let Err(e) = log_raw_transaction(&txn, &log_messages) {
                            logger.log(format!("[ERROR] Failed to log Meteora DLMM transaction: {}", e).red().to_string());
                        } else {
                            logger.log(format!("[METEORA-DLMM] Logged transaction #{}: Slot {}", 
                                meteora_dlmm_count, txn.slot).green().to_string());
                        }
                    }
                }
            }
            Err(e) => {
                logger.log(format!("[ERROR] Stream error: {:?}", e).red().to_string());
                // Just log the error and continue - the stream may have ended or there might be a temporary issue
                break;
            }
        }
    }

    logger.log("Stream ended.".yellow().to_string());
    Ok(())
} 