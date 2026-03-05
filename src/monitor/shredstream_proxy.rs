use crate::{
    alt_resolver::AltResolver,
    dex::pumpfun::{PUMP_BUY_METHOD, PUMPFUN_AMM_PROGRAM_ID, PUMPFUN_PROGRAM_ID},
    types::TradeSignal,
};
use anyhow::Result;
use futures::StreamExt;
use moka::sync::Cache as SyncCache;
use solana_entry::entry::Entry as SolanaEntry;
use solana_sdk::{
    hash::Hash,
    instruction::CompiledInstruction,
    message::{v0::LoadedAddresses, MessageHeader, VersionedMessage},
    pubkey::Pubkey,
    signature::Signature,
    transaction::VersionedTransaction,
};
use std::{
    collections::HashSet,
    str::FromStr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

pub mod proto {
    tonic::include_proto!("shredstream");
}
use proto::shredstream_proxy_client::ShredstreamProxyClient;
use proto::SubscribeEntriesRequest;

// Axiom Trade program (wrapper)
const AXIOM_PROGRAM_ID: &str = "FLASHX8DrLbgeR8FcfNV1F5krxYcYMUdBkrP1EPBtxB9";

// Trojan Trade program (often fee-only on top-level; can spam false signals)
// If you want to mirror Trojan as a true trade wrapper, remove the skip below.
const TROJAN_PROGRAM_ID: &str = "troyXT7Ty3s2rjJe4bqWaroUrS4Fjd8rbHHNHxcACF4";

// WSOL mint
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

// Pump fees program (required in newer pump fun trades)
const PUMPFUN_FEE_PROGRAM_ID: &str = "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ";

// Buy olarak saymak için “wrap edilen SOL” alt eşiği.
// (sell tarafında çoğunlukla sadece rent/ufak değerler olur)
const MIN_WRAP_LAMPORTS: u64 = 10_000_000; // 0.01 SOL

#[derive(Clone)]
pub struct MonitorConfig {
    pub grpc_url: String,
    pub leaders: Arc<HashSet<Pubkey>>,
    pub strict_signer: bool,
    pub resolve_alts: bool,
    pub parse_concurrency: usize,
    pub sig_dedup_ttl_ms: u64,
    pub cpi_heuristic_mint: bool,
    pub stats_interval_secs: u64,
    pub debug_leader_sample: u64,

    // Wrapper/router allowlist
    pub wrapper_program_ids: Arc<HashSet<Pubkey>>,

    // ALT behavior
    pub alt_miss_skip: bool,
}

pub async fn run_monitor(cfg: MonitorConfig, alt: AltResolver, out: mpsc::Sender<TradeSignal>) -> Result<()> {
    let pump_curve: Pubkey = PUMPFUN_PROGRAM_ID.parse()?;
    let pump_amm: Pubkey = PUMPFUN_AMM_PROGRAM_ID.parse()?;

    // Stats
    let c_rx_batches = Arc::new(AtomicU64::new(0));
    let c_rx_entries = Arc::new(AtomicU64::new(0));
    let c_rx_txs = Arc::new(AtomicU64::new(0));
    let c_leader_signer_txs = Arc::new(AtomicU64::new(0));
    let c_stage0_candidates = Arc::new(AtomicU64::new(0));
    let c_signals = Arc::new(AtomicU64::new(0));
    let c_curve_direct_buy = Arc::new(AtomicU64::new(0));
    let c_amm_axiom_buy = Arc::new(AtomicU64::new(0));
    let c_pda_curve_buyish = Arc::new(AtomicU64::new(0));
    let c_rpc_buyish = Arc::new(AtomicU64::new(0));

    if cfg.stats_interval_secs > 0 {
        let interval = Duration::from_secs(cfg.stats_interval_secs.max(1));
        let grpc_url = cfg.grpc_url.clone();

        let a = c_rx_batches.clone();
        let b = c_rx_entries.clone();
        let c = c_rx_txs.clone();
        let d = c_leader_signer_txs.clone();
        let e = c_stage0_candidates.clone();
        let f = c_signals.clone();
        let g = c_curve_direct_buy.clone();
        let h = c_amm_axiom_buy.clone();
        let i = c_pda_curve_buyish.clone();
        let j = c_rpc_buyish.clone();

        tokio::spawn(async move {
            let mut last = Instant::now();
            loop {
                tokio::time::sleep(interval).await;
                let dt = last.elapsed().as_secs_f64();
                last = Instant::now();
                info!(
                    "[stats {}] dt={:.1}s batches={} entries={} txs={} leaderSignerTx={} stage0Candidates={} signals={} curveDirectBuy={} ammAxiomBuy={} pdaCurveBuyish={} rpcBuyish={}",
                    grpc_url,
                    dt,
                    a.swap(0, Ordering::Relaxed),
                    b.swap(0, Ordering::Relaxed),
                    c.swap(0, Ordering::Relaxed),
                    d.swap(0, Ordering::Relaxed),
                    e.swap(0, Ordering::Relaxed),
                    f.swap(0, Ordering::Relaxed),
                    g.swap(0, Ordering::Relaxed),
                    h.swap(0, Ordering::Relaxed),
                    i.swap(0, Ordering::Relaxed),
                    j.swap(0, Ordering::Relaxed),
                );
            }
        });
    }

    // Signature dedupe
    let sig_dedup: SyncCache<String, ()> = SyncCache::builder()
        .time_to_live(Duration::from_millis(cfg.sig_dedup_ttl_ms.max(200)))
        .max_capacity(300_000)
        .build();

    let mut backoff_ms: u64 = 250;

    loop {
        let (batch_tx, mut batch_rx) = mpsc::channel::<(u64, Vec<u8>)>(512);

        let endpoint = match tonic::transport::Endpoint::from_shared(cfg.grpc_url.clone()) {
            Ok(e) => e
                .connect_timeout(Duration::from_secs(5))
                .tcp_keepalive(Some(Duration::from_secs(30)))
                .http2_keep_alive_interval(Duration::from_secs(20))
                .keep_alive_timeout(Duration::from_secs(10)),
            Err(e) => {
                error!("invalid grpc url ({}): {e}", cfg.grpc_url);
                return Ok(());
            }
        };

        let channel = match endpoint.connect().await {
            Ok(c) => c,
            Err(e) => {
                warn!("connect failed ({}): {e}; retrying", cfg.grpc_url);
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(5_000);
                continue;
            }
        };

        let mut client = ShredstreamProxyClient::new(channel);
        let resp = match client.subscribe_entries(SubscribeEntriesRequest {}).await {
            Ok(r) => r,
            Err(e) => {
                warn!("subscribe_entries failed ({}): {e}; retrying", cfg.grpc_url);
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(5_000);
                continue;
            }
        };

        info!("connected shredstream monitor: {}", cfg.grpc_url);
        backoff_ms = 250;

        // Reader
        let grpc_url = cfg.grpc_url.clone();
        let reader = tokio::spawn(async move {
            let mut stream = resp.into_inner();
            let mut dropped: u64 = 0;
            loop {
                match stream.message().await {
                    Ok(Some(m)) => {
                        if batch_tx.try_send((m.slot, m.entries)).is_err() {
                            dropped += 1;
                            if dropped % 200 == 1 {
                                warn!("entries queue full; dropped={dropped}");
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        warn!("stream error ({}): {e}", grpc_url);
                        break;
                    }
                }
            }
        });

        // Parser loop
        while let Some((slot, entries_bytes)) = batch_rx.recv().await {
            c_rx_batches.fetch_add(1, Ordering::Relaxed);

            let leaders = cfg.leaders.clone();
            let strict_signer = cfg.strict_signer;
            let resolve_alts = cfg.resolve_alts;
            let cpi_heuristic_mint = cfg.cpi_heuristic_mint;
            let alt = alt.clone();

            let entries: Vec<SolanaEntry> =
                match tokio::task::spawn_blocking(move || bincode::deserialize(&entries_bytes)).await {
                    Ok(Ok(v)) => v,
                    Ok(Err(e)) => {
                        debug!("deserialize entries failed: {e}");
                        continue;
                    }
                    Err(e) => {
                        debug!("spawn_blocking join error: {e}");
                        continue;
                    }
                };

            c_rx_entries.fetch_add(entries.len() as u64, Ordering::Relaxed);

            // Stage-0 candidates
            let mut candidates: Vec<VersionedTransaction> = Vec::new();
            for e in entries {
                for tx in e.transactions {
                    c_rx_txs.fetch_add(1, Ordering::Relaxed);

                    let (hdr0, keys0, _ix0, has0) = decompose_message(&tx.message);

                    let leader_signer = if leaders.is_empty() {
                        true
                    } else {
                        keys0.iter()
                            .take(hdr0.num_required_signatures as usize)
                            .any(|k| leaders.contains(k))
                    };
                    let leader_present = if leaders.is_empty() {
                        true
                    } else {
                        keys0.iter().any(|k| leaders.contains(k))
                    };

                    if !leaders.is_empty() && leader_signer {
                        c_leader_signer_txs.fetch_add(1, Ordering::Relaxed);
                    }

                    let leader_ok = if strict_signer { leader_signer } else { leader_present };
                    if !leader_ok {
                        continue;
                    }

                    if is_pump_related_candidate_no_alt(&pump_curve, &pump_amm, &tx.message) {
                        c_stage0_candidates.fetch_add(1, Ordering::Relaxed);
                        candidates.push(tx);
                    } else if cpi_heuristic_mint && has0 {
                        // ALT içinde pump_amm olabilir
                        candidates.push(tx);
                    }
                }
            }

            if candidates.is_empty() {
                continue;
            }

            let wrapper_ids = cfg.wrapper_program_ids.clone();
            let alt_miss_skip = cfg.alt_miss_skip;

            let mut futs = futures::stream::iter(candidates.into_iter())
                .map(|tx| {
                    let leaders = leaders.clone();
                    let alt = alt.clone();
                    let wrapper_ids = wrapper_ids.clone();
                    async move {
                        parse_buy_signal_from_tx(
                            &pump_curve,
                            &pump_amm,
                            wrapper_ids.as_ref(),
                            &leaders,
                            strict_signer,
                            resolve_alts,
                            alt_miss_skip,
                            slot,
                            tx,
                            &alt,
                            cpi_heuristic_mint,
                        )
                        .await
                    }
                })
                .buffer_unordered(cfg.parse_concurrency.max(1));

            while let Some(res) = futs.next().await {
                let sig_opt = match res {
                    Ok(v) => v,
                    Err(e) => {
                        debug!("parse error: {e}");
                        continue;
                    }
                };
                let sig = match sig_opt {
                    Some(s) => s,
                    None => continue,
                };

                let sig_str = sig.signature.to_string();
                if sig_dedup.get(&sig_str).is_some() {
                    continue;
                }
                sig_dedup.insert(sig_str, ());

                c_signals.fetch_add(1, Ordering::Relaxed);
                match sig.source {
                    "curve_direct_buy" => c_curve_direct_buy.fetch_add(1, Ordering::Relaxed),
                    "amm_axiom_buy" => c_amm_axiom_buy.fetch_add(1, Ordering::Relaxed),
                    "curve_pda_buyish" => c_pda_curve_buyish.fetch_add(1, Ordering::Relaxed),
                    "rpc_buyish" => c_rpc_buyish.fetch_add(1, Ordering::Relaxed),
                    _ => 0,
                };

                let _ = out.try_send(sig);
            }
        }

        let _ = reader.await;
        warn!("monitor ended (stream closed): {}; reconnecting", cfg.grpc_url);
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn decompose_message(msg: &VersionedMessage) -> (MessageHeader, Vec<Pubkey>, Vec<CompiledInstruction>, bool) {
    match msg {
        VersionedMessage::Legacy(m) => (m.header, m.account_keys.clone(), m.instructions.clone(), false),
        VersionedMessage::V0(m) => (
            m.header,
            m.account_keys.clone(),
            m.instructions.clone(),
            !m.address_table_lookups.is_empty(),
        ),
    }
}

fn is_pump_related_candidate_no_alt(pump_curve: &Pubkey, pump_amm: &Pubkey, msg: &VersionedMessage) -> bool {
    let static_keys = msg.static_account_keys();
    let global = pump_global_pda(pump_curve);
    let event = pump_event_authority_pda(pump_curve);
    static_keys.iter().any(|k| *k == *pump_curve || *k == *pump_amm || *k == global || *k == event)
}

async fn resolve_full_keys_if_needed(
    msg: &VersionedMessage,
    static_keys: &mut Vec<Pubkey>,
    resolve_alts: bool,
    alt_miss_skip: bool,
    alt: &AltResolver,
) -> Result<bool> {
    let lookups = match msg {
        VersionedMessage::V0(m) => &m.address_table_lookups,
        _ => return Ok(true),
    };

    if lookups.is_empty() || !resolve_alts {
        return Ok(true);
    }

    let mut loaded = LoadedAddresses { writable: vec![], readonly: vec![] };

    for l in lookups {
        let all = if alt_miss_skip {
            if let Some(v) = alt.get_cached_alt_addresses(l.account_key).await {
                v
            } else {
                // Prefetch in background and skip this tx (fastpath should not block).
                let alt2 = alt.clone();
                let key = l.account_key;
                tokio::spawn(async move {
                    let _ = alt2.get_alt_addresses(key).await;
                });
                return Ok(false);
            }
        } else {
            alt.get_alt_addresses(l.account_key).await?
        };
        for &i in &l.writable_indexes {
            let idx = i as usize;
            if idx < all.len() {
                loaded.writable.push(all[idx]);
            }
        }
        for &i in &l.readonly_indexes {
            let idx = i as usize;
            if idx < all.len() {
                loaded.readonly.push(all[idx]);
            }
        }
    }

    static_keys.extend_from_slice(&loaded.writable);
    static_keys.extend_from_slice(&loaded.readonly);
    Ok(true)
}


fn recent_blockhash_from_msg(msg: &VersionedMessage) -> Hash {
    match msg {
        VersionedMessage::Legacy(m) => m.recent_blockhash,
        VersionedMessage::V0(m) => m.recent_blockhash,
    }
}

async fn parse_buy_signal_from_tx(
    pump_curve: &Pubkey,
    pump_amm: &Pubkey,
    wrapper_ids: &HashSet<Pubkey>,
    leaders: &HashSet<Pubkey>,
    strict_signer: bool,
    resolve_alts: bool,
    alt_miss_skip: bool,
    slot: u64,
    tx: VersionedTransaction,
    alt: &AltResolver,
    cpi_heuristic_mint: bool,
) -> Result<Option<TradeSignal>> {
    let sig = tx.signatures.first().copied().unwrap_or(Signature::default());
    let msg = &tx.message;

    let (header, mut keys, instructions, has_lookups) = decompose_message(msg);

    if has_lookups && resolve_alts {
        let ok = resolve_full_keys_if_needed(msg, &mut keys, true, alt_miss_skip, alt).await?;
        if !ok {
            // ALT miss: skip tx to keep fastpath stable.
            return Ok(None);
        }
    }

    // Leader filter
    let leader_opt = if leaders.is_empty() {
        keys.first().copied()
    } else if strict_signer {
        keys.iter()
            .take(header.num_required_signatures as usize)
            .find(|k| leaders.contains(*k))
            .copied()
    } else {
        keys.iter().find(|k| leaders.contains(*k)).copied()
    };

    if !leaders.is_empty() && leader_opt.is_none() {
        return Ok(None);
    }
    let leader = leader_opt.unwrap_or_default();

    // 1) DIRECT curve buy (top-level Pump.fun program) — BUY-only
    for ix in &instructions {
        let program_id = match keys.get(ix.program_id_index as usize) {
            Some(p) => *p,
            None => continue,
        };
        if program_id != *pump_curve {
            continue;
        }
        if ix.data.len() < 8 {
            continue;
        }
        let method = u64::from_le_bytes(ix.data[0..8].try_into().unwrap());

        // BUY-only: SELL’i tamamen ignore ediyoruz
        if method != PUMP_BUY_METHOD {
            continue;
        }

        if ix.accounts.len() <= 2 {
            continue;
        }
        let mint_idx = ix.accounts[2] as usize;
        let mint = match keys.get(mint_idx) {
            Some(m) => *m,
            None => continue,
        };

        // Odin-fastpath: carry leader ix accounts + blockhash to avoid RPC in executor
        let bh = recent_blockhash_from_msg(msg);

        let mut leader_ix_accounts: Vec<Pubkey> = Vec::with_capacity(ix.accounts.len());
        for a in &ix.accounts {
            if let Some(pk) = keys.get(*a as usize) {
                leader_ix_accounts.push(*pk);
            }
        }

        let (leader_in, leader_min_out) = if ix.data.len() >= 24 {
            let li = u64::from_le_bytes(ix.data[8..16].try_into().unwrap());
            let lm = u64::from_le_bytes(ix.data[16..24].try_into().unwrap());
            (Some(li), Some(lm))
        } else {
            (None, None)
        };

        return Ok(Some(TradeSignal {
            slot,
            leader,
            mint,
            signature: sig,
            source: "curve_direct_buy",
            recent_blockhash: Some(bh),
            leader_ix_accounts: Some(leader_ix_accounts),
            leader_in,
            leader_min_out,
            wrapper_program_id: None,
            wrapper_ix_data: None,
        }));
    }


    if !cpi_heuristic_mint {
        return Ok(None);
    }


// 2) Curve buy via Axiom wrapper (Pump.fun bonding curve invoked under FLASH).
// Fastpath: extract the Pump.fun "buy" account block from the Axiom ix and reuse leader blockhash.
// This avoids RPC and keeps build time ~0ms for Token-2022 mints too.
{
    let axiom: Pubkey = AXIOM_PROGRAM_ID.parse().expect("AXIOM_PROGRAM_ID");
    let fee_prog: Pubkey = PUMPFUN_FEE_PROGRAM_ID.parse().expect("PUMPFUN_FEE_PROGRAM_ID");
    let global = pump_global_pda(pump_curve);

    // BUY-ish gate: only accept if ATA-create exists at top-level (first buy).
    if has_ata_top_level(&keys, &instructions) {
        for ix in &instructions {
            let program_id = match keys.get(ix.program_id_index as usize) {
                Some(p) => *p,
                None => continue,
            };
            if program_id != axiom {
                continue;
            }

            let mut accs: Vec<Pubkey> = Vec::with_capacity(ix.accounts.len());
            for &ai in &ix.accounts {
                if let Some(pk) = keys.get(ai as usize) {
                    accs.push(*pk);
                }
            }

            // must include Pump.fun program id (bonding curve path)
            if !accs.contains(pump_curve) {
                continue;
            }

            let Some(pos) = accs.iter().position(|k| *k == global) else { continue };
            if pos + 16 > accs.len() {
                continue;
            }
            let slice = accs[pos..pos + 16].to_vec();

            // validate expected layout
            if slice[0] != global {
                continue;
            }
            if slice.get(7).copied() != Some(system_program_id()) {
                continue;
            }
            let tok = token_program_id();
            let tok22 = token_2022_program_id();
            let tp = slice.get(8).copied().unwrap_or_default();
            if tp != tok && tp != tok22 {
                continue;
            }
            if slice.get(11).copied() != Some(*pump_curve) {
                continue;
            }
            if slice.get(15).copied() != Some(fee_prog) {
                continue;
            }

            let mint = slice[2];
            if !mint.to_string().ends_with("pump") {
                continue;
            }

            return Ok(Some(TradeSignal {
                slot,
                leader,
                mint,
                signature: sig,
                source: "curve_cpi_buy",
                recent_blockhash: Some(recent_blockhash_from_msg(msg)),
                leader_ix_accounts: Some(slice),
                leader_in: None,
                leader_min_out: None,
                wrapper_program_id: None,
                wrapper_ix_data: None,
            }));
        }
    }
}

// 3) AMM via Axiom wrapper — BUY-only doğrulama:

    // 3) AMM via Axiom wrapper — BUY-only doğrulama:
    // “SOL->WSOL wrap amount” büyükse buy say, küçük/yoksa sell/other say ve ignore et.
    if keys.contains(pump_amm) {
        let axiom: Pubkey = AXIOM_PROGRAM_ID.parse().expect("AXIOM_PROGRAM_ID");
        let wsol: Pubkey = WSOL_MINT.parse().expect("WSOL");

        let wrap_amt = axiom_wrap_amount_lamports(&keys, &instructions, &axiom, &wsol);
        if wrap_amt >= MIN_WRAP_LAMPORTS {
            if let Some(mint) = axiom_infer_amm_base_mint(&keys, &instructions, &axiom, pump_amm, &wsol) {
                return Ok(Some(TradeSignal {
                    slot,
                    leader,
                    mint,
                    signature: sig,
                    source: "amm_axiom_buy",
                    recent_blockhash: Some(recent_blockhash_from_msg(msg)),
                    leader_ix_accounts: None,
                    leader_in: None,
                    leader_min_out: None,
                    wrapper_program_id: None,
                    wrapper_ix_data: None,
                }));
            }
        }
    }

    // 3) Curve wrapper/CPI — PDA inference (BUY-ish gating: ATA var ise)
    // (Sells için en büyük false-positive kaynağı burasıydı; ATA yoksa sinyal üretme.)
    if keys.contains(pump_curve) && has_ata_top_level(&keys, &instructions) {
        if let Some(mint2) = infer_pump_mint_by_pdas(&keys, pump_curve) {
            return Ok(Some(TradeSignal {
                slot,
                leader,
                mint: mint2,
                signature: sig,
                source: "curve_pda_buyish",
                recent_blockhash: Some(recent_blockhash_from_msg(msg)),
                leader_ix_accounts: None,
                leader_in: None,
                leader_min_out: None,
                wrapper_program_id: None,
                wrapper_ix_data: None,
            }));
        }
    }

    // 4) Optional RPC-assisted mint discovery (BUY-ish gating)
    // - AMM için: wrap>=MIN_WRAP
    // - Curve için: ATA var
    let buyish_ok = if keys.contains(pump_amm) {
        let axiom: Pubkey = AXIOM_PROGRAM_ID.parse().expect("AXIOM_PROGRAM_ID");
        let wsol: Pubkey = WSOL_MINT.parse().expect("WSOL");
        axiom_wrap_amount_lamports(&keys, &instructions, &axiom, &wsol) >= MIN_WRAP_LAMPORTS
    } else {
        has_ata_top_level(&keys, &instructions)
    };

    if buyish_ok {
        if let Some(mint2) = find_first_spl_mint_in_accounts(&keys, &instructions, alt).await? {
            return Ok(Some(TradeSignal {
                slot,
                leader,
                mint: mint2,
                signature: sig,
                source: "rpc_buyish",
                recent_blockhash: Some(recent_blockhash_from_msg(msg)),
                leader_ix_accounts: None,
                leader_in: None,
                leader_min_out: None,
                wrapper_program_id: None,
                wrapper_ix_data: None,
            }));
        }
    }

    // 5) WRAPPER mirror (Axiom/GMGN/etc) as last resort
    if !wrapper_ids.is_empty() {
        let trojan: Pubkey = TROJAN_PROGRAM_ID.parse().expect("TROJAN_PROGRAM_ID");
        for ix in &instructions {
            let program_id = match keys.get(ix.program_id_index as usize) {
                Some(p) => *p,
                None => continue,
            };
            if !wrapper_ids.contains(&program_id) {
                continue;
            }

            // Trojan top-level instruction is very often just fee transfer (both on buys and sells).
            // We skip it to avoid false positives and Jito rate-limit spam.
            if program_id == trojan {
                continue;
            }

            let mut wrapper_accounts: Vec<Pubkey> = Vec::with_capacity(ix.accounts.len());
            for a in &ix.accounts {
                if let Some(pk) = keys.get(*a as usize) {
                    wrapper_accounts.push(*pk);
                }
            }

            let mint = {
                let axiom: Pubkey = AXIOM_PROGRAM_ID.parse().expect("AXIOM_PROGRAM_ID");
                let wsol: Pubkey = WSOL_MINT.parse().expect("WSOL");
                if program_id == axiom && keys.contains(pump_amm) {
                    let wrap_amt = axiom_wrap_amount_lamports(&keys, &instructions, &axiom, &wsol);
                    // BUY-only: if there is no meaningful SOL->WSOL wrap, this is almost always sell/fee/noise.
                    if wrap_amt < MIN_WRAP_LAMPORTS {
                        continue;
                    }
                    axiom_infer_amm_base_mint(&keys, &instructions, &axiom, pump_amm, &wsol)
                        .or_else(|| infer_mint_by_suffix_pump(&keys))
                        .unwrap_or_default()
                } else {
                    infer_mint_by_suffix_pump(&keys).unwrap_or_default()
                }
            };

            // If we couldn't infer a mint, don't emit a signal.
            if mint == Pubkey::default() {
                continue;
            }

            return Ok(Some(TradeSignal {
                slot,
                leader,
                mint,
                signature: sig,
                source: "wrapper_mirror",
                recent_blockhash: Some(recent_blockhash_from_msg(msg)),
                leader_ix_accounts: Some(wrapper_accounts),
                leader_in: None,
                leader_min_out: None,
                wrapper_program_id: Some(program_id),
                wrapper_ix_data: Some(ix.data.clone()),
            }));
        }
    }

    Ok(None)
}

// ---------- Axiom helpers (stream-only) ----------

fn axiom_wrap_amount_lamports(
    keys: &[Pubkey],
    instructions: &[CompiledInstruction],
    axiom_program: &Pubkey,
    wsol: &Pubkey,
) -> u64 {
    // Axiom wrap ix tipik olarak:
    // - program_id = AXIOM
    // - accounts içinde WSOL + Token Program + System Program var
    // - data[0] == 0x01 ve ardından u64 lamports gelir
    let sys = system_program_id();
    let tok = token_program_id();

    for ix in instructions {
        let Some(pid) = keys.get(ix.program_id_index as usize) else { continue };
        if *pid != *axiom_program {
            continue;
        }

        // quick accounts contains check
        let mut has_wsol = false;
        let mut has_sys = false;
        let mut has_tok = false;
        for &ai in &ix.accounts {
            let Some(k) = keys.get(ai as usize) else { continue };
            if *k == *wsol { has_wsol = true; }
            if *k == sys { has_sys = true; }
            if *k == tok { has_tok = true; }
        }
        if !(has_wsol && has_sys && has_tok) {
            continue;
        }

        if ix.data.len() >= 9 && ix.data[0] == 0x01 {
            let amt = u64::from_le_bytes(ix.data[1..9].try_into().unwrap());
            return amt;
        }
    }

    0
}

fn axiom_infer_amm_base_mint(
    keys: &[Pubkey],
    instructions: &[CompiledInstruction],
    axiom_program: &Pubkey,
    pump_amm: &Pubkey,
    wsol: &Pubkey,
) -> Option<Pubkey> {
    // Swap ix: AXIOM program, accounts içinde pump_amm + wsol var.
    // Bu listede WSOL’un hemen solundaki account base mint (Solscan’de Base Mint).
    for ix in instructions {
        let pid = *keys.get(ix.program_id_index as usize)?;
        if pid != *axiom_program {
            continue;
        }

        let mut accs: Vec<Pubkey> = Vec::with_capacity(ix.accounts.len());
        let mut has_pamm = false;
        for &ai in &ix.accounts {
            let k = *keys.get(ai as usize)?;
            if k == *pump_amm {
                has_pamm = true;
            }
            accs.push(k);
        }
        if !has_pamm {
            continue;
        }

        let wpos = accs.iter().position(|k| *k == *wsol)?;
        if wpos == 0 {
            continue;
        }
        let cand = accs[wpos - 1];

        // sanity: cand should not be obvious programs
        let sys = system_program_id();
        let tok = token_program_id();
        if cand == *wsol || cand == sys || cand == tok || cand == *axiom_program || cand == *pump_amm {
            continue;
        }

        return Some(cand);
    }

    None
}

fn has_ata_top_level(keys: &[Pubkey], instructions: &[CompiledInstruction]) -> bool {
    let ata = associated_token_program_id();
    for ix in instructions {
        if let Some(pid) = keys.get(ix.program_id_index as usize) {
            if *pid == ata {
                return true;
            }
        }
    }
    false
}

// ---------- RPC mint discovery (Token + Token-2022) ----------

async fn find_first_spl_mint_in_accounts(
    keys: &[Pubkey],
    instructions: &[CompiledInstruction],
    alt: &AltResolver,
) -> Result<Option<Pubkey>> {
    let tok = token_program_id();
    let tok22 = token_2022_program_id();
    let wsol: Pubkey = WSOL_MINT.parse().expect("WSOL");

    let mut uniq: Vec<Pubkey> = Vec::with_capacity(80);
    for ix in instructions {
        for &ai in &ix.accounts {
            let idx = ai as usize;
            if let Some(k) = keys.get(idx) {
                if *k == tok || *k == tok22 || *k == wsol {
                    continue;
                }
                if !uniq.contains(k) {
                    uniq.push(*k);
                }
                if uniq.len() >= 80 {
                    break;
                }
            }
        }
        if uniq.len() >= 80 {
            break;
        }
    }

    if uniq.is_empty() {
        return Ok(None);
    }

    let accts = alt.get_multiple_accounts(&uniq).await?;
    for (k, aopt) in uniq.iter().zip(accts.into_iter()) {
        let Some(a) = aopt else { continue };

        // Legacy mint is 82 bytes.
        if a.owner == tok && a.data.len() == 82 {
            return Ok(Some(*k));
        }
        // Token-2022 mint: >=82, but NOT the common token-account size (170).
        if a.owner == tok22 && a.data.len() >= 82 && a.data.len() != 170 {
            return Ok(Some(*k));
        }
    }

    Ok(None)
}

// ---------- Pump curve PDA inference ----------

fn system_program_id() -> Pubkey {
    static SYS: std::sync::OnceLock<Pubkey> = std::sync::OnceLock::new();
    *SYS.get_or_init(|| Pubkey::from_str("11111111111111111111111111111111").unwrap())
}

fn token_program_id() -> Pubkey {
    static TOKEN: std::sync::OnceLock<Pubkey> = std::sync::OnceLock::new();
    *TOKEN.get_or_init(|| Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap())
}

fn token_2022_program_id() -> Pubkey {
    static TOKEN22: std::sync::OnceLock<Pubkey> = std::sync::OnceLock::new();
    *TOKEN22.get_or_init(|| Pubkey::from_str("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb").unwrap())
}

fn associated_token_program_id() -> Pubkey {
    static ATA: std::sync::OnceLock<Pubkey> = std::sync::OnceLock::new();
    *ATA.get_or_init(|| Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL").unwrap())
}

fn pump_global_pda(pump_program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"global"], pump_program).0
}

fn pump_event_authority_pda(pump_program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], pump_program).0
}

fn pump_bonding_curve_pda(pump_program: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"bonding-curve", mint.as_ref()], pump_program).0
}

