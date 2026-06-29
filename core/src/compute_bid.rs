use serde::{Deserialize, Serialize};

use crate::auction::AuctionWindow;
use crate::bid::Regime;

pub const MIN_TIP_LAMPORTS: u64 = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TxType {
    Snipe,
    Swap,
    Arb,
    Memo,
}

pub fn compute_tip(
    window: &AuctionWindow,
    forward_multiplier: f64,
    tx_type: TxType,
    value_lamports: u64,
    ceiling: u64,
) -> u64 {
    let baseline = window.compute_baseline() as f64;

    let safety = match window.regime {
        Regime::Cold  => 1.05,
        Regime::Warm  => 1.10,
        Regime::Hot   => 1.20,
        Regime::Manic => 1.50,
    };

    let raw = (baseline * safety * forward_multiplier.clamp(0.1, 10.0)) as u64;

    let value_cap: u64 = match tx_type {
        TxType::Snipe => (value_lamports as f64 * 0.80) as u64,
        TxType::Swap  => ((value_lamports as f64 * 0.05) as u64).max(MIN_TIP_LAMPORTS),
        TxType::Arb   => (value_lamports as f64 * 0.60) as u64,
        TxType::Memo  => u64::MAX,
    };

    let effective_cap = ceiling.min(value_cap).max(MIN_TIP_LAMPORTS);
    raw.max(MIN_TIP_LAMPORTS).min(effective_cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bootstrapped_window(tip: u64) -> AuctionWindow {
        let mut w = AuctionWindow::new();
        for slot in 100..106u64 {
            w.ingest(slot, tip);
        }
        w
    }

    #[test]
    fn warm_regime_1x_multiplier() {
        let w = bootstrapped_window(60_000);
        let tip = compute_tip(&w, 1.0, TxType::Memo, 0, 10_000_000);
        assert_eq!(tip, 66_000);
    }

    #[test]
    fn floor_is_always_respected() {
        let w = AuctionWindow::new();
        let tip = compute_tip(&w, 0.1, TxType::Memo, 0, 10_000_000);
        assert!(tip >= MIN_TIP_LAMPORTS);
    }

    #[test]
    fn ceiling_clamps() {
        let w = bootstrapped_window(500_000);
        let tip = compute_tip(&w, 5.0, TxType::Memo, 0, 100_000);
        assert_eq!(tip, 100_000);
    }

    #[test]
    fn swap_value_cap() {
        let w = bootstrapped_window(60_000);
        let tip = compute_tip(&w, 2.0, TxType::Swap, 1_000_000, 10_000_000);
        assert!(tip <= 50_000);
    }
}
