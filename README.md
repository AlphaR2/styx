# Styx

Styx is a Solana transaction execution SDK built for production MEV and high-frequency submission workloads. It combines live Yellowstone gRPC streaming, Jito bundle construction, AI-assisted tip pricing, and full transaction lifecycle tracking into a single coherent stack.

The name comes from the mythological river that bundles had to cross to reach their destination. Styx gets them across.

## What It Does

Styx observes the network in real time via Yellowstone gRPC, tracking slot progression, leader windows, and Jito tip auction clearing prices. When a bundle is submitted, an AI agent analyzes current network conditions and determines how much to tip. The stack then submits to all Jito block engine regions concurrently, confirms via both Yellowstone stream subscription and RPC polling fallback, and retries autonomously when a bundle drops. On retry, the agent reasons about the specific failure cause and adjusts its strategy before resubmitting.

## Project Structure

```
Styx/
  ingest/     Yellowstone subscriber, network event bus
  core/       Auction window, bid types, compose, retry loops, lifecycle tracker
  agent/      Claude/LLM classifier, overpayer baseline, execution engine
  styx/       Public SDK re-export crate
  demo/       Axum HTTP API + WebSocket server, serves the UI backend
  ui/         Leptos WASM frontend (network dashboard, execution log, snipe feed)
```

## Prerequisites

- Rust 1.78 or later (install via rustup)
- For the UI: `trunk` and the `wasm32-unknown-unknown` target

```
cargo install trunk
rustup target add wasm32-unknown-unknown
```

## Environment Setup

Copy `.env.example` to `.env` and fill in the required values:

```
cp .env.example .env
```

Required variables:

```
YELLOWSTONE_ENDPOINT   Full gRPC endpoint including scheme and port
                       Example: https://fra.grpc.solinfra.dev:443

YELLOWSTONE_TOKEN      Bearer token for the Yellowstone provider

RPC_URL                Standard Solana JSON-RPC endpoint
                       Example: https://mainnet.helius-rpc.com/?api-key=...

KEYPAIR_JSON           Base64-encoded Solana keypair JSON for the signing wallet
                       Generate: base64 -w 0 < ~/.config/solana/id.json

LLM_BASE_URL           OpenAI-compatible chat completions endpoint
LLM_API_KEY            API key for that provider
LLM_MODEL              Model name as the provider expects it
```

Optional variables:

```
LLM_KIND                Wire protocol: openai-compatible (default) or anthropic

JITO_BLOCK_ENGINE_URLS  Comma-separated block engine URLs. Defaults to all four
                        mainnet regions (amsterdam, frankfurt, ny, tokyo).

TIP_CEILING_LAMPORTS    Hard cap on tip per bundle. Default 500000.
SNIPE_TIP_CEILING_LAMPORTS  Cap used for pump.fun snipe bundles. Default 5000000.
```

Do not set `TEST_TIP_LAMPORTS` in production. That variable forces a fixed tip and bypasses the entire AI formula. It exists only for infrastructure smoke-testing.

## Running the Demo Server

```
RUST_LOG=info,hyper=warn,tonic=warn,h2=warn cargo run -p demo
```

The server listens on port 3000. All endpoints are prefixed at `/` for the demo binary and `/api/` when served through the UI reverse proxy.

## Running the UI

In a second terminal from the `ui/` directory:

```
cd ui
trunk serve --proxy-backend=http://localhost:3000/
```

The UI is then available at `http://localhost:8080`.

## API Reference

```
POST /execute               Submit a bundle. Body is JSON. See ExecuteRequest below.
GET  /status/{bundle_id}    Lifecycle stage for a submitted bundle.
GET  /log                   Full execution log as JSON array.
GET  /bundle/{id}/events    Replay buffer of all ExecLog and Execution events for a bundle.
GET  /bundle/{id}/summary   AI-generated plain-English summary of a completed bundle.
GET  /tip_floor             Current auction window statistics.
GET  /leader                Current slot and next 16 leader slots.
GET  /launches              Detected pump.fun token launches.
POST /snipe                 Submit a pump.fun snipe bundle.
GET  /ws                    WebSocket for live network events.
GET  /health                Returns {"status":"ok"}.
```

### POST /execute

```json
{
  "scenario":            "memo | jupiter | fault",
  "lane":                "jito | priority",
  "sol_amount_lamports": 1000000,
  "slippage_bps":        300
}
```

The `fault` scenario injects a stale blockhash to demonstrate the autonomous retry path. Set `lane` to `jito` to exercise the Jito retry loop. Set `lane` to `priority` for the priority-fee retry loop. The AI agent handles both paths.

### GET /tip_floor

