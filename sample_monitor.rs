use borsh::from_slice;
use maplit::hashmap;
use anchor_client::solana_sdk::{hash::Hash, pubkey::Pubkey, signature::Signature};
use spl_token::solana_program::native_token::{lamports_to_sol, LAMPORTS_PER_SOL};
use tokio::process::Command;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::{collections::HashSet, time::Duration};

use super::swap::{SwapDirection, SwapInType};
use crate::common::config::{JUPITER_PROGRAM, OKX_DEX_PROGRAM, PROGRAM_DATA_PREFIX};
use crate::common::{
    blacklist::Blacklist,
    config::{AppState, LiquidityPool, Status, SwapConfig, PUMP_LOG_INSTRUCTION},
    logger::Logger,
};
use crate::core::tx;
use crate::dex::pump_fun::{Pump, INITIAL_VIRTUAL_SOL_RESERVES, INITIAL_VIRTUAL_TOKEN_RESERVES, PUMP_FUN_CREATE_IX_DISCRIMINATOR, PUMP_PROGRAM};
use anyhow::{anyhow, Result};
use chrono::Utc;
use colored::Colorize;
use futures_util::stream::StreamExt;
use futures_util::SinkExt;
use tokio::{
    task,
    time::{self, Instant},
};
use yellowstone_grpc_client::{ClientTlsConfig, GeyserGrpcClient};
use yellowstone_grpc_proto::geyser::{
    subscribe_update::UpdateOneof, CommitmentLevel, SubscribeRequest,
    SubscribeRequestFilterTransactions, SubscribeUpdateTransaction,
};
use std::str::FromStr;

#[derive(Clone, Debug)]
pub struct BondingCurveInfo {
    pub bonding_curve: Pubkey,
    pub new_virtual_sol_reserve: u64,
    pub new_virtual_token_reserve: u64,
}

#[derive(Clone, Debug)]
pub struct TradeInfoFromToken {
    pub slot: u64,
    pub recent_blockhash: Hash,
    pub signature: String,
    pub target: String,
    pub mint: String,
    pub bonding_curve: String,
    pub volume_change: i64,
    pub bonding_curve_info: BondingCurveInfo,
}

pub struct FilterConfig {
    program_ids: Vec<String>,
    _instruction_discriminators: &'static [&'static [u8]],
}


lazy_static::lazy_static! {
    static ref COUNTER: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
    static ref SOLD: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
}

fn increment() {
    let mut count = COUNTER.lock().unwrap();
    *count += 1;
}

fn get_count() -> u64 {
    let count = COUNTER.lock().unwrap();
    *count
}

fn incre_sold() {
    let mut count = SOLD.lock().unwrap();
    *count += 1;
}

fn get_sold() -> u64 {
    let count = SOLD.lock().unwrap();
    *count
}


