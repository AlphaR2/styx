use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::signature::Signature;
use tokio::sync::{broadcast, Mutex};
use tracing::{info, warn};

use crate::jito_client::JitoClient;
use styx_ingest::bus::{LifecycleEvent, NetworkEvent, SlotStatus};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// Emit a lifecycle stage transition onto the network bus so WebSocket clients see
// Processed/Confirmed/Finalized in real time, driven straight off the Yellowstone stream.
fn emit_stage(
    bus: &Option<broadcast::Sender<NetworkEvent>>,
    bundle_id: &str,
    stage: &str,
    slot: u64,
    message: &str,
) {
    if let Some(tx) = bus {
        let _ = tx.send(NetworkEvent::Execution {
            bundle_id: bundle_id.to_string(),
            stage: stage.to_string(),
            tip_lamports: 0,
            retry: 0,
            regime: String::new(),
            message: message.to_string(),
            ts_ms: now_ms(),
        });
        // slot rides in the message; keep the event shape stable for the UI.
        let _ = slot;
    }
}

// The ordered commitment stages a bundle moves through.
// Processed -> Confirmed -> Finalized is the happy path.
#[derive(Debug, Clone, PartialEq)]
pub enum LifecycleStage {
    Submitted,                         // sent to the block engine, not yet seen on-chain
    Pending,                           // engine acknowledged but no tx-seen event yet
    Processed { landing_slot: u64 },   // tx seen on-chain; waiting for slot confirmation
    Confirmed { landing_slot: u64 },   // landing slot reached supermajority vote
    Finalized { landing_slot: u64 },   // landing slot is rooted (irreversible)
    Failed { reason: String },         // terminal: engine rejected or timeout
}

// Per-bundle state held by the tracker.
#[derive(Debug, Clone)]
pub struct BundleHandle {
    pub bundle_id: String,
    pub signatures: Vec<String>, // base58, one per tx in the bundle
    pub stage: LifecycleStage,
    pub submitted_at: Instant,
    // Unix-ms timestamps at each commitment stage — used for latency reporting.
    pub submitted_at_ms: u64,
    pub processed_at_ms: Option<u64>,
    pub confirmed_at_ms: Option<u64>,
    pub finalized_at_ms: Option<u64>,
    pub landing_slot: Option<u64>,
}

// Tracks all in-flight bundles and updates their stage as bus events arrive.
pub struct LifecycleTracker {
    handles: HashMap<String, BundleHandle>, // bundle_id -> handle
    sig_index: HashMap<String, String>,     // sig -> bundle_id for O(1) tx-seen lookup
    // Optional network bus to stream stage transitions to WebSocket clients.
    exec_bus: Option<broadcast::Sender<NetworkEvent>>,
}

impl LifecycleTracker {
    pub fn new() -> Self {
        LifecycleTracker {
            handles: HashMap::new(),
            sig_index: HashMap::new(),
            exec_bus: None,
        }
    }

    /// Attach a network bus so on-chain stage transitions are streamed live.
    pub fn with_exec_bus(mut self, bus: broadcast::Sender<NetworkEvent>) -> Self {
        self.exec_bus = Some(bus);
        self
    }

    /// Call immediately after submitting a bundle to start tracking it.
    pub fn register(&mut self, bundle_id: String, signatures: Vec<String>) {
        for sig in &signatures {
            self.sig_index.insert(sig.clone(), bundle_id.clone());
        }
        self.handles.insert(
            bundle_id.clone(),
            BundleHandle {
                bundle_id,
                signatures,
                stage: LifecycleStage::Submitted,
                submitted_at: Instant::now(),
                submitted_at_ms: now_ms(),
                processed_at_ms: None,
                confirmed_at_ms: None,
                finalized_at_ms: None,
                landing_slot: None,
            },
        );
    }

