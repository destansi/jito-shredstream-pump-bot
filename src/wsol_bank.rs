use crate::{blockhash_cache::BlockhashCache, jito::JitoRpcClient, pumpswap};
use anyhow::Result;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_instruction,
    transaction::Transaction,
};
use std::sync::Arc;
use tracing::{info, warn};

/// WSOL bank:
/// - Creates user's WSOL ATA (idempotent)
/// - Wraps a fixed reserve once at startup (optional)
///
/// This removes 2 instructions (transfer+sync) from every AMM buy.
#[derive(Clone)]
pub struct WsolBank {
    pub wsol_ata: Pubkey,
    reserve_lamports: u64,
    min_lamports: u64,
    target_lamports: u64,
}

impl WsolBank {
    pub fn new(user: &Pubkey, reserve_sol: f64, min_sol: f64, target_sol: f64) -> Self {
        let wsol_ata = pumpswap::ata(user, &*pumpswap::WSOL_MINT, &*pumpswap::TOKEN_PROGRAM_ID);
        let reserve_lamports = (reserve_sol.max(0.0) * 1_000_000_000.0) as u64;
        let min_lamports = (min_sol.max(0.0) * 1_000_000_000.0) as u64;
        let target_lamports = (target_sol.max(0.0) * 1_000_000_000.0) as u64;
        Self { wsol_ata, reserve_lamports, min_lamports, target_lamports }
    }

    pub fn reserve_lamports(&self) -> u64 {
        self.reserve_lamports
    }

    fn desired_wrap_lamports(&self, current_amount: u64) -> u64 {
        // Priority:
        // 1) If min/target configured: top-up to target if below min
        // 2) Else: wrap fixed reserve once
        if self.min_lamports > 0 && self.target_lamports > 0 {
            if current_amount >= self.min_lamports {
                return 0;
            }
            return self.target_lamports.saturating_sub(current_amount);
        }
        self.reserve_lamports
    }

    pub async fn bootstrap(
        &self,
        payer: &Keypair,
        blockhash: &BlockhashCache,
        jito: Option<&Arc<JitoRpcClient>>,
        compute_unit_limit: u32,
        compute_unit_price_micro_lamports: u64,
    ) -> Result<()> {
        // Read current WSOL amount from account data (best-effort). If account doesn't exist, treat as 0.
        let mut current_amount: u64 = 0;
        if let Ok(acc) = blockhash.rpc().get_account(&self.wsol_ata).await {
            if acc.data.len() >= 72 {
                current_amount = u64::from_le_bytes(acc.data[64..72].try_into().unwrap());
            }
        }

        let wrap_lamports = self.desired_wrap_lamports(current_amount);
        if wrap_lamports == 0 {
            info!("WSOL bank already sufficient: current={} lamports", current_amount);
            return Ok(());
        }

        let bh = blockhash.get_or_fetch().await?;
        let mut ixs = Vec::with_capacity(6);

        // light CU settings (bootstrap not super time-critical)
        ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit.min(200_000)));
        if compute_unit_price_micro_lamports > 0 {
            ixs.push(ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price_micro_lamports));
        }

        // ensure ATA
        ixs.push(pumpswap::ata_create_idempotent_ix(
            &payer.pubkey(),
            &payer.pubkey(),
            &*pumpswap::WSOL_MINT,
            &*pumpswap::TOKEN_PROGRAM_ID,
        ));

        // wrap (top-up)
        ixs.push(system_instruction::transfer(
            &payer.pubkey(),
            &self.wsol_ata,
            wrap_lamports,
        ));
        ixs.push(pumpswap::sync_native_ix(&self.wsol_ata));

        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&payer.pubkey()),
            &[payer],
            bh,
        );
        let tx_bytes = bincode::serialize(&tx)?;

        if let Some(jito) = jito {
            // send via block engine's sendTransaction (base64)
            match jito.send_transaction_bytes_base64(&tx_bytes).await {
                Ok(sig) => {
                    info!("WSOL bootstrap sent via Jito sendTransaction: {sig}");
                    Ok(())
                }
                Err(e) => {
                    warn!("WSOL bootstrap sendTransaction failed: {e}");
                    Err(e)
                }
            }
        } else {
            // fallback: use RPC send (not implemented here)
            info!("WSOL bootstrap built (no jito client configured).");
            Ok(())
        }
    }
}
