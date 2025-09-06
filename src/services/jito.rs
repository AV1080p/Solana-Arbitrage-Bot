use anyhow::{anyhow, Result};
use indicatif::{ProgressBar, ProgressStyle};
use rand::{seq::IteratorRandom, thread_rng};
use serde::Deserialize;
use serde_json::Value;
use anchor_client::solana_sdk::pubkey::Pubkey;
use std::{future::Future, str::FromStr, sync::LazyLock, time::Duration};
use tokio::time::{sleep, Instant};

use crate::common::config::import_env_var;

pub static BLOCK_ENGINE_URL: LazyLock<String> =
    LazyLock::new(|| import_env_var("JITO_BLOCK_ENGINE_URL"));

pub fn get_tip_account() -> Result<(Pubkey, Pubkey)> {
    let accounts = [
        "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5".to_string(),
        "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt".to_string(),
        "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL".to_string(),
        "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT".to_string(),
        "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe".to_string(),
        "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh".to_string(),
        "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49".to_string(),
        "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY".to_string(),
    ];
    let mut rng = thread_rng();
    let tip_account = match accounts.iter().choose(&mut rng) {
        Some(acc) => Ok(Pubkey::from_str(acc).inspect_err(|err| {
            println!("jito: failed to parse Pubkey: {:?}", err);
        })?),
        None => Err(anyhow!("jito: no tip accounts available")),
    };
    let tip1_account = Pubkey::from_str("JitoFSvbiCrygnx4HZzau4LdeyBU6VUeyf9jt8F8bMk")
        .inspect_err(|err| {
            println!("jito: failed to parse Pubkey: {:?}", err);
        })?;
    let tip_account = tip_account?;
    Ok((tip_account, tip1_account))
}
// unit sol
pub async fn get_tip_value() -> Result<f64> {
    // If TIP_VALUE is set, use it
    if let Ok(tip_value) = std::env::var("JITO_TIP_VALUE") {
        match f64::from_str(&tip_value) {
            Ok(value) => Ok(value),
            Err(_) => {
                println!(
                    "Invalid JITO_TIP_VALUE in environment variable: '{}'. Falling back to percentile calculation.",
                    tip_value
                );
                Err(anyhow!("Invalid TIP_VALUE in environment variable"))
            }
        }
    } else {
        Err(anyhow!("JITO_TIP_VALUE environment variable not set"))
    }
}

pub async fn get_priority_fee() -> Result<f64> {
    // If TIP_VALUE is set, use it
    if let Ok(priority_fee) = std::env::var("JITO_PRIORITY_FEE") {
        match f64::from_str(&priority_fee) {
            Ok(value) => Ok(value),
            Err(_) => {
                println!(
                    "Invalid JITO_PRIORITY_FEE in environment variable: '{}'. Falling back to percentile calculation.",
                    priority_fee
                );
                Err(anyhow!("Invalid JITO_PRIORITY_FEE in environment variable"))
            }
        }
    } else {
        Err(anyhow!("JITO_PRIORITY_FEE environment variable not set"))
    }
}

#[derive(Deserialize, Debug)]
pub struct BundleStatus {
    pub bundle_id: String,
    pub transactions: Vec<String>,
    pub slot: u64,
    pub confirmation_status: String,
    pub err: ErrorStatus,
}
#[derive(Deserialize, Debug)]
pub struct ErrorStatus {
    #[serde(rename = "Ok")]
    pub ok: Option<()>,
}

