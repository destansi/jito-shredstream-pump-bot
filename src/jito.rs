#![allow(dead_code)]
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose, Engine as _};
use bs58;
use reqwest::Client;
use serde_json::json;
use solana_sdk::hash::Hasher;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[derive(Clone)]
pub struct JitoRpcClient {
    client: Client,
    /// Base URL like https://amsterdam.mainnet.block-engine.jito.wtf
    base_url: String,
    /// Optional auth UUID
    uuid: String,
    id_counter: Arc<AtomicU64>,
}

impl JitoRpcClient {
    pub fn new(base_url: String, uuid: String) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            uuid,
            id_counter: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn next_id(&self) -> u64 {
        self.id_counter.fetch_add(1, Ordering::Relaxed)
    }

    fn bundles_url(&self) -> String {
        format!("{}/api/v1/bundles", self.base_url)
    }

    fn txs_url(&self) -> String {
        format!("{}/api/v1/transactions", self.base_url)
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.uuid.trim().is_empty() {
            return req;
        }
        // Some providers accept x-jito-auth. We send it whenever uuid is set.
        req.header("x-jito-auth", self.uuid.trim().to_string())
    }

    pub async fn get_tip_accounts(&self) -> Result<Vec<String>> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "getTipAccounts",
            "params": []
        });

        let mut req = self.client.post(self.bundles_url()).json(&payload);
        req = self.apply_auth(req);

        let resp = req.send().await?;
        let v: serde_json::Value = resp.json().await?;
        if let Some(err) = v.get("error") {
            return Err(anyhow!("getTipAccounts error: {err}"));
        }
        let arr = v
            .get("result")
            .and_then(|x| x.as_array())
            .ok_or_else(|| anyhow!("getTipAccounts missing result"))?;
        Ok(arr
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect())
    }

    /// Jito sendTransaction expects base64-encoded wire tx bytes.
    pub async fn send_transaction_bytes_base64(&self, tx_bytes: &[u8]) -> Result<String> {
        let tx_b64 = general_purpose::STANDARD.encode(tx_bytes);
        let payload = json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "sendTransaction",
            "params": [tx_b64, {"encoding": "base64"}]
        });

        let mut req = self.client.post(self.txs_url()).json(&payload);
        req = self.apply_auth(req);

        let resp = req.send().await?;
        let v: serde_json::Value = resp.json().await?;
        if let Some(err) = v.get("error") {
            return Err(anyhow!("sendTransaction error: {err}"));
        }
        let sig = v
            .get("result")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow!("sendTransaction missing result"))?;
        Ok(sig.to_string())
    }

    /// sendBundle requires base58-encoded transactions (NOT base64).
    pub async fn send_bundle_bytes_base58(&self, txs: &[Vec<u8>]) -> Result<String> {
        let txs_b58: Vec<String> = txs.iter().map(|b| bs58::encode(b).into_string()).collect();

        let payload = json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "sendBundle",
            "params": [txs_b58]
        });

        let mut req = self.client.post(self.bundles_url()).json(&payload);
        req = self.apply_auth(req);

        let resp = req.send().await?;
        let v: serde_json::Value = resp.json().await?;
        if let Some(err) = v.get("error") {
            return Err(anyhow!("sendBundle error: {err}"));
        }
        let bundle_id = v
            .get("result")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow!("sendBundle missing result"))?;
        Ok(bundle_id.to_string())
    }


    /// sendBundle with base64-encoded wire tx bytes (some BE deployments expect base64).
    pub async fn send_bundle_bytes_base64(&self, txs: &[Vec<u8>]) -> Result<String> {
        let txs_b64: Vec<String> = txs
            .iter()
            .map(|b| general_purpose::STANDARD.encode(b))
            .collect();

        let payload = json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "sendBundle",
            "params": [txs_b64]
        });

        let mut req = self.client.post(self.bundles_url()).json(&payload);
        req = self.apply_auth(req);

        let resp = req.send().await?;
        let v: serde_json::Value = resp.json().await?;
        if let Some(err) = v.get("error") {
            return Err(anyhow!("sendBundle error: {err}"));
        }
        let bundle_id = v
            .get("result")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow!("sendBundle missing result"))?;
        Ok(bundle_id.to_string())
    }

    /// Compute the "bundle id" as sha256(concat(tx_signatures)).
    /// Jito docs describe bundle_id as sha256 hash of the bundle's tx signatures.
    pub fn compute_bundle_id_hex(tx_sigs_base58: &[String]) -> String {
        let mut hasher = Hasher::default();
        for sig_str in tx_sigs_base58 {
            if let Ok(sig) = solana_sdk::signature::Signature::from_str(sig_str) {
                hasher.hash(sig.as_ref());
            }
        }
        let hash = hasher.result();
        to_hex(hash.as_ref())
    }

    pub async fn get_bundle_statuses(&self, bundle_ids: &[String]) -> Result<serde_json::Value> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "getBundleStatuses",
            "params": [bundle_ids]
        });
        let mut req = self.client.post(self.bundles_url()).json(&payload);
        req = self.apply_auth(req);
        let resp = req.send().await?;
        Ok(resp.json().await?)
    }

    pub async fn get_inflight_bundle_statuses(&self, bundle_ids: &[String]) -> Result<serde_json::Value> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "getInflightBundleStatuses",
            "params": [bundle_ids]
        });
        let mut req = self.client.post(self.bundles_url()).json(&payload);
        req = self.apply_auth(req);
        let resp = req.send().await?;
        Ok(resp.json().await?)
    }
}
