# ingest

Yellowstone gRPC subscriber and network event bus.

## What it does

Connects to any Yellowstone-compatible Geyser provider over gRPC with TLS and maintains three concurrent filter subscriptions on a single stream.

**Slot subscription** emits a `SlotUpdate` event on every slot boundary carrying the commitment level (Processed, Confirmed, Finalized). This drives the leader clock and lifecycle tracker in the core crate.

**Transaction subscription** filters for all transactions signed by the configured payer public key. When a matching transaction appears at processed commitment, a `TxSeen` event is emitted with the signature and slot. The lifecycle tracker uses this as the first confirmation signal, well ahead of RPC polling.

**Account subscription** monitors the eight Jito tip accounts. Each time any account balance increases, the delta is emitted as a `JitoTip` event with the slot and lamport amount. These deltas feed the `AuctionWindow` in the core crate to build empirical clearing price statistics.

## Reconnection

If the gRPC stream drops, the subscriber reconnects immediately with exponential backoff capped at 30 seconds. Two specific error conditions receive special handling: `ResourceExhausted` (provider concurrent-stream limit) and gateway protocol errors both trigger a fixed 15-second hold before reconnecting, giving the provider time to release the previous session slot.

## Event bus

All events are published to a `tokio::sync::broadcast` channel of capacity 1024. Consumers are fully independent. A consumer that falls behind receives a `Lagged` notification with the count of dropped events and continues from the current position.

Lifecycle events (`SlotUpdate`, `TxSeen`) are also published to a separate high-capacity broadcast channel (4096) that the lifecycle tracker subscribes to exclusively. This prevents bundle confirmation from being affected by slow WebSocket clients consuming the main bus.

## Modules

- `bus.rs` — `NetworkEvent` and `LifecycleEvent` enum definitions, bus constructors
- `subscriber.rs` — Yellowstone gRPC client, filter setup, event dispatch loop
- `tip_stream.rs` — legacy Jito REST tip stream (unused in current stack, kept for reference)
