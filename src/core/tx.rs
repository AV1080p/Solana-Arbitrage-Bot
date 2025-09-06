use std::{sync::Arc, time::Duration};
use std::{str::FromStr, env};
use anyhow::Result;
use colored::Colorize;
use std::hash;
use anchor_client::solana_client::rpc_client::RpcClient;
use anchor_client::solana_sdk::{
    commitment_config::CommitmentConfig,
    hash::Hash,
    instruction::Instruction,
    signature::Keypair,
    signer::Signer,
    system_instruction, system_transaction,
    transaction::{Transaction, VersionedTransaction},
};
use spl_token::ui_amount_to_amount;

use jito_json_rpc_client::jsonrpc_client::rpc_client::RpcClient as JitoRpcClient;
use tokio::time::Instant;

use crate::common::config::{create_nonblocking_rpc_client, Config};
use crate::{
    common::logger::Logger,
    services::{
        jito::{self, JitoClient},
        nozomi,
        zeroslot::{self, ZeroSlotClient},
    },
};

use lazy_static;

// Define default values for tips
const NOZOMI_TIP: f64 = 0.001;
const JITO_TIP: f64 = 0.001;

// Cache these values to avoid repeated env lookups
lazy_static::lazy_static! {
    static ref UNIT_PRICE: u64 = env::var("UNIT_PRICE")
        .ok()
        .and_then(|v| u64::from_str(&v).ok())
        .unwrap_or(20000);
        
    static ref UNIT_LIMIT: u32 = env::var("UNIT_LIMIT")
        .ok()
        .and_then(|v| u32::from_str(&v).ok())
        .unwrap_or(200_000);
}

// Functions to get tip values at runtime
pub fn get_nozomi_tip() -> f64 {
    std::env::var("NOZOMI_TIP_VALUE")
        .ok()
        .and_then(|v| f64::from_str(&v).ok())
        .unwrap_or(NOZOMI_TIP)
}

pub fn get_jito_tip() -> f64 {
    std::env::var("JITO_TIP_VALUE")
        .ok()
        .and_then(|v| f64::from_str(&v).ok())
        .unwrap_or(JITO_TIP)
}

pub async fn jito_confirm(
    client: &RpcClient,
    keypair: &Keypair,
    version_tx: VersionedTransaction,
    recent_block_hash: &Hash,
    logger: &Logger,
) -> Result<Vec<String>> {
    let (tip_account, tip1_account) = jito::get_tip_account()?;
    let jito_client = Arc::new(JitoRpcClient::new(format!(
        "{}/api/v1/bundles",
        *jito::BLOCK_ENGINE_URL
    )));
    // jito tip, the upper limit is 0.1
    let mut tip_value = jito::get_tip_value().await.unwrap();
    let tip = 0.0004_f64;
    tip_value -= tip;
    let tip_lamports = ui_amount_to_amount(tip, spl_token::native_mint::DECIMALS);
    let tip_value_lamports = ui_amount_to_amount(tip_value, spl_token::native_mint::DECIMALS); // tip tx

    let simulate_result = client.simulate_transaction(&version_tx)?;
    // logger.log("Tx Stimulate".to_string());
    // if let Some(logs) = simulate_result.value.logs {
    //     for log in logs {
    //         logger.log(log.to_string());
    //     }
    // }
    if let Some(err) = simulate_result.value.err {
        return Err(anyhow::anyhow!("{}", err));
    };
    let bundle: Vec<VersionedTransaction> = if tip_value > 0_f64 {
        vec![
            version_tx,
            VersionedTransaction::from(system_transaction::transfer(
                keypair,
                &tip_account,
                tip_lamports,
                *recent_block_hash,
            )),
            VersionedTransaction::from(system_transaction::transfer(
                keypair,
                &tip1_account,
                tip_value_lamports,
                *recent_block_hash,
            )),
        ]
    } else {
        vec![
            version_tx,
            VersionedTransaction::from(system_transaction::transfer(
                keypair,
                &tip_account,
                tip_lamports,
                *recent_block_hash,
            )),
        ]
    };
    let start_time = Instant::now();
    let bundle_id = jito_client.send_bundle(&bundle).await.unwrap();
    logger.log(
        format!("txn ellapsed({}): {:?}", bundle_id, start_time.elapsed())
            .yellow()
            .to_string(),
    );
    jito::wait_for_bundle_confirmation(
        move |id: String| {
            let client = Arc::clone(&jito_client);
            async move {
                let response = client.get_bundle_statuses(&[id]).await;
                let statuses = response.inspect_err(|err| {
                    println!("Error fetching bundle status: {:?}", err);
                })?;
                Ok(statuses.value)
            }
        },
        bundle_id,
        Duration::from_millis(1000),
        Duration::from_secs(10),
    )
    .await
}

