# styx

Public SDK re-export crate.

## What it does

A thin façade that re-exports the public surface of the ingest, core, and agent crates under a single name. External code and the demo binary import from `styx` rather than reaching into the individual crates directly.

## What is re-exported

```rust
// From styx_core
pub use styx_core::config::Config;
pub use styx_core::compute_bid::{TxType, MIN_TIP_LAMPORTS};
pub use styx_core::auction::AuctionWindow;
pub use styx_core::bid::BundleOutcome;

// From styx_agent
pub use styx_agent::execute::{
    prepare, prepare_jupiter, submit,
    ExecuteLane, ExecuteOpts, ExecutionRecord, StyxClient,
};

// From styx_ingest
pub use styx_ingest::bus::NetworkEvent;
```

## Usage

Add `styx` as a dependency instead of the individual workspace crates:

```toml
[dependencies]
styx = { path = "../styx" }
```

Then import everything from the single namespace:

```rust
use styx::{Config, StyxClient, ExecuteLane, ExecuteOpts, prepare, submit, NetworkEvent};
```

The SDK never holds private keys. Signing is handled by a caller-supplied `SignerFn`:

```rust
type SignerFn = Arc<dyn Fn(Vec<VersionedTransaction>) -> Result<Vec<VersionedTransaction>> + Send + Sync>;
```

The caller signs however it likes — file keypair, hardware wallet, remote signer, anything. The SDK passes the unsigned transactions to the function and receives back signed ones.
