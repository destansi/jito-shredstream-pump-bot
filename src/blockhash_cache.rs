use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::hash::Hash;
use std::sync::{Arc, RwLock};
use tokio::time::{sleep, Duration};
use tracing::{debug, warn};

/// Very small recent blockhash cache refreshed on an interval.
/// This keeps the hot-path from calling RPC for every buy.
///
/// NOTE: We intentionally use "processed" blockhash for speed.
/// If your RPC is flaky, increase refresh_ms.
#[derive(Clone)]
pub struct BlockhashCache {
    rpc: Arc<RpcClient>,
    latest: Arc<RwLock<Option<Hash>>>,
}

impl BlockhashCache {
    pub fn new(rpc_url: String) -> Self {
        Self {
            rpc: Arc::new(RpcClient::new(rpc_url)),
            latest: Arc::new(RwLock::new(None)),
        }
    }

    /// Background refresher. Safe to call once.
    pub fn spawn_refresher(&self, refresh_ms: u64) {
        if refresh_ms == 0 {
            debug!("blockhash refresher disabled (BLOCKHASH_REFRESH_MS=0)");
            return;
        }

        let rpc = self.rpc.clone();
        let latest = self.latest.clone();
        tokio::spawn(async move {
            let mut backoff_ms: u64 = refresh_ms.max(50);
            loop {
                match rpc.get_latest_blockhash().await {
                    Ok(h) => {
                        if let Ok(mut w) = latest.write() {
                            *w = Some(h);
                        }
                        backoff_ms = refresh_ms.max(50);
                        sleep(Duration::from_millis(refresh_ms.max(50))).await;
                    }
                    Err(e) => {
                        warn!("blockhash refresh failed: {e}; backing off {backoff_ms}ms");
                        sleep(Duration::from_millis(backoff_ms)).await;
                        backoff_ms = (backoff_ms * 2).min(20_000);
                    }
                }
            }
        });
        debug!("blockhash refresher started: {}ms", refresh_ms);
    }

    pub fn get_latest(&self) -> Option<Hash> {
        self.latest.read().ok().and_then(|g| *g)
    }

    pub async fn get_or_fetch(&self) -> anyhow::Result<Hash> {
        if let Some(h) = self.get_latest() {
            return Ok(h);
        }
        let h = self.rpc.get_latest_blockhash().await?;
        if let Ok(mut w) = self.latest.write() {
            *w = Some(h);
        }
        Ok(h)
    }

    pub fn rpc(&self) -> Arc<RpcClient> {
        self.rpc.clone()
    }
}