impl TradeInfoFromToken {
    pub fn from_json(txn: SubscribeUpdateTransaction, log_messages: Vec<String>) -> Result<Self> {
        let slot = txn.slot;
        let (recent_blockhash, signature, target, mint, bonding_curve, volume_change, bonding_curve_info) = {
            let mut mint_flag = false;
            let mut encoded_data = String::new();

            for log in log_messages.iter() {
                if log.starts_with(PROGRAM_DATA_PREFIX) {
                    encoded_data = log.split_whitespace().nth(2).unwrap_or("").to_string();
                    mint_flag = true;
                    break;
                }
            }

            if mint_flag {
                let decoded_data = base64::decode(encoded_data).unwrap();
                let mut offset = 8;
                
                let mut name = String::new();
                let mut symbol = String::new();
                let mut uri = String::new();
                let mut mint = String::new();
                let mut bonding_curve = String::new();
                let mut target = String::new();

                // Read fields
                let fields = vec![
                    (&mut name, "string"),
                    (&mut symbol, "string"),
                    (&mut uri, "string"),
                    (&mut mint, "publicKey"),
                    (&mut bonding_curve, "publicKey"),
                    (&mut target, "publicKey"),
                ];

                for (value, field_type) in fields {
                    if field_type == "string" {
                        let length: u32 = u32::from_le_bytes(decoded_data[offset..offset + 4].try_into().unwrap());
                        offset += 4;
                        *value = String::from_utf8(decoded_data[offset..offset + length as usize].to_vec()).ok().unwrap();
                        offset += length as usize;
                    } else if field_type == "publicKey" {
                        let key_bytes: [u8; 32] = decoded_data[offset..offset + 32].try_into().expect("Slice with incorrect length");
                        *value = Pubkey::new_from_array(key_bytes).to_string(); // Convert bytes to Base58
                        offset += 32;
                    }
                }

                if let Some(transaction) = txn.transaction {
                    let signature = match Signature::try_from(transaction.signature) {
                        Ok(signature) => format!("{:?}", signature),
                        Err(_) => "".to_string(),
                    };
                    let recent_blockhash_slice = &transaction
                        .transaction
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Failed to get recent blockhash"))?
                        .message
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Failed to get recent blockhash"))?
                        .recent_blockhash;
                    let recent_blockhash = Hash::new(recent_blockhash_slice);
    
                    let account_keys = &transaction
                        .transaction
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Failed to get account keys"))?
                        .message
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Failed to get account keys"))?
                        .account_keys;
                    
                    let mut sol_post_amount = 0_u64;
                    let mut volume_change = 0_i64;    
    
                    if let Some(meta) = transaction.meta {
                        let bonding_curve_index = account_keys
                            .iter()
                            .position(|account_key| {
                                Pubkey::try_from(account_key.clone()).unwrap().to_string()
                                    == bonding_curve
                            })
                            .unwrap_or(0);
    
                        sol_post_amount = *meta
                        .post_balances
                        .get(bonding_curve_index)
                        .unwrap_or(&0_u64);
    
                        let sol_pre_amount = *meta
                            .pre_balances
                            .get(bonding_curve_index)
                            .unwrap_or(&0_u64);
    
                        volume_change = (sol_post_amount as i64) - (sol_pre_amount as i64);
                    }
    
                    let bonding_curve_pubkey = Pubkey::from_str(&bonding_curve).map_err(|_| anyhow!(""))?;
    
                    let new_virtual_sol_reserve = INITIAL_VIRTUAL_SOL_RESERVES + sol_post_amount;
                    let new_virtual_token_reserve = ((INITIAL_VIRTUAL_SOL_RESERVES as u128 * INITIAL_VIRTUAL_TOKEN_RESERVES as u128) as f64 / new_virtual_sol_reserve as f64) as u64; //formular
                    
                    let bonding_curve_info = BondingCurveInfo {
                        bonding_curve: bonding_curve_pubkey,
                        new_virtual_sol_reserve,
                        new_virtual_token_reserve,
                    };
    
                    (
                        recent_blockhash,
                        signature,
                        target,
                        mint,
                        bonding_curve,
                        volume_change,
                        bonding_curve_info,
                    )
                } else {
                    return Err(anyhow::anyhow!("Transaction is None"));
                }
            } else {
                if let Some(transaction) = txn.transaction {
                    let signature = match Signature::try_from(transaction.signature) {
                        Ok(signature) => format!("{:?}", signature),
                        Err(_) => "".to_string(),
                    };
                    let recent_blockhash_slice = &transaction
                        .transaction
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Failed to get recent blockhash"))?
                        .message
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Failed to get recent blockhash"))?
                        .recent_blockhash;
                    let recent_blockhash = Hash::new(recent_blockhash_slice);
    
                    // Retrieve Target Wallet Pubkey
                    let account_keys = &transaction
                        .transaction
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Failed to get account keys"))?
                        .message
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Failed to get account keys"))?
                        .account_keys;
    
                    let target = Pubkey::try_from(account_keys[0].clone())
                        .unwrap()
                        .to_string();
                    
                    let mut mint = String::new();
                    let mut bonding_curve = String::new();
                    let mut volume_change = 0_i64;    
                    let mut token_post_amount = 0_f64;
                    let mut sol_post_amount = 0_u64;
    
                    if let Some(meta) = transaction.meta {
                        for post_token_balance in &meta.post_token_balances {
                            let owner = &post_token_balance.owner;
                            if owner != &target {
                                bonding_curve = owner.clone();
                            }
                            if owner == &target || owner == &bonding_curve {
                                mint = post_token_balance.mint.clone();
                            }
                        }
                        if mint.is_empty() {
                            for pre_token_balance in &meta.pre_token_balances {
                                let owner = &pre_token_balance.owner;
                                if owner != &target {
                                    bonding_curve = owner.clone();
                                }
                                if owner == &target || owner == &bonding_curve {
                                    mint = pre_token_balance.mint.clone();
                                }
                            }
                        };
    
                        if mint.is_empty() {
                            return Err(anyhow::anyhow!(format!(
                                "signature[{}]: mint is None",
                                signature
                            )));
                        };
    
                        let bonding_curve_index = account_keys
                            .iter()
                            .position(|account_key| {
                                Pubkey::try_from(account_key.clone()).unwrap().to_string()
                                    == bonding_curve
                            })
                            .unwrap_or(0);
    
                        sol_post_amount = *meta
                            .post_balances
                            .get(bonding_curve_index)
                            .unwrap_or(&0_u64);
    
                        let sol_pre_amount = *meta
                            .pre_balances
                            .get(bonding_curve_index)
                            .unwrap_or(&0_u64);
    
                        token_post_amount = meta
                            .post_token_balances
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
                            .unwrap_or(0_f64);
    
                        volume_change = (sol_post_amount as i64) - (sol_pre_amount as i64);
                    }
    
                    let bonding_curve_pubkey = Pubkey::from_str(&bonding_curve).map_err(|_| anyhow!(""))?;
    
                    let new_virtual_sol_reserve = INITIAL_VIRTUAL_SOL_RESERVES + sol_post_amount;
                    let new_virtual_token_reserve = (INITIAL_VIRTUAL_TOKEN_RESERVES as f64 - (token_post_amount * 1000_000_f64)) as u64;

                    let bonding_curve_info = BondingCurveInfo {
                        bonding_curve: bonding_curve_pubkey,
                        new_virtual_sol_reserve,
                        new_virtual_token_reserve,
                    };
    
                    (
                        recent_blockhash,
                        signature,
                        target,
                        mint,
                        bonding_curve,
                        volume_change,
                        bonding_curve_info,
                    )
                } else {
                    return Err(anyhow::anyhow!("Transaction is None"));
                }
            }
        };

        Ok(Self {
            slot,
            recent_blockhash,
            signature,
            target,
            mint,
            bonding_curve,
            volume_change,
            bonding_curve_info,
        })
    }
}

