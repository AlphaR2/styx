use serde::Serialize;
use tokio::sync::broadcast;

// Commitment level of a slot as reported by Yellowstone.
// Rooted (proto status=2) maps to Finalized for our purposes.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotStatus {
    Processed,
    Confirmed,
    Finalized,
}

// All events that flow from ingest to the rest of the system.
// Serialize is needed so the WebSocket handler can forward events as JSON to clients.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NetworkEvent {
    // A slot advanced to a new commitment level.
    SlotUpdate {
        slot: u64,
        parent: Option<u64>,
        status: SlotStatus,
    },
    // A transaction was seen on-chain, carrying the slot it landed in.
    TxSeen {
        sig: String, // base58-encoded signature
        slot: u64,
    },
    // A lamport delta observed on one of the 8 Jito tip accounts via Yellowstone.
    // tip_lamports is the balance increase this write, not the cumulative balance.
    JitoTip {
        slot: u64,
        tip_lamports: u64,
        ts_ms: u64,
    },
    // A step in a bundle's execution lifecycle, emitted live so the UI can render
    // the submit -> retry -> confirm flow as it happens rather than refetching /log.
    Execution {
        bundle_id: String,
        // "submitted" | "waiting" | "retrying" | "confirmed" | "exhausted" | "terminal"
        stage: String,
        tip_lamports: u64,
        retry: u32,
        regime: String,
        message: String,
        ts_ms: u64,
    },
    // A raw log line tagged with a bundle_id, bridged straight from the tracing
    // subscriber so the UI mirrors the server logs verbatim for that transaction.
    ExecLog {
        bundle_id: String,
        level: String,   // "INFO" | "WARN" | "ERROR" | "DEBUG" | "TRACE"
        target: String,  // e.g. "agent::execute", "core::retry"
        message: String, // the formatted log message plus any non-bundle fields
        ts_ms: u64,
    },
}

// 1024-capacity channel for market data. Stale tips and slot updates can be dropped.
pub const BUS_CAPACITY: usize = 1024;

/// This is used to broadcast the values of `NetworkEvent` via the entire stream so that all recievers recieve it. channel capacity is set to 1024. 

pub fn new_bus() -> broadcast::Sender<NetworkEvent> {
    let (tx, _) = broadcast::channel(BUS_CAPACITY);
    tx
}

// Lifecycle events must never be silently lost: a missed TxSeen means a bundle looks stuck.
// Separate bus with 4x capacity so the tracker stays current even under load.
#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    SlotUpdate { slot: u64, parent: Option<u64>, status: SlotStatus },
    TxSeen { sig: String, slot: u64 },
}

pub const LIFECYCLE_BUS_CAPACITY: usize = 4096;

pub fn new_lifecycle_bus() -> broadcast::Sender<LifecycleEvent> {
    let (tx, _) = broadcast::channel(LIFECYCLE_BUS_CAPACITY);
    tx
}
