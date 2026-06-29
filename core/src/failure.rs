use std::sync::Arc;

use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::hash::Hash;
use tokio::sync::Mutex;
use tracing::info;

use crate::lifecycle::{LifecycleStage, LifecycleTracker};

// Every distinct reason a bundle can fail to land.
// Only the recoverable variants trigger a retry -- BundleFailure is terminal.
#[derive(Debug, Clone, PartialEq)]
pub enum FailureKind {
    // Blockhash aged past its ~150-slot validity window before the bundle landed.
    // Recoverable: fetch a fresh blockhash, re-sign, resubmit.
    ExpiredBlockhash,

    // One transaction in the bundle failed simulation or execution.
    // The whole bundle is rejected atomically. Not recoverable without caller action.
    BundleFailure,

    // The transaction exceeded its SetComputeUnitLimit during execution.
    // Recoverable: raise the CU limit and resubmit.
    ComputeExceeded,

    // Tip was not high enough to win the Jito slot auction.
    // Recoverable: re-classify contention and bid higher.
    FeeTooLow,

    // Bundle was never acknowledged by the engine, usually a transient network issue.
    // Recoverable: resubmit as-is.
    Dropped,
}

impl FailureKind {
    // True for all failure types where resubmitting may succeed.
    pub fn is_recoverable(&self) -> bool {
        !matches!(self, FailureKind::BundleFailure)
    }
}

// Pure substring classification of a block-engine / runtime error message.
fn classify_error_str(error_msg: &str) -> FailureKind {
    let msg = error_msg.to_lowercase();

    if msg.contains("blockhash not found") || msg.contains("blockhash has expired") {
        FailureKind::ExpiredBlockhash
    } else if msg.contains("compute") && msg.contains("exceed") {
        FailureKind::ComputeExceeded
    } else if msg.contains("bundle") && (msg.contains("fail") || msg.contains("reject")) {
        FailureKind::BundleFailure
    } else if msg.contains("fee") && msg.contains("low")
        || msg.contains("tip") && msg.contains("low")
        || msg.contains("insufficient tip")
    {
        FailureKind::FeeTooLow
    } else {
        // Unrecognized errors are treated as transient drops so we attempt one retry.
        FailureKind::Dropped
    }
}

// Idempotency: true if the bundle already reached a landed state.
async fn already_landed(bundle_id: &str, tracker: &Arc<Mutex<LifecycleTracker>>) -> bool {
    let t = tracker.lock().await;
    match t.get(bundle_id) {
        Some(handle) => matches!(
            handle.stage,
            LifecycleStage::Confirmed { .. } | LifecycleStage::Finalized { .. }
        ),
        None => false,
    }
}

/// String-only classification (idempotency guard first). Kept for callers that
/// don't have an RpcClient handy; `detect` is preferred in the retry loop.
pub async fn classify(
    bundle_id: &str,
    error_msg: &str,
    tracker: Arc<Mutex<LifecycleTracker>>,
) -> FailureKind {
    if already_landed(bundle_id, &tracker).await {
        info!(bundle_id = %bundle_id, "bundle already confirmed, ignoring late error");
        return FailureKind::Dropped;
    }
    classify_error_str(error_msg)
}

/// Detect the real failure cause. Beyond string matching, this asks the cluster
/// whether the blockhash the failed attempt used is still valid — so a stale
/// blockhash is detected from on-chain truth (`isBlockhashValid`), not guessed
/// from an error string or assumed. This is what lets the agent recover a real
/// (or fault-injected) blockhash expiry: the detection is observed, not hardcoded.
pub async fn detect(
    bundle_id: &str,
    blockhash: &Hash,
    error_msg: &str,
    rpc: Arc<RpcClient>,
    tracker: Arc<Mutex<LifecycleTracker>>,
) -> FailureKind {
    // The error may arrive after the bundle actually landed -- never a failure.
    if already_landed(bundle_id, &tracker).await {
        info!(bundle_id = %bundle_id, "bundle already confirmed, ignoring late error");
        return FailureKind::Dropped;
    }

    // If the engine named the cause explicitly, trust it.
    let from_str = classify_error_str(error_msg);
    if from_str != FailureKind::Dropped {
        return from_str;
    }

    // Otherwise investigate: a bundle that did not land while the blockhash it
    // was signed with is no longer valid is, by definition, a blockhash expiry.
    let bh = *blockhash;
    let valid = tokio::task::spawn_blocking(move || {
        rpc.is_blockhash_valid(&bh, CommitmentConfig::processed())
    })
    .await;

    match valid {
        Ok(Ok(false)) => {
            info!(bundle_id = %bundle_id, "submitted blockhash is no longer valid — expired");
            FailureKind::ExpiredBlockhash
        }
        _ => FailureKind::Dropped,
    }
}