pub async fn pumpfun_autosell_monitor(
    existing_liquidity_pools: Arc<Mutex<HashSet<LiquidityPool>>>,
    app_state: AppState,
    swap_config: SwapConfig,
    time_exceed: u64,
    counter_limit: u64,
) {
    let mut existing_pools = existing_liquidity_pools.lock().unwrap();
    let logger = Logger::new(
        format!("[AUTO-SELL]({}) => ", existing_pools.len())
            .yellow()
            .to_string(),
    );
    for existing_pool in existing_pools.clone().iter() {
        let timeout = Duration::from_secs(time_exceed);
        let start_time = Instant::now();
        if existing_pool.status == Status::Bought {
            if let Some(timestamp) = existing_pool.timestamp {
                if timestamp.elapsed() >= timeout {
                    // Now Auto-Sell
                    // -------------
                    logger.log(
                        format!(
                            "[DETECT-POOL]({}): Reached at selling time, Selling at {} ({:?}).",
                            existing_pool.clone().mint.clone(),
                            Utc::now(),
                            start_time.elapsed()
                        )
                        .yellow()
                        .to_string(),
                    );
                    let rpc_nonblocking_client = app_state.clone().rpc_nonblocking_client;
                    let rpc_client = app_state.clone().rpc_client;
                    let wallet = app_state.clone().wallet;
                    let swapx = Pump::new(rpc_nonblocking_client, rpc_client, wallet);
                    let sell_config = SwapConfig {
                        swap_direction: SwapDirection::Sell,
                        in_type: SwapInType::Pct,
                        amount_in: 1_f64,
                        slippage: 100_u64,
                        use_jito: swap_config.clone().use_jito,
                    };

                    // Update status into ING status..
                    let selling_pool = LiquidityPool {
                        mint: existing_pool.clone().mint,
                        buy_price: existing_pool.buy_price,
                        sell_price: existing_pool.sell_price,
                        status: Status::Selling,
                        timestamp: Some(Instant::now()),
                    };
                    existing_pools.retain(|pool| pool.mint != existing_pool.clone().mint);
                    existing_pools.insert(selling_pool.clone());

                    let swapx_clone = swapx.clone();
                    let logger_clone = logger.clone();
                    let existing_pool_clone = existing_pool.clone();
                    let sell_config_clone = sell_config.clone();
                    let mint_str = existing_pool.clone().mint;
                    let rpc_nonblocking_client_clone = app_state.clone().rpc_nonblocking_client;
                    let existing_pools_clone = Arc::clone(&existing_liquidity_pools);
                    let task = tokio::spawn(async move {
                        // Build ixn 'n Calc the Cost of token buying
                        // -------------------
                        match swapx_clone
                            .build_swap_ixn_by_mint(&mint_str, None, sell_config_clone, start_time)
                            .await
                        {
                            Ok(result) => {
                                // Send Instructions and Confirm
                                // -------------------
                                let (keypair, instructions, token_price) =
                                    (result.0, result.1, result.2);
                                let recent_blockhash = rpc_nonblocking_client_clone
                                    .get_latest_blockhash()
                                    .await
                                    .unwrap();
                                match tx::new_signed_and_send_nozomi(
                                    recent_blockhash,
                                    &keypair,
                                    instructions,
                                    &logger_clone,
                                )
                                .await
                                {
                                    Ok(res) => {
                                        let sold_pool = LiquidityPool {
                                            mint: mint_str.clone(),
                                            buy_price: existing_pool_clone.buy_price,
                                            sell_price: token_price,
                                            status: Status::Sold,
                                            timestamp: Some(Instant::now()),
                                        };
                                        let mut update_pools = existing_pools_clone.lock().unwrap();
                                        update_pools.retain(|pool| pool.mint != mint_str);
                                        update_pools.insert(sold_pool.clone());
                                        logger_clone.log(format!(
                                            "\n\t * [SUCCESSFUL-SELL] => TX_HASH: (https://solscan.io/tx/{}) \n\t * [POOL] => ({}) \n\t * [SOLD] => {} :: ({:?}).",
                                            &res[0], mint_str, Utc::now(), start_time.elapsed()
                                        ).green().to_string());
                                        incre_sold();
                                        let sold = get_sold();
                                        if (counter_limit != 0) && (sold >= counter_limit) {
                                            println!(
                                                "Counter is reached {}. Stopping process",
                                                sold
                                            );
                                            let _ = Command::new("pm2")
                                                .arg("stop")
                                                .arg("solana-vntr-sniper")
                                                .status();
                                        }
                                    }
                                    Err(_e) => {
                                        // logger_clone.log(
                                        //     format!("Skip {}: {}", mint_str.clone(), e)
                                        //         .red()
                                        //         .italic()
                                        //         .to_string(),
                                        // );
                                        let bought_pool = LiquidityPool {
                                            mint: mint_str.clone(),
                                            buy_price: existing_pool_clone.buy_price,
                                            sell_price: existing_pool_clone.sell_price,
                                            status: Status::Bought,
                                            timestamp: Some(Instant::now()),
                                        };
                                        let mut update_pools = existing_pools_clone.lock().unwrap();
                                        update_pools.retain(|pool| pool.mint != mint_str);
                                        update_pools.insert(bought_pool.clone());
                                    }
                                };
                            }
                            Err(error) => {
                                // logger_clone.log(
                                //     format!("Skip {} by {}", mint_str.clone(), error)
                                //         .red()
                                //         .italic()
                                //         .to_string(),
                                // );
                                let bought_pool = LiquidityPool {
                                    mint: mint_str.clone(),
                                    buy_price: existing_pool_clone.buy_price,
                                    sell_price: existing_pool_clone.sell_price,
                                    status: Status::Bought,
                                    timestamp: Some(Instant::now()),
                                };
                                let mut update_pools = existing_pools_clone.lock().unwrap();
                                update_pools.retain(|pool| pool.mint != mint_str);
                                update_pools.insert(bought_pool.clone());
                            }
                        };
                    });
                    drop(task);
                }
            }
        }
    }
}

