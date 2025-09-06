use std::collections::HashMap;
use anchor_client::solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use anyhow::Result;
use std::sync::Arc;

/// DEX represents a decentralized exchange on Solana
#[derive(Debug, Clone)]
pub struct DEX {
    /// Name of the DEX
    pub name: String,
    /// Program ID of the DEX
    pub program_id: Pubkey,
    /// Size of pool accounts for this DEX (used for filtering)
    pub pool_account_size: usize,
    /// Whether this DEX uses constant product AMM
    pub is_constant_product: bool,
    /// Whether this DEX uses stable curve
    pub is_stable_curve: bool,
    /// Whether this DEX uses concentrated liquidity
    pub is_concentrated_liquidity: bool,
}

/// Registry of all supported DEXes
#[derive(Debug)]
pub struct DEXRegistry {
    dexes: HashMap<String, DEX>,
}

impl DEXRegistry {
    /// Create a new DEX registry with default supported DEXes
    pub fn new() -> Self {
        let mut registry = Self {
            dexes: HashMap::new(),
        };
        
        // Register known DEXes
        registry.register_pumpswap();
        registry.register_raydium_amm();
        registry.register_raydium_clmm();
        registry.register_raydium_cpmm();
        registry.register_orca_whirlpool();
        registry.register_meteora_dlmm();
        registry.register_meteora_pools();
        
        registry
    }
    
    /// Register PumpSwap DEX
    fn register_pumpswap(&mut self) {
        let dex = DEX {
            name: "pumpswap".to_string(),
            program_id: Pubkey::from_str("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA").unwrap(),
            pool_account_size: 300, // PUMP_SWAP_POOL_SIZE
            is_constant_product: true,
            is_stable_curve: false,
            is_concentrated_liquidity: false,
        };
        
        self.dexes.insert(dex.name.clone(), dex);
    }
    
    /// Register Raydium AMM DEX
    fn register_raydium_amm(&mut self) {
        let dex = DEX {
            name: "raydium_amm".to_string(),
            program_id: Pubkey::from_str("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8").unwrap(),
            pool_account_size: 752, // RAYDIUM_AMM_POOL_SIZE
            is_constant_product: true,
            is_stable_curve: false,
            is_concentrated_liquidity: false,
        };
        
        self.dexes.insert(dex.name.clone(), dex);
    }
    
    /// Register Raydium CLMM DEX
    fn register_raydium_clmm(&mut self) {
        let dex = DEX {
            name: "raydium_clmm".to_string(),
            program_id: Pubkey::from_str("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK").unwrap(),
            pool_account_size: 1544, // RAYDIUM_CLMM_POOL_SIZE
            is_constant_product: false,
            is_stable_curve: false,
            is_concentrated_liquidity: true,
        };
        
        self.dexes.insert(dex.name.clone(), dex);
    }
    
    /// Register Raydium CPMM DEX
    fn register_raydium_cpmm(&mut self) {
        let dex = DEX {
            name: "raydium_cpmm".to_string(),
            program_id: Pubkey::from_str("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C").unwrap(),
            pool_account_size: 637, // RAYDIUM_CPMM_POOL_SIZE
            is_constant_product: true,
            is_stable_curve: false,
            is_concentrated_liquidity: false,
        };
        
        self.dexes.insert(dex.name.clone(), dex);
    }
    
    /// Register Orca Whirlpool DEX
    fn register_orca_whirlpool(&mut self) {
        let dex = DEX {
            name: "whirlpool".to_string(),
            program_id: Pubkey::from_str("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc").unwrap(),
            pool_account_size: 653, // WHIRLPOOLS_POOL_SIZE
            is_constant_product: false,
            is_stable_curve: false,
            is_concentrated_liquidity: true,
        };
        
        self.dexes.insert(dex.name.clone(), dex);
    }
    
    /// Register Meteora DLMM DEX
    fn register_meteora_dlmm(&mut self) {
        let dex = DEX {
            name: "meteora_dlmm".to_string(),
            program_id: Pubkey::from_str("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo").unwrap(),
            pool_account_size: 904, // METEORA_DLMM_POOL_SIZE
            is_constant_product: false,
            is_stable_curve: false,
            is_concentrated_liquidity: true,
        };
        
        self.dexes.insert(dex.name.clone(), dex);
    }
    
    /// Register Meteora Pools DEX
    fn register_meteora_pools(&mut self) {
        let dex = DEX {
            name: "meteora_pools".to_string(),
            program_id: Pubkey::from_str("Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB").unwrap(),
            pool_account_size: 944, // METEORA_POOL_SIZE
            is_constant_product: false,
            is_stable_curve: true,
            is_concentrated_liquidity: false,
        };
        
        self.dexes.insert(dex.name.clone(), dex);
    }
    
    /// Register a new DEX
    pub fn register_dex(&mut self, dex: DEX) {
        self.dexes.insert(dex.name.clone(), dex);
    }
    
    /// Get a DEX by name
    pub fn get_dex(&self, name: &str) -> Option<&DEX> {
        self.dexes.get(name)
    }
    
    /// Get all registered DEXes
    pub fn get_all_dexes(&self) -> Vec<&DEX> {
        self.dexes.values().collect()
    }
    
    /// Get all DEXes that use constant product AMM
    pub fn get_constant_product_dexes(&self) -> Vec<&DEX> {
        self.dexes.values().filter(|dex| dex.is_constant_product).collect()
    }
    
    /// Get all DEXes that use stable curves
    pub fn get_stable_curve_dexes(&self) -> Vec<&DEX> {
        self.dexes.values().filter(|dex| dex.is_stable_curve).collect()
    }
    
    /// Get all DEXes that use concentrated liquidity
    pub fn get_concentrated_liquidity_dexes(&self) -> Vec<&DEX> {
        self.dexes.values().filter(|dex| dex.is_concentrated_liquidity).collect()
    }
    
    /// Find a DEX by program ID
    pub fn find_dex_by_program_id(&self, program_id: &Pubkey) -> Option<&DEX> {
        self.dexes.values().find(|dex| dex.program_id == *program_id)
    }
}

/// Helper function to identify which DEX a pool belongs to
pub async fn identify_dex_from_pool(
    client: Arc<anchor_client::solana_client::nonblocking::rpc_client::RpcClient>,
    pool_address: &Pubkey,
) -> Result<Option<String>> {
    use anchor_client::solana_client::client_error::ClientError;
    
    // Get the account info to determine the program that owns it
    let account_info = match client.get_account(pool_address).await {
        Ok(info) => info,
        Err(e) => {
            // Check if it's an account not found error
            if e.to_string().contains("AccountNotFound") {
                return Ok(None);
            }
            return Err(e.into());
        }
    };
    
    // Create a registry to check against
    let registry = DEXRegistry::new();
    
    // Check if the pool's owner matches any known DEX program
    if let Some(dex) = registry.find_dex_by_program_id(&account_info.owner) {
        return Ok(Some(dex.name.clone()));
    }
    
    Ok(None)
} 