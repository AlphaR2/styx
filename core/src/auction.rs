use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::bid::Regime;
use crate::compute_bid::MIN_TIP_LAMPORTS;

pub const WINDOW_SLOTS: usize = 20;
const MIN_BOOTSTRAP_SLOTS: usize = 5;

// One slot's observed tips. Cleared as the ring buffer rotates.
#[derive(Debug, Clone)]
struct SlotAuction {
    slot: u64,
    tips: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Trend {
    Rising,
    Stable,
    Falling,
}

// Ring buffer of the last WINDOW_SLOTS slots of Jito tip auction data.
// Built from live Jito tip-account balance deltas observed via Yellowstone.
// All statistics are recomputed eagerly on every ingest() call so callers
// always read a consistent snapshot without an extra lock.
#[derive(Debug, Clone, Serialize)]
pub struct AuctionWindow {
    #[serde(skip)]
    slots: VecDeque<SlotAuction>,
    // Per-slot clearing prices derived from the max tip seen in each slot.
    pub clearing_price_min: u64,
    pub clearing_price_median: u64,
    pub clearing_price_max: u64,
    // Average bundle count per slot over the window.
    pub bundles_per_slot: f64,
    pub trend: Trend,
    pub regime: Regime,
    // False until MIN_BOOTSTRAP_SLOTS slots have been ingested.
    // While false, callers should use a conservative cold-start baseline.
    pub is_bootstrapped: bool,
}

impl Default for AuctionWindow {
    fn default() -> Self {
        let mut w = AuctionWindow {
            slots: VecDeque::with_capacity(WINDOW_SLOTS),
            clearing_price_min: MIN_TIP_LAMPORTS * 2,
            clearing_price_median: MIN_TIP_LAMPORTS * 2,
            clearing_price_max: MIN_TIP_LAMPORTS * 2,
            bundles_per_slot: 0.0,
            trend: Trend::Stable,
            regime: Regime::Cold,
            is_bootstrapped: false,
        };
        w.recompute();
        w
    }
}

impl AuctionWindow {
    pub fn new() -> Self {
        Self::default()
    }

    // Ingest one tip observation (balance increase on a Jito tip account).
    // Ignores sub-protocol noise (< 1_000 lamports = below Jito's protocol floor).
    pub fn ingest(&mut self, slot: u64, tip_lamports: u64) {
        if tip_lamports < 1_000 {
            return;
        }

        match self.slots.back_mut() {
            Some(last) if last.slot == slot => {
                last.tips.push(tip_lamports);
            }
            Some(last) if last.slot < slot => {
                if self.slots.len() >= WINDOW_SLOTS {
                    self.slots.pop_front();
                }
                self.slots.push_back(SlotAuction { slot, tips: vec![tip_lamports] });
            }
            None => {
                self.slots.push_back(SlotAuction { slot, tips: vec![tip_lamports] });
            }
            // out-of-order slot (shouldn't happen with Yellowstone) — ignore
            _ => return,
        }

        self.recompute();
    }

    // The baseline bid: lowest clearing price observed in the window (or conservative
    // cold-start value before we have enough data).
    pub fn compute_baseline(&self) -> u64 {
        if self.is_bootstrapped {
            // Use median rather than min: forward_multiplier=1.0 should mean
            // "bid at the typical clearing price × safety", not the floor.
            // Min is too conservative — in Hot/Manic regimes it sits at
            // MIN_TIP_LAMPORTS while the actual auction clears 5-20× higher.
            self.clearing_price_median.max(MIN_TIP_LAMPORTS)
        } else {
            MIN_TIP_LAMPORTS * 2
        }
    }

    fn recompute(&mut self) {
        self.is_bootstrapped = self.slots.len() >= MIN_BOOTSTRAP_SLOTS;

        // Clearing price for a slot = max tip seen in that slot.
        let clearing_prices: Vec<u64> = self.slots.iter()
            .filter(|s| !s.tips.is_empty())
            .map(|s| *s.tips.iter().max().unwrap())
            .collect();

        if clearing_prices.is_empty() {
            self.clearing_price_min = MIN_TIP_LAMPORTS * 2;
            self.clearing_price_median = MIN_TIP_LAMPORTS * 2;
            self.clearing_price_max = MIN_TIP_LAMPORTS * 2;
            self.bundles_per_slot = 0.0;
            self.trend = Trend::Stable;
            self.regime = Regime::Cold;
            return;
        }

        let mut sorted = clearing_prices.clone();
        sorted.sort_unstable();

        self.clearing_price_min = sorted[0];
        self.clearing_price_median = sorted[sorted.len() / 2];
        self.clearing_price_max = *sorted.last().unwrap();

        let total_tips: usize = self.slots.iter().map(|s| s.tips.len()).sum();
        self.bundles_per_slot = if self.slots.is_empty() {
            0.0
        } else {
            total_tips as f64 / self.slots.len() as f64
        };

        // Trend: compare average clearing price of the first half vs the second half.
        let n = clearing_prices.len();
        self.trend = if n >= 4 {
            let first_avg = clearing_prices[..n / 2].iter().sum::<u64>() as f64 / (n / 2) as f64;
            let second_avg =
                clearing_prices[n / 2..].iter().sum::<u64>() as f64 / (n - n / 2) as f64;
            let ratio = second_avg / first_avg.max(1.0);
            if ratio > 1.15 {
                Trend::Rising
            } else if ratio < 0.85 {
                Trend::Falling
            } else {
                Trend::Stable
            }
        } else {
            Trend::Stable
        };

        // Regime thresholds calibrated to June 2025 mainnet observations.
        self.regime = match self.clearing_price_median {
            m if m < 10_000 => Regime::Cold,
            m if m < 100_000 => Regime::Warm,
            m if m < 1_000_000 => Regime::Hot,
            _ => Regime::Manic,
        };
    }
}