async fn run_at_intervals(
    existing_liquidity_pools: Arc<Mutex<HashSet<LiquidityPool>>>,
    app_state: AppState,
    swap_config: SwapConfig,
    time_exceed: u64,
    counter_limit: u64,
) {
    let mut interval = time::interval(Duration::from_millis(500));

    loop {
        interval.tick().await;

        pumpfun_autosell_monitor(
            Arc::clone(&existing_liquidity_pools),
            app_state.clone(),
            swap_config.clone(),
            time_exceed,
            counter_limit,
        )
        .await;
    }
}

pub async fn new_token_trader_pumpfun(
    yellowstone_grpc_http: String,
    yellowstone_grpc_token: String,
    app_state: AppState,
    swap_config: SwapConfig,
    _blacklist: Blacklist,
    time_exceed: u64,
    counter_limit: u64,
    min_dev_buy: u64,
    max_dev_buy: u64,
) -> Result<(), String> {
    // INITIAL SETTING FOR SUBSCIBE
    // -----------------------------------------------------------------------------------------------------------------------------
    let mut client = GeyserGrpcClient::build_from_shared(yellowstone_grpc_http)
        .map_err(|e| format!("Failed to build client: {}", e))?
        .x_token::<String>(Some(yellowstone_grpc_token))
        .map_err(|e| format!("Failed to set x_token: {}", e))?
        .tls_config(ClientTlsConfig::new().with_native_roots())
        .map_err(|e| format!("Failed to set tls config: {}", e))?
        .connect()
        .await
        .map_err(|e| format!("Failed to connect: {}", e))?;

    // HEALTH CHECK
    // -----------------
    // match client.health_check().await {
    //     Ok(health) => {
    //         println!("Health check: {:#?}", health.status);
    //     }
    //     Err(err) => {
    //         println!("Error in Health check: {:#?}", err);
    //     }
    // };

    // SUBSCRIBE
    // -----------------
    let (mut subscribe_tx, mut stream) = client
        .subscribe()
        .await
        .map_err(|e| format!("Failed to subscribe: {}", e))?;

    let filter_config = FilterConfig {
        program_ids: vec![PUMP_PROGRAM.to_string()],
        _instruction_discriminators: &[PUMP_FUN_CREATE_IX_DISCRIMINATOR],
    };

    subscribe_tx
        .send(SubscribeRequest {
            slots: HashMap::new(),
            accounts: HashMap::new(),
            transactions: hashmap! {
                "All".to_owned() => SubscribeRequestFilterTransactions {
                    vote: None,
                    failed: Some(false),
                    signature: None,
                    account_include: filter_config.program_ids,
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

    let rpc_nonblocking_client = app_state.clone().rpc_nonblocking_client;
    let rpc_client = app_state.clone().rpc_client;
    let wallet = app_state.clone().wallet;
    let swapx = Pump::new(
        rpc_nonblocking_client.clone(),
        rpc_client.clone(),
        wallet.clone(),
    );

    let logger = Logger::new("[PUMPFUN-MONITOR] => ".blue().bold().to_string());
    logger.log("[STARTED. MONITORING]...".blue().bold().to_string());

    // CHECK PT/SL TO AUTOSELL
    // ---------------
    let check_pt_sl_autosell_handler = task::spawn(run_at_intervals(
        Arc::clone(&existing_liquidity_pools),
        app_state.clone(),
        swap_config.clone(),
        time_exceed,
        counter_limit,
    ));
    check_pt_sl_autosell_handler;

    // NOW SUBSCRIBE
    // -----------------------------------------------------------------------------------------------------------------------------
    while let Some(message) = stream.next().await {
        match message {
            Ok(msg) => {
                if let Some(UpdateOneof::Transaction(txn)) = msg.update_oneof {
                    let start_time = Instant::now();
                    if let Some(log_messages) = txn
                        .clone()
                        .transaction
                        .and_then(|txn1| txn1.meta)
                        .map(|meta| meta.log_messages)
                    {
                        let mut mint_flag = false;
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
                        // CHECK BLACKLIST
                        // -----------------
                        // if blacklist.is_blacklisted(&trade_info.target) {
                        //     logger.log(format!(
                        //         "\n\t * [BLACKLIST-NOTIFICATION] => (https://solscan.io/tx/{}) \n\t * [SIGNER] => ({}) \n\t * [DETECT] => ({}) \n\t * [TIMESTAMP] => {} :: ({:?}).",
                        //         trade_info.signature,
                        //         trade_info.target,
                        //         trade_info.mint,
                        //         Utc::now(),
                        //         start_time.elapsed(),
                        //     ).yellow().to_string());
                        //     continue;
                        // }

                        for log_message in log_messages.iter() {
                            if log_message.contains(PUMP_LOG_INSTRUCTION) {
                                //TODO: Add the condition that filters the token.
                                // ---------------

                                // NEW POOL DETECT!
                                // ---------------
                                logger.log(format!(
                                    "\n\t * [NEW POOL|BUY] => (https://solscan.io/tx/{}) - SLOT:({}) \n\t * [DETECT] => ({}) \n\t * [DEV BUY AMOUNT] => ({})  * [TIMESTAMP] => {} :: ({:?}).",
                                    trade_info.signature,
                                    trade_info.slot,
                                    trade_info.mint,
                                    lamports_to_sol(trade_info.volume_change as u64),
                                    Utc::now(),
                                    start_time.elapsed(),
                                ).yellow().to_string());
                                // println!("**>>{:#?}", trade_info);
                                mint_flag = true;
                                break;
                            }
                        }
                        if mint_flag {
                            // NOW BUY!
                            // Update into ...ing status
                            // ---------------
                            let counter = get_count();

                            if counter < counter_limit {

                                let dev_buy_amount = lamports_to_sol(trade_info.volume_change as u64);

                                // Check dev buy amount
                                if dev_buy_amount < min_dev_buy as f64 {
                                    logger.log(format!(
                                        "\n\t * [DEV BOUGHT LESS THAN] => {} > {}",
                                        min_dev_buy, dev_buy_amount
                                    ).yellow().to_string());
                                    continue;
                                } else if dev_buy_amount > max_dev_buy as f64 {
                                    logger.log(format!(
                                        "\n\t * [DEV BOUGHT MORE THAN] => {} < {}",
                                        max_dev_buy, dev_buy_amount
                                    ).yellow().to_string());
                                    continue;
                                }

                                let buying_pool = LiquidityPool {
                                    mint: trade_info.clone().mint,
                                    buy_price: 0_f64,
                                    sell_price: 0_f64,
                                    status: Status::Buying,
                                    timestamp: None,
                                };
                                let mut existing_pools = existing_liquidity_pools.lock().unwrap();
                                existing_pools.insert(buying_pool.clone());
    
                                // Buy token 'n Calc the Cost of token buying
                                // -------------------
                                let swapx_clone = swapx.clone();
                                let logger_clone = logger.clone();
                                let swap_config_clone = swap_config.clone();
                                let mint_str = trade_info.mint.clone();
                                let bonding_curve_info = trade_info.bonding_curve_info.clone();
                                let existing_liquidity_pools_clone =
                                    Arc::clone(&existing_liquidity_pools);
                                // logger_clone.log(
                                //     format!(
                                //         "[BUILD-IXN]({}) - {} :: {:?}",
                                //         mint_str.clone(),
                                //         Utc::now(),
                                //         start_time.elapsed()
                                //     )
                                //     .yellow()
                                //     .to_string(),
                                // );
                                let recent_blockhash = trade_info.clone().recent_blockhash;
    
                                let task = tokio::spawn(async move {
                                    // Build ixn 'n Calc the Cost of token buying
                                    // -------------------
                                    match swapx_clone
                                        .build_swap_ixn_by_mint(
                                            &mint_str,
                                            Some(bonding_curve_info),
                                            swap_config_clone,
                                            start_time,
                                        )
                                        .await
                                    {
                                        Ok(result) => {
                                            // Send Instructions and Confirm
                                            // -------------------
                                            let (keypair, instructions, token_price) =
                                                (result.0, result.1, result.2);
                                            // logger_clone.log(
                                            //     format!(
                                            //         "[CONFIRM-TXN]({}) - {} :: {:?}",
                                            //         mint_str.clone(),
                                            //         Utc::now(),
                                            //         start_time.elapsed()
                                            //     )
                                            //     .yellow()
                                            //     .to_string(),
                                            // );
    
                                            match tx::new_signed_and_send_nozomi(
                                                recent_blockhash,
                                                &keypair,
                                                instructions,
                                                &logger_clone,
                                            )
                                            .await
                                            {
                                                Ok(res) => {
                                                    let bought_pool = LiquidityPool {
                                                        mint: mint_str.clone(),
                                                        buy_price: token_price,
                                                        sell_price: 0_f64,
                                                        status: Status::Bought,
                                                        timestamp: Some(Instant::now()),
                                                    };
                                                    let mut existing_pools =
                                                        existing_liquidity_pools_clone.lock().unwrap();
                                                    existing_pools.retain(|pool| pool.mint != mint_str);
                                                    existing_pools.insert(bought_pool.clone());
                                                    increment();
                                                    logger_clone.log(format!(
                                                        "\n\t * [SUCCESSFUL-BUY] => TX_HASH: (https://solscan.io/tx/{}) 
                            * [POOL] => ({}) 
                            * [DONE] => {} :: ({:?}).",
                                                        &res[0], mint_str, Utc::now(), start_time.elapsed()
                                                    ).green().to_string());
                                                }
                                                Err(e) => {
                                                    logger_clone.log(
                                                        format!("Skip {}: {}", mint_str.clone(), e)
                                                            .red()
                                                            .italic()
                                                            .to_string(),
                                                    );
                                                    let failed_pool = LiquidityPool {
                                                        mint: mint_str.clone(),
                                                        buy_price: 0_f64,
                                                        sell_price: 0_f64,
                                                        status: Status::Failure,
                                                        timestamp: None,
                                                    };
                                                    let mut update_pools =
                                                        existing_liquidity_pools_clone.lock().unwrap();
                                                    update_pools.retain(|pool| pool.mint != mint_str);
                                                    update_pools.insert(failed_pool.clone());
                                                }
                                            };
                                        }
                                        Err(error) => {
                                            logger_clone.log(
                                                format!("Skip {} by {}", mint_str.clone(), error)
                                                    .red()
                                                    .italic()
                                                    .to_string(),
                                            );
                                            let failed_pool = LiquidityPool {
                                                mint: mint_str.clone(),
                                                buy_price: 0_f64,
                                                sell_price: 0_f64,
                                                status: Status::Failure,
                                                timestamp: None,
                                            };
                                            let mut update_pools =
                                                existing_liquidity_pools_clone.lock().unwrap();
                                            update_pools.retain(|pool| pool.mint != mint_str);
                                            update_pools.insert(failed_pool.clone());
                                        }
                                    };
                                });
                                drop(task);
                            }

                        } 
                        // else {
                        //     // CHECK IF `trade_info.mint` EXISTS IN 'existing_liquidity_pool'.
                        //     // --------------------------------------------------
                        //     let mut existing_pools = existing_liquidity_pools.lock().unwrap();
                        //     if let Some(existing_pool) = existing_pools
                        //         .clone()
                        //         .iter()
                        //         .find(|pool| pool.mint == trade_info.clone().mint)
                        //     {
                        //         if existing_pool.status == Status::Bought {
                        //             // Sync `volume` 'n `txn_num` | Update `timestamp`
                        //             // Check `volume_change`
                        //             // --------------------
                        //             // logger.log(format!(
                        //             //     "[VOLUME-CHECK]({}) => {}",
                        //             //     trade_info.mint, trade_info.volume_change
                        //             // ));
                        //             // Update into ...ing status
                        //             // --------------------
                        //             if trade_info.volume_change <= 0 {
                        //                 let check_pool = LiquidityPool {
                        //                     mint: existing_pool.clone().mint,
                        //                     buy_price: existing_pool.buy_price,
                        //                     sell_price: 0_f64,
                        //                     status: Status::Checking,
                        //                     timestamp: existing_pool.timestamp,
                        //                 };
                        //                 existing_pools
                        //                     .retain(|pool| pool.mint != existing_pool.clone().mint);
                        //                 existing_pools.insert(check_pool.clone());
                        //             } else {
                        //                 let check_pool = LiquidityPool {
                        //                     mint: existing_pool.clone().mint,
                        //                     buy_price: existing_pool.buy_price,
                        //                     sell_price: 0_f64,
                        //                     status: Status::Checking,
                        //                     timestamp: Some(Instant::now()),
                        //                 };
                        //                 existing_pools
                        //                     .retain(|pool| pool.mint != existing_pool.clone().mint);
                        //                 existing_pools.insert(check_pool.clone());
                        //             }

                        //             // CHECK|SELL TOKEN: BY PT/SL
                        //             // -------------------
                        //             let swapx_clone = swapx.clone();
                        //             let logger_clone = logger.clone();
                        //             let sell_config = SwapConfig {
                        //                 swap_direction: SwapDirection::Sell,
                        //                 in_type: SwapInType::Pct,
                        //                 amount_in: 1_f64,
                        //                 slippage: 100_u64,
                        //                 use_jito: swap_config.clone().use_jito,
                        //             };
                        //             let mint_str = existing_pool.clone().mint;
                        //             let recent_blockhash = trade_info.clone().recent_blockhash;

                        //             let existing_pool_clone = existing_pool.clone();
                        //             let existing_liquidity_pools_clone =
                        //                 Arc::clone(&existing_liquidity_pools);
                        //             let task = tokio::spawn(async move {
                        //                 // Build Ixn 'n Calc the Cost of token buying
                        //                 // -------------------
                        //                 match swapx_clone
                        //                     .build_swap_ixn_by_mint(
                        //                         &mint_str,
                        //                         None,
                        //                         sell_config,
                        //                         start_time,
                        //                     )
                        //                     .await
                        //                 {
                        //                     Ok(result) => {
                        //                         // Check TP/SL
                        //                         // -------------------
                        //                         let (keypair, instructions, token_price) =
                        //                             (result.0, result.1, result.2);
                        //                         let profit_rate =
                        //                             token_price / existing_pool_clone.buy_price;
                        //                         // logger_clone.log(format!(
                        //                         //     "[TP/SL-CHECK]:{} => {} :: {}",
                        //                         //     profit_rate,
                        //                         //     token_price,
                        //                         //     existing_pool_clone.buy_price
                        //                         // ));
                        //                         if profit_rate >= take_profit
                        //                             || profit_rate <= stop_loss
                        //                         {
                        //                             // Send Instructions and Confirm
                        //                             // -------------------
                        //                             match tx::new_signed_and_send_nozomi(
                        //                                 recent_blockhash,
                        //                                 &keypair,
                        //                                 instructions,
                        //                                 &logger_clone,
                        //                             )
                        //                             .await
                        //                             {
                        //                                 Ok(res) => {
                        //                                     let sold_pool = LiquidityPool {
                        //                                         mint: mint_str.clone(),
                        //                                         buy_price: existing_pool_clone
                        //                                             .buy_price,
                        //                                         sell_price: token_price,
                        //                                         status: Status::Sold,
                        //                                         timestamp: Some(Instant::now()),
                        //                                     };
                        //                                     let mut update_pools =
                        //                                         existing_liquidity_pools_clone
                        //                                             .lock()
                        //                                             .unwrap();
                        //                                     update_pools.retain(|pool| {
                        //                                         pool.mint != mint_str
                        //                                     });
                        //                                     update_pools.insert(sold_pool.clone());
                        //                                     logger_clone.log(format!(
                        //                                         "\n\t * [SUCCESSFUL-SELL] => TX_HASH: (https://solscan.io/tx/{}) \n\t * [POOL] => ({}) \n\t * [SOLD] => {} :: ({:?}).",
                        //                                         &res[0], mint_str, Utc::now(), start_time.elapsed()
                        //                                     ).green().to_string());
                        //                                 }
                        //                                 Err(e) => {
                        //                                     logger_clone.log(
                        //                                         format!(
                        //                                             "Skip {}: {}",
                        //                                             mint_str.clone(),
                        //                                             e
                        //                                         )
                        //                                         .red()
                        //                                         .italic()
                        //                                         .to_string(),
                        //                                     );
                        //                                     let bought_pool = LiquidityPool {
                        //                                         mint: mint_str.clone(),
                        //                                         buy_price: existing_pool_clone
                        //                                             .buy_price,
                        //                                         sell_price: existing_pool_clone
                        //                                             .sell_price,
                        //                                         status: Status::Bought,
                        //                                         timestamp: existing_pool_clone
                        //                                             .timestamp,
                        //                                     };
                        //                                     let mut update_pools =
                        //                                         existing_liquidity_pools_clone
                        //                                             .lock()
                        //                                             .unwrap();
                        //                                     update_pools.retain(|pool| {
                        //                                         pool.mint != mint_str
                        //                                     });
                        //                                     update_pools
                        //                                         .insert(bought_pool.clone());
                        //                                 }
                        //                             };
                        //                         } else {
                        //                             let bought_pool = LiquidityPool {
                        //                                 mint: mint_str.clone(),
                        //                                 buy_price: existing_pool_clone.buy_price,
                        //                                 sell_price: existing_pool_clone.sell_price,
                        //                                 status: Status::Bought,
                        //                                 timestamp: existing_pool_clone.timestamp,
                        //                             };
                        //                             let mut update_pools =
                        //                                 existing_liquidity_pools_clone
                        //                                     .lock()
                        //                                     .unwrap();
                        //                             update_pools
                        //                                 .retain(|pool| pool.mint != mint_str);
                        //                             update_pools.insert(bought_pool.clone());
                        //                         }
                        //                     }
                        //                     Err(_) => {
                        //                         // Skip checking TP/SL
                        //                         // -------------------
                        //                         let bought_pool = LiquidityPool {
                        //                             mint: mint_str.clone(),
                        //                             buy_price: existing_pool_clone.buy_price,
                        //                             sell_price: existing_pool_clone.sell_price,
                        //                             status: Status::Bought,
                        //                             timestamp: existing_pool_clone.timestamp,
                        //                         };
                        //                         let mut update_pools =
                        //                             existing_liquidity_pools_clone.lock().unwrap();
                        //                         update_pools.retain(|pool| pool.mint != mint_str);
                        //                         update_pools.insert(bought_pool.clone());
                        //                     }
                        //                 };
                        //             });
                        //             drop(task);
                        //         } else {
                        //             // Already Sold or Failed Pool, Skipping at {existing_pool.mint}.
                        //             // ---------------------------------------
                        //         }
                        //     }
                        // }
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