pub async fn new_signed_and_send_normal(
    recent_blockhash: anchor_client::solana_sdk::hash::Hash,
    keypair: &Keypair,
    mut instructions: Vec<Instruction>,
    logger: &Logger,
) -> Result<Vec<String>> {
    let start_time = Instant::now();
    
    // ADD Priority fee
    // -------------
    let unit_limit = get_unit_limit();
    let unit_price = get_unit_price();

    let modify_compute_units =
        anchor_client::solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(unit_limit);
    let add_priority_fee =
        anchor_client::solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_price(unit_price);
    
    // Insert priority fee instructions at the beginning
    instructions.insert(0, modify_compute_units);
    instructions.insert(1, add_priority_fee);
    
    // Create and sign transaction
    let txn = Transaction::new_signed_with_payer(
        &instructions,
        Some(&keypair.pubkey()),
        &vec![keypair],
        recent_blockhash,
    );
    
    // Log before sending
    logger.log("Attempting to send normal transaction...".to_string());
    
    // Get RPC client
    let config = Config::get().await;
    let rpc_client = Arc::clone(&config.app_state.rpc_nonblocking_client);
    
    // Create transaction config with skip_preflight
    let tx_config = anchor_client::solana_client::rpc_config::RpcSendTransactionConfig {
        skip_preflight: true,
        ..anchor_client::solana_client::rpc_config::RpcSendTransactionConfig::default()
    };
    
    // Send transaction and directly handle the result
    let result = rpc_client.send_transaction_with_config(&txn, tx_config).await;
    
    match result {
        Ok(signature) => {
            let sig_str = signature.to_string();
            logger.log(format!("Transaction sent successfully: {}", sig_str).green().to_string());
            
            logger.log(
                format!("[TXN-ELAPSED(NORMAL)]: {:?}", start_time.elapsed())
                    .yellow()
                    .to_string(),
            );
            
            // Return the signature string in the expected Vec format
            Ok(vec![sig_str])
        },
        Err(err) => {
            // Log error and return it
            logger.log(format!("Failed to send transaction: {}", err).red().to_string());
            Err(anyhow::anyhow!("Failed to send transaction: {}", err))
        }
    }
}

pub async fn new_signed_and_send(
    recent_blockhash: anchor_client::solana_sdk::hash::Hash,
    keypair: &Keypair,
    mut instructions: Vec<Instruction>,
    logger: &Logger,
) -> Result<Vec<String>> {
    let start_time = Instant::now();

    let mut txs = vec![];
    let (tip_account, tip1_account) = jito::get_tip_account()?;

    // jito tip, the upper limit is 0.1
    let tip = jito::get_tip_value().await?;
    let fee = jito::get_priority_fee().await?;
    let tip_lamports = ui_amount_to_amount(tip, spl_token::native_mint::DECIMALS);
    let fee_lamports = ui_amount_to_amount(fee, spl_token::native_mint::DECIMALS);

    let jito_tip_instruction =
        system_instruction::transfer(&keypair.pubkey(), &tip_account, tip_lamports);
    let jito_tip2_instruction =
        system_instruction::transfer(&keypair.pubkey(), &tip1_account, fee_lamports);

        // ADD Priority fee
        // -------------
        let unit_limit = get_unit_limit();
        let unit_price = get_unit_price();

    let modify_compute_units =
        anchor_client::solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(
            unit_limit,
        );
    let add_priority_fee =
        anchor_client::solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_price(
            unit_price,
        );
    instructions.insert(1, modify_compute_units);
    instructions.insert(2, add_priority_fee);
    
    instructions.push(jito_tip_instruction);
    // instructions.push(jito_tip2_instruction);

    // send init tx
    let txn = Transaction::new_signed_with_payer(
        &instructions,
        Some(&keypair.pubkey()),
        &vec![keypair],
        recent_blockhash,
    );

    // let simulate_result = client.simulate_transaction(&txn)?;
    // logger.log("Tx Stimulate".to_string());
    // if let Some(logs) = simulate_result.value.logs {
    //     for log in logs {
    //         logger.log(log.to_string());
    //     }
    // }
    // if let Some(err) = simulate_result.value.err {
    //     return Err(anyhow::anyhow!("{}", err));
    // };

    let jito_client = Arc::new(JitoClient::new(
        format!("{}/api/v1/transactions", *jito::BLOCK_ENGINE_URL).as_str(),
    ));
    let sig = match jito_client.send_transaction(&txn).await {
        Ok(signature) => signature,
        Err(_) => {
            // logger.log(format!("{}", e));
            return Err(anyhow::anyhow!("Bundle status get timeout"
                .red()
                .italic()
                .to_string()));
        }
    };
    txs.push(sig.clone().to_string());
    logger.log(
        format!("[TXN-ELLAPSED(JITO)]: {:?}", start_time.elapsed())
            .yellow()
            .to_string(),
    );

    Ok(txs)
}