fn infer_pump_mint_by_pdas(keys: &[Pubkey], pump_program: &Pubkey) -> Option<Pubkey> {
    let global = pump_global_pda(pump_program);
    let event = pump_event_authority_pda(pump_program);
    if !keys.contains(&global) || !keys.contains(&event) {
        return None;
    }

    let set: HashSet<Pubkey> = keys.iter().copied().collect();
    let ata_program = associated_token_program_id();
    let tok = token_program_id();
    let tok22 = token_2022_program_id();

    for mint in keys {
        if *mint == *pump_program
            || *mint == global
            || *mint == event
            || *mint == tok
            || *mint == tok22
            || *mint == ata_program
        {
            continue;
        }

        let bonding_curve = pump_bonding_curve_pda(pump_program, mint);
        if !set.contains(&bonding_curve) {
            continue;
        }

        // Token-2022 mints use Tokenz... in ATA seeds; legacy mints use Tokenkeg...
        for tp in [tok, tok22] {
            let associated_bonding_curve = Pubkey::find_program_address(
                &[bonding_curve.as_ref(), tp.as_ref(), mint.as_ref()],
                &ata_program,
            )
            .0;
            if set.contains(&associated_bonding_curve) {
                return Some(*mint);
            }
        }
    }

    None
}

fn infer_mint_by_suffix_pump(keys: &[Pubkey]) -> Option<Pubkey> {
    for k in keys {
        if k.to_string().ends_with("pump") {
            return Some(*k);
        }
    }
    None
}