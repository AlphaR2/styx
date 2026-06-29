# Bundle Lifecycle

## Stages

Every bundle or transaction submitted through Styx passes through these stages in order:

| Stage | Meaning |
|---|---|
| Submitted | The bundle has been sent to all Jito block engine regions (or the transaction to RPC). |
| Pending | Accepted by the block engine. Waiting for inclusion in a block. |
| Processed | A validator included the transaction in a block. Detected via Yellowstone payer subscription. |
| Confirmed | A supermajority of stake has voted on the block. Safe for most operations. |
| Finalized | The block is rooted. Cannot be rolled back under any circumstances. |
| Failed | The transaction failed on-chain (instruction error, compute exceeded, etc.). |

## Timestamps

Each stage has a corresponding millisecond timestamp recorded in the `ExecutionRecord`:

- `submitted_at_ms` — wall-clock time when `submit` was called
- `processed_at_ms` — wall-clock time when Yellowstone reported the transaction at processed commitment
- `confirmed_at_ms` — wall-clock time when Yellowstone reported confirmed commitment
- `finalized_at_ms` — wall-clock time when Yellowstone reported finalized commitment

All timestamps are Unix milliseconds (UTC).

## Confirmation detection

Two watchers run concurrently after submission:

**Yellowstone watcher** subscribes to the payer account's transaction stream. When a matching signature appears in a processed slot, the processed stage is recorded. Confirmation and finalization are inferred from subsequent slot updates on the landing slot's commitment progression.

**RPC watcher** polls `getSignatureStatuses` every two seconds as a fallback. If the Yellowstone path is slow or the connection was briefly interrupted, the RPC watcher catches up.

Whichever path detects confirmation first wins. The other is immediately aborted. In practice the Yellowstone path leads by 200 to 500 milliseconds on a healthy connection.

## Confirmation timeout and retry

The confirmation watcher runs for 60 seconds. If no confirmation is detected, the retry system takes over.

## Failure classification

Before retrying, the failure is classified:

| Kind | Cause | Retryable |
|---|---|---|
| ExpiredBlockhash | The transaction's blockhash is more than 150 slots old | Yes |
| FeeTooLow | The tip was below the auction clearing price | Yes |
| Dropped | Bundle was accepted but not included (slot skip, leader offline) | Yes |
| ComputeExceeded | Transaction exceeded its compute unit budget | No |
| BundleFailure | A transaction in the bundle failed on-chain | No |

## Retry flow

For retryable failures:

1. Classification result is sent to the AI agent as a `RetrySignal`.
2. The agent returns a `RetryAdvice` with a new `forward_multiplier`, whether to refresh the blockhash, and reasoning.
3. A fresh blockhash is always fetched at `confirmed` commitment before resubmitting.
4. The tip is recomputed with the new multiplier and clamped by the same value cap.
5. The bundle is rebuilt, re-signed, and submitted to all Jito regions again.
6. A 30-second confirmation window is given for the retry.

Maximum three retries. After exhaustion, the bundle is marked as failed with `retry_count = 3`.

## Jito-specific behavior

Jito bundles are identified by a UUID returned by the block engine at submission time. The Jito `getBundleStatuses` API is also polled during the retry evaluation phase to check whether the block engine has any status information.

If the Jito leader skips their slot, the bundle is silently dropped. `getBundleStatuses` returns an empty array indefinitely. Styx detects this via the 60-second timeout and classifies it as `Dropped`.

The `landed_bundle_id` field in the execution log may differ from the original `bundle_id` when a retry was the one that confirmed. Both are recorded.

## Reading the log

```
GET /log
```

Returns an array of `ExecutionRecord` objects. Each contains all timing fields, the AI reasoning, failure classification if any, and all transaction signatures for Solscan lookup.

A specific bundle's event stream (live AI reasoning, submission events, retry events) is available at:

```
GET /bundle/{id}/events
```

An AI-generated plain-English summary combining the on-chain data and lifecycle events is at:

```
GET /bundle/{id}/summary
```
