#![allow(dead_code)]
use anyhow::{anyhow, Context, Result};
use bs58;
use solana_sdk::signature::{Keypair, Signer};
use std::{fs, path::Path};

pub fn load_payer(keypair_path: &str, wallet_private_key: &str, wallet_private_key_path: &str) -> Result<Keypair> {
    if !keypair_path.trim().is_empty() {
        return read_keypair_json(Path::new(keypair_path)).context("KEYPAIR_PATH");
    }
    if !wallet_private_key.trim().is_empty() {
        return parse_secret(wallet_private_key).context("WALLET_PRIVATE_KEY");
    }
    if !wallet_private_key_path.trim().is_empty() {
        let s = fs::read_to_string(wallet_private_key_path).context("WALLET_PRIVATE_KEY_PATH read")?;
        return parse_secret(s.trim()).context("WALLET_PRIVATE_KEY_PATH parse");
    }
    Err(anyhow!("No wallet provided. Set KEYPAIR_PATH or WALLET_PRIVATE_KEY or WALLET_PRIVATE_KEY_PATH"))
}

fn read_keypair_json(path: &Path) -> Result<Keypair> {
    let data = fs::read_to_string(path)?;
    let v: Vec<u8> = serde_json::from_str(&data)?;
    if v.len() != 64 {
        return Err(anyhow!("keypair json must be 64 bytes, got {}", v.len()));
    }
    let kp = Keypair::from_bytes(&v)?;
    Ok(kp)
}

fn parse_secret(s: &str) -> Result<Keypair> {
    // Accept JSON array like [1,2,3,...] or base58 64-byte secret
    if s.trim_start().starts_with('[') {
        let v: Vec<u8> = serde_json::from_str(s)?;
        if v.len() != 64 {
            return Err(anyhow!("secret json must be 64 bytes, got {}", v.len()));
        }
        return Ok(Keypair::from_bytes(&v)?);
    }
    let bytes = bs58::decode(s.trim()).into_vec().context("base58 decode")?;
    if bytes.len() != 64 {
        return Err(anyhow!("base58 secret must decode to 64 bytes, got {}", bytes.len()));
    }
    Ok(Keypair::from_bytes(&bytes)?)
}

pub fn pubkey_string(kp: &Keypair) -> String {
    kp.pubkey().to_string()
}
