#!/usr/bin/env bash
set -euo pipefail

# This script is an EXAMPLE wrapper for the official jito-labs/shredstream-proxy binary.
# You run it in the shredstream-proxy repo after building it.

: "${JITO_REGION:=amsterdam}"             # amsterdam | ny | tokyo (example)
: "${PROXY_GRPC_BIND:=0.0.0.0:50051}"     # your machine IP may change; bind to 0.0.0.0
: "${AUTH_KEYPAIR_PATH:=./jito_shred_auth.json}"

# NOTE: You must whitelist the pubkey of AUTH_KEYPAIR_PATH on Jito (NOT your payer).

# Replace the following with the exact command from Jito docs for your region.
# Example (pseudo):
# ./target/release/shredstream-proxy \
#   --region "$JITO_REGION" \
#   --grpc-bind "$PROXY_GRPC_BIND" \
#   --auth-keypair "$AUTH_KEYPAIR_PATH"

echo "Edit this script with the exact proxy flags from docs.jito.wtf for your setup."
