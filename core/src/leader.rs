// Leader-window awareness for submission timing.
//
// Jito only lands a bundle when a Jito-Solana validator is the leader: the block
// engine buffers a bundle and forwards it to the *next Jito leader*, after which
// the bundle expires. So the operationally useful signal at submission time is
// "where are we in the current leader's 4-slot window, and who leads next."
//
// LeaderClock maintains that view continuously off two live sources — the
// Yellowstone slot stream (current slot) and the RPC leader schedule
// (getSlotLeaders) — so the hot submission path never makes an RPC call. It is
// used to annotate every Jito submission with its leader-window context (visible
// in the lifecycle log and the live UI stream), making submission leader-aware
// rather than blind.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use solana_rpc_client::rpc_client::RpcClient;
use tokio::sync::{broadcast, RwLock};
use tracing::warn;

use styx_ingest::bus::{NetworkEvent, SlotStatus};

// A validator leads NUM_CONSECUTIVE_LEADER_SLOTS = 4 consecutive slots, and the
// windows are aligned to absolute slot indices divisible by 4.
pub const SLOTS_PER_LEADER: u64 = 4;

#[derive(Default)]
struct LeaderState {
    current_slot: u64,
    // Absolute slot -> leader identity pubkey (base58), covering an upcoming span.
    schedule: BTreeMap<u64, String>,
}

/// Leader-window context for a single submission instant.
#[derive(Debug, Clone)]
pub struct SubmissionWindow {
    pub current_slot: u64,
    pub leader: Option<String>,    // identity pubkey of the current slot's leader
    pub slot_in_window: u64,       // 0..=3 — position within the leader's 4-slot window
    pub slots_left_in_window: u64, // slots remaining before the next leader takes over
    pub next_leader: Option<String>,
}

#[derive(Clone)]
pub struct LeaderClock {
    state: Arc<RwLock<LeaderState>>,
}

impl Default for LeaderClock {
    fn default() -> Self {
        Self::new()
    }
}

impl LeaderClock {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(LeaderState::default())),
        }
    }

    /// Current processed slot as seen on the Yellowstone stream (0 until first slot).
    pub async fn current_slot(&self) -> u64 {
        self.state.read().await.current_slot
    }

    /// Leader-window context for the current slot — used to make submission
    /// leader-window-aware and to annotate the lifecycle log.
    pub async fn submission_window(&self) -> SubmissionWindow {
        let s = self.state.read().await;
        let slot = s.current_slot;
        let window_start = slot - (slot % SLOTS_PER_LEADER);
        let slot_in_window = slot % SLOTS_PER_LEADER;
        let slots_left_in_window = SLOTS_PER_LEADER - slot_in_window;
        // Prefer the exact slot's leader; fall back to the window-start leader.
        let leader = s
            .schedule
            .get(&slot)
            .cloned()
            .or_else(|| s.schedule.get(&window_start).cloned());
        let next_leader = s.schedule.get(&(window_start + SLOTS_PER_LEADER)).cloned();
        SubmissionWindow {
            current_slot: slot,
            leader,
            slot_in_window,
            slots_left_in_window,
            next_leader,
        }
    }
}

/// Keeps `current_slot` live off the network bus — just records the latest
/// processed/confirmed slot. Runs forever.
pub async fn run_slot_listener(clock: LeaderClock, mut rx: broadcast::Receiver<NetworkEvent>) {
    loop {
        match rx.recv().await {
            Ok(NetworkEvent::SlotUpdate { slot, status, .. }) => {
                // Track the leading edge of the chain (processed tip).
                if matches!(status, SlotStatus::Processed | SlotStatus::Confirmed) {
                    let mut s = clock.state.write().await;
                    if slot > s.current_slot {
                        s.current_slot = slot;
                    }
                }
            }
            Ok(_) => {}
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Refreshes the upcoming slot-leader schedule from RPC (getSlotLeaders, no
/// hardcoded data) so `submission_window()` can name the current and next
/// leaders. The schedule is deterministic per epoch, so a coarse refresh is
/// enough to stay ahead of the live slot. Runs forever.
pub async fn run_schedule_refresher(clock: LeaderClock, rpc: Arc<RpcClient>) {
    loop {
        // Anchor on the live slot; if the stream hasn't produced one yet, ask RPC.
        let cur = clock.current_slot().await;
        let anchor = if cur == 0 {
            let rpc2 = rpc.clone();
            tokio::task::spawn_blocking(move || rpc2.get_slot().unwrap_or(0))
                .await
                .unwrap_or(0)
        } else {
            cur
        };

        if anchor > 0 {
            let rpc2 = rpc.clone();
            // ~8 leader windows ahead so the window is always populated between refreshes.
            let fetched =
                tokio::task::spawn_blocking(move || rpc2.get_slot_leaders(anchor, 32)).await;
            match fetched {
                Ok(Ok(leaders)) => {
                    let mut s = clock.state.write().await;
                    s.schedule.clear();
                    for (i, pk) in leaders.iter().enumerate() {
                        s.schedule.insert(anchor + i as u64, pk.to_string());
                    }
                }
                Ok(Err(e)) => warn!("leader schedule refresh failed: {}", e),
                Err(e) => warn!("leader schedule refresh task error: {}", e),
            }
        }

        // Refresh roughly every 8 slots (~3s) — keeps the window current without
        // hammering RPC.
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}