pub async fn wait_for_bundle_confirmation<F, Fut>(
    fetch_statuses: F,
    bundle_id: String,
    interval: Duration,
    timeout: Duration,
) -> Result<Vec<String>>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<Vec<Value>>>,
{
    let progress_bar = new_progress_bar();
    let start_time = Instant::now();

    loop {
        let statuses = fetch_statuses(bundle_id.clone()).await?;

        if let Some(status) = statuses.first() {
            let bundle_status: BundleStatus =
                serde_json::from_value(status.clone()).inspect_err(|err| {
                    println!(
                        "Failed to parse JSON when get_bundle_statuses, err: {}",
                        err,
                    );
                })?;

            println!("{:?}", bundle_status);
            match bundle_status.confirmation_status.as_str() {
                "finalized" | "confirmed" => {
                    progress_bar.finish_and_clear();
                    println!(
                        "Finalized bundle {}: {}",
                        bundle_id, bundle_status.confirmation_status
                    );
                    // // print tx
                    // bundle_status
                    //     .transactions
                    //     .iter()
                    //     .for_each(|tx| println!("https://solscan.io/tx/{}", tx));
                    return Ok(bundle_status.transactions[0..2].to_vec());
                }
                _ => {
                    progress_bar.set_message(format!(
                        "Finalizing bundle {}: {}",
                        bundle_id, bundle_status.confirmation_status
                    ));
                }
            }
        } else {
            progress_bar.set_message(format!("Finalizing bundle {}: {}", bundle_id, "None"));
        }

        // check loop exceeded 1 minute,
        if start_time.elapsed() > timeout {
            println!("Loop exceeded {:?}, breaking out.", timeout);
            return Err(anyhow!("Bundle status get timeout"));
        }

        // Wait for a certain duration before retrying
        sleep(interval).await;
    }
}
pub fn new_progress_bar() -> ProgressBar {
    let progress_bar = ProgressBar::new(42);
    progress_bar.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {wide_msg}")
            .expect("ProgressStyle::template direct input to be correct"),
    );
    progress_bar.enable_steady_tick(Duration::from_millis(100));
    progress_bar
}

use crate::error::ClientError;
use bincode;
use bs64;
use reqwest;
use serde_json::json;
use anchor_client::solana_sdk::{
    commitment_config::CommitmentConfig, signature::Signature, transaction::Transaction,
};

pub const MAX_RETRIES: u8 = 3;
pub const RETRY_DELAY: Duration = Duration::from_millis(200);

#[derive(Debug, Clone)]
pub struct TransactionConfig {
    pub skip_preflight: bool,
    pub preflight_commitment: CommitmentConfig,
    pub encoding: String,
    pub last_n_blocks: u64,
}

impl Default for TransactionConfig {
    fn default() -> Self {
        Self {
            skip_preflight: true,
            preflight_commitment: CommitmentConfig::confirmed(),
            encoding: "base64".to_string(),
            last_n_blocks: 100,
        }
    }
}

#[derive(Clone, Debug)]
pub struct JitoClient {
    endpoint: String,
    client: reqwest::Client,
    config: TransactionConfig,
}

impl JitoClient {
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            client: reqwest::Client::new(),
            config: TransactionConfig::default(),
        }
    }

    pub async fn send_transaction(
        &self,
        transaction: &Transaction,
    ) -> Result<Signature, ClientError> {
        let wire_transaction = bincode::serialize(transaction).map_err(|e| {
            ClientError::Parse(
                "Transaction serialization failed".to_string(),
                e.to_string(),
            )
        })?;

        let encoded_tx = &bs64::encode(&wire_transaction);

        for retry in 0..MAX_RETRIES {
            match self.try_send_transaction(encoded_tx).await {
                Ok(signature) => {
                    return Signature::from_str(&signature).map_err(|e| {
                        ClientError::Parse("Invalid signature".to_string(), e.to_string())
                    });
                }
                Err(e) => {
                    println!("Retry {} failed: {:?}", retry, e);
                    if retry == MAX_RETRIES - 1 {
                        return Err(e);
                    }
                    // tokio::time::sleep(RETRY_DELAY).await;
                }
            }
        }

        Err(ClientError::Other("Max retries exceeded".to_string()))
    }

    async fn try_send_transaction(&self, encoded_tx: &str) -> Result<String, ClientError> {
        let params = json!([
            encoded_tx,
            {
                "skipPreflight": self.config.skip_preflight,
                "preflightCommitment": self.config.preflight_commitment.commitment,
                "encoding": self.config.encoding,
                "maxRetries": 1,
            }
        ]);

        let response_data = self.send_request("sendTransaction", params).await?;
        
        if let Some(result) = response_data.get("result") {
            if let Some(signature) = result.as_str() {
                return Ok(signature.to_string());
            }
        }
        
        Err(ClientError::Parse(
            "Invalid response format".to_string(),
            response_data.to_string(),
        ))
    }

    async fn send_request(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let request_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params
        });

        let response = self
            .client
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| ClientError::Solana("Request failed".to_string(), e.to_string()))?;

        let response_data: Value = response
            .json()
            .await
            .map_err(|e| ClientError::Parse("Invalid JSON response".to_string(), e.to_string()))?;

        if let Some(error) = response_data.get("error") {
            return Err(ClientError::Solana(
                "RPC error".to_string(),
                error.to_string(),
            ));
        }

        Ok(response_data)
    }
}
