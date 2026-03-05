use crate::{
    blockhash_cache::BlockhashCache,
    config::Config,
    executor::{demo::build_demo_tx_bytes, pumpbuy::build_trade_bundle_txs},
    jito::JitoRpcClient,
    rpc_submit::submit_rpc,
    wsol_bank::WsolBank,
};
use anyhow::{anyhow, Result};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};
use std::{str::FromStr, sync::{Arc, atomic::{AtomicU64, Ordering}}, time::{Duration, Instant}};
use tokio::sync::Semaphore;
use tokio::sync::Mutex;
use tracing::{info, warn};

#[derive(Clone)]
pub struct Executor {
    cfg: Config,
    payer: Arc<Keypair>,
    rpc: Arc<RpcClient>,
    blockhash: BlockhashCache,

    jito_clients: Vec<Arc<JitoRpcClient>>,
    tip_account: Pubkey,

    sem: Arc<Semaphore>,
    exec_gate: Arc<Semaphore>,
    oneshot: bool,

    wsol_bank: Option<WsolBank>,

    last_jito_submit: Arc<Mutex<Instant>>,

    jito_rr: Arc<AtomicU64>,
    jito_endpoint_cooldowns: Arc<Mutex<Vec<Instant>>>,
}

impl Executor {
    pub async fn new(cfg: Config, payer: Arc<Keypair>, blockhash: BlockhashCache) -> Result<Self> {
        let rpc = blockhash.rpc();

        // Jito clients (try multiple endpoints if provided)
//
// Important:
// - In bundle_pumpbuy mode we MUST have Jito, regardless of USE_JITO env.
// - In log mode we should NOT fail startup if Jito isn't configured.
        let want_jito = cfg.use_jito || matches!(cfg.execution_mode.as_str(), "demo" | "bundle_pumpbuy");

        let mut urls = cfg.jito_block_engine_urls.clone();
        if urls.is_empty() {
            urls.push(cfg.jito_block_engine_url.clone());
        }

        let mut jito_clients = Vec::new();
        if want_jito {
            for u in urls {
                let u = u.trim();
                if u.is_empty() {
                    continue;
                }
                jito_clients.push(Arc::new(JitoRpcClient::new(u.to_string(), cfg.jito_uuid.clone())));
            }
        }

        // We'll move `jito_clients` into the executor below; capture the length now.
        let jito_clients_len = jito_clients.len().max(1);

        // Resolve tip account (only if we will actually use Jito)
        let tip_account = if want_jito {
            if !cfg.jito_tip_account.trim().is_empty() {
                Pubkey::from_str(cfg.jito_tip_account.trim())
                    .map_err(|e| anyhow!("invalid JITO_TIP_ACCOUNT: {e}"))?
            } else if let Some(jito) = jito_clients.first() {
                // Fetch and pick one
                let tips = jito.get_tip_accounts().await.unwrap_or_default();
                if tips.is_empty() {
                    return Err(anyhow!(
                        "JITO_TIP_ACCOUNT is empty and getTipAccounts returned empty; set JITO_TIP_ACCOUNT"
                    ));
                }
                let idx = (std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos() as usize)
                    % tips.len();
                Pubkey::from_str(&tips[idx])
                    .map_err(|e| anyhow!("invalid tip account from getTipAccounts: {e}"))?
            } else {
                // If we're in bundle mode, this is fatal. Otherwise, we can proceed.
                if cfg.execution_mode.as_str() == "bundle_pumpbuy" {
                    return Err(anyhow!(
                        "bundle_pumpbuy requires a Jito client; set JITO_BLOCK_ENGINE_URL and USE_JITO=true (or provide JITO_TIP_ACCOUNT)"
                    ));
                }
                Pubkey::default()
            }
        } else {
            Pubkey::default()
        };

        let want_wsol_bank = cfg.wsol_reserve_sol > 0.0 || cfg.wsol_min_sol > 0.0 || cfg.wsol_target_sol > 0.0;
        let wsol_bank = if want_wsol_bank {
            Some(WsolBank::new(&payer.pubkey(), cfg.wsol_reserve_sol, cfg.wsol_min_sol, cfg.wsol_target_sol))
        } else {
            None
        };

        let ex = Self {
            cfg: cfg.clone(),
            payer: payer.clone(),
            rpc,
            blockhash: blockhash.clone(),
            jito_clients,
            tip_account,
            sem: Arc::new(Semaphore::new(cfg.executor_concurrency.max(1))),
            exec_gate: Arc::new(Semaphore::new(1)),
            oneshot: cfg.oneshot,
            wsol_bank,

            last_jito_submit: Arc::new(Mutex::new(Instant::now() - Duration::from_millis(cfg.jito_min_submit_interval_ms))),
            jito_rr: Arc::new(AtomicU64::new(0)),
            jito_endpoint_cooldowns: Arc::new(Mutex::new(vec![Instant::now() - Duration::from_secs(3600); jito_clients_len])),
        };

        // WSOL bootstrap (optional)
        if let Some(bank) = &ex.wsol_bank {
            let jito = ex.jito_clients.first();
            bank.bootstrap(
                &ex.payer,
                &ex.blockhash,
                jito,
                ex.cfg.compute_unit_limit,
                ex.cfg.compute_unit_price_micro_lamports,
            )
            .await?;
            info!(
                "WSOL bank ready: {} (reserve_lamports={})",
                bank.wsol_ata,
                bank.reserve_lamports()
            );
        }

        Ok(ex)
    }


