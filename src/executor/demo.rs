use crate::{blockhash_cache::BlockhashCache, config::Config};
use anyhow::Result;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_instruction,
    transaction::Transaction,
};

pub async fn build_demo_tx_bytes(
    cfg: &Config,
    blockhash: &BlockhashCache,
    payer: &Keypair,
    tip_account: &Pubkey,
) -> Result<Vec<u8>> {
    let bh = blockhash.get_or_fetch().await?;
    let mut ixs = Vec::new();
    ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(cfg.compute_unit_limit.min(50_000)));
    if cfg.compute_unit_price_micro_lamports > 0 {
        ixs.push(ComputeBudgetInstruction::set_compute_unit_price(cfg.compute_unit_price_micro_lamports));
    }
    if cfg.jito_tip_lamports > 0 {
        ixs.push(system_instruction::transfer(&payer.pubkey(), tip_account, cfg.jito_tip_lamports));
    }
    // noop
    ixs.push(system_instruction::transfer(&payer.pubkey(), &payer.pubkey(), 1));

    let tx = Transaction::new_signed_with_payer(&ixs, Some(&payer.pubkey()), &[payer], bh);
    Ok(bincode::serialize(&tx)?)
}
