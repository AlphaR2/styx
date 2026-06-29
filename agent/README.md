# agent

LLM-powered bid classifier, overpayer baseline, and execution engine.

## What it does

This crate owns the decision-making layer and the submission orchestration. It sits between the core crate (types, retry logic, lifecycle) and the demo crate (HTTP handlers).

## Modules

### `claude.rs` (LlmClassifier)

Wraps any OpenAI-compatible or Anthropic LLM. Implements two traits from core:

**`BidStrategy`** is called before every submission. It receives a `BidContext` containing the current `AuctionWindow` snapshot, transaction type, economic value, and the last ten `BundleOutcome` records. It returns a `ForwardMultiplier` with `regime`, `forward_multiplier` (0.1 to 10.0), `reasoning`, and `confidence`.

A `forward_multiplier` of 1.0 means "bid at the clearing price median plus the regime safety margin." Values above 1.0 are aggressive; below are conservative.

**`RetryAdvisor`** is called after a confirmed failure. It receives a `RetrySignal` with the failure kind, attempt number, previous tip, previous multiplier, current auction window, and seconds elapsed. It returns a `RetryAdvice` with `action` (Retry or Abort), a new `forward_multiplier`, `refresh_blockhash`, `reasoning`, and `confidence`.

Both methods call the LLM synchronously and parse the JSON response. If parsing fails or the model returns an error, a conservative fallback is used rather than crashing.

### `baseline.rs` (OverpayerBaseline)

A deterministic implementation of `BidStrategy` that always returns `forward_multiplier = 2.0`. It runs in parallel with the LLM classifier on every submission. Its computed tip is logged as `baseline_tip_lamports`. The delta between the LLM tip and the baseline is reported as savings in the execution log, providing an objective measure of AI value over many submissions.

### `execute.rs`

The main execution orchestrator. `prepare` builds a `PreparedBundle` from user instructions, lane configuration, and the AI pricing decision. `submit` signs and sends the bundle, then spawns a background task that tracks to finality or hands off to the retry loop.

Key flows:
- `prepare` reads the current auction window and last ten outcomes, queries both the LLM classifier and the baseline in parallel, then builds either a Jito bundle or a priority-fee transaction.
- `submit_jito` sends to all Jito regions concurrently and spawns the Yellowstone confirmation watcher and RPC polling fallback in parallel. Whichever fires first wins; the other is aborted.
- `push_outcome` records a `BundleOutcome` to the rolling history (cap 50) after each submission resolves, enabling the LLM to self-calibrate over time.

Fault injection: when `inject_blockhash_expiry` is set, the bundle is built with `Hash::default()` as the blockhash. This always fails on-chain and demonstrates the autonomous retry path.
