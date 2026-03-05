use solana_sdk::pubkey::Pubkey;

// Pump.fun bonding curve program id (mainnet)
pub const PUMPFUN_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

// PumpSwap / Pump AMM program id (mainnet)
pub const PUMPFUN_AMM_PROGRAM_ID: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

// Anchor discriminator for "global:buy".
pub const PUMP_BUY_METHOD: u64 = 16927863322537952870;

// Anchor discriminator for "global:sell".
pub const PUMP_SELL_METHOD: u64 = 12502976635542562355;

pub fn pump_curve_program() -> Pubkey {
    PUMPFUN_PROGRAM_ID.parse().expect("invalid PUMPFUN_PROGRAM_ID")
}
pub fn pump_amm_program() -> Pubkey {
    PUMPFUN_AMM_PROGRAM_ID.parse().expect("invalid PUMPFUN_AMM_PROGRAM_ID")
}