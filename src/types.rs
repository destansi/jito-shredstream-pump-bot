#![allow(dead_code)]
use solana_sdk::{hash::Hash, pubkey::Pubkey, signature::Signature};

#[derive(Debug, Clone)]
pub struct TradeSignal {
    pub slot: u64,
    pub leader: Pubkey,
    pub mint: Pubkey,
    pub signature: Signature,
    pub source: &'static str,

    // Odin-fastpath: leader tx data for 0-RPC clone builds
    pub recent_blockhash: Option<Hash>,
    pub leader_ix_accounts: Option<Vec<Pubkey>>,
    pub leader_in: Option<u64>,
    pub leader_min_out: Option<u64>,

    // Wrapper mirror (Axiom/GMGN/etc): top-level instruction to mirror.
    pub wrapper_program_id: Option<Pubkey>,
    pub wrapper_ix_data: Option<Vec<u8>>,
}