pub async fn new_signed_and_send_zeroslot(
    recent_blockhash: anchor_client::solana_sdk::hash::Hash,
    keypair: &Keypair,
    mut instructions: Vec<Instruction>,
    logger: &Logger,
) -> Result<Vec<String>> {
    let start_time = Instant::now();

    let mut txs = vec![];
    let tip_account = zeroslot::get_tip_account()?;

    // zeroslot tip, the upper limit is 0.1
    let tip = zeroslot::get_tip_value().await?;
    let tip_lamports = ui_amount_to_amount(tip, spl_token::native_mint::DECIMALS);

    let zeroslot_tip_instruction =
        system_instruction::transfer(&keypair.pubkey(), &tip_account, tip_lamports);
    instructions.insert(0, zeroslot_tip_instruction);

    // send init tx
    let txn = Transaction::new_signed_with_payer(
        &instructions,
        Some(&keypair.pubkey()),
        &vec![keypair],
        recent_blockhash,
    );

    // let simulate_result = client.simulate_transaction(&txn)?;
    // logger.log("Tx Stimulate".to_string());
    // if let Some(logs) = simulate_result.value.logs {
    //     for log in logs {
    //         logger.log(log.to_string());
    //     }
    // }
    // if let Some(err) = simulate_result.value.err {
    //     return Err(anyhow::anyhow!("{}", err));
    // };

    let zeroslot_client = Arc::new(ZeroSlotClient::new((*zeroslot::ZERO_SLOT_URL).as_str()));
    let sig = match zeroslot_client.send_transaction(&txn).await {
        Ok(signature) => signature,
        Err(_) => {
            return Err(anyhow::anyhow!("send_transaction status get timeout"
                .red()
                .italic()
                .to_string()));
        }
    };
    txs.push(sig.clone().to_string());
    logger.log(
        format!("[TXN-ELLAPSED]: {:?}", start_time.elapsed())
            .yellow()
            .to_string(),
    );

    Ok(txs)
}

// prioritization fee = UNIT_PRICE * UNIT_LIMIT
fn get_unit_price() -> u64 {
    *UNIT_PRICE
}

fn get_unit_limit() -> u32 {
    *UNIT_LIMIT
}

