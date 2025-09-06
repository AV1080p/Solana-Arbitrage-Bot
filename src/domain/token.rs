use anchor_client::solana_sdk::{pubkey::Pubkey, signature::Keypair};
use spl_token_2022::{
    extension::StateWithExtensionsOwned,
    state::{Account, Mint},
};
use spl_token_client::{
    client::{ProgramClient, ProgramRpcClient, ProgramRpcClientSendTransaction},
    token::{Token, TokenError, TokenResult},
};
use std::{collections::HashMap, sync::Arc, str::FromStr};
use base64;
use anyhow::{Result, anyhow};
use solana_client;
use solana_client::rpc_request::TokenAccountsFilter;
use spl_token::state::Account as TokenAccount;
use spl_token::state::Mint as TokenMint;
use anchor_client::solana_sdk::program_pack::Pack;

/// TokenPrice represents the price of a token on a specific DEX
#[derive(Debug, Clone)]
pub struct TokenPrice {
    pub price: f64,
    pub timestamp: i64,
    pub liquidity: u64,
}

/// TokenModel tracks token prices across different DEXes
#[derive(Debug)]
pub struct TokenModel {
    /// Map of token mint address to a map of DEX name to price information
    prices: HashMap<String, HashMap<String, TokenPrice>>,
    /// Map of token mint address to token metadata
    tokens: HashMap<String, TokenMetadata>,
    /// Map of DEX name to program ID
    dex_programs: HashMap<String, Pubkey>,
    /// Map of DEX name to pool account size (for finding pools)
    dex_pool_sizes: HashMap<String, usize>,
}

/// TokenMetadata stores information about a token
#[derive(Debug, Clone)]
pub struct TokenMetadata {
    pub mint: Pubkey,
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
}

impl TokenModel {
    pub fn new() -> Self {
        let mut dex_programs = HashMap::new();
        let mut dex_pool_sizes = HashMap::new();
        
        // Initialize with known DEX program IDs
        dex_programs.insert("pumpswap".to_string(), Pubkey::from_str("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA").unwrap());
        dex_programs.insert("pumpfun".to_string(), Pubkey::from_str("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P").unwrap());
        
        // Initialize with known pool sizes
        dex_pool_sizes.insert("pumpswap".to_string(), 300);
        
        Self {
            prices: HashMap::new(),
            tokens: HashMap::new(),
            dex_programs,
            dex_pool_sizes,
        }
    }
    
    /// Register a new DEX with its program ID and pool account size
    pub fn register_dex(&mut self, name: &str, program_id: Pubkey, pool_account_size: usize) {
        self.dex_programs.insert(name.to_string(), program_id);
        self.dex_pool_sizes.insert(name.to_string(), pool_account_size);
    }
    
    /// Register a token with metadata
    pub fn register_token(&mut self, metadata: TokenMetadata) {
        self.tokens.insert(metadata.mint.to_string(), metadata);
    }
    
    /// Update price for a token on a specific DEX
    pub fn update_price(&mut self, mint: &str, dex: &str, price: f64, liquidity: u64, timestamp: i64) {
        let dex_prices = self.prices
            .entry(mint.to_string())
            .or_insert_with(HashMap::new);
            
        dex_prices.insert(dex.to_string(), TokenPrice {
            price,
            timestamp,
            liquidity,
        });
    }
    
    /// Get price for a token on a specific DEX
    pub fn get_price(&self, mint: &str, dex: &str) -> Option<&TokenPrice> {
        self.prices.get(mint)
            .and_then(|dex_prices| dex_prices.get(dex))
    }
    
    /// Get all prices for a token across all DEXes
    pub fn get_all_prices(&self, mint: &str) -> Option<&HashMap<String, TokenPrice>> {
        self.prices.get(mint)
    }
    
    /// Get the program ID for a DEX
    pub fn get_dex_program(&self, dex: &str) -> Option<&Pubkey> {
        self.dex_programs.get(dex)
    }
    
    /// Get the pool account size for a DEX
    pub fn get_dex_pool_size(&self, dex: &str) -> Option<&usize> {
        self.dex_pool_sizes.get(dex)
    }
    
    /// Find arbitrage opportunities for a token
    pub fn find_arbitrage_opportunities(&self, mint: &str) -> Vec<(String, String, f64)> {
        let mut opportunities = Vec::new();
        
        if let Some(dex_prices) = self.prices.get(mint) {
            let dexes: Vec<&String> = dex_prices.keys().collect();
            
            for i in 0..dexes.len() {
                for j in i+1..dexes.len() {
                    let dex1 = dexes[i];
                    let dex2 = dexes[j];
                    
                    if let (Some(price1), Some(price2)) = (dex_prices.get(dex1), dex_prices.get(dex2)) {
                        let price_diff_pct = (price1.price - price2.price).abs() / price2.price * 100.0;
                        
                        if price_diff_pct > 1.0 { // 1% threshold for arbitrage
                            if price1.price > price2.price {
                                opportunities.push((dex2.clone(), dex1.clone(), price_diff_pct));
                            } else {
                                opportunities.push((dex1.clone(), dex2.clone(), price_diff_pct));
                            }
                        }
                    }
                }
            }
        }
        
        opportunities
    }
}

