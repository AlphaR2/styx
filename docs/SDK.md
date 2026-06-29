# Styx SDK Reference

The `styx` crate re-exports everything a caller needs to submit and track transactions without touching the ingest, core, or agent crates directly.

## Adding the dependency

In a workspace that includes Styx as a member, or as a path dependency:

```toml
[dependencies]
styx = { path = "../styx" }
```

## StyxClient

`StyxClient` is the central context object. It holds every shared resource and is cloned cheaply (all fields are `Arc`-wrapped).

```rust
pub struct StyxClient {
    pub claude:        Arc<dyn BidStrategy + RetryAdvisor + Send + Sync>,
    pub baseline:      Arc<dyn BidStrategy + Send + Sync>,
    pub config:        Arc<Config>,
    pub rpc:           Arc<RpcClient>,
    pub jito:          Arc<JitoClient>,
    pub tracker:       Arc<Mutex<LifecycleTracker>>,
    pub tip_account:   Pubkey,
    pub auction_window: Arc<Mutex<AuctionWindow>>,
    pub outcomes:      Arc<Mutex<VecDeque<BundleOutcome>>>,
    pub log:           Arc<Mutex<Vec<ExecutionRecord>>>,
    pub exec_bus:      Option<broadcast::Sender<NetworkEvent>>,
    pub leader:        Option<LeaderClock>,
}
```

## Submitting a bundle

```rust
use styx::{prepare, submit, ExecuteLane, ExecuteOpts, TxType};
use solana_sdk::instruction::Instruction;

// 1. Build your instructions
let instructions: Vec<Instruction> = vec![/* your ixs */];

// 2. Configure execution
let opts = ExecuteOpts {
    compute_unit_limit: 50_000,
    simulate: false,
    address_lookup_tables: vec![],
    lane: ExecuteLane::JitoBundle,
    tip_ceiling_override: None,
    inject_blockhash_expiry: false,
    tx_type: TxType::Memo,
    value_lamports: 0,
};

// 3. Price and assemble the bundle
let bundle = prepare(payer_pubkey, instructions, opts, &ctx).await?;

// 4. Sign and submit
let handle = submit(bundle, signer_fn, &ctx).await?;
println!("bundle {} submitted, tip {} lamports", handle.bundle_id, handle.tip_lamports);
```

## SignerFn

The SDK never holds private keys. You supply a signing function:

```rust
use std::sync::Arc;
use solana_sdk::transaction::VersionedTransaction;

let keypair = Arc::new(my_keypair);
let signer: Arc<dyn Fn(Vec<VersionedTransaction>) -> anyhow::Result<Vec<VersionedTransaction>> + Send + Sync>
    = Arc::new(move |mut txs| {
        for tx in &mut txs {
            let msg_bytes = tx.message.serialize();
            let keys = tx.message.static_account_keys();
            if let Some(slot) = keys.iter().position(|k| *k == keypair.pubkey()) {
                tx.signatures[slot] = keypair.sign_message(&msg_bytes);
            }
        }
        Ok(txs)
    });
```

The signer can wrap a hardware wallet, a remote signing service, or any key management system.

## ExecuteOpts fields

| Field | Type | Description |
|---|---|---|
| `compute_unit_limit` | `u32` | CU budget for the transaction. 50,000 for a memo; up to 1,400,000 for complex swaps. |
| `simulate` | `bool` | Run `simulateTransaction` before submitting. Catches instruction errors early. |
| `address_lookup_tables` | `Vec<AddressLookupTableAccount>` | ALTs resolved from the chain, required for v0 transactions with > 32 accounts. |
| `lane` | `ExecuteLane` | `JitoBundle` or `PriorityFee`. |
| `tip_ceiling_override` | `Option<u64>` | Override the config ceiling for this submission only. |
| `inject_blockhash_expiry` | `bool` | Build with `Hash::default()` to force a failure and demonstrate retry. |
| `tx_type` | `TxType` | Determines the value cap applied to the tip: Snipe, Swap, Arb, or Memo. |
| `value_lamports` | `u64` | Economic value of the transaction. Used to compute the value cap. Set 0 for Memo. |

## ExecutionHandle fields

Returned from `submit`:

| Field | Description |
|---|---|
| `bundle_id` | Jito bundle ID (UUID) or transaction signature for priority-fee lane. |
| `tip_lamports` | Actual tip calculated by the AI agent. |
| `baseline_tip_lamports` | What the OverpayerBaseline (2x) would have paid. |
| `delta_lamports` | `baseline - actual`. Positive means savings. |
| `regime` | Network regime string at time of submission: Cold, Warm, Hot, Manic. |
| `forward_multiplier` | The AI's output multiplier. |
| `reasoning` | Full AI reasoning text. |
| `confidence` | AI self-reported confidence, 0.0 to 1.0. |
| `lane` | Lane used: JitoBundle or PriorityFee. |
| `solscan_url` | Direct link to the transaction on Solscan. |

## TxType and value caps

The tip is clamped to a fraction of the transaction's economic value to prevent overpaying on small transactions:

| TxType | Cap |
|---|---|
| Snipe | 80% of value_lamports |
| Swap | 5% of value_lamports, minimum MIN_TIP_LAMPORTS |
| Arb | 60% of value_lamports |
| Memo | No value cap (hard ceiling from config applies) |

Set `value_lamports = 0` for Memo. For a swap of 0.001 SOL (1,000,000 lamports), the Swap cap is 50,000 lamports which equals MIN_TIP_LAMPORTS, so the minimum floor applies.

## Tracking lifecycle

```rust
use styx_core::lifecycle::LifecycleStage;

let tracker = ctx.tracker.lock().await;
if let Some(handle) = tracker.get(&bundle_id) {
    match &handle.stage {
        LifecycleStage::Confirmed { landing_slot } =>
            println!("confirmed at slot {}", landing_slot),
        LifecycleStage::Failed { reason } =>
            println!("failed: {}", reason),
        _ => {}
    }
}
```

## AuctionWindow

Read the live tip auction state at any time:

```rust
let w = ctx.auction_window.lock().await.clone();
println!(
    "regime={:?} median={}L trend={:?} bootstrapped={}",
    w.regime, w.clearing_price_median, w.trend, w.is_bootstrapped
);
```

Feed it directly if you are running your own Yellowstone subscriber:

```rust
ctx.auction_window.lock().await.ingest(slot, tip_lamports);
```

## Jupiter integration

```rust
use styx::prepare_jupiter;
use styx_core::jupiter::{WSOL_MINT, USDC_MINT};

let bundle = prepare_jupiter(
    payer,
    WSOL_MINT,
    USDC_MINT,
    1_000_000,   // 0.001 SOL
    300,         // 3% slippage
    ExecuteLane::JitoBundle,
    &ctx,
).await?;

let handle = submit(bundle, signer_fn, &ctx).await?;
```