pub async fn new_signed_and_send_jito_tip(
    recent_blockhash: anchor_client::solana_sdk::hash::Hash,
    keypair: &Keypair,
    mut instructions: Vec<Instruction>,
    logger: &Logger,
) -> Result<Vec<String>> {
    let start_time = Instant::now();

    let mut txs = vec![];
    let tip_account = match jito::get_tip_account() {
        Ok(account) => account,
        Err(e) => return Err(anyhow::anyhow!("Failed to get jito tip account: {}", e)),
    };

    // jito tip, the upper limit is 0.1
    let tip_lamports = ui_amount_to_amount(JITO_TIP, spl_token::native_mint::DECIMALS);

    let jito_tip_instruction =
        system_instruction::transfer(&keypair.pubkey(), &tip_account.0, tip_lamports);
    instructions.insert(0, jito_tip_instruction);

    // ADD Priority fee
    // -------------
    let unit_limit = get_unit_limit();
    let unit_price = get_unit_price();

    let modify_compute_units =
        anchor_client::solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(
            unit_limit,
        );
    let add_priority_fee =
        anchor_client::solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_price(
            unit_price,
        );
    instructions.insert(1, modify_compute_units);
    instructions.insert(2, add_priority_fee);
    
    // send init tx
    let txn = Transaction::new_signed_with_payer(
        &instructions,
        Some(&keypair.pubkey()),
        &vec![keypair],
        recent_blockhash,
    );

    // Get config to access the RPC client
    let config = Config::get().await;
    let client: Arc<anchor_client::solana_client::nonblocking::rpc_client::RpcClient> = 
        Arc::clone(&config.app_state.rpc_nonblocking_client);
    
    // Set transaction config with skip_preflight=true for faster processing
    let tx_config = anchor_client::solana_client::rpc_config::RpcSendTransactionConfig {
        skip_preflight: true,
        ..anchor_client::solana_client::rpc_config::RpcSendTransactionConfig::default()
    };
    
    // Send transaction through normal RPC client
    let tx_result = client.send_transaction_with_config(&txn, tx_config).await;
    
    match tx_result {
        Ok(signature) => {
            txs.push(signature.to_string());
            logger.log(
                format!("[TXN-ELAPSED(JITO-TIP)]: {:?}", start_time.elapsed())
                    .green()
                    .to_string(),
            );
            Ok(txs)
        }
        Err(e) => {
            // Convert the error to a Send-compatible form
            logger.log(format!("jito_tip send_transaction failed: {}", e).red().to_string());
            Err(anyhow::anyhow!("jito_tip send_transaction failed: {}", e.to_string()))
        }
    }
}

pub async fn new_signed_and_send_nozomi(
    recent_blockhash: anchor_client::solana_sdk::hash::Hash,
    keypair: &Keypair,
    mut instructions: Vec<Instruction>,
    logger: &Logger,
) -> Result<Vec<String>> {
    let start_time = Instant::now();

    let mut txs = vec![];
    let tip_account = nozomi::get_tip_account()?;
    
    let tip_lamports = ui_amount_to_amount(NOZOMI_TIP, spl_token::native_mint::DECIMALS);

    let nozomi_tip_instruction =
        system_instruction::transfer(&keypair.pubkey(), &tip_account, tip_lamports);
    instructions.insert(0, nozomi_tip_instruction);

    // ADD Priority fee
    // -------------
    let unit_limit = 200000;
    let unit_price = 20000;

    let modify_compute_units =
        anchor_client::solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(
            unit_limit,
        );
    let add_priority_fee =
        anchor_client::solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_price(
            unit_price,
        );
    instructions.insert(1, modify_compute_units);
    instructions.insert(2, add_priority_fee);

    // send init tx
    let txn = Transaction::new_signed_with_payer(
        &instructions,
        Some(&keypair.pubkey()),
        &vec![keypair],
        recent_blockhash,
    );

    let zeroslot_client = Arc::new(ZeroSlotClient::new((*nozomi::NOZOMI_URL).as_str()));
    // Store the result first to avoid capturing non-Send error types
    let tx_result = zeroslot_client.send_transaction(&txn).await;
    
    match tx_result {
        Ok(signature) => {
            txs.push(signature.to_string());
            logger.log(
                format!("[TXN-ELAPSED(NOZOMI)]: {:?}", start_time.elapsed())
                    .yellow()
                    .to_string(),
            );
        }
        Err(_) => {
            // Convert the error to a Send-compatible form
            return Err(anyhow::anyhow!("nozomi send_transaction failed"));
        }
    };

    Ok(txs)
}

