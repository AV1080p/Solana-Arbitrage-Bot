use anchor_lang::AccountDeserialize;
use anyhow::{Result, anyhow};
use base64::{prelude::BASE64_STANDARD, Engine};
use solana_account_decoder::UiAccountEncoding;
use solana_client::{
    rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    rpc_filter::{RpcFilterType, Memcmp, MemcmpEncodedBytes},
};
use anchor_client::solana_sdk::{pubkey::Pubkey, account::Account};
use std::{collections::HashMap, fs::{self, File}, path::Path, io::{Write, Read}, sync::{Arc, Mutex}};
use serde::{Serialize, Deserialize};

use crate::dex::dex_registry::DEXRegistry;

/// Structure to store pool information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolInfo {
    pub pool_id: String,
    pub dex_name: String,
    pub base_mint: String,
    pub quote_mint: String,
    pub last_known_price: Option<f64>,
    pub last_updated: Option<i64>,
    pub liquidity: Option<u64>,
}

/// Cache for token pools across different DEXes
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PoolCache {
    pub pools: HashMap<String, Vec<PoolInfo>>, // token_mint -> pools
    pub last_updated: Option<i64>,
}

impl PoolCache {
    /// Create a new empty pool cache
    pub fn new() -> Self {
        Self {
            pools: HashMap::new(),
            last_updated: None,
        }
    }

    /// Load pool cache from file
    pub fn load(path: &str) -> Result<Self> {
        if Path::new(path).exists() {
            let file_content = fs::read_to_string(path)?;
            let cache: PoolCache = serde_json::from_str(&file_content)?;
            Ok(cache)
        } else {
            Ok(Self::new())
        }
    }

    /// Save pool cache to file
    pub fn save(&self, path: &str) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        let mut file = File::create(path)?;
        file.write_all(json.as_bytes())?;
        Ok(())
    }

    /// Add a pool to the cache
    pub fn add_pool(&mut self, token_mint: &str, pool_info: PoolInfo) {
        let pools = self.pools.entry(token_mint.to_string()).or_insert_with(Vec::new);
        
        // Check if pool already exists, update it if it does
        let mut found = false;
        for existing_pool in pools.iter_mut() {
            if existing_pool.pool_id == pool_info.pool_id {
                *existing_pool = pool_info.clone();
                found = true;
                break;
            }
        }
        
        // Add new pool if not found
        if !found {
            pools.push(pool_info);
        }
        
        // Update last_updated timestamp
        self.last_updated = Some(chrono::Utc::now().timestamp());
    }

    /// Get pools for a token
    pub fn get_pools_for_token(&self, token_mint: &str) -> Option<&Vec<PoolInfo>> {
        self.pools.get(token_mint)
    }

    /// Get all token mints in the cache
    pub fn get_all_token_mints(&self) -> Vec<String> {
        self.pools.keys().cloned().collect()
    }
}

/// In-memory pool cache with thread-safe access
pub struct PoolCacheManager {
    cache: Arc<Mutex<PoolCache>>,
    file_path: String,
}

impl PoolCacheManager {
    /// Create a new pool cache manager
    pub fn new(file_path: &str) -> Result<Self> {
        let cache = PoolCache::load(file_path)?;
        Ok(Self {
            cache: Arc::new(Mutex::new(cache)),
            file_path: file_path.to_string(),
        })
    }

    /// Get a clone of the current cache
    pub fn get_cache(&self) -> Result<PoolCache> {
        let cache = self.cache.lock().map_err(|_| anyhow!("Failed to lock cache"))?;
        Ok(cache.clone())
    }

    /// Add a pool to the cache and save to disk
    pub fn add_pool(&self, token_mint: &str, pool_info: PoolInfo) -> Result<()> {
        let mut cache = self.cache.lock().map_err(|_| anyhow!("Failed to lock cache"))?;
        cache.add_pool(token_mint, pool_info);
        cache.save(&self.file_path)?;
        Ok(())
    }

