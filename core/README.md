# core

Auction window, bid types, compose, lifecycle tracker, retry loop, and all shared transaction logic.

## What it does

This crate is the engine room. It has no opinions about the AI model or the HTTP server. It defines the types and algorithms that the agent and demo crates build on.

## Modules

### `auction.rs`

`AuctionWindow` is a ring buffer of the last 20 slots of Jito tip observations. For each slot, the clearing price is the maximum tip seen in that slot. From the window, it derives clearing price min, median, and max; bundles per slot; trend (Rising, Stable, Falling); and network regime (Cold, Warm, Hot, Manic).

The regime is classified from the median clearing price:
- Cold: below 10,000 lamports
- Warm: 10,000 to 100,000
- Hot: 100,000 to 1,000,000
- Manic: above 1,000,000

`compute_baseline()` returns the median clearing price (floored at `MIN_TIP_LAMPORTS`) once bootstrapped (five or more slots observed), otherwise returns twice the minimum tip floor.

### `bid.rs`

Core bid types: `Regime`, `BidContext`, `BundleOutcome`. `BidContext` is what the agent receives on each submission — the full auction window snapshot, transaction type, economic value, and the last ten bundle outcomes for self-calibration. `BundleOutcome` records what happened after each submission and is stored in a rolling deque (cap 50).

### `compute_bid.rs`

`TxType` (Snipe, Swap, Arb, Memo) and the value cap logic. The tip formula is:

```
tip = baseline * safety_margin * forward_multiplier
```

Clamped by the value cap for the transaction type (Snipe: 80%, Swap: 5%, Arb: 60%, Memo: unlimited) and then by the configured hard ceiling.

`MIN_TIP_LAMPORTS` is 50,000.

### `compose.rs`

`build_bundle_unsigned` — takes a `BundleSpec` and a blockhash and produces a vector of unsigned `VersionedTransaction` objects ready for signing. The tip transaction is appended as the last transaction in the bundle.

### `config.rs`

`Config` loaded from environment variables. All tunable parameters live here: RPC URL, Yellowstone endpoint and token, LLM configuration, tip ceiling, and Jito block engine URLs.

### `failure.rs`

Failure classification. After a confirmation timeout, `detect` inspects the transaction's blockhash age, on-chain status, and any error message to assign one of: `ExpiredBlockhash`, `FeeTooLow`, `Dropped`, `ComputeExceeded`, `BundleFailure`. The first three are retryable; the last two terminate the retry sequence immediately.

### `jito_client.rs`

`JitoClient` wraps the Jito block engine HTTP API. `send_bundle` fans out to all configured regions concurrently. `get_bundle_statuses` polls the Jito REST API for bundle status. `get_tip_accounts` fetches the current set of eight tip accounts.

### `jupiter.rs`

Jupiter v6 HTTP client. `get_quote` and `get_swap_transaction` for building real SOL-to-USDC swap bundles.

### `lane_router.rs`

Selects the execution lane (Jito bundle or priority-fee) based on `ExecuteLane` and dispatches accordingly.

### `leader.rs`

`LeaderClock` tracks the current slot and leader schedule via the Yellowstone slot subscription. `run_schedule_refresher` polls the RPC every 30 seconds to update the schedule. Used to annotate submissions with their slot position within the leader's four-slot window.

### `lifecycle.rs`

`LifecycleTracker` maps bundle IDs to `LifecycleHandle` structs. Each handle records the current stage (Submitted, Pending, Processed, Confirmed, Finalized, Failed), the landing slot, and timestamps for each commitment level. `run_event_loop` processes `LifecycleEvent` messages from the ingest bus and advances the stage machine.

### `retry.rs`

`RetryLoop` drives the autonomous retry flow. After a confirmation timeout, it classifies the failure, invokes the AI agent for a retry decision, fetches a fresh blockhash, recomputes the tip, rebuilds and re-signs the bundle, and resubmits. Maximum three retries before marking the bundle as Exhausted.
