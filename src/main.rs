mod alt_resolver;
mod blockhash_cache;
mod config;
mod dex;
mod executor;
mod jito;
mod pumpswap;
mod wsol_bank;
mod keypair;
mod monitor;
mod rpc_submit;
mod types;

use clap::Parser;
use solana_sdk::signer::Signer;

use crate::{
    alt_resolver::AltResolver,
    blockhash_cache::BlockhashCache,
    config::Config,
    executor::dispatcher::Executor,
    keypair::load_payer,
    monitor::shredstream_proxy::{run_monitor, MonitorConfig},
    types::TradeSignal,
};
use anyhow::{Context, Result};
use dotenvy::dotenv;
use moka::sync::Cache as SyncCache;
use solana_sdk::pubkey::Pubkey;
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio::sync::Semaphore;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cfg = Config::parse();

    let payer = load_payer(&cfg.keypair_path, &cfg.wallet_private_key, &cfg.wallet_private_key_path)
        .context("load payer")?;
    info!("Payer loaded: {}", payer.pubkey());
    let payer = Arc::new(payer);

    let leaders = parse_leaders(&cfg.leader_wallets)?;
    info!("Leaders loaded: {}", leaders.len());

    let mut wrapper_ids = parse_pubkey_list(&cfg.wrapper_program_ids)?;
    // Always include Axiom wrapper program (common for platform leaders)
    if let Ok(pk) = "FLASHX8DrLbgeR8FcfNV1F5krxYcYMUdBkrP1EPBtxB9".parse() {
        wrapper_ids.insert(pk);
    }
    if !wrapper_ids.is_empty() {
        info!("Wrapper allowlist loaded: {}", wrapper_ids.len());
    }

    let alt = AltResolver::new(cfg.rpc_http_url.clone(), cfg.alt_max_concurrent_fetch);

    // Cached blockhash refresher (used by demo/pumpbuy submission paths)
    let blockhash = BlockhashCache::new(cfg.rpc_http_url.clone());
    blockhash.spawn_refresher(cfg.blockhash_refresh_ms);

    let exec = Executor::new(cfg.clone(), payer.clone(), blockhash.clone()).await?;

    // mint done dedupe (ONLY after a successful execute)
    let mint_done: SyncCache<String, ()> = SyncCache::builder()
        .time_to_live(Duration::from_secs(cfg.dedup_mint_ttl_secs.max(1)))
        .max_capacity(500_000)
        .build();

    // short in-flight gate to avoid bursty multi-signal spam for the same mint
    // (this prevents SELL/fee/noise bursts from blocking future BUYs for hours)
    let mint_inflight: SyncCache<String, ()> = SyncCache::builder()
        .time_to_live(Duration::from_millis(cfg.dedup_sig_ttl_ms.max(200)))
        .max_capacity(500_000)
        .build();

    // Large buffer so monitors never block on bursts.
    let (tx_out, mut rx) = mpsc::channel::<TradeSignal>(50_000);

    // Spawn monitors (multi-region proxies)
    let urls: Vec<String> = cfg
        .shredstream_proxy_grpc_urls
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if urls.is_empty() {
        anyhow::bail!("SHREDSTREAM_PROXY_GRPC_URLS is empty");
    }

    for u in urls {
        let mc = MonitorConfig {
            grpc_url: u.clone(),
            leaders: Arc::new(leaders.clone()),
            strict_signer: cfg.strict_signer,
            resolve_alts: cfg.resolve_alts,
            parse_concurrency: cfg.parse_concurrency,
            sig_dedup_ttl_ms: cfg.dedup_sig_ttl_ms,
            cpi_heuristic_mint: cfg.cpi_heuristic_mint,
            stats_interval_secs: cfg.stats_interval_secs,
            debug_leader_sample: cfg.debug_leader_sample,
            wrapper_program_ids: Arc::new(wrapper_ids.clone()),
            alt_miss_skip: cfg.alt_miss_skip,
        };

        let alt2 = alt.clone();
        let out2 = tx_out.clone();

        tokio::spawn(async move {
            if let Err(e) = run_monitor(mc, alt2, out2).await {
                error!("monitor task failed: {e}");
            }
        });
    }

    drop(tx_out);

    // Consume signals fast, then execute on a separate pool (never block monitor path)
    let sem = Arc::new(Semaphore::new(cfg.executor_concurrency.max(1)));
    while let Some(sig) = rx.recv().await {
        let mint_key = sig.mint.to_string();

        // Long dedup only after success
        if mint_done.get(&mint_key).is_some() {
            continue;
        }
        // Short burst gate
        if mint_inflight.get(&mint_key).is_some() {
            continue;
        }
        mint_inflight.insert(mint_key.clone(), ());

        let exec2 = exec.clone();
        let mint_done2 = mint_done.clone();
        let mint_key2 = mint_key.clone();

        let permit = match sem.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break,
        };

        tokio::spawn(async move {
            let _permit = permit;
            match exec2.on_signal(sig).await {
                Ok(_) => {
                    mint_done2.insert(mint_key2, ());
                }
                Err(e) => {
                    warn!("executor error: {e}");
                }
            }
        });
    }

    Ok(())
}

fn parse_leaders(list: &[String]) -> Result<HashSet<Pubkey>> {
    let mut set = HashSet::new();
    for item in list {
        let t = item.trim();
        if t.is_empty() {
            continue;
        }
        let pk: Pubkey = t.parse().with_context(|| format!("invalid leader pubkey: {t}"))?;
        set.insert(pk);
    }
    Ok(set)
}

fn parse_pubkey_list(list: &[String]) -> Result<HashSet<Pubkey>> {
    let mut set = HashSet::new();
    for item in list {
        let t = item.trim();
        if t.is_empty() {
            continue;
        }
        let pk: Pubkey = t.parse().with_context(|| format!("invalid pubkey: {t}"))?;
        set.insert(pk);
    }
    Ok(set)
}