    /// Update price information for a pool
    pub fn update_pool_price(&self, token_mint: &str, pool_id: &str, price: f64, liquidity: u64) -> Result<()> {
        let mut cache = self.cache.lock().map_err(|_| anyhow!("Failed to lock cache"))?;
        
        if let Some(pools) = cache.pools.get_mut(token_mint) {
            for pool in pools.iter_mut() {
                if pool.pool_id == pool_id {
                    pool.last_known_price = Some(price);
                    pool.last_updated = Some(chrono::Utc::now().timestamp());
                    pool.liquidity = Some(liquidity);
                    break;
                }
            }
        }
        
        cache.save(&self.file_path)?;
        Ok(())
    }
}

/// Discover pools for a token across all supported DEXes
pub async fn discover_pools_for_token(
    rpc_client: &RpcClient, 
    token_mint: &Pubkey,
) -> Result<Vec<PoolInfo>> {
    let mut pools = Vec::new();
    let dex_registry = DEXRegistry::new();
    
    for dex in dex_registry.get_all_dexes() {
        println!("Searching for {} pools for token {}", dex.name, token_mint);
        
        // Get the offset for the token mint in the pool account data
        // This is DEX-specific and would need to be adjusted for each DEX
        let offset = match dex.name.as_str() {
            "pumpswap" => 8, // Example offset, would need actual value
            "raydium_amm" => 200, // Example offset
            "raydium_clmm" => 300, // Example offset
            "raydium_cpmm" => 100, // Example offset
            "whirlpool" => 200, // Example offset
            "meteora_dlmm" => 250, // Example offset
            "meteora_pools" => 150, // Example offset
            _ => continue, // Skip if offset is unknown
        };
        
        // Create filter to find pools containing the token mint
        let filters = vec![
            RpcFilterType::DataSize(dex.pool_account_size as u64),
            RpcFilterType::Memcmp(Memcmp::new_base58_encoded(offset, &token_mint.to_string())),
        ];
        
        // Query for pools
        match get_program_accounts_with_filters(rpc_client, dex.program_id, Some(filters)) {
            Ok(accounts) => {
                for (pubkey, _account) in accounts {
                    // Here we would parse the account data to extract more information
                    // For now, we'll just create a basic PoolInfo
                    let pool_info = PoolInfo {
                        pool_id: pubkey.to_string(),
                        dex_name: dex.name.clone(),
                        base_mint: token_mint.to_string(),
                        quote_mint: "11111111111111111111111111111111".to_string(), // Placeholder, would extract from account data
                        last_known_price: None,
                        last_updated: None,
                        liquidity: None,
                    };
                    
                    pools.push(pool_info);
                    println!("Found pool {} on {}", pubkey, dex.name);
                }
            },
            Err(e) => {
                println!("Error discovering pools for {} on {}: {}", token_mint, dex.name, e);
            }
        }
    }
    
    Ok(pools)
}

/// Helper function to get program accounts with filters
pub fn get_program_accounts_with_filters(
    client: &RpcClient,
    program: Pubkey,
    filters: Option<Vec<RpcFilterType>>,
) -> Result<Vec<(Pubkey, Account)>> {
    let accounts = client
        .get_program_accounts_with_config(
            &program,
            RpcProgramAccountsConfig {
                filters,
                account_config: RpcAccountInfoConfig {
                    encoding: Some(UiAccountEncoding::Base64Zstd),
                    ..RpcAccountInfoConfig::default()
                },
                with_context: Some(false),
            },
        )?;
    Ok(accounts)
}

/// Function to initialize pool cache for a list of token mints
pub async fn initialize_pool_cache(
    rpc_client: &RpcClient,
    token_mints: &[Pubkey],
    cache_manager: &PoolCacheManager,
) -> Result<()> {
    for token_mint in token_mints {
        println!("Discovering pools for token {}", token_mint);
        let pools = discover_pools_for_token(rpc_client, token_mint).await?;
        
        for pool in pools {
            cache_manager.add_pool(&token_mint.to_string(), pool)?;
        }
    }
    
    Ok(())
} 