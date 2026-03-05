#![allow(dead_code)]
use once_cell::sync::Lazy;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
    sysvar,
};

use std::str::FromStr;

pub static TOKEN_PROGRAM_ID: Lazy<Pubkey> = Lazy::new(|| {
    Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap()
});

pub static TOKEN_2022_PROGRAM_ID: Lazy<Pubkey> = Lazy::new(|| {
    Pubkey::from_str("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb").unwrap()
});

pub static ASSOCIATED_TOKEN_PROGRAM_ID: Lazy<Pubkey> = Lazy::new(|| {
    Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL").unwrap()
});

pub static WSOL_MINT: Lazy<Pubkey> = Lazy::new(|| {
    Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap()
});

/// Mayhem fee recipients (bonding curve + PumpSwap) — use any randomly.
/// Source: pump-public-docs README.
pub static MAYHEM_FEE_RECIPIENTS: Lazy<[Pubkey; 8]> = Lazy::new(|| {
    [
        Pubkey::from_str("GesfTA3X2arioaHp8bbKdjG9vJtskViWACZoYvxp4twS").unwrap(),
        Pubkey::from_str("4budycTjhs9fD6xw62VBducVTNgMgJJ5BgtKq7mAZwn6").unwrap(),
        Pubkey::from_str("8SBKzEQU4nLSzcwF4a74F2iaUDQyTfjGndn6qUWBnrpR").unwrap(),
        Pubkey::from_str("4UQeTP1T39KZ9Sfxzo3WR5skgsaP6NZa87BAkuazLEKH").unwrap(),
        Pubkey::from_str("8sNeir4QsLsJdYpc9RZacohhK1Y5FLU3nC5LXgYB4aa6").unwrap(),
        Pubkey::from_str("Fh9HmeLNUMVCvejxCtCL2DbYaRyBFVJ5xrWkLnMH6fdk").unwrap(),
        Pubkey::from_str("463MEnMeGyJekNZFQSTUABBEbLnvMTALbT6ZmsxAbAdq").unwrap(),
        Pubkey::from_str("6AUH3WEHucYZyC61hqpqYUWVto5qA5hjHuNQ32GNnNxA").unwrap(),
    ]
});

/// Derive associated token account for (owner, mint, token_program).
pub fn ata(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    let seeds: [&[u8]; 3] = [owner.as_ref(), token_program.as_ref(), mint.as_ref()];
    Pubkey::find_program_address(&seeds, &ASSOCIATED_TOKEN_PROGRAM_ID).0
}

/// Create ATA idempotent instruction (works even if ATA already exists).
pub fn ata_create_idempotent_ix(
    payer: &Pubkey,
    owner: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Instruction {
    let ata_addr = ata(owner, mint, token_program);
    Instruction {
        program_id: *ASSOCIATED_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(ata_addr, false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(*token_program, false),
            AccountMeta::new_readonly(sysvar::rent::id(), false),
        ],
        // Associated token program: CreateIdempotent = 1
        data: vec![1u8],
    }
}

/// SPL Token SyncNative instruction for WSOL accounts (legacy token program).
pub fn sync_native_ix(wsol_ata: &Pubkey) -> Instruction {
    // TokenInstruction::SyncNative = 17
    Instruction {
        program_id: *TOKEN_PROGRAM_ID,
        accounts: vec![AccountMeta::new(*wsol_ata, false)],
        data: vec![17u8],
    }
}
