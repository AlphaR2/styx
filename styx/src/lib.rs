// Styx — Solana execution SDK.
//
// Typical usage:
//
//   use styx::{Config, StyxClient, PreparedBundle, prepare, submit, ExecuteLane, ExecuteOpts};
//
//   // Build unsigned transactions — Styx never sees your keypair.
//   let bundle = prepare(payer_pubkey, instructions, ExecuteOpts::default(), &ctx).await?;
//
//   // Inspect bundle.transactions before signing.
//   let handle = submit(bundle, |mut txs| {
//       // sign each tx, fill the zeroed payer slot
//       Ok(txs)
//   }, &ctx).await?;

pub use styx_core::config::{Config, LlmConfig};

pub use styx_agent::execute::{
    StyxClient,
    PreparedBundle,
    prepare,
    prepare_jupiter,
    submit,
    ExecuteLane,
    ExecuteOpts,
    ExecutionHandle,
    ExecutionRecord,
    rolling_landing_rate,
};

pub use styx_ingest::bus::NetworkEvent;

pub use styx_core::auction::AuctionWindow;
pub use styx_core::bid::BundleOutcome;
pub use styx_core::compute_bid::TxType;
