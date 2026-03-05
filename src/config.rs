use clap::Parser;

/// Main configuration for the bot.
///
/// Notes:
/// - Keep values small/fast by default.
/// - Anything time-critical should already be cached (blockhash refresher, ALT cache, etc.).
#[derive(Parser, Debug, Clone)]
#[command(author, version, about)]
pub struct Config {
    // =========
    // Network
    // =========
    /// HTTP RPC endpoint (Helius or any RPC). Used for minimal reads + blockhash refresh.
    #[arg(long, env = "RPC_HTTP_URL", default_value = "https://api.mainnet-beta.solana.com")]
    pub rpc_http_url: String,

    /// Shredstream proxy gRPC URLs (comma-separated).
    #[arg(long, env = "SHREDSTREAM_PROXY_GRPC_URLS", value_delimiter = ',', default_value = "")]
    pub shredstream_proxy_grpc_urls: Vec<String>,

    /// Leader wallets to follow (comma-separated pubkeys). Used by monitor.
    #[arg(long, env = "LEADER_WALLETS", value_delimiter = ',', default_value = "")]
    pub leader_wallets: Vec<String>,

    // =========
    // Keypair
    // =========
    /// Path to a Solana keypair JSON file.
    #[arg(long, env = "KEYPAIR_PATH", default_value = "")]
    pub keypair_path: String,

    /// Base58 private key string (optional).
    #[arg(long, env = "WALLET_PRIVATE_KEY", default_value = "")]
    pub wallet_private_key: String,

    /// Path to a file that contains base58 private key string (optional).
    #[arg(long, env = "WALLET_PRIVATE_KEY_PATH", default_value = "")]
    pub wallet_private_key_path: String,

    /// If true, monitor requires the leader tx signer to be exactly in leader set.
    #[arg(long, env = "STRICT_SIGNER", default_value_t = true)]
    pub strict_signer: bool,

    // =========
    // Monitor tuning
    // =========
    #[arg(long, env = "RESOLVE_ALTS", default_value_t = true)]
    pub resolve_alts: bool,

    #[arg(long, env = "ALT_MAX_CONCURRENT_FETCH", default_value_t = 64)]
    pub alt_max_concurrent_fetch: usize,

    #[arg(long, env = "PARSE_CONCURRENCY", default_value_t = 64)]
    pub parse_concurrency: usize,

    /// Signature dedup TTL inside monitor.
    #[arg(long, env = "DEDUP_SIG_TTL_MS", default_value_t = 3_000)]
    pub dedup_sig_ttl_ms: u64,

    /// Mint dedup TTL in main loop.
    #[arg(long, env = "DEDUP_MINT_TTL_SECS", default_value_t = 30)]
    pub dedup_mint_ttl_secs: u64,

    #[arg(long, env = "CPI_HEURISTIC_MINT", default_value_t = true)]
    pub cpi_heuristic_mint: bool,

    #[arg(long, env = "STATS_INTERVAL_SECS", default_value_t = 10)]
    pub stats_interval_secs: u64,

    #[arg(long, env = "DEBUG_LEADER_SAMPLE", default_value_t = 0)]
    pub debug_leader_sample: u64,

    // =========
    // Wrapper mirror
    // =========
    /// Comma-separated allowlist of wrapper/router program IDs (Axiom/GMGN/Trojan/etc).
    /// If a tx contains a top-level instruction to one of these programs, the bot will try to
    /// mirror that instruction (rewrite payer + ATAs) and submit via Jito.
    #[arg(long, env = "WRAPPER_PROGRAM_IDS", value_delimiter = ',', default_value = "")]
    pub wrapper_program_ids: Vec<String>,

    /// If true, for v0 txs: when an ALT lookup table is not in cache, skip the tx immediately
    /// and prefetch the ALT in background. This keeps fastpath build time stable.
    #[arg(long, env = "ALT_MISS_SKIP", default_value_t = true)]
    pub alt_miss_skip: bool,

    // =========
    // Execution tuning
    // =========
    /// Executor worker threads. (Tokio multi-thread runtime already exists; this gates tasks.)
    #[arg(long, env = "EXECUTOR_CONCURRENCY", default_value_t = 16)]
    pub executor_concurrency: usize,

    /// Execution mode:
    /// - log: only log signals
    /// - demo: send a tiny demo tx
    /// - bundle_pumpbuy: build Pump/PumpSwap buy and send via Jito bundle
    #[arg(long, env = "EXECUTION_MODE", default_value = "log")]
    pub execution_mode: String,