```json
{
  "clearing_price_min":    42014,
  "clearing_price_median": 303900,
  "clearing_price_max":    4000000,
  "bundles_per_slot":      35.2,
  "trend":                 "Falling",
  "regime":                "Hot",
  "is_bootstrapped":       true
}
```

These values are derived from live Jito tip account balance observations via Yellowstone. They represent empirical auction clearing prices, not static tables.

## How Tips Are Calculated

Styx monitors the eight Jito tip accounts via Yellowstone account subscriptions. Each time a tip account balance increases, the delta is recorded as an observed tip. Over a rolling window of 20 slots, the system computes per-slot clearing prices (the maximum tip seen in each slot), then derives a minimum, median, and maximum across the window.

The tip formula is:

```
tip = baseline * safety_margin * forward_multiplier
```

Where:
- `baseline` is `clearing_price_median` once bootstrapped (5 slots minimum), otherwise `2 * MIN_TIP_LAMPORTS`
- `safety_margin` is regime-dependent: Cold 1.05, Warm 1.10, Hot 1.20, Manic 1.50
- `forward_multiplier` is the AI agent output, ranging from 0.1 to 10.0

The result is clamped by a value cap that depends on transaction type:
- Snipe: 80% of transaction economic value
- Swap: 5% of economic value, minimum MIN_TIP_LAMPORTS
- Arb: 60% of economic value
- Memo: uncapped (hard ceiling from config applies)

The baseline of `forward_multiplier = 1.0` means the agent is bidding exactly at the going clearing price plus the safety margin for the current regime. Values above 1.0 are aggressive; values below are conservative.

## AI Agent

The agent is a Claude or any OpenAI-compatible LLM configured via environment variables. It receives a JSON snapshot of the current auction window, the transaction type, economic value, and the last ten bundle outcomes. It returns a `forward_multiplier` with reasoning and a confidence score.

On retry, the agent receives a `RetrySignal` describing the failure kind, previous tip, seconds elapsed, and current network conditions. It returns an action (Retry or Abort), a new forward_multiplier, whether to refresh the blockhash, and reasoning.

The reasoning is logged to both the server console and the WebSocket event stream, making the decision process visible in the UI.

A deterministic `OverpayerBaseline` agent runs in parallel on every submission, always returning `forward_multiplier = 2.0`. Its tip is logged as the market baseline, and the delta between Claude's tip and the baseline tip is reported as savings or cost. This gives an objective measure of the AI agent's value over time.

## Transaction Lifecycle

Every bundle passes through these stages, each with a recorded timestamp and slot number:

```
Submitted   The bundle has been sent to the Jito block engine.
Processed   A validator accepted the transaction and included it in a block.
            Detected via the Yellowstone payer account subscription.
Confirmed   The block has received votes from a supermajority of stake weight.
            This is the first commitment level that is safe for most operations.
Finalized   The block is rooted and cannot be rolled back under any circumstances.
```

Confirmation is detected via Yellowstone stream subscription first. RPC polling runs concurrently as a fallback, using `get_signature_status` every two seconds. The Yellowstone path typically wins by 200 to 500 milliseconds.

## Failure Classification and Retry

When a bundle does not confirm within 60 seconds, the retry system classifies the failure:

```
ExpiredBlockhash    The transaction's recent blockhash is no longer valid.
FeeTooLow           The bundle tip was below the auction clearing price.
ComputeExceeded     The transaction exceeded its compute unit budget.
BundleFailure       A transaction in the bundle failed on-chain.
Dropped             The bundle was accepted but not included (slot skip or competition).
```

The AI agent receives this classification along with the current network snapshot and decides whether to retry and at what multiplier. If the blockhash is expired, the retry loop always fetches a fresh one before resubmitting, regardless of what the agent decides about the tip. The agent has three retries before the stack marks the bundle as Exhausted.

## Fault Injection

To demonstrate the autonomous retry path, set the scenario to `fault` in a POST to `/execute`. This builds the transaction with `Hash::default()` as the blockhash, which is immediately invalid. The Jito block engine will accept the bundle (bundle-level validation does not check per-transaction blockhashes at submission time), but the transaction will fail on-chain with `BlockhashNotFound`. The stack detects this, the AI agent reasons about the cause and prescribes a blockhash refresh, and the retry loop resubmits with a live blockhash and recalculated tip.

For the priority-fee lane, the stale blockhash causes the RPC to reject the transaction outright, triggering the same retry path.

## Stream Reconnection and Backpressure

The Yellowstone subscriber runs in a dedicated Tokio task. If the gRPC stream drops, the task reconnects immediately with a one-second delay between attempts. The subscriber logs each reconnection attempt so operators can distinguish transient drops from persistent endpoint failures.

