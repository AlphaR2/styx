# Setup Guide

## Prerequisites

Install Rust via rustup if you do not already have it:

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Rust 1.78 or later is required. Verify with `rustc --version`.

That is the only required install for running the demo server.

## Optional: UI

The web dashboard requires two additional one-liners:

```
cargo install trunk
rustup target add wasm32-unknown-unknown
```

## Environment

Copy `.env.example` to `.env` and fill in the six required values:

```
cp .env.example .env
```

### YELLOWSTONE_ENDPOINT and YELLOWSTONE_TOKEN

You need a Yellowstone-compatible Geyser provider. Options include:
- Helius: available in their dashboard under gRPC
- Triton: triton.one
- Shyft: shyft.to
- SolInfra: solinfra.dev

The endpoint must include the scheme and port, for example `https://fra.grpc.solinfra.dev:443`.

### RPC_URL

Any standard Solana JSON-RPC endpoint. Helius, Triton, QuickNode, and Alchemy all work. The free public endpoint `https://api.mainnet-beta.solana.com` will work for testing but has rate limits that may affect tip account balance fetching.

### KEYPAIR_JSON

Base64-encode your Solana keypair JSON file:

```
base64 -w 0 < ~/.config/solana/id.json
```

Paste the output as the value. The wallet needs a small SOL balance for tips. Around 0.05 SOL is enough for extended testing (each memo transaction costs roughly 0.0005 SOL in tips at Hot regime prices).

### LLM credentials

Any OpenAI-compatible provider works. Recommended options for testing:

| Provider | LLM_BASE_URL | LLM_MODEL |
|---|---|---|
| Together AI | https://api.together.xyz/v1/chat/completions | meta-llama/Llama-3.3-70B-Instruct-Turbo |
| OpenAI | https://api.openai.com/v1/chat/completions | gpt-4o-mini |
| Groq (free tier) | https://api.groq.com/openai/v1/chat/completions | llama-3.3-70b-versatile |
| Anthropic | https://api.anthropic.com/v1/messages | claude-haiku-4-5-20251001 |
| Local Ollama | http://localhost:11434/v1/chat/completions | llama3 |

For Anthropic, also set `LLM_KIND=anthropic`.

If no LLM credentials are provided, the system runs baseline-only (the `OverpayerBaseline` always bids 2x the clearing price, and AI reasoning is skipped).

## Running the demo server

```
RUST_LOG=info,hyper=warn,tonic=warn,h2=warn cargo run -p demo
```

Wait for the log line `Styx demo listening on 0.0.0.0:3000` before sending requests.

The server takes about 2 seconds at startup to fetch tip accounts from Jito and establish the Yellowstone connection. The auction window begins bootstrapping immediately and is ready after about 5 slots (roughly 2 seconds on mainnet).

## Running the UI (optional)

In a second terminal from the `ui/` directory:

```
cd ui && trunk serve --proxy-backend=http://localhost:3000/
```

Open `http://localhost:8080` in your browser.

## Verifying everything works

After startup, run:

```
curl -s http://localhost:3000/health
# {"status":"ok"}

curl -s http://localhost:3000/tip_floor | jq .
# Should show live clearing prices once the window bootstraps

curl -s -X POST http://localhost:3000/execute \
  -H 'Content-Type: application/json' \
  -d '{"scenario":"memo","lane":"jito"}' | jq .
```

The execute call returns immediately with the bundle ID and tip details. The bundle lands within a few seconds. Check `/log` to see the full lifecycle record.

## Demonstrating fault injection and retry

```
curl -s -X POST http://localhost:3000/execute \
  -H 'Content-Type: application/json' \
  -d '{"scenario":"fault","lane":"jito"}' | jq .
```

This submits with an expired blockhash. Watch the server logs for the AI retry decision, blockhash refresh, and resubmission.

## What testers do not need

- No local Solana validator
- No Docker
- No Node.js or npm (the UI uses Trunk, which is a Rust tool)
- No local Jito node
- No special OS configuration

Everything runs over HTTPS/gRPC to your existing Yellowstone and RPC providers.