    /// Called when a TxSeen event arrives from the Yellowstone stream.
    /// Records the landing slot and advances to Processed.
    pub fn on_tx_seen(&mut self, sig: &str, slot: u64) {
        if let Some(bundle_id) = self.sig_index.get(sig).cloned() {
            if let Some(handle) = self.handles.get_mut(&bundle_id) {
                // Only advance if we haven't already moved past this stage.
                if matches!(
                    handle.stage,
                    LifecycleStage::Submitted | LifecycleStage::Pending
                ) {
                    info!(bundle_id = %bundle_id, sig = %sig, slot = %slot, "tx seen on-chain");
                    handle.stage = LifecycleStage::Processed { landing_slot: slot };
                    handle.processed_at_ms = Some(now_ms());
                    handle.landing_slot = Some(slot);
                    emit_stage(&self.exec_bus, &bundle_id, "processed", slot,
                        &format!("seen on-chain in slot {}", slot));
                }
            }
        }
    }

    /// Called when a SlotUpdate event arrives from the Yellowstone stream.
    /// Advances bundles whose landing slot just reached Confirmed or Finalized.
    pub fn on_slot_update(&mut self, slot: u64, status: &SlotStatus) {
        for handle in self.handles.values_mut() {
            match (&handle.stage, status) {
                (LifecycleStage::Processed { landing_slot }, SlotStatus::Confirmed)
                    if *landing_slot == slot =>
                {
                    info!(bundle_id = %handle.bundle_id, slot, "bundle confirmed");
                    handle.stage = LifecycleStage::Confirmed { landing_slot: slot };
                    handle.confirmed_at_ms = Some(now_ms());
                    emit_stage(&self.exec_bus, &handle.bundle_id, "confirmed", slot,
                        &format!("slot {} reached supermajority", slot));
                }
                (LifecycleStage::Confirmed { landing_slot }, SlotStatus::Finalized)
                    if *landing_slot == slot =>
                {
                    info!(bundle_id = %handle.bundle_id, slot, "bundle finalized");
                    handle.stage = LifecycleStage::Finalized { landing_slot: slot };
                    handle.finalized_at_ms = Some(now_ms());
                    emit_stage(&self.exec_bus, &handle.bundle_id, "finalized", slot,
                        &format!("slot {} rooted — irreversible", slot));
                }
                _ => {}
            }
        }
    }

    /// Advances the bundle directly to Confirmed, driven by the RPC signature-status fallback.
    /// Bypasses the Yellowstone TxSeen → SlotUpdate two-step when Yellowstone misses the tx.
    pub fn mark_confirmed_rpc(&mut self, sig: &str, landing_slot: u64) {
        if let Some(bundle_id) = self.sig_index.get(sig).cloned() {
            if let Some(handle) = self.handles.get_mut(&bundle_id) {
                if matches!(
                    handle.stage,
                    LifecycleStage::Submitted
                        | LifecycleStage::Pending
                        | LifecycleStage::Processed { .. }
                ) {
                    info!(bundle_id = %bundle_id, sig = %sig, slot = %landing_slot, "confirmed via RPC fallback");
                    handle.stage = LifecycleStage::Confirmed { landing_slot };
                    let now = now_ms();
                    // RPC bypasses Yellowstone's TxSeen → Processed step, so processed_at_ms
                    // would otherwise stay None. Backfill it here so timing metrics are complete.
                    if handle.processed_at_ms.is_none() { handle.processed_at_ms = Some(now); }
                    handle.confirmed_at_ms = Some(now);
                    handle.landing_slot = Some(landing_slot);
                    emit_stage(&self.exec_bus, &bundle_id, "confirmed", landing_slot,
                        &format!("slot {} confirmed (RPC fallback)", landing_slot));
                }
            }
        }
    }