pub fn get_associated_token_address(
    client: Arc<anchor_client::solana_client::nonblocking::rpc_client::RpcClient>,
    keypair: Arc<Keypair>,
    address: &Pubkey,
    owner: &Pubkey,
) -> Pubkey {
    let token_client = Token::new(
        Arc::new(ProgramRpcClient::new(
            client.clone(),
            ProgramRpcClientSendTransaction,
        )),
        &spl_token::ID,
        address,
        None,
        Arc::new(Keypair::from_bytes(&keypair.to_bytes()).expect("failed to copy keypair")),
    );
    token_client.get_associated_token_address(owner)
}

pub async fn get_account_info(
    client: Arc<anchor_client::solana_client::nonblocking::rpc_client::RpcClient>,
    address: Pubkey,
    account: Pubkey,
) -> TokenResult<StateWithExtensionsOwned<Account>> {
    let program_client = Arc::new(ProgramRpcClient::new(
        client.clone(),
        ProgramRpcClientSendTransaction,
    ));
    let account = program_client
        .get_account(account)
        .await
        .map_err(TokenError::Client)?
        .ok_or(TokenError::AccountNotFound)
        .inspect_err(|_err| {
            // logger.log(format!(
            //     "get_account_info: {} {}: mint {}",
            //     account, err, address
            // ));
        })?;

    if account.owner != spl_token::ID {
        return Err(TokenError::AccountInvalidOwner);
    }
    let account = StateWithExtensionsOwned::<Account>::unpack(account.data)?;
    if account.base.mint != address {
        return Err(TokenError::AccountInvalidMint);
    }

    Ok(account)
}

pub async fn get_mint_info(
    client: Arc<anchor_client::solana_client::nonblocking::rpc_client::RpcClient>,
    _keypair: Arc<Keypair>,
    address: Pubkey,
) -> TokenResult<StateWithExtensionsOwned<Mint>> {
    let program_client = Arc::new(ProgramRpcClient::new(
        client.clone(),
        ProgramRpcClientSendTransaction,
    ));
    let account = program_client
        .get_account(address)
        .await
        .map_err(TokenError::Client)?
        .ok_or(TokenError::AccountNotFound)
        .inspect_err(|err| println!("{} {}: mint {}", address, err, address))?;

    if account.owner != spl_token::ID {
        return Err(TokenError::AccountInvalidOwner);
    }

    let mint_result = StateWithExtensionsOwned::<Mint>::unpack(account.data).map_err(Into::into);
    let decimals: Option<u8> = None;
    if let (Ok(mint), Some(decimals)) = (&mint_result, decimals) {
        if decimals != mint.base.decimals {
            return Err(TokenError::InvalidDecimals);
        }
    }

    mint_result
}

/// Find pools for a token on a specific DEX
pub async fn find_pools_for_token(
    client: Arc<anchor_client::solana_client::nonblocking::rpc_client::RpcClient>,
    dex_program: &Pubkey,
    mint: &Pubkey,
    pool_account_size: usize,
) -> Result<Vec<Pubkey>, anyhow::Error> {
    use solana_account_decoder::UiAccountEncoding;
    use solana_client::{
        rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
        rpc_filter::{Memcmp, RpcFilterType},
    };
    use solana_client::rpc_filter::MemcmpEncodedBytes;
    
    let accounts = client.get_program_accounts_with_config(
        dex_program,
        RpcProgramAccountsConfig {
            filters: Some(vec![
                RpcFilterType::DataSize(pool_account_size.try_into().unwrap()),
                RpcFilterType::Memcmp(Memcmp::new(43, MemcmpEncodedBytes::Base64(base64::encode(mint.to_bytes())))),
            ]),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                ..RpcAccountInfoConfig::default()
            },
            ..RpcProgramAccountsConfig::default()
        },
    ).await?;
    
    Ok(accounts.into_iter().map(|(pubkey, _)| pubkey).collect())
}