pub async fn new_signed_and_send_spam(
    recent_blockhash: anchor_client::solana_sdk::hash::Hash,
    keypair: &std::sync::Arc<Keypair>,
    instructions: Vec<Instruction>,
    logger: &Logger,
) -> Result<Vec<String>> {
    // Instead of spawning tasks in separate threads, run them concurrently in the same thread
    let logger_clone = logger.clone();
    let keypair_clone = Arc::clone(keypair);
    let instructions_clone = instructions.clone();
    
    // Create futures but don't spawn them in separate threads
    let jito_future = new_signed_and_send_jito_tip(
        recent_blockhash,
        &keypair_clone,
        instructions.clone(),
        &logger_clone,
    );
    
    let nozomi_future = new_signed_and_send_nozomi_tip(
        recent_blockhash,
        &keypair_clone,
        instructions_clone.clone(),
        &logger_clone,
    );
    
    let zeroslot_future = new_signed_and_send_zeroslot_tip(
        recent_blockhash,
        &keypair_clone,
        instructions_clone,
        &logger_clone,
    );
    
    // Run all futures concurrently using futures::join
    let results = futures::future::join3(jito_future, nozomi_future, zeroslot_future).await;
    
    // Process results
    let mut successful_results = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    // Handle each result
    match results.0 {
        Ok(res) => successful_results.push(res[0].clone()),
        Err(e) => errors.push(format!("Jito tip failed: {}", e)),
    }
    
    match results.1 {
        Ok(res) => successful_results.push(res[0].clone()),
        Err(e) => errors.push(format!("Nozomi tip failed: {}", e)),
    }
    
    match results.2 {
        Ok(res) => successful_results.push(res[0].clone()),
        Err(e) => errors.push(format!("ZeroSlot tip failed: {}", e)),
    }

    // If there are any errors but we have at least one success, continue
    if successful_results.is_empty() && !errors.is_empty() {
        return Err(anyhow::anyhow!(format!("All tasks failed with these errors: {:?}", errors)));
    }

    Ok(successful_results)
}

pub async fn new_signed_and_send_nozomi_tip(
    recent_blockhash: anchor_client::solana_sdk::hash::Hash,
    keypair: &Keypair,
    mut instructions: Vec<Instruction>,
    logger: &Logger,
) -> Result<Vec<String>> {
    let start_time = Instant::now();
    let mut txs = vec![];

    // Get configuration - directly get the mutex guard
    let config = crate::common::config::Config::get().await;

    // Add compute budget instructions
    let unit_limit = get_unit_limit();
    let unit_price = get_unit_price();

    let modify_compute_units =
        anchor_client::solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(unit_limit);
    let add_priority_fee =
        anchor_client::solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_price(unit_price);
    
    instructions.insert(0, modify_compute_units);
    instructions.insert(1, add_priority_fee);

    // Add tip for Nozomi - handle errors immediately
    let tip_account = match nozomi::get_tip_account() {
        Ok(account) => account,
        Err(e) => return Err(anyhow::anyhow!("Failed to get nozomi tip account: {}", e)),
    };
    
    let tip_amount = get_nozomi_tip();
    let tip_lamports = ui_amount_to_amount(tip_amount, spl_token::native_mint::DECIMALS);
    
    let tip_instruction = system_instruction::transfer(
        &keypair.pubkey(),
        &tip_account,
        tip_lamports
    );
    instructions.push(tip_instruction);

    // Build and send transaction
    let txn = Transaction::new_signed_with_payer(
        &instructions,
        Some(&keypair.pubkey()),
        &vec![keypair],
        recent_blockhash,
    );

    // Use Nozomi client to send transaction
    let zeroslot_client = Arc::new(ZeroSlotClient::new((*nozomi::NOZOMI_URL).as_str()));
    let nozomi_result = zeroslot_client.send_transaction(&txn).await;
    
    // Process result immediately to avoid capturing non-Send errors
    match nozomi_result {
        Ok(signature) => {
            txs.push(signature.to_string());
            logger.log(
                format!("[TXN-ELAPSED(NOZOMI-TIP)]: {:?}", start_time.elapsed())
                    .green()
                    .to_string(),
            );
            return Ok(txs);
        }
        Err(e) => {
            // Log error and try fallback - Convert error to String immediately
            logger.log(format!("Nozomi send failed: {}", e).red().to_string());
            
            // Continue with fallback without the error in scope
        }
    }
    
    // Fallback to standard RPC
    let client: Arc<anchor_client::solana_client::nonblocking::rpc_client::RpcClient> = 
        Arc::clone(&config.app_state.rpc_nonblocking_client);
    
    let tx_config = anchor_client::solana_client::rpc_config::RpcSendTransactionConfig {
        skip_preflight: true,
        ..anchor_client::solana_client::rpc_config::RpcSendTransactionConfig::default()
    };
    
    // Handle potential error immediately
    let fallback_result = client.send_transaction_with_config(&txn, tx_config).await;
    match fallback_result {
        Ok(signature) => {
            txs.push(signature.to_string());
            logger.log(
                format!("[TXN-ELAPSED(NOZOMI-TIP-FALLBACK)]: {:?}", start_time.elapsed())
                    .yellow()
                    .to_string(),
            );
            Ok(txs)
        }
        Err(e) => {
            // Convert error to string immediately to make it Send-compatible
            Err(anyhow::anyhow!("fallback send_transaction failed: {}", e.to_string()))
        }
    }
}

