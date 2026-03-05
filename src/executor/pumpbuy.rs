use crate::{
    blockhash_cache::BlockhashCache,
    config::Config,
    dex::pumpfun::{PUMPFUN_AMM_PROGRAM_ID, PUMPFUN_PROGRAM_ID},
    pumpswap,
    types::TradeSignal,
};
use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use std::str::FromStr;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_instruction,
    system_program,
    transaction::Transaction,
};

const BUY_EXACT_SOL_IN_DISCRIMINATOR: [u8; 8] = [56, 252, 116, 8, 158, 223, 205, 95];

const PUMPSWAP_BUY_DISCRIMINATOR: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];

pub static PUMPFUN_PROGRAM_PUBKEY: Lazy<Pubkey> =
    Lazy::new(|| Pubkey::from_str(PUMPFUN_PROGRAM_ID).expect("PUMPFUN_PROGRAM_ID"));
pub static PUMPFUN_AMM_PROGRAM_PUBKEY: Lazy<Pubkey> =
    Lazy::new(|| Pubkey::from_str(PUMPFUN_AMM_PROGRAM_ID).expect("PUMPFUN_AMM_PROGRAM_ID"));
pub static PUMPFUN_FEE_PROGRAM_PUBKEY: Lazy<Pubkey> = Lazy::new(|| {
    Pubkey::from_str("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ").expect("PUMPFUN_FEE_PROGRAM_ID")
});


fn pda(program: &Pubkey, seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, program).0
}

fn pubkey_from_slice(data: &[u8], off: usize) -> Result<Pubkey> {
    let slice = data.get(off..off + 32).ok_or_else(|| anyhow!("pubkey slice oob"))?;
    let arr: [u8; 32] = slice.try_into().map_err(|_| anyhow!("bad pubkey slice"))?;
    Ok(Pubkey::new_from_array(arr))
}


fn scale_min_out(leader_in: u64, leader_min_out: u64, our_in: u64, slippage_bps: u16) -> u64 {
    if leader_in == 0 || leader_min_out == 0 {
        return 1;
    }
    let mut v = (leader_min_out as u128)
        .saturating_mul(our_in as u128)
        / (leader_in as u128);
    // more tolerant = reduce min_out by slippage_bps
    v = v.saturating_mul((10_000u128).saturating_sub(slippage_bps as u128)) / 10_000u128;
    (v as u64).max(1)
}

fn user_volume_accumulator_pda(user: &Pubkey) -> Pubkey {
    pda(&*PUMPFUN_PROGRAM_PUBKEY, &[b"user_volume_accumulator", user.as_ref()])
}

fn build_pump_buy_exact_sol_in_ix_from_accounts(
    accounts: &[Pubkey],
    user: &Pubkey,
    spendable_sol_in: u64,
    min_tokens_out: u64,
) -> Result<Instruction> {
    if accounts.len() < 16 {
        return Err(anyhow!("leader_ix_accounts too short: {}", accounts.len()));
    }

    let mut data = Vec::with_capacity(8 + 8 + 8 + 1);
    data.extend_from_slice(&BUY_EXACT_SOL_IN_DISCRIMINATOR);
    data.extend_from_slice(&spendable_sol_in.to_le_bytes());
    data.extend_from_slice(&min_tokens_out.to_le_bytes());
    data.push(0u8);

    Ok(Instruction {
        program_id: *PUMPFUN_PROGRAM_PUBKEY,
        accounts: vec![
            AccountMeta::new_readonly(accounts[0], false), // global
            AccountMeta::new(accounts[1], false),          // fee_recipient (w)
            AccountMeta::new_readonly(accounts[2], false), // mint
            AccountMeta::new(accounts[3], false),          // bonding_curve (w)
            AccountMeta::new(accounts[4], false),          // assoc_bonding_curve (w)
            AccountMeta::new(accounts[5], false),          // assoc_user (w)
            AccountMeta::new(*user, true),                 // user (w,s)
            AccountMeta::new_readonly(accounts[7], false), // system program
            AccountMeta::new_readonly(accounts[8], false), // token program
            AccountMeta::new(accounts[9], false),          // creator_vault (w)
            AccountMeta::new_readonly(accounts[10], false),// event_authority
            AccountMeta::new_readonly(accounts[11], false),// pump program
            AccountMeta::new_readonly(accounts[12], false),// global_volume_accumulator
            AccountMeta::new(accounts[13], false),         // user_volume_accumulator (w)
            AccountMeta::new_readonly(accounts[14], false),// fee_config
            AccountMeta::new_readonly(accounts[15], false),// fee_program
        ],
        data,
    })
}
#[derive(Debug, Clone)]
pub struct BondingCurveState {
    pub complete: bool,
    pub creator: Pubkey,
    pub is_mayhem_mode: bool,
    pub token_program: Pubkey,
}