    /// Advance a bundle's stage from authoritative getBundleStatuses data.
    /// `status` is "processed" | "confirmed" | "finalized" and `slot` is the real
    /// landed slot reported by Jito — the source of accurate slot numbers and the
    /// Finalized timestamp for the Jito lane. Backfills earlier-stage timestamps
    /// when a stage is first observed already past (e.g. seen confirmed directly).
    pub fn mark_bundle_commitment(&mut self, bundle_id: &str, slot: u64, status: &str) {
        let Some(handle) = self.handles.get_mut(bundle_id) else { return; };
        if matches!(handle.stage, LifecycleStage::Failed { .. }) { return; }
        handle.landing_slot = Some(slot);
        let now = now_ms();
        match status {
            "processed" => {
                if matches!(handle.stage, LifecycleStage::Submitted | LifecycleStage::Pending) {
                    handle.stage = LifecycleStage::Processed { landing_slot: slot };
                    if handle.processed_at_ms.is_none() { handle.processed_at_ms = Some(now); }
                    emit_stage(&self.exec_bus, bundle_id, "processed", slot,
                        &format!("bundle landed in slot {} (processed)", slot));
                }
            }
            "confirmed" => {
                if handle.processed_at_ms.is_none() { handle.processed_at_ms = Some(now); }
                if !matches!(handle.stage,
                    LifecycleStage::Confirmed { .. } | LifecycleStage::Finalized { .. }) {
                    handle.stage = LifecycleStage::Confirmed { landing_slot: slot };
                    if handle.confirmed_at_ms.is_none() { handle.confirmed_at_ms = Some(now); }
                    emit_stage(&self.exec_bus, bundle_id, "confirmed", slot,
                        &format!("slot {} reached supermajority", slot));
                }
            }
            "finalized" => {
                if handle.processed_at_ms.is_none() { handle.processed_at_ms = Some(now); }
                if handle.confirmed_at_ms.is_none() { handle.confirmed_at_ms = Some(now); }
                if !matches!(handle.stage, LifecycleStage::Finalized { .. }) {
                    handle.stage = LifecycleStage::Finalized { landing_slot: slot };
                    if handle.finalized_at_ms.is_none() { handle.finalized_at_ms = Some(now); }
                    emit_stage(&self.exec_bus, bundle_id, "finalized", slot,
                        &format!("slot {} rooted — irreversible", slot));
                }
            }
            _ => {}
        }
    }

    /// Called by the Jito status watcher when the block engine reports a bundle as Failed.
    pub fn mark_failed(&mut self, bundle_id: &str, reason: String) {
        if let Some(handle) = self.handles.get_mut(bundle_id) {
            warn!(bundle_id = %bundle_id, reason = %reason, "marking bundle failed");
            handle.stage = LifecycleStage::Failed { reason };
        }
    }

    pub fn get(&self, bundle_id: &str) -> Option<&BundleHandle> {
        self.handles.get(bundle_id)
    }
}