pub async fn new_signed_and_send_zeroslot_tip(
    recent_blockhash: anchor_client::solana_sdk::hash::Hash,
    keypair: &Keypair,
    mut instructions: Vec<Instruction>,
    logger: &Logger,
) -> Result<Vec<String>> {
    let start_time = Instant::now();
    let mut txs = vec![];

    // Add compute budget instructions
    let unit_limit = get_unit_limit();
    let unit_price = get_unit_price();

    let modify_compute_units =
        anchor_client::solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(unit_limit);
    let add_priority_fee =
        anchor_client::solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_price(unit_price);
    
    instructions.insert(0, modify_compute_units);
    instructions.insert(1, add_priority_fee);

    // Add ZeroSlot tip - Handle any potential error immediately
    let tip_account = match zeroslot::get_tip_account() {
        Ok(account) => account,
        Err(e) => return Err(anyhow::anyhow!("Failed to get zeroslot tip account: {}", e)),
    };
    
    let tip_amount = match zeroslot::get_tip_value().await {
        Ok(amount) => amount,
        Err(e) => return Err(anyhow::anyhow!("Failed to get zeroslot tip value: {}", e)),
    };
    
    let tip_lamports = ui_amount_to_amount(tip_amount, spl_token::native_mint::DECIMALS);
    
    let tip_instruction = system_instruction::transfer(
        &keypair.pubkey(),
        &tip_account,
        tip_lamports
    );
    instructions.push(tip_instruction);

    // Build and send transaction
    let txn = Transaction::new_signed_with_payer(
        &instructions,
        Some(&keypair.pubkey()),
        &vec![keypair],
        recent_blockhash,
    );

    // Use ZeroSlot client to send transaction
    let zeroslot_client = Arc::new(ZeroSlotClient::new((*zeroslot::ZERO_SLOT_URL).as_str()));
    let zeroslot_result = zeroslot_client.send_transaction(&txn).await;
    
    // Process the result immediately to avoid capturing non-Send errors
    match zeroslot_result {
        Ok(signature) => {
            txs.push(signature.to_string());
            logger.log(
                format!("[TXN-ELAPSED(ZEROSLOT-TIP)]: {:?}", start_time.elapsed())
                    .green()
                    .to_string(),
            );
            return Ok(txs);
        }
        Err(e) => {
            // Log error and try fallback - Convert error to String immediately
            logger.log(format!("ZeroSlot send failed: {}", e).red().to_string());
            
            // Instead of continuing with the error in scope, we proceed with fallback
            // This ensures the error doesn't get captured in subsequent await points
        }
    }
    
    // Fallback to standard RPC - get config without having the zeroslot_result in scope
    let config = Config::get().await;
    
    let client: Arc<anchor_client::solana_client::nonblocking::rpc_client::RpcClient> = 
        Arc::clone(&config.app_state.rpc_nonblocking_client);
    
    let tx_config = anchor_client::solana_client::rpc_config::RpcSendTransactionConfig {
        skip_preflight: true,
        ..anchor_client::solana_client::rpc_config::RpcSendTransactionConfig::default()
    };
    
    // Handle potential error immediately to avoid capturing non-Send error
    let fallback_result = client.send_transaction_with_config(&txn, tx_config).await;
    match fallback_result {
        Ok(signature) => {
            txs.push(signature.to_string());
            logger.log(
                format!("[TXN-ELAPSED(ZEROSLOT-TIP-FALLBACK)]: {:?}", start_time.elapsed())
                    .yellow()
                    .to_string(),
            );
            Ok(txs)
        }
        Err(e) => {
            // Convert error to string immediately to make it Send-compatible
            Err(anyhow::anyhow!("zeroslot fallback send_transaction failed: {}", e.to_string()))
        }
    }
}
