use anyhow::{anyhow, Context, Result};
use moka::future::Cache;
use solana_address_lookup_table_program::state::AddressLookupTable;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{account::Account, pubkey::Pubkey};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct AltResolver {
    rpc: Arc<RpcClient>,
    cache: Cache<Pubkey, Arc<Vec<Pubkey>>>,
    sem: Arc<Semaphore>,
}

impl AltResolver {
    pub fn new(rpc_http_url: String, max_concurrent_fetch: usize) -> Self {
        let rpc = Arc::new(RpcClient::new(rpc_http_url));
        let cache = Cache::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(300))
            .build();

        Self {
            rpc,
            cache,
            sem: Arc::new(Semaphore::new(max_concurrent_fetch.max(1))),
        }
    }

    /// Returns the FULL address list stored in the ALT account.
    pub async fn get_alt_addresses(&self, table_key: Pubkey) -> Result<Arc<Vec<Pubkey>>> {
        self.cache
            .try_get_with(table_key, {
                let this = self.clone();
                async move {
                    let _permit = this.sem.acquire().await.expect("semaphore closed");

                    let acc = this
                        .rpc
                        .get_account(&table_key)
                        .await
                        .with_context(|| format!("get_account(ALT {table_key})"))?;

                    let state = AddressLookupTable::deserialize(&acc.data).map_err(|e| {
                        anyhow!("ALT state deserialize failed for {table_key}: {e}")
                    })?;

                    Ok::<Arc<Vec<Pubkey>>, anyhow::Error>(Arc::new(state.addresses.to_vec()))
                }
            })
            .await
            .map_err(|e| anyhow!("ALT cache init failed: {e}"))
    }

    /// Returns cached ALT addresses if present (NO fetch). Useful for fastpath.
    pub async fn get_cached_alt_addresses(&self, table_key: Pubkey) -> Option<Arc<Vec<Pubkey>>> {
        self.cache.get(&table_key).await
    }

    /// Best-effort batch fetch (used by CPI heuristic to identify SPL mints).
    pub async fn get_multiple_accounts(&self, keys: &[Pubkey]) -> Result<Vec<Option<Account>>> {
        let _permit = self.sem.acquire().await.expect("semaphore closed");
        let res = self
            .rpc
            .get_multiple_accounts(keys)
            .await
            .with_context(|| format!("get_multiple_accounts(len={})", keys.len()))?;
        Ok(res)
    }
}