The internal event bus is a Tokio broadcast channel. Consumers that fall behind (for example, a slow WebSocket client) receive a `Lagged` notification indicating how many events were dropped. The demo UI displays this notification and continues without crashing. Internally, the auction window ingestion task, lifecycle tracker, and launch feed all run as independent consumers and do not block each other.

The Yellowstone stream uses gRPC streaming with TLS. No custom backpressure protocol is needed at the application level because the network event bus is bounded; a slow consumer simply receives a lagged notification and skips to the current event.

## Lifecycle Log

To capture a lifecycle log for submission, run the demo server against mainnet, then exercise the following sequence:

1. Ten or more POST /execute calls with the default memo scenario.
2. At least one POST /execute with scenario=fault and lane=jito to produce a classified failure.
3. At least one POST /execute with scenario=fault and lane=priority to demonstrate the priority-fee retry path.

After the runs complete, fetch the full log:

```
curl http://localhost:3000/log | jq . > lifecycle_log.json
```

Each entry contains the bundle ID, lane, tip paid, market baseline tip, regime, AI forward multiplier, AI reasoning, slot numbers for each commitment stage, timing deltas, failure classification, and retry count. Cross-reference the landing slots against Solscan or Solana Explorer to verify on-chain execution.

## KEY POINTS

### What does the delta between processed_at and confirmed_at tell you about network health at the time of submission?

The processed stage occurs the moment a single validator includes the transaction in a block. Confirmed means a supermajority of stake-weighted validators have voted on that block. The delta between them measures how quickly the rest of the network agreed on the block.

In practice on a healthy mainnet, this delta is between 200 and 800 milliseconds. The first Styx memo submission in our test run showed 324 milliseconds. A delta consistently below 400 milliseconds indicates the network is propagating blocks cleanly and validators are voting promptly. A delta climbing above two seconds suggests one of three things: the block is on a minority fork and the network is resolving a small split, vote propagation is degraded due to network congestion among validators, or the slot leader produced a block that was slow to shred-propagate across the cluster.

For Jito bundles specifically, the processed-to-confirmed delta matters because Styx uses the confirmed commitment to fetch blockhashes for retries. If confirmation is lagging, the retry blockhash may itself be marginally stale by the time the retry bundle is submitted, compressing the validity window further. Operators should treat a sustained processed-to-confirmed delta above 1.5 seconds as a signal to widen their retry timing thresholds.

### Why should you never use finalized commitment when fetching a blockhash for a time-sensitive transaction?

A blockhash is valid for approximately 150 slots, which is roughly 60 seconds under normal conditions. The finalized commitment lags behind the current slot by approximately 31 to 32 slots, translating to about 13 seconds of staleness at the moment you receive it.

If you fetch a finalized blockhash and submit a bundle immediately, you are beginning with a blockhash that will expire in roughly 47 seconds instead of 60. Under normal conditions this is survivable. Under congestion, when the first submission does not land and a retry is needed, the window shrinks further. By the second or third retry, the blockhash may have already expired, forcing an additional round-trip to fetch a new one and adding latency at precisely the moment competition is highest.

There is also a compounding effect: under high load, finalization can lag further than the typical 32 slots. During manic network conditions, Styx has observed finalization running 50 or more slots behind. A blockhash fetched at that point might already be 20 seconds stale.

Always use confirmed commitment for blockhash fetches in time-sensitive submission paths. The confirmed commitment typically lags by only one to two slots, leaving the full validity window available.

### What happens to your bundle if the Jito leader skips their slot?

The bundle is silently dropped. Jito's block engine queues submitted bundles and routes them to the leader scheduled for the current window, which spans four consecutive slots. If the leader skips their slot, the scheduled block is never produced. Validators observe the missing block and advance the slot clock, handing leadership to the next scheduled leader. Any bundles queued for the skipped leader's window are never processed.

From the submitter's perspective, `getBundleStatuses` returns an empty array for the bundle indefinitely. There is no explicit rejection signal. Styx detects this via the 60-second confirmation timeout. When the timeout fires, the failure classifier checks the blockhash validity, inspects any on-chain status for the transaction signature, and infers a `Dropped` classification if the blockhash is still live but no confirmation has been observed.

The AI agent then receives the retry signal. In the case of a slot skip, the agent typically observes a Dropped failure in a stable or improving regime and recommends resubmission at a modestly higher multiplier to compensate for the additional competition from other bundles that also missed the skipped slot. The retry loop fetches a fresh blockhash, re-signs the bundle, and submits it to the new leader's window.

The Styx leader clock tracks the current leader schedule and annotates each submission with its slot position within the leader's four-slot window. Submitting in slots one or two of a leader's window is preferable to slots three or four, which is what the leader window display in the Mission Control dashboard communicates.