fn parse_bonding_curve(data: &[u8]) -> Result<(bool, Pubkey, bool)> {
    if data.len() < 8 + 5 * 8 + 1 {
        return Err(anyhow!("bonding curve account too small: {}", data.len()));
    }
    // discriminator [0..8]
    let mut off = 8;
    // skip 5 u64
    off += 5 * 8;
    let complete = data[off] != 0;
    off += 1;

    // creator (optional on old accounts)
    let mut creator = Pubkey::default();
    if data.len() >= off + 32 {
        creator = pubkey_from_slice(data, off)?;
        off += 32;
    }

    // is_mayhem_mode (newer accounts add this bool)
    let is_mayhem_mode = if data.len() >= off + 1 { data[off] != 0 } else { false };
    Ok((complete, creator, is_mayhem_mode))
}

async fn fetch_bonding_curve_state(rpc: &RpcClient, mint: &Pubkey) -> Result<(BondingCurveState, Pubkey, Pubkey)> {
    let bonding_curve = pda(&*PUMPFUN_PROGRAM_PUBKEY, &[b"bonding-curve", mint.as_ref()]);

    let mint_acc = rpc
        .get_account(mint)
        .await
        .map_err(|e| anyhow!("mint account fetch failed: {e}"))?;
    let token_program = mint_acc.owner;

    let bc_acc = rpc
        .get_account(&bonding_curve)
        .await
        .map_err(|e| anyhow!("bonding curve fetch failed: {e}"))?;
    let (complete, creator, is_mayhem_mode) = parse_bonding_curve(&bc_acc.data)?;

    Ok((
        BondingCurveState {
            complete,
            creator,
            is_mayhem_mode,
            token_program,
        },
        bonding_curve,
        mint_acc.owner,
    ))
}

async fn pick_fee_recipient_pump(
    rpc: &RpcClient,
    is_mayhem_mode: bool,
) -> Result<Pubkey> {
    if is_mayhem_mode {
        // Use static list; pick based on time to reduce contention.
        let idx = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as usize)
            % pumpswap::MAYHEM_FEE_RECIPIENTS.len();
        return Ok(pumpswap::MAYHEM_FEE_RECIPIENTS[idx]);
    }

    // Fetch pump Global account and pick one of the fee recipients.
    let global = pda(&*PUMPFUN_PROGRAM_PUBKEY, &[b"global"]);
    let acc = rpc.get_account(&global).await?;
    let data = acc.data;
    if data.len() < 8 + 1 + 32 + 32 {
        return Err(anyhow!("global account too small: {}", data.len()));
    }
    // anchor disc [0..8]
    let mut off = 8;
    off += 1; // initialized
    off += 32; // authority
    let fee_recipient = pubkey_from_slice(&data, off)?;
    // There is also fee_recipients array later, but fee_recipient is always valid.
    Ok(fee_recipient)
}