/// Get the price of a token from a PumpSwap pool
pub async fn get_pumpswap_token_price(
    client: Arc<anchor_client::solana_client::nonblocking::rpc_client::RpcClient>,
    pool_id: &Pubkey,
) -> Result<f64, anyhow::Error> {
    use crate::dex::pump_swap::SOL_MINT;
    
    // Get the pool information
    let sol_mint = Pubkey::from_str(SOL_MINT)?;
    
    // Get token accounts for the pool
    let pool_accounts = match client.get_token_accounts_by_owner(pool_id, 
        TokenAccountsFilter::ProgramId(spl_token::ID.to_string())).await {
        Ok(accounts) => accounts,
        Err(e) => return Err(anyhow!("Failed to get token accounts: {}", e)),
    };
    
    if pool_accounts.is_empty() {
        return Err(anyhow!("No token accounts found for pool"));
    }
    
    let mut base_reserve = 0;
    let mut quote_reserve = 0;
    let mut base_mint = Pubkey::default();
    let mut quote_mint = Pubkey::default();
    
    // Find base and quote token accounts
    for account in pool_accounts {
        let token_account = match TokenAccount::unpack(&account.account.data) {
            Ok(account) => account,
            Err(e) => return Err(anyhow!("Failed to unpack token account: {}", e)),
        };
        
        if token_account.mint == sol_mint {
            quote_mint = token_account.mint;
            quote_reserve = token_account.amount;
        } else {
            base_mint = token_account.mint;
            base_reserve = token_account.amount;
        }
    }
    
    // Calculate price (SOL per token)
    if base_reserve == 0 || quote_reserve == 0 {
        return Err(anyhow!("Zero reserves in pool"));
    }
    
    // Get token decimals
    let base_mint_info = match client.get_account(&base_mint).await {
        Ok(info) => info,
        Err(e) => return Err(anyhow!("Failed to get base mint account: {}", e)),
    };
    
    let base_mint_data = match TokenMint::unpack(&base_mint_info.data) {
        Ok(data) => data,
        Err(e) => return Err(anyhow!("Failed to unpack base mint data: {}", e)),
    };
    
    let base_decimals = base_mint_data.decimals;
    
    // SOL has 9 decimals
    let quote_decimals = 9;
    
    // Calculate price normalized by decimals
    let base_amount_normalized = base_reserve as f64 / 10f64.powf(base_decimals as f64);
    let quote_amount_normalized = quote_reserve as f64 / 10f64.powf(quote_decimals as f64);
    
    if base_amount_normalized == 0.0 {
        return Err(anyhow!("Zero normalized base amount"));
    }
    
    let price = quote_amount_normalized / base_amount_normalized;
    
    Ok(price)
}

/// Get the price of a token from a PumpFun bonding curve
pub async fn get_pumpfun_token_price(
    client: Arc<anchor_client::solana_client::nonblocking::rpc_client::RpcClient>,
    mint: &Pubkey,
) -> Result<f64, anyhow::Error> {
    use crate::dex::pump_fun::{get_bonding_curve_account, PUMP_PROGRAM};
    
    // Get the bonding curve account for this token
    let pump_program = Pubkey::from_str(PUMP_PROGRAM)?;
    
    // Create a synchronous client for the bonding curve function
    let sync_client = Arc::new(anchor_client::solana_client::rpc_client::RpcClient::new(
        client.url().to_string(),
    ));
    
    // Get bonding curve reserves
    let (_, _, bonding_curve_reserves) = match get_bonding_curve_account(
        sync_client,
        *mint,
        pump_program,
    ).await {
        Ok(result) => result,
        Err(e) => return Err(anyhow!("Failed to get bonding curve account: {}", e)),
    };
    
    // Calculate price based on bonding curve formula
    let virtual_sol_reserves = bonding_curve_reserves.virtual_sol_reserves as f64;
    let virtual_token_reserves = bonding_curve_reserves.virtual_token_reserves as f64;
    
    // PumpFun uses a bonding curve formula: price = sol_reserves / token_reserves
    if virtual_token_reserves == 0.0 {
        return Err(anyhow!("Zero token reserves in bonding curve"));
    }
    
    // SOL has 9 decimals, normalize by token decimals
    let mint_info = match client.get_account(mint).await {
        Ok(info) => info,
        Err(e) => return Err(anyhow!("Failed to get mint account: {}", e)),
    };
    
    let mint_data = match TokenMint::unpack(&mint_info.data) {
        Ok(data) => data,
        Err(e) => return Err(anyhow!("Failed to unpack mint data: {}", e)),
    };
    
    let token_decimals = mint_data.decimals;
    
    // Calculate price normalized by decimals
    let sol_amount_normalized = virtual_sol_reserves / 1e9;
    let token_amount_normalized = virtual_token_reserves / 10f64.powf(token_decimals as f64);
    
    if token_amount_normalized == 0.0 {
        return Err(anyhow!("Zero normalized token amount"));
    }
    
    let price = sol_amount_normalized / token_amount_normalized;
    
    Ok(price)
}
