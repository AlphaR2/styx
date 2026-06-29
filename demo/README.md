# demo

Axum HTTP server and WebSocket relay. The backend for the UI.

## What it does

Starts all background tasks (Yellowstone subscriber, lifecycle tracker, auction window, leader clock), wires them into shared state, and exposes an HTTP API that the UI consumes.

## Running

```
RUST_LOG=info,hyper=warn,tonic=warn,h2=warn cargo run -p demo
```

Listens on `0.0.0.0:3000`.

## Endpoints

```
POST /execute               Submit a bundle (memo, jupiter, or fault scenario)
GET  /status/{bundle_id}    Current lifecycle stage for a bundle
GET  /log                   Full execution log as JSON array
GET  /bundle/{id}/events    Replay buffer of ExecLog and Execution events for a bundle
GET  /bundle/{id}/summary   AI-generated plain-English summary of a completed bundle
GET  /tip_floor             Current AuctionWindow statistics
GET  /leader                Current slot and next 16 leader slots
GET  /ws                    WebSocket stream of live NetworkEvents
GET  /health                {"status":"ok"}
```

## POST /execute

```json
{
  "scenario": "memo | jupiter | fault",
  "lane":     "jito | priority"
}
```

- `memo` — sends a proof-of-life SPL Memo transaction
- `jupiter` — routes a 0.001 SOL to USDC swap via Jupiter v6
- `fault` — builds the transaction with `Hash::default()` to trigger the AI retry loop

## State

All state lives in `AppState`:

- `ctx: StyxClient` — the SDK client with the auction window, outcomes ring, LLM classifier, baseline, RPC, Jito client, lifecycle tracker, and leader clock
- `keypair` — the signing keypair loaded from `KEYPAIR_JSON` or `KEYPAIR_PATH`
- `bus` — the broadcast sender for the network event stream
- `bundle_events` — per-bundle replay buffer (last 500 events per bundle ID)

## Background tasks

Spawned at startup before the server begins accepting requests:

1. `subscriber::run` — Yellowstone gRPC subscriber, publishes to `bus` and `lifecycle_bus`
2. `run_event_loop` — lifecycle tracker driven by `lifecycle_bus`
3. Auction window listener — consumes `JitoTip` events from `bus`, feeds `AuctionWindow`
4. Bundle event recorder — consumes `ExecLog` and `Execution` events from `bus`, writes to per-bundle replay buffer
5. `run_slot_listener` + `run_schedule_refresher` — leader clock maintenance

## Modules

- `main.rs` — startup, state, all handlers
- `log_bridge.rs` — tracing subscriber layer that bridges `tracing` events to the `ExecLog` bus event, so server logs appear in the UI WebSocket stream per bundle
