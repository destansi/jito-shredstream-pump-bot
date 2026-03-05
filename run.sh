#!/usr/bin/env bash
set -euo pipefail

# If a .env file exists in the project root, load it first (optional)
if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

# --- Wallet ---
export KEYPAIR_PATH="${KEYPAIR_PATH:-/home/destantaham/.config/gcloud/solana/id.json}"

# --- RPC (ALT + minimal reads + blockhash) ---
export RPC_HTTP_URL="${RPC_HTTP_URL:-https://mainnet.helius-rpc.com/?api-key=apikey}"

# --- Shredstream ---
export SHREDSTREAM_PROXY_GRPC_URLS="${SHREDSTREAM_PROXY_GRPC_URLS:-grpc://127.0.0.1:50051}"

# --- Leaders ---
export LEADER_WALLETS="${LEADER_WALLETS:-4EsYuWFZAt1PfNJq8Jr7monip43gNqrQ7k2Kne1npqJx,HRV512AYkkxhHpcERBN2x8Xf2mGksPFavwfAr22XSrGb,BNnN2MqfWLvgThYBsv6v8JQaYZXYKYahC5YCy27Ct1cX}"
export STRICT_SIGNER="${STRICT_SIGNER:-false}"

# --- ALT (DO NOT MISS v0+LUT buys) ---
export RESOLVE_ALTS="${RESOLVE_ALTS:-true}"
export ALT_MISS_SKIP="${ALT_MISS_SKIP:-false}"
export ALT_MAX_CONCURRENT_FETCH="${ALT_MAX_CONCURRENT_FETCH:-6}"
export PARSE_CONCURRENCY="${PARSE_CONCURRENCY:-128}"

# --- Dedup ---
# Sig dedup: short burst gate
export DEDUP_SIG_TTL_MS="${DEDUP_SIG_TTL_MS:-3000}"
# Mint dedup: applied ONLY after a successful execute (so a failed submit won't block future attempts)
export DEDUP_MINT_TTL_SECS="${DEDUP_MINT_TTL_SECS:-43200}"

# --- Wrapper allowlist (Trojan removed) ---
# Keep FLASH (Axiom) or you can miss Axiom-driven buys.
export WRAPPER_PROGRAM_IDS="${WRAPPER_PROGRAM_IDS:-JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4,FLASHX8DrLbgeR8FcfNV1F5krxYcYMUdBkrP1EPBtxB9}"

# --- Trade ---
export EXECUTION_MODE="${EXECUTION_MODE:-bundle_pumpbuy}"
export BUY_SOL="${BUY_SOL:-0.1}"
export SLIPPAGE_BPS="${SLIPPAGE_BPS:-5000}"
export COMPUTE_UNIT_LIMIT="${COMPUTE_UNIT_LIMIT:-600000}"
export COMPUTE_UNIT_PRICE_MICRO_LAMPORTS="${COMPUTE_UNIT_PRICE_MICRO_LAMPORTS:-0}"

# --- WSOL bank ---
export WSOL_MIN_SOL="${WSOL_MIN_SOL:-0.1}"
export WSOL_TARGET_SOL="${WSOL_TARGET_SOL:-0.3}"
export WSOL_RESERVE_SOL="${WSOL_RESERVE_SOL:-0}"

# --- Jito ---
export USE_JITO="${USE_JITO:-true}"
export JITO_BLOCK_ENGINE_URLS="${JITO_BLOCK_ENGINE_URLS:-https://amsterdam.mainnet.block-engine.jito.wtf}"
# ",https://frankfurt.mainnet.block-engine.jito.wtf,https://dublin.mainnet.block-engine.jito.wtf}"
export JITO_TIP_ACCOUNT="${JITO_TIP_ACCOUNT:-96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5}"
export JITO_TIP_LAMPORTS="${JITO_TIP_LAMPORTS:-20000000}"
export JITO_MIN_SUBMIT_INTERVAL_MS="${JITO_MIN_SUBMIT_INTERVAL_MS:-1400}"
export JITO_FAILOVER_MAX="${JITO_FAILOVER_MAX:-0}"
export JITO_COOLDOWN_MS="${JITO_COOLDOWN_MS:-2000}"
export JITO_FALLBACK_SENDTX="${JITO_FALLBACK_SENDTX:-true}"

# --- Blockhash cache ---
# Keep this ON to avoid a blocking RPC call at the exact trade moment.
export BLOCKHASH_REFRESH_MS="${BLOCKHASH_REFRESH_MS:-250}"

# --- Logging ---
export CPI_HEURISTIC_MINT="${CPI_HEURISTIC_MINT:-true}"
export STATS_INTERVAL_SECS="${STATS_INTERVAL_SECS:-0}"
export RUST_LOG="${RUST_LOG:-info}"

env | grep -E 'USE_JITO|EXECUTION_MODE|JITO_|KEYPAIR_PATH|RPC_HTTP_URL|ALT_MISS_SKIP|ALT_MAX_CONCURRENT_FETCH|DEDUP_' | sort
echo "[ENV] USE_JITO=$USE_JITO EXECUTION_MODE=$EXECUTION_MODE KEYPAIR_PATH=$KEYPAIR_PATH ALT_MISS_SKIP=$ALT_MISS_SKIP"

exec cargo run --release
