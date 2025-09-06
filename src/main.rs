use solana_vntr_sniper::{
    shared::{config::Config, constants::RUN_MSG},
    domain::token::{TokenModel, TokenMetadata, find_pools_for_token},
    infrastructure::dex::{DEXRegistry, identify_dex_from_pool},
    application::monitoring::arbitrage_monitor,
};
use anchor_client::solana_sdk::pubkey::Pubkey;
use std::{str::FromStr, sync::Arc};
use chrono::Utc;
use tokio::time::{sleep, Duration};
use solana_vntr_sniper::shared::config::SwapConfig;
use solana_vntr_sniper::application::swapping::SwapDirection;
use solana_vntr_sniper::application::swapping::SwapInType;

#[tokio::main]
async fn main() {
    /* Initial Settings */
    let config = Config::new().await;
    let config = config.lock().await;

    /* Running Bot */
    let run_msg = RUN_MSG;
    println!("{}", run_msg);
    println!("ARBITRAGE BOT: Monitoring token prices across multiple DEXes");
    
    /* Display supported DEXes */
    let dex_registry = DEXRegistry::new();
    println!("Tracking DEXes:");
    for dex in dex_registry.get_all_dexes() {
        println!("  - {} ({})", dex.name, dex.program_id);
    }

    /* Get arbitrage settings from environment */
    let arbitrage_threshold = std::env::var("ARBITRAGE_THRESHOLD")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(1.5); // Default to 1.5% if not specified
    
    let min_liquidity = std::env::var("MIN_LIQUIDITY")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(10_000_000_000); // Default to 10 SOL if not specified
    
    /* Setup swap config for arbitrage */
    let swap_config = SwapConfig {
        swap_direction: SwapDirection::Buy,
        in_type: SwapInType::Sol,
        amount_in: 0.1, // Default to 0.1 SOL per trade
        slippage: 50, // 0.5% slippage
        use_jito: false, // Don't use Jito MEV protection by default
    };
    
    /* Start arbitrage monitor */
    println!("Starting arbitrage monitor with threshold: {}%, min liquidity: {} SOL", 
        arbitrage_threshold, min_liquidity as f64 / 1_000_000_000.0);
    
    match arbitrage_monitor(
        config.yellowstone_grpc_http.clone(),
        config.yellowstone_grpc_token.clone(),
        config.app_state.clone(),
        swap_config,
        arbitrage_threshold,
        min_liquidity,
    ).await {
        Ok(_) => println!("Arbitrage monitor completed successfully"),
        Err(e) => eprintln!("Arbitrage monitor error: {}", e),
    }
}