    fn retry_after_ms_from_err(err: &str) -> Option<u64> {
        let marker = "Retry after ";
        let idx = err.find(marker)?;
        let s = &err[idx + marker.len()..];
        let mut num = String::new();
        for ch in s.chars() {
            if ch.is_ascii_digit() {
                num.push(ch);
            } else {
                break;
            }
        }
        if num.is_empty() {
            None
        } else {
            num.parse::<u64>().ok()
        }
    }

    async fn throttle_jito(&self) {
        let min_ms = self.cfg.jito_min_submit_interval_ms.max(1);
        loop {
            let now = Instant::now();
            let wait_ms = {
                let last = self.last_jito_submit.lock().await;
                let next = (*last) + Duration::from_millis(min_ms);
                if now >= next {
                    0u64
                } else {
                    (next - now).as_millis() as u64
                }
            };
            if wait_ms == 0 {
                let mut last = self.last_jito_submit.lock().await;
                *last = now;
                return;
            }
            tokio::time::sleep(Duration::from_millis(wait_ms)).await;
        }
    }

    pub async fn on_signal(&self, sig: crate::types::TradeSignal) -> Result<()> {
        let _permit = self.sem.acquire().await?;

        match self.cfg.execution_mode.as_str() {
            "log" => {
                info!("[EXEC log] mint={} slot={} src={}", sig.mint, sig.slot, sig.source);
                Ok(())
            }
            "demo" => {
                // Execute only one tx at a time; drop bursts.
                let _exec = match self.exec_gate.try_acquire() {
                    Ok(p) => p,
                    Err(_) => {
                        tracing::debug!(mint=%sig.mint, slot=sig.slot, src=sig.source, "exec busy; skipping");
                        return Ok(());
                    }
                };

                let start = Instant::now();
                let tx_bytes =
                    build_demo_tx_bytes(&self.cfg, &self.blockhash, &self.payer, &self.tip_account).await?;
                let ms_build = start.elapsed().as_millis();

                if let Some(jito) = self.jito_clients.first() {
                    let t0 = Instant::now();
                    let tx_sig = jito.send_transaction_bytes_base64(&tx_bytes).await?;
                    info!(
                        "[EXEC demo] sent sig={} build={}ms submit={}ms",
                        tx_sig,
                        ms_build,
                        t0.elapsed().as_millis()
                    );
                    if self.cfg.dedup_sig_ttl_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(self.cfg.dedup_sig_ttl_ms)).await;
                    }
                    if self.oneshot {
                        std::process::exit(0);
                    }
                    Ok(())
                } else {
                    // RPC fallback
                    let tx_sig = submit_rpc(&self.cfg, &tx_bytes).await?;
                    info!("[EXEC demo RPC] sent sig={} build={}ms", tx_sig, ms_build);
                    if self.cfg.dedup_sig_ttl_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(self.cfg.dedup_sig_ttl_ms)).await;
                    }
                    Ok(())
                }
            }
            "bundle_pumpbuy" => {
                // Execute only one tx at a time; drop bursts.
                let _exec = match self.exec_gate.try_acquire() {
                    Ok(p) => p,
                    Err(_) => {
                        tracing::debug!(mint=%sig.mint, slot=sig.slot, src=sig.source, "exec busy; skipping");
                        return Ok(());
                    }
                };

                if self.jito_clients.is_empty() {
                    return Err(anyhow!("bundle_pumpbuy requires USE_JITO=true"));
                }

                // Free endpoints are typically rate-limited (~1 txn/sec). Throttle globally.
                {
                    let mut last = self.last_jito_submit.lock().await;
                    let min_iv = std::time::Duration::from_millis(self.cfg.jito_min_submit_interval_ms.max(0));
                    let elapsed = last.elapsed();
                    if elapsed < min_iv {
                        tokio::time::sleep(min_iv - elapsed).await;
                    }
                    *last = Instant::now();
                }

                let start = Instant::now();
                let txs = build_trade_bundle_txs(
                    &self.cfg,
                    &self.rpc,
                    &self.blockhash,
                    &self.payer,
                    &self.tip_account,
                    &sig,
                )
                .await?;
                let ms_build = start.elapsed().as_millis();

                
                if txs.is_empty() || txs.iter().any(|b| b.is_empty()) {
                    return Err(anyhow!("built empty transaction bytes (txs={})", txs.len()));
                }

                let n = self.jito_clients.len().max(1);
                let start_idx = (self.jito_rr.fetch_add(1, Ordering::Relaxed) as usize) % n;
                let max_failover = self.cfg.jito_failover_max.min(n.saturating_sub(1));

                // Build an endpoint order (rotating) and pick up to 1+max_failover endpoints.
                let now = Instant::now();
                let order: Vec<usize> = (0..n).map(|o| (start_idx + o) % n).collect();
                let cooldowns_snapshot = { self.jito_endpoint_cooldowns.lock().await.clone() };

                let mut chosen: Vec<usize> = Vec::with_capacity(1 + max_failover);
                for &i in &order {
                    if now >= cooldowns_snapshot.get(i).copied().unwrap_or(now) {
                        chosen.push(i);
                        if chosen.len() >= 1 + max_failover {
                            break;
                        }
                    }
                }
                if chosen.is_empty() {
                    chosen.push(start_idx);
                }
                // If we still have room, fill with remaining (even if cooled) so we can failover immediately.
                for &i in &order {
                    if chosen.len() >= 1 + max_failover {
                        break;
                    }
                    if !chosen.contains(&i) {
                        chosen.push(i);
                    }
                }

                let mut last_err: Option<anyhow::Error> = None;
                let mut saw_rate_limit = false;

                for (pos, &idx) in chosen.iter().enumerate() {
                    let jito = &self.jito_clients[idx];

                    self.throttle_jito().await;
                    let t0 = Instant::now();
                    match jito.send_bundle_bytes_base58(&txs).await {
                        Ok(bundle_id) => {
                            let ms_submit = t0.elapsed().as_millis();
                            info!(
                                "[EXEC bundle] mint={} slot={} src={} bundle_id={} build={}ms submit={}ms txs={} be={}",
                                sig.mint,
                                sig.slot,
                                sig.source,
                                bundle_id,
                                ms_build,
                                ms_submit,
                                txs.len(),
                                jito.base_url(),
                            );
                            if self.cfg.dedup_sig_ttl_ms > 0 {
                                tokio::time::sleep(Duration::from_millis(self.cfg.dedup_sig_ttl_ms)).await;
                            }
                            if self.oneshot {
                                std::process::exit(0);
                            }
                            return Ok(());
                        }
                        Err(e) => {
                            let es = e.to_string();

                            // Some BE nodes expect base64; retry once with base64 if decode fails.
                            if es.contains("could not be decoded") {
                                warn!("[EXEC bundle] base58 decode failed; retrying with base64 on {}", jito.base_url());
                                match jito.send_bundle_bytes_base64(&txs).await {
                                    Ok(bundle_id) => {
                                        let ms_submit = t0.elapsed().as_millis();
                                        info!(
                                            "[EXEC bundle] mint={} slot={} src={} bundle_id={} build={}ms submit={}ms txs={} (encoding=base64) be={}",
                                            sig.mint,
                                            sig.slot,
                                            sig.source,
                                            bundle_id,
                                            ms_build,
                                            ms_submit,
                                            txs.len(),
                                            jito.base_url(),
                                        );
                                        if self.cfg.dedup_sig_ttl_ms > 0 {
                                            tokio::time::sleep(Duration::from_millis(self.cfg.dedup_sig_ttl_ms)).await;
                                        }
                                        if self.oneshot {
                                            std::process::exit(0);
                                        }
                                        return Ok(());
                                    }
                                    Err(e2) => {
                                        last_err = Some(e2);
                                    }
                                }
                                continue;
                            }

                            // Rate limit / congestion: mark endpoint cooldown and fail over immediately.
                            let is_rl = es.contains("Rate limit exceeded")
                                || es.contains("globally rate limited")
                                || es.contains("Network congested")
                                || es.contains("\"code\":-32097");

                            if is_rl {
                                saw_rate_limit = true;
                                let wait_ms = Self::retry_after_ms_from_err(&es)
                                    .unwrap_or(self.cfg.jito_cooldown_ms.max(1000));
                                {
                                    let mut cds = self.jito_endpoint_cooldowns.lock().await;
                                    if idx < cds.len() {
                                        cds[idx] = Instant::now() + Duration::from_millis(wait_ms);
                                    }
                                }
                                info!(
                                    "[EXEC bundle] rate-limited on {} (pos {}/{}); failover mint={} slot={} src={}",
                                    jito.base_url(),
                                    pos + 1,
                                    chosen.len(),
                                    sig.mint,
                                    sig.slot,
                                    sig.source
                                );
                                last_err = Some(e);
                                continue;
                            }

                            // Other errors
                            warn!(
                                "[EXEC bundle] submit failed on {}: {}",
                                jito.base_url(),
                                es
                            );
                            last_err = Some(e);
                        }
                    }
                }

                // If bundle endpoints are rate-limited, try sendTransaction fallback (single tx) instead of skipping.
                if self.cfg.jito_fallback_sendtx && saw_rate_limit {
                    if let Some(jito) = self.jito_clients.get(start_idx) {
                        self.throttle_jito().await;
                        let t0 = Instant::now();
                        match jito.send_transaction_bytes_base64(&txs[0]).await {
                            Ok(tx_sig) => {
                                info!(
                                    "[EXEC sendtx] mint={} slot={} src={} sig={} build={}ms submit={}ms be={}",
                                    sig.mint,
                                    sig.slot,
                                    sig.source,
                                    tx_sig,
                                    ms_build,
                                    t0.elapsed().as_millis(),
                                    jito.base_url(),
                                );
                                if self.cfg.dedup_sig_ttl_ms > 0 {
                                    tokio::time::sleep(Duration::from_millis(self.cfg.dedup_sig_ttl_ms)).await;
                                }
                                if self.oneshot {
                                    std::process::exit(0);
                                }
                                return Ok(());
                            }
                            Err(e) => {
                                warn!("[EXEC sendtx] fallback failed on {}: {}", jito.base_url(), e);
                                last_err = Some(e);
                            }
                        }
                    }
                }

                Err(last_err.unwrap_or_else(|| anyhow!("sendBundle failed")))

            }
            other => Err(anyhow!("unknown EXECUTION_MODE: {}", other)),
        }
    }
}