    /// Exit after first successful submission.
    #[arg(long, env = "ONESHOT", default_value_t = false)]
    pub oneshot: bool,

    // =========
    // Blockhash cache
    // =========
    /// Refresh interval for recent blockhash cache.
    #[arg(long, env = "BLOCKHASH_REFRESH_MS", default_value_t = 1200)]
    pub blockhash_refresh_ms: u64,

    // =========
    // Trade params
    // =========
    /// SOL budget per buy (in SOL).
    #[arg(long, env = "BUY_SOL", default_value_t = 0.05)]
    pub buy_sol: f64,

    /// Slippage bps used for PumpSwap quote input calculations.
    #[arg(long, env = "SLIPPAGE_BPS", default_value_t = 1500)]
    pub slippage_bps: u16,

    /// Compute unit limit.
    #[arg(long, env = "COMPUTE_UNIT_LIMIT", default_value_t = 250_000)]
    pub compute_unit_limit: u32,

    /// Compute unit price in micro-lamports.
    #[arg(long, env = "COMPUTE_UNIT_PRICE_MICRO_LAMPORTS", default_value_t = 0)]
    pub compute_unit_price_micro_lamports: u64,

    // =========
    // Jito
    // =========
    #[arg(long, env = "USE_JITO", default_value_t = true)]
    pub use_jito: bool,

    /// Primary Jito block engine URL (bundles endpoint).
    #[arg(long, env = "JITO_BLOCK_ENGINE_URL", default_value = "https://amsterdam.mainnet.block-engine.jito.wtf")]
    pub jito_block_engine_url: String,

    /// Optional comma-separated list of block engine URLs to try (first success wins).
    #[arg(long, env = "JITO_BLOCK_ENGINE_URLS", value_delimiter = ',', default_value = "")]
    pub jito_block_engine_urls: Vec<String>,

    /// Optional UUID / auth (some providers require).
    #[arg(long, env = "JITO_UUID", default_value = "")]
    pub jito_uuid: String,

    /// Tip lamports (paid to a Jito tip account in the tx).
    #[arg(long, env = "JITO_TIP_LAMPORTS", default_value_t = 50_000)]
    pub jito_tip_lamports: u64,

    /// Tip account (if empty, fetch from block engine and pick one).
    #[arg(long, env = "JITO_TIP_ACCOUNT", default_value = "")]
    pub jito_tip_account: String,

    // =========
    // Optional RPC dual submit (not used in bundle mode)
    // =========
    #[arg(long, env = "DUAL_SUBMIT_RPC", default_value_t = false)]
    pub dual_submit_rpc: bool,

    #[arg(long, env = "RPC_SKIP_PREFLIGHT", default_value_t = true)]
    pub rpc_skip_preflight: bool,

    // =========
    // WSOL bank (AMM buys)
    // =========
    /// If > 0, create WSOL ATA at startup and wrap this amount once.
    #[arg(long, env = "WSOL_RESERVE_SOL", default_value_t = 0.0)]
    pub wsol_reserve_sol: f64,

    /// Minimum WSOL balance (in SOL) required before starting leader mirroring.
    /// If current WSOL is below this, bot will top-up to WSOL_TARGET_SOL once at startup.
    #[arg(long, env = "WSOL_MIN_SOL", default_value_t = 0.0)]
    pub wsol_min_sol: f64,

    /// Target WSOL balance (in SOL) after startup top-up.
    #[arg(long, env = "WSOL_TARGET_SOL", default_value_t = 0.0)]
    pub wsol_target_sol: f64,

    /// Minimum interval between Jito txn submissions (ms). Helps with free endpoint limits.
    #[arg(long, env = "JITO_MIN_SUBMIT_INTERVAL_MS", default_value_t = 1100)]
    pub jito_min_submit_interval_ms: u64,

    /// On rate-limit/congestion errors, try a few other block-engine URLs (fast failover).
    #[arg(long, env = "JITO_FAILOVER_MAX", default_value_t = 2)]
    pub jito_failover_max: usize,

    /// Cooldown applied to a block-engine URL after a -32097 / rate-limit response.
    #[arg(long, env = "JITO_COOLDOWN_MS", default_value_t = 2000)]
    pub jito_cooldown_ms: u64,

    /// If all bundle endpoints are rate-limited, fall back to sendTransaction (single tx) instead of skipping.
    #[arg(long, env = "JITO_FALLBACK_SENDTX", default_value_t = true)]
    pub jito_fallback_sendtx: bool,
}