pub fn build_pump_buy_exact_sol_in_ix(
    mint: &Pubkey,
    bonding_curve: &Pubkey,
    creator: &Pubkey,
    token_program: &Pubkey,
    user: &Pubkey,
    fee_recipient: &Pubkey,
    spendable_sol_in: u64,
    min_tokens_out: u64,
) -> Instruction {
    let global = pda(&*PUMPFUN_PROGRAM_PUBKEY, &[b"global"]);
    let associated_bonding_curve = pumpswap::ata(bonding_curve, mint, token_program);
    let associated_user = pumpswap::ata(user, mint, token_program);

    let creator_vault = pda(&*PUMPFUN_PROGRAM_PUBKEY, &[b"creator-vault", creator.as_ref()]);
    let event_authority = pda(&*PUMPFUN_PROGRAM_PUBKEY, &[b"__event_authority"]);
    let global_volume_accumulator = pda(&*PUMPFUN_PROGRAM_PUBKEY, &[b"global_volume_accumulator"]);
    let user_volume_accumulator = pda(&*PUMPFUN_PROGRAM_PUBKEY, &[b"user_volume_accumulator", user.as_ref()]);

    let fee_config = pda(&*PUMPFUN_FEE_PROGRAM_PUBKEY, &[b"fee_config", (&*PUMPFUN_PROGRAM_PUBKEY).as_ref()]);

    let mut data = Vec::with_capacity(8 + 8 + 8 + 1);
    data.extend_from_slice(&BUY_EXACT_SOL_IN_DISCRIMINATOR);
    data.extend_from_slice(&spendable_sol_in.to_le_bytes());
    data.extend_from_slice(&min_tokens_out.to_le_bytes());
    data.push(0u8); // OptionBool::None (track_volume)

    Instruction {
        program_id: *PUMPFUN_PROGRAM_PUBKEY,
        accounts: vec![
            AccountMeta::new_readonly(global, false),
            AccountMeta::new(*fee_recipient, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(*bonding_curve, false),
            AccountMeta::new(associated_bonding_curve, false),
            AccountMeta::new(associated_user, false),
            AccountMeta::new(*user, true),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(*token_program, false),
            AccountMeta::new(creator_vault, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(*PUMPFUN_PROGRAM_PUBKEY, false),
            AccountMeta::new_readonly(global_volume_accumulator, false),
            AccountMeta::new(user_volume_accumulator, false),
            AccountMeta::new_readonly(fee_config, false),
            AccountMeta::new_readonly(*PUMPFUN_FEE_PROGRAM_PUBKEY, false),
        ],
        data,
    }
}

pub async fn build_curve_buy_tx_bytes(
    cfg: &Config,
    rpc: &RpcClient,
    blockhash: &BlockhashCache,
    payer: &Keypair,
    tip_account: &Pubkey,
    mint: &Pubkey,
) -> Result<Vec<u8>> {
    let (state, bonding_curve, token_program) = {
        let (s, bc, tp) = fetch_bonding_curve_state(rpc, mint).await?;
        (s, bc, tp)
    };

    // Pick fee recipient (mayhem uses special list).
    let fee_recipient = pick_fee_recipient_pump(rpc, state.is_mayhem_mode).await?;

    let bh = blockhash.get_or_fetch().await?;

    let spendable_sol_in = (cfg.buy_sol.max(0.0) * 1_000_000_000.0) as u64;
    let min_tokens_out = 1u64; // ultra-safe

    let mut ixs = Vec::with_capacity(8);
    ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(cfg.compute_unit_limit));
    if cfg.compute_unit_price_micro_lamports > 0 {
        ixs.push(ComputeBudgetInstruction::set_compute_unit_price(cfg.compute_unit_price_micro_lamports));
    }

    // Ensure user's mint ATA exists (idempotent) — required for brand new mints.
    ixs.push(pumpswap::ata_create_idempotent_ix(
        &payer.pubkey(),
        &payer.pubkey(),
        mint,
        &token_program,
    ));

    ixs.push(build_pump_buy_exact_sol_in_ix(
        mint,
        &bonding_curve,
        &state.creator,
        &token_program,
        &payer.pubkey(),
        &fee_recipient,
        spendable_sol_in,
        min_tokens_out,
    ));

    // Tip (must go to a Jito tip account; caller supplies).
    if cfg.jito_tip_lamports > 0 {
        ixs.push(system_instruction::transfer(
            &payer.pubkey(),
            tip_account,
            cfg.jito_tip_lamports,
        ));
    }

    let tx = Transaction::new_signed_with_payer(&ixs, Some(&payer.pubkey()), &[payer], bh);
    Ok(bincode::serialize(&tx)?)
}

fn parse_amm_global_config_fee_recipient(data: &[u8]) -> Option<Pubkey> {
    // Based on PumpSwap README: after 8-byte discriminator:
    // admin pubkey (32), lp_fee_bps u64, protocol_fee_bps u64, disable_flags u8, protocol_fee_recipients [8 pubkey]
    if data.len() < 8 + 32 + 8 + 8 + 1 + 8 * 32 {
        return None;
    }
    let mut off = 8;
    off += 32; // admin
    off += 8; // lp fee
    off += 8; // protocol fee
    off += 1; // disable flags
    // recipients array begins here
    let first = pubkey_from_slice(data, off).ok()?;
    Some(first)
}

fn parse_pool_accounts(data: &[u8]) -> Option<(Pubkey, Pubkey)> {
    // Based on PumpSwap README Pool example:
    // discriminator (8)
    // pool_bump u8
    // index u16
    // creator pubkey (32)
    // base_mint pubkey (32)
    // quote_mint pubkey (32)
    // lp_mint pubkey (32)
    // pool_base_token_account pubkey (32)
    // pool_quote_token_account pubkey (32)
    if data.len() < 8 + 1 + 2 + 32 * 6 {
        return None;
    }
    let mut off = 8;
    off += 1; // bump
    off += 2; // index
    off += 32; // creator
    off += 32; // base_mint
    off += 32; // quote_mint
    off += 32; // lp_mint
    let pool_base = pubkey_from_slice(data, off).ok()?;
    off += 32;
    let pool_quote = pubkey_from_slice(data, off).ok()?;
    Some((pool_base, pool_quote))
}

fn parse_spl_token_amount(data: &[u8]) -> Option<u64> {
    // SPL token account amount at offset 64..72
    if data.len() < 72 {
        return None;
    }
    let amt_bytes: [u8; 8] = data[64..72].try_into().ok()?;
    Some(u64::from_le_bytes(amt_bytes))
}

pub async fn build_amm_buy_tx_bytes(
    cfg: &Config,
    rpc: &RpcClient,
    blockhash: &BlockhashCache,
    payer: &Keypair,
    tip_account: &Pubkey,
    mint: &Pubkey,
    curve_state: &BondingCurveState,
) -> Result<Vec<u8>> {
    // Global config PDA
    let global_config = pda(&*PUMPFUN_AMM_PROGRAM_PUBKEY, &[b"global_config"]);

    let mut fee_recipient = None;
    if curve_state.is_mayhem_mode {
        // Use Mayhem list
        let idx = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as usize)
            % pumpswap::MAYHEM_FEE_RECIPIENTS.len();
        fee_recipient = Some(pumpswap::MAYHEM_FEE_RECIPIENTS[idx]);
    } else {
        // Fetch global_config to pick a protocol fee recipient
        if let Ok(acc) = rpc.get_account(&global_config).await {
            fee_recipient = parse_amm_global_config_fee_recipient(&acc.data);
        }
    }
    let protocol_fee_recipient = fee_recipient.ok_or_else(|| anyhow!("cannot determine PumpSwap fee recipient"))?;

    let quote_mint = *pumpswap::WSOL_MINT;
    let quote_token_program = *pumpswap::TOKEN_PROGRAM_ID;

    // Derive canonical pool creator and pool PDA.
    // Canonical creator is a PDA derived from ["pool-authority", baseMint] (by convention in PumpSwap).
    let pool_creator = pda(&*PUMPFUN_AMM_PROGRAM_PUBKEY, &[b"pool-authority", mint.as_ref()]);
    let idx0: [u8; 2] = 0u16.to_le_bytes();
    let pool = pda(
        &*PUMPFUN_AMM_PROGRAM_PUBKEY,
        &[b"pool", &idx0, pool_creator.as_ref(), mint.as_ref(), quote_mint.as_ref()],
    );

    let pool_acc = rpc.get_account(&pool).await.map_err(|e| anyhow!("pool fetch failed: {e}"))?;
    let (pool_base_ta, pool_quote_ta) =
        parse_pool_accounts(&pool_acc.data).ok_or_else(|| anyhow!("pool parse failed"))?;

    // Fetch reserves
    let (base_acc, quote_acc) = tokio::try_join!(
        rpc.get_account(&pool_base_ta),
        rpc.get_account(&pool_quote_ta)
    )?;
    let base_reserve = parse_spl_token_amount(&base_acc.data).ok_or_else(|| anyhow!("pool base reserve parse fail"))?;
    let quote_reserve = parse_spl_token_amount(&quote_acc.data).ok_or_else(|| anyhow!("pool quote reserve parse fail"))?;

    let quote_in = (cfg.buy_sol.max(0.0) * 1_000_000_000.0) as u64;
    if quote_in == 0 {
        return Err(anyhow!("BUY_SOL too small"));
    }

    // Simple constant-product quote->base estimate (ignores fees; we under-shoot output using slippage_bps).
    let base_out_est = (quote_in as u128)
        .saturating_mul(base_reserve as u128)
        / ((quote_reserve as u128).saturating_add(quote_in as u128));
    let base_out_min = (base_out_est
        .saturating_mul((10_000u128).saturating_sub(cfg.slippage_bps as u128)))
        / 10_000u128;
    let base_out = std::cmp::max(1u64, base_out_min as u64);

    let max_quote_in = (quote_in as u128)
        .saturating_mul((10_000u128).saturating_add(cfg.slippage_bps as u128))
        / 10_000u128;

    // User token accounts
    let user_base_ta = pumpswap::ata(&payer.pubkey(), mint, &curve_state.token_program);
    let user_quote_ta = pumpswap::ata(&payer.pubkey(), &quote_mint, &quote_token_program);

    // Coin creator vault accounts (best-effort: use bonding curve creator as pool.coin_creator)
    let coin_creator = curve_state.creator;
    let coin_creator_vault_authority = pda(&*PUMPFUN_AMM_PROGRAM_PUBKEY, &[b"creator_vault", coin_creator.as_ref()]);
    let coin_creator_vault_ata = pumpswap::ata(&coin_creator_vault_authority, &quote_mint, &quote_token_program);

    // Protocol fee recipient WSOL token account
    let protocol_fee_recipient_wsol = pumpswap::ata(&protocol_fee_recipient, &quote_mint, &quote_token_program);

    let event_authority = pda(&*PUMPFUN_AMM_PROGRAM_PUBKEY, &[b"__event_authority"]);

    // Fee config accounts (required by Sept 2025 update on both programs)
    let fee_config = pda(&*PUMPFUN_FEE_PROGRAM_PUBKEY, &[b"fee_config", (&*PUMPFUN_AMM_PROGRAM_PUBKEY).as_ref()]);

    // Volume accumulators (best-effort; required by newer IDLs)
    let global_volume_accumulator = pda(&*PUMPFUN_AMM_PROGRAM_PUBKEY, &[b"global_volume_accumulator"]);
    let user_volume_accumulator = pda(&*PUMPFUN_AMM_PROGRAM_PUBKEY, &[b"user_volume_accumulator", payer.pubkey().as_ref()]);

    // Build instruction data: discriminator + base_amount_out(u64) + max_quote_amount_in(u64)
    let mut data = Vec::with_capacity(8 + 8 + 8);
    data.extend_from_slice(&PUMPSWAP_BUY_DISCRIMINATOR);
    data.extend_from_slice(&base_out.to_le_bytes());
    data.extend_from_slice(&(max_quote_in as u64).to_le_bytes());

    let buy_ix = Instruction {
        program_id: *PUMPFUN_AMM_PROGRAM_PUBKEY,
        accounts: vec![
            AccountMeta::new(pool, false),                       // pool (mutable)
            AccountMeta::new(payer.pubkey(), true),              // user
            AccountMeta::new_readonly(global_config, false),
            AccountMeta::new_readonly(*mint, false),             // base_mint
            AccountMeta::new_readonly(quote_mint, false),        // quote_mint
            AccountMeta::new(user_base_ta, false),
            AccountMeta::new(user_quote_ta, false),
            AccountMeta::new(pool_base_ta, false),
            AccountMeta::new(pool_quote_ta, false),
            AccountMeta::new_readonly(protocol_fee_recipient, false),
            AccountMeta::new(protocol_fee_recipient_wsol, false),
            AccountMeta::new_readonly(curve_state.token_program, false),
            AccountMeta::new_readonly(quote_token_program, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(*pumpswap::ASSOCIATED_TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(*PUMPFUN_AMM_PROGRAM_PUBKEY, false),
            AccountMeta::new(coin_creator_vault_ata, false),
            AccountMeta::new_readonly(coin_creator_vault_authority, false),
            // newer extras appended
            AccountMeta::new_readonly(global_volume_accumulator, false),
            AccountMeta::new(user_volume_accumulator, false),
            AccountMeta::new_readonly(fee_config, false),
            AccountMeta::new_readonly(*PUMPFUN_FEE_PROGRAM_PUBKEY, false),
        ],
        data,
    };

    let bh = blockhash.get_or_fetch().await?;
    let mut ixs = Vec::with_capacity(10);
    ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(cfg.compute_unit_limit));
    if cfg.compute_unit_price_micro_lamports > 0 {
        ixs.push(ComputeBudgetInstruction::set_compute_unit_price(cfg.compute_unit_price_micro_lamports));
    }

    // Ensure base ATA exists (idempotent)
    ixs.push(pumpswap::ata_create_idempotent_ix(
        &payer.pubkey(),
        &payer.pubkey(),
        mint,
        &curve_state.token_program,
    ));
    // Ensure WSOL ATA exists (idempotent)
    ixs.push(pumpswap::ata_create_idempotent_ix(
        &payer.pubkey(),
        &payer.pubkey(),
        &quote_mint,
        &quote_token_program,
    ));

    ixs.push(buy_ix);

    if cfg.jito_tip_lamports > 0 {
        ixs.push(system_instruction::transfer(
            &payer.pubkey(),
            tip_account,
            cfg.jito_tip_lamports,
        ));
    }

    let tx = Transaction::new_signed_with_payer(&ixs, Some(&payer.pubkey()), &[payer], bh);
    Ok(bincode::serialize(&tx)?)
}

pub async fn build_trade_bundle_txs(
    cfg: &Config,
    rpc: &RpcClient,
    blockhash: &BlockhashCache,
    payer: &Keypair,
    tip_account: &Pubkey,
    sig: &TradeSignal,
) -> Result<Vec<Vec<u8>>> {
    let mint = &sig.mint;

    // Wrapper mirror: clone top-level wrapper ix (Axiom/GMGN/etc) and rewrite payer + ATAs.
    if sig.source == "wrapper_mirror" {
        let bh = if let Some(bh) = sig.recent_blockhash {
            bh
        } else {
            blockhash.get_or_fetch().await?
        };

        let wrapper_pid = sig
            .wrapper_program_id
            .ok_or_else(|| anyhow!("wrapper_mirror missing wrapper_program_id"))?;
        let wrapper_data = sig
            .wrapper_ix_data
            .as_ref()
            .ok_or_else(|| anyhow!("wrapper_mirror missing wrapper_ix_data"))?
            .clone();
        let leader_accounts = sig
            .leader_ix_accounts
            .as_ref()
            .ok_or_else(|| anyhow!("wrapper_mirror missing leader_ix_accounts"))?
            .clone();

        let tx_bytes = build_wrapper_mirror_tx_bytes(
            cfg,
            payer,
            tip_account,
            &sig.leader,
            mint,
            wrapper_pid,
            &leader_accounts,
            &wrapper_data,
            bh,
        )?;
        return Ok(vec![tx_bytes]);
    }


    // Odin-fastpath: for direct curve buys, build from leader ix accounts + stream blockhash (0 RPC).
    if sig.source == "curve_direct_buy" {
        if let (Some(bh), Some(leader_accounts), Some(leader_in), Some(leader_min)) = (
            sig.recent_blockhash,
            sig.leader_ix_accounts.as_ref(),
            sig.leader_in,
            sig.leader_min_out,
        ) {
            // clone and swap user-specific accounts
            let mut accts = leader_accounts.clone();

            // token program is index 8 in Pump buy ix
            let token_program = accts.get(8).copied().ok_or_else(|| anyhow!("leader_ix_accounts missing token_program"))?;

            // user mint ATA (index 5)
            let user_ata = pumpswap::ata(&payer.pubkey(), mint, &token_program);
            if accts.len() > 5 { accts[5] = user_ata; }
            if accts.len() > 6 { accts[6] = payer.pubkey(); }
            if accts.len() > 13 { accts[13] = user_volume_accumulator_pda(&payer.pubkey()); }

            let spendable_sol_in = (cfg.buy_sol.max(0.0) * 1_000_000_000.0) as u64;
            let min_tokens_out = scale_min_out(leader_in, leader_min, spendable_sol_in, cfg.slippage_bps);

            let mut ixs = Vec::with_capacity(8);
            ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(cfg.compute_unit_limit));
            if cfg.compute_unit_price_micro_lamports > 0 {
                ixs.push(ComputeBudgetInstruction::set_compute_unit_price(cfg.compute_unit_price_micro_lamports));
            }

            // Ensure ATA exists (idempotent)
            ixs.push(pumpswap::ata_create_idempotent_ix(
                &payer.pubkey(),
                &payer.pubkey(),
                mint,
                &token_program,
            ));

            ixs.push(build_pump_buy_exact_sol_in_ix_from_accounts(
                &accts,
                &payer.pubkey(),
                spendable_sol_in,
                min_tokens_out,
            )?);

            if cfg.jito_tip_lamports > 0 {
                ixs.push(system_instruction::transfer(
                    &payer.pubkey(),
                    tip_account,
                    cfg.jito_tip_lamports,
                ));
            }

            let tx = Transaction::new_signed_with_payer(
                &ixs,
                Some(&payer.pubkey()),
                &[payer],
                bh,
            );
            let tx_bytes = bincode::serialize(&tx)?;
            return Ok(vec![tx_bytes]);
        }
    }


// Fastpath: Pump.fun curve buy invoked under wrapper (e.g., Axiom/FLASH).
// We reuse the extracted Pump.fun account block + leader recent_blockhash to avoid RPC.
if sig.source == "curve_cpi_buy" {
    if let (Some(bh), Some(leader_accounts)) = (sig.recent_blockhash, sig.leader_ix_accounts.as_ref()) {
        let mut accts = leader_accounts.clone();

        // token program is index 8 in Pump buy ix (Tokenkeg or Token-2022)
        let token_program = accts
            .get(8)
            .copied()
            .ok_or_else(|| anyhow!("leader_ix_accounts missing token_program"))?;

        // user mint ATA (index 5)
        let user_ata = pumpswap::ata(&payer.pubkey(), mint, &token_program);
        if accts.len() > 5 { accts[5] = user_ata; }
        if accts.len() > 6 { accts[6] = payer.pubkey(); }
        if accts.len() > 13 { accts[13] = user_volume_accumulator_pda(&payer.pubkey()); }

        let spendable_sol_in = (cfg.buy_sol.max(0.0) * 1_000_000_000.0) as u64;
        let min_tokens_out = 1u64; // ultra-safe (same policy as build_curve_buy_tx_bytes)

        let mut ixs = Vec::with_capacity(8);
        ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(cfg.compute_unit_limit));
        if cfg.compute_unit_price_micro_lamports > 0 {
            ixs.push(ComputeBudgetInstruction::set_compute_unit_price(cfg.compute_unit_price_micro_lamports));
        }

        // Ensure ATA exists (idempotent)
        ixs.push(pumpswap::ata_create_idempotent_ix(
            &payer.pubkey(),
            &payer.pubkey(),
            mint,
            &token_program,
        ));

        ixs.push(build_pump_buy_exact_sol_in_ix_from_accounts(
            &accts,
            &payer.pubkey(),
            spendable_sol_in,
            min_tokens_out,
        )?);

        if cfg.jito_tip_lamports > 0 {
            ixs.push(system_instruction::transfer(
                &payer.pubkey(),
                tip_account,
                cfg.jito_tip_lamports,
            ));
        }

        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&payer.pubkey()),
            &[payer],
            bh,
        );
        let tx_bytes = bincode::serialize(&tx)?;
        return Ok(vec![tx_bytes]);
    }
}


    let (state, _bonding_curve, _token_program) = fetch_bonding_curve_state(rpc, mint).await?;

    if !state.complete {
        let tx = build_curve_buy_tx_bytes(cfg, rpc, blockhash, payer, tip_account, mint).await?;
        return Ok(vec![tx]);
    }

    // AMM path for completed curve:
    // NOTE: requires WSOL. We assume WSOL bank was bootstrapped already.
    match build_amm_buy_tx_bytes(cfg, rpc, blockhash, payer, tip_account, mint, &state).await {
        Ok(tx) => Ok(vec![tx]),
        Err(e) => {
            let es = e.to_string();
            // During bond->AMM transition the pool PDA may not exist yet.
            // Don't spam errors; fall back to curve buy once.
            if es.contains("AccountNotFound") || es.contains("pool fetch failed") {
                let tx = build_curve_buy_tx_bytes(cfg, rpc, blockhash, payer, tip_account, mint).await?;
                Ok(vec![tx])
            } else {
                Err(e)
            }
        }
    }
}