/// Feeds the dedicated lifecycle bus into the tracker.
// Uses LifecycleEvent (not NetworkEvent) so TipFloor market data never touches this path.
// Call once at startup before any bundles are submitted.
pub async fn run_event_loop(
    tracker: Arc<Mutex<LifecycleTracker>>,
    mut rx: broadcast::Receiver<LifecycleEvent>,
) {
    loop {
        match rx.recv().await {
            Ok(event) => {
                let mut t = tracker.lock().await;
                match event {
                    LifecycleEvent::TxSeen { sig, slot } => t.on_tx_seen(&sig, slot),
                    LifecycleEvent::SlotUpdate { slot, status, .. } => {
                        t.on_slot_update(slot, &status)
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                // Tracker fell behind even the high-capacity bus -- something is very wrong.
                warn!("lifecycle tracker lagged {} events -- bundle state may be stale", n);
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

// Checks the tracker every 200ms until the bundle reaches Confirmed or fails.
// Primary confirmation path, driven by the Yellowstone stream; the Jito status
// watcher and RPC signature-status check are fallbacks.
pub async fn wait_confirmed(
    tracker: Arc<Mutex<LifecycleTracker>>,
    bundle_id: &str,
    timeout_secs: u64,
) -> Result<BundleHandle> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);

    loop {
        if Instant::now() > deadline {
            anyhow::bail!("bundle {} confirmation timed out after {}s", bundle_id, timeout_secs);
        }

        {
            let t = tracker.lock().await;
            if let Some(handle) = t.get(bundle_id) {
                match &handle.stage {
                    // Confirmed is good enough to report success.
                    LifecycleStage::Confirmed { .. } | LifecycleStage::Finalized { .. } => {
                        return Ok(handle.clone());
                    }
                    LifecycleStage::Failed { reason } => {
                        anyhow::bail!("bundle {} failed: {}", bundle_id, reason);
                    }
                    // Still in progress -- fall through to sleep.
                    _ => {}
                }
            }
        }

        // Check every 200ms -- fast enough to catch confirmation promptly.
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// RPC signature-status fallback: every 500ms checks `getSignatureStatuses` and,
/// once the cluster reports the tx confirmed, advances the LifecycleTracker to
/// Confirmed using the REAL landed slot the RPC returns (no get_slot
/// approximation, so the logged slot matches what explorers show). Runs in
/// parallel with the Yellowstone stream as a safety net for when the stream does
/// not emit a TxSeen event (e.g. the tx touches accounts outside the subscription).
pub async fn rpc_confirm_watcher(
    rpc: Arc<RpcClient>,
    sig_str: String,
    tracker: Arc<Mutex<LifecycleTracker>>,
    timeout_secs: u64,
) {
    let Ok(sig) = sig_str.parse::<Signature>() else {
        warn!(sig = %sig_str, "rpc_confirm_watcher: invalid signature");
        return;
    };

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);

    loop {
        if Instant::now() > deadline {
            return;
        }

        tokio::time::sleep(Duration::from_millis(500)).await;

        let rpc2 = rpc.clone();
        let result = tokio::task::spawn_blocking(move || {
            rpc2.get_signature_statuses(&[sig])
        })
        .await;

        match result {
            Ok(Ok(resp)) => {
                // resp.value[0] is None until the tx is visible to the RPC.
                if let Some(Some(status)) = resp.value.into_iter().next() {
                    if let Some(tx_err) = status.err {
                        warn!(sig = %sig_str, error = ?tx_err, "tx landed with on-chain error");
                        return;
                    }
                    // Only advance once the cluster reports confirmed/finalized (not merely
                    // processed) so we never mark a tx confirmed too early. Read the level off
                    // the Debug form to avoid pulling in the confirmation-status enum type.
                    let level = format!("{:?}", status.confirmation_status).to_lowercase();
                    if level.contains("confirmed") || level.contains("finalized") {
                        // status.slot is the actual landed slot — what explorers show.
                        tracker.lock().await.mark_confirmed_rpc(&sig_str, status.slot);
                        return;
                    }
                }
            }
            Ok(Err(e)) => {
                tracing::debug!(sig = %sig_str, "rpc sig status error: {}", e);
            }
            Err(e) => {
                tracing::debug!(sig = %sig_str, "rpc sig status task error: {}", e);
            }
        }
    }
}

/// Best-effort wait until a bundle reaches Finalized (or timeout). Called after
/// confirmation so the execution log can record the finalized timestamp and the
/// full Processed -> Confirmed -> Finalized progression.
pub async fn wait_finalized(
    tracker: Arc<Mutex<LifecycleTracker>>,
    bundle_id: &str,
    timeout_secs: u64,
) {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if Instant::now() > deadline {
            return;
        }
        {
            let t = tracker.lock().await;
            if let Some(h) = t.get(bundle_id) {
                if matches!(h.stage, LifecycleStage::Finalized { .. }) {
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

/// Watches `getBundleStatuses` for a Jito bundle and feeds authoritative slot +
/// commitment transitions into the tracker until finalized or timeout. This is
/// the source of accurate landing slots and the Finalized timestamp on the Jito
/// lane; the Yellowstone stream remains the primary signal for Processed.
pub async fn bundle_status_watcher(
    jito: Arc<JitoClient>,
    bundle_id: String,
    tracker: Arc<Mutex<LifecycleTracker>>,
    timeout_secs: u64,
) {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if Instant::now() > deadline {
            return;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        match jito.get_bundle_statuses(&bundle_id).await {
            Ok(Some(c)) => {
                tracing::info!(bundle_id = %bundle_id, slot = c.slot, status = %c.confirmation_status,
                    sigs = ?c.signatures, "getBundleStatuses: bundle landed");
                tracker
                    .lock()
                    .await
                    .mark_bundle_commitment(&bundle_id, c.slot, &c.confirmation_status);
                if c.confirmation_status == "finalized" {
                    return;
                }
            }
            Ok(None) => {
                tracing::info!(bundle_id = %bundle_id, "getBundleStatuses: not yet landed");
            }
            Err(e) => tracing::warn!(bundle_id = %bundle_id, error = %e, "getBundleStatuses error"),
        }
    }
}
