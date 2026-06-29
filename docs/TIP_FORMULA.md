# Tip Pricing Formula

## Overview

Tips are priced dynamically from live Jito tip auction data observed via Yellowstone. There are no static tables or hardcoded percentiles.

## Data source

The eight Jito tip accounts are monitored via a Yellowstone account subscription. Each time any account balance increases, the delta (lamports received) is recorded as an observed tip for that slot. This gives empirical clearing prices for the auction in real time, without polling the Jito REST API.

## AuctionWindow

A ring buffer of the last 20 slots. For each slot, the clearing price is the maximum tip seen in that slot. From the window the system computes:

- `clearing_price_min` — lowest slot clearing price in the window
- `clearing_price_median` — median slot clearing price
- `clearing_price_max` — highest slot clearing price
- `bundles_per_slot` — average number of tips per slot (proxy for competition)
- `trend` — Rising (ratio > 1.15), Falling (ratio < 0.85), or Stable
- `regime` — classified from `clearing_price_median`
- `is_bootstrapped` — true after five or more slots have been observed

The window is not bootstrapped on cold start. During this period the baseline defaults to `2 * MIN_TIP_LAMPORTS` to avoid underbidding.

## Regime classification

| Regime | Median clearing price | Safety margin |
|---|---|---|
| Cold | below 10,000 lamports | 1.05 |
| Warm | 10,000 to 100,000 | 1.10 |
| Hot | 100,000 to 1,000,000 | 1.20 |
| Manic | above 1,000,000 | 1.50 |

Thresholds calibrated against June 2025 mainnet observations. Typical mainnet during active trading hours is Hot (100k to 400k lamports median clearing price).

## Tip formula

```
baseline = max(clearing_price_median, MIN_TIP_LAMPORTS)
raw_tip  = baseline * safety_margin * forward_multiplier
tip      = clamp(raw_tip, MIN_TIP_LAMPORTS, effective_ceiling)
```

Where:
- `MIN_TIP_LAMPORTS` = 50,000
- `safety_margin` = regime-dependent (table above)
- `forward_multiplier` = AI agent output, clamped to [0.1, 10.0]
- `effective_ceiling` = min(config_ceiling, value_cap)

## forward_multiplier semantics

`forward_multiplier = 1.0` means: bid at exactly the median clearing price plus the regime safety margin.

- 1.0 = match the going rate
- 1.5 = 50% above the going rate (aggressive)
- 0.7 = 30% below the going rate (conservative, may miss)
- 2.0+ = very aggressive, used by the OverpayerBaseline for benchmarking

## Value cap by transaction type

The tip is additionally clamped to a fraction of the transaction's economic value, preventing overpaying on low-value transactions:

| TxType | Value cap |
|---|---|
| Snipe | 80% of value_lamports |
| Swap | max(5% of value_lamports, MIN_TIP_LAMPORTS) |
| Arb | 60% of value_lamports |
| Memo | None (config ceiling applies) |

## OverpayerBaseline

A deterministic agent that always returns `forward_multiplier = 2.0`. It runs in parallel with the LLM on every submission. Its computed tip is logged as `baseline_tip_lamports`. The difference (`baseline - actual`) is reported as savings in the execution log.

A positive delta means the AI saved money vs. the naive 2x strategy. Over many submissions, this accumulates into a measurable SOL saving.

## Example calculation

Regime: Hot. Clearing price median: 303,900 lamports. Forward multiplier: 1.2.

```
baseline = max(303900, 50000) = 303900
safety   = 1.20
raw_tip  = 303900 * 1.20 * 1.2 = 437,616
tip      = clamp(437616, 50000, 500000) = 437,616 lamports (~0.00044 SOL)
```

Baseline (2x):
```
raw_tip  = 303900 * 1.20 * 2.0 = 729,360
tip      = clamp(729360, 50000, 500000) = 500,000 lamports (ceiling hit)
```

Savings vs. baseline: 500,000 - 437,616 = 62,384 lamports per transaction.
