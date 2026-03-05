#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [ ! -f .env ]; then
  echo "No .env found. Copy from .env.example first."
  exit 1
fi
export $(grep -v '^#' .env | xargs) >/dev/null 2>&1 || true
cargo run --release
