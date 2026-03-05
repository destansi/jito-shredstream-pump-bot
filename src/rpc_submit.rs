use crate::config::Config;
use anyhow::{Context, Result};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_sdk::transaction::Transaction;

pub async fn submit_rpc(cfg: &Config, tx_bytes: &[u8]) -> Result<String> {
    let rpc = RpcClient::new(cfg.rpc_http_url.clone());
    let send_cfg = RpcSendTransactionConfig {
        skip_preflight: cfg.rpc_skip_preflight,
        preflight_commitment: None,
        encoding: None,
        max_retries: None,
        min_context_slot: None,
    };
    let tx: Transaction = bincode::deserialize(tx_bytes)?;
    let sig = rpc
        .send_transaction_with_config(&tx, send_cfg)
        .await
        .context("RPC send_transaction_with_config")?;
    Ok(sig.to_string())
}