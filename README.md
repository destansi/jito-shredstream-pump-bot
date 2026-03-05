# jito_shredstream_pump_bot

Low-latency Solana bot skeleton:
- Reads pre-confirmation Entries from a local `shredstream-proxy` (gRPC).
- Detects Pump.fun BUY instructions (top-level) with **2-stage filtering**:
  - Stage-0: ultra-fast Pump program + buy discriminator check **without ALT**
  - Stage-1: only for Stage-0 candidates, resolve v0 ALTs (cache + TTL + concurrency limits)
- Emits TradeSignals and optionally submits a (demo) transaction via Jito Amsterdam Block Engine.

Stability focus:
- Reader/Parser split via a bounded queue so the gRPC stream doesn't stall on CPU spikes.
- Execution happens on a separate worker pool (`EXECUTOR_CONCURRENCY`) so monitor/parse never waits on HTTP.

> This repo ships with a **safe default** execution mode: `EXECUTION_MODE=log`.
> Switch to `demo` to validate Jito submission works end-to-end.
> Pump.fun buy builder is left as a stub you can plug your existing `pumpfun.rs` into.

## Quick start

1) Copy env:
```bash
cp .env.example .env
```

2) Run:
```bash
cargo run --release
```

## Running shredstream-proxy (separate)
You should run the official proxy separately. See scripts in `scripts/`.

### Critical auth note
The **pubkey you whitelist on Jito** must be the pubkey of the keypair you pass to proxy with `--auth-keypair`.
