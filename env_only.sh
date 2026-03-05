#!/usr/bin/env bash
set -e

# --- Wallet ---
export KEYPAIR_PATH="/home/destantaham/.config/gcloud/solana/id.json"

# --- RPC (ALT resolve için önerilir) ---
export RPC_HTTP_URL="https://mainnet.helius-rpc.com/?api-key=8b763ff5-ce93-4a71-bee2-e85663f40f33"

# --- Shredstream ---
export SHREDSTREAM_PROXY_GRPC_URLS="grpc://127.0.0.1:50051"

# --- Leaders ---
export LEADER_WALLETS="4EsYuWFZAt1PfNJq8Jr7monip43gNqrQ7k2Kne1npqJx,2sT2usN51Zdoqt6wvTHFa8AySdLAi2rsGybsHwegVnX9,HRV512AYkkxhHpcERBN2x8Xf2mGksPFavwfAr22XSrGb,BNnN2MqfWLvgThYBsv6v8JQaYZXYKYahC5YCy27Ct1cX"
export STRICT_SIGNER=false

# --- ALT ---
export BLOCKHASH_REFRESH_MS=0
export RESOLVE_ALTS=true
export ALT_MAX_CONCURRENT_FETCH=2
export ALT_MISS_SKIP=true

# --- Wrapper allowlist (Jupiter + Axiom + Trojan) ---
export WRAPPER_PROGRAM_IDS="JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4,FLASHX8DrLbgeR8FcfNV1F5krxYcYMUdBkrP1EPBtxB9,troyXT7Ty3s2rjJe4bqWaroUrS4Fjd8rbHHNHxcACF4"

# --- Trade ---
export EXECUTION_MODE=bundle_pumpbuy
export BUY_SOL=0.1
export SLIPPAGE_BPS=500

# --- WSOL bank (startup top-up) ---
export WSOL_MIN_SOL=0.1
export WSOL_TARGET_SOL=0.3

# --- Jito ---
export USE_JITO=true
export JITO_BLOCK_ENGINE_URL="https://amsterdam.mainnet.block-engine.jito.wtf"
export JITO_BLOCK_ENGINE_URLS="$JITO_BLOCK_ENGINE_URL"
export JITO_TIP_ACCOUNT="96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5"
export JITO_TIP_LAMPORTS=30000
export JITO_MIN_SUBMIT_INTERVAL_MS=1100

# --- Logging ---
export STATS_INTERVAL_SECS=5
export CPI_HEURISTIC_MINT=true
export RUST_LOG=info

env | grep -E 'USE_JITO|EXECUTION_MODE|JITO_|KEYPAIR_PATH|RPC_HTTP_URL' | sort
echo "[ENV] USE_JITO=$USE_JITO EXECUTION_MODE=$EXECUTION_MODE KEYPAIR_PATH=$KEYPAIR_PATH"
exec cargo run --release