fn is_program_or_sysvar_readonly(k: &Pubkey) -> bool {
    let sys = system_program::id();
    let tok = *pumpswap::TOKEN_PROGRAM_ID;
    let tok22 = *pumpswap::TOKEN_2022_PROGRAM_ID;
    let ata = *pumpswap::ASSOCIATED_TOKEN_PROGRAM_ID;
    let rent: Pubkey = "SysvarRent111111111111111111111111111111111".parse().unwrap();
    let ixsys: Pubkey = "Sysvar1nstructions1111111111111111111111111".parse().unwrap();
    *k == sys || *k == tok || *k == tok22 || *k == ata || *k == rent || *k == ixsys
}

fn build_wrapper_mirror_tx_bytes(
    cfg: &Config,
    payer: &Keypair,
    tip_account: &Pubkey,
    leader: &Pubkey,
    mint: &Pubkey,
    wrapper_program_id: Pubkey,
    leader_ix_accounts: &[Pubkey],
    wrapper_ix_data: &[u8],
    bh: solana_sdk::hash::Hash,
) -> Result<Vec<u8>> {
    let our = payer.pubkey();

    // Derive our WSOL ATA (bank should keep it funded).
    let our_wsol_ata = pumpswap::ata(&our, &*pumpswap::WSOL_MINT, &*pumpswap::TOKEN_PROGRAM_ID);
    let leader_wsol_ata = pumpswap::ata(leader, &*pumpswap::WSOL_MINT, &*pumpswap::TOKEN_PROGRAM_ID);

    // Derive leader/our token ATA (best-effort) for inferred mint.
    let leader_token_ata = if *mint != Pubkey::default() {
        Some(pumpswap::ata(leader, mint, &*pumpswap::TOKEN_PROGRAM_ID))
    } else {
        None
    };
    let our_token_ata = if *mint != Pubkey::default() {
        Some(pumpswap::ata(&our, mint, &*pumpswap::TOKEN_PROGRAM_ID))
    } else {
        None
    };

    // Rewrite accounts
    let mut rewritten: Vec<Pubkey> = Vec::with_capacity(leader_ix_accounts.len());
    for a in leader_ix_accounts {
        if *a == *leader {
            rewritten.push(our);
        } else if *a == leader_wsol_ata {
            rewritten.push(our_wsol_ata);
        } else if let (Some(l_ata), Some(o_ata)) = (leader_token_ata, our_token_ata) {
            if *a == l_ata {
                rewritten.push(o_ata);
            } else {
                rewritten.push(*a);
            }
        } else {
            rewritten.push(*a);
        }
    }

    // Build metas (conservative: writable for non-program/sysvar accounts)
    let mut metas: Vec<AccountMeta> = Vec::with_capacity(rewritten.len());
    for a in &rewritten {
        if *a == our {
            metas.push(AccountMeta::new(*a, true));
        } else if is_program_or_sysvar_readonly(a) {
            metas.push(AccountMeta::new_readonly(*a, false));
        } else {
            metas.push(AccountMeta::new(*a, false));
        }
    }

    let mut ixs: Vec<Instruction> = Vec::with_capacity(8);
    ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(cfg.compute_unit_limit));
    if cfg.compute_unit_price_micro_lamports > 0 {
        ixs.push(ComputeBudgetInstruction::set_compute_unit_price(cfg.compute_unit_price_micro_lamports));
    }

    // Ensure our WSOL ATA exists (idempotent). Even if bank is funded, ATA must exist.
    ixs.push(pumpswap::ata_create_idempotent_ix(
        &our,
        &our,
        &*pumpswap::WSOL_MINT,
        &*pumpswap::TOKEN_PROGRAM_ID,
    ));

    // Ensure output ATA if mint is known
    if *mint != Pubkey::default() {
        ixs.push(pumpswap::ata_create_idempotent_ix(
            &our,
            &our,
            mint,
            &*pumpswap::TOKEN_PROGRAM_ID,
        ));
    }

    // Wrapper ix
    ixs.push(Instruction {
        program_id: wrapper_program_id,
        accounts: metas,
        data: wrapper_ix_data.to_vec(),
    });

    // Jito tip
    if cfg.jito_tip_lamports > 0 {
        ixs.push(system_instruction::transfer(&our, tip_account, cfg.jito_tip_lamports));
    }

    let tx = Transaction::new_signed_with_payer(&ixs, Some(&our), &[payer], bh);
    Ok(bincode::serialize(&tx)?)
}
