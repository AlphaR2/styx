use futures::StreamExt;
use gloo_net::websocket::Message;
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::time::Duration;
use wasm_bindgen::JsValue;

use crate::api::{fetch_leaders, fetch_tip_floor, format_utc_hms, ws_connect, ws_reconnect_delay, LeaderSchedule, SlotStatus, WsEvent};
use crate::utils::{lamports_to_sol, short};

// One live bundle-lifecycle event for the mission-control flow feed.
#[derive(Clone)]
struct FlowEvt {
    bundle_id: String,
    stage: String,
    message: String,
    tip_lamports: u64,
    retry: u32,
    ts_ms: u64,
}

#[component]
pub fn NetworkPage() -> impl IntoView {
    let (slot, set_slot)               = signal(0u64);
    let (slot_status, set_slot_status) = signal(SlotStatus::Processed);
    let (tip_min, set_tip_min)         = signal(0u64);
    let (tip_med, set_tip_med)         = signal(0u64);
    let (tip_max, set_tip_max)         = signal(0u64);
    let (connected, set_connected)     = signal(false);
    let (flow, set_flow)               = signal(Vec::<FlowEvt>::new());
    let (leaders, set_leaders)         = signal(None::<LeaderSchedule>);

    // Refresh the live leader schedule (getSlot + getSlotLeaders) every 2s.
    let load_leaders = move || {
        spawn_local(async move {
            if let Ok(s) = fetch_leaders().await { set_leaders.set(Some(s)); }
        });
    };
    load_leaders();
    set_interval(load_leaders, Duration::from_secs(2));

    // Seed the fee tiles from the current snapshot immediately, then poll as a
    // fallback. The WS still updates these live between polls; this just guarantees
    // real values on load and across socket reconnects (otherwise they sit at 0
    // until the next push, reading as "Calm" / "0 SOL").
    let load_tip_floor = move || {
        spawn_local(async move {
            if let Ok(s) = fetch_tip_floor().await {
                set_tip_min.set(s.clearing_price_min);
                set_tip_med.set(s.clearing_price_median);
                set_tip_max.set(s.clearing_price_max);
            }
        });
    };
    load_tip_floor();
    set_interval(load_tip_floor, Duration::from_secs(2));

    // Connect to the live event stream, updating slot + tip-floor signals as events
    // arrive. Reconnects automatically on drop so the tab never gets stuck on "connecting…".
    spawn_local(async move {
        loop {
            let Some(ws) = ws_connect().await else {
                set_connected.set(false);
                ws_reconnect_delay().await;
                continue;
            };
            set_connected.set(true);
            let (_, mut read) = ws.split();
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(text)) => match serde_json::from_str::<WsEvent>(&text) {
                        Ok(WsEvent::SlotUpdate { slot: s, status: st, .. }) => {
                            set_slot.set(s);
                            set_slot_status.set(st);
                        }
                        Ok(WsEvent::Execution { bundle_id, stage, message, tip_lamports, retry, ts_ms, .. }) => {
                            set_flow.update(|v| {
                                v.insert(0, FlowEvt { bundle_id, stage, message, tip_lamports, retry, ts_ms });
                                v.truncate(12);
                            });
                        }
                        _ => {}
                    },
                    Err(e) => {
                        web_sys::console::log_1(&JsValue::from_str(&format!("WS error: {:?}", e)));
                        break;
                    }
                    _ => {}
                }
            }
            set_connected.set(false);
            ws_reconnect_delay().await;
        }
    });

    view! {
        <div class="space-y-8">

            // ── Page header ─────────────────────────────────────────────
            <div class="flex items-end justify-between">
                <div>
                    <p class="text-xs font-mono uppercase tracking-widest text-zinc-600 mb-1">
                        "Dashboard / Mission Control"
                    </p>
                    <h1 class="text-3xl font-bold tracking-tight">"Mission Control"</h1>
                    <p class="text-sm text-zinc-500 mt-1">
                        "Live slots, leader windows, fee pressure, and transactions moving through the network — in real time."
                    </p>
                </div>
                <div class=move || format!(
                    "flex items-center gap-2 px-3 py-1.5 rounded-full text-xs font-medium {}",
                    if connected.get() {
                        "border border-amber-800/60 bg-amber-950 text-amber-400"
                    } else {
                        "border border-[#2e2e2e] bg-[#222] text-zinc-600"
                    }
                )>
                    <span class=move || if connected.get() {
                        "h-1.5 w-1.5 rounded-full bg-amber-400 animate-pulse"
                    } else {
                        "h-1.5 w-1.5 rounded-full bg-zinc-700"
                    }></span>
                    {move || if connected.get() { "Live · connected" } else { "Connecting…" }}
                </div>
            </div>

            // ── Hero tiles ──────────────────────────────────────────────
            <div class="grid grid-cols-1 sm:grid-cols-3 gap-4">

                // Network pressure — the headline, plain-English read on competition.
                <div class="relative overflow-hidden rounded-xl border border-amber-900 bg-[#1e1a10] p-6">
                    <div class="absolute -top-8 -right-8 h-28 w-28 rounded-full
                                bg-amber-500/10 blur-2xl pointer-events-none"></div>
                    <p class="text-xs uppercase tracking-widest text-amber-900/80">"Network Pressure"</p>
                    <p class=move || format!(
                        "mt-3 text-4xl font-bold tracking-tight glow-text {}", pressure(tip_med.get()).2
                    ) style="line-height:1.1">
                        {move || pressure(tip_med.get()).0}
                    </p>
                    <p class="mt-2 text-xs text-amber-200/50 leading-snug">
                        {move || pressure(tip_med.get()).1}
                    </p>
                </div>

                // Current block (slot) — renamed + plain status words.
                <div class="relative overflow-hidden rounded-xl border border-[#2e2e2e] bg-[#222] p-6">
                    <div class="absolute -top-8 -right-8 h-24 w-24 rounded-full
                                bg-zinc-600/10 blur-2xl pointer-events-none"></div>
                    <p class="text-xs uppercase tracking-widest text-zinc-600">"Current Block"</p>
                    <p class="mt-3 font-mono text-3xl font-bold tracking-tight text-zinc-100" style="line-height:1.1">
                        {move || format!("{}", slot.get())}
                    </p>
                    <div class="mt-2 flex items-center gap-2">
                        <span class=move || format!(
                            "inline-flex h-1.5 w-1.5 rounded-full {}", block_state(&slot_status.get()).1
                        )></span>
                        <span class="text-xs text-zinc-500">
                            {move || block_state(&slot_status.get()).0}
                        </span>
                    </div>
                </div>

                // Fast-lane cost — the number that answers "will my transaction land?".
                <div class="relative overflow-hidden rounded-xl border border-[#2e2e2e] bg-[#222] p-6">
                    <div class="absolute -top-8 -right-8 h-24 w-24 rounded-full
                                bg-zinc-600/10 blur-2xl pointer-events-none"></div>
                    <p class="text-xs uppercase tracking-widest text-zinc-600">"Cost to Land Fast"</p>
                    <p class="mt-3 font-mono text-2xl font-bold tracking-tight text-zinc-100" style="line-height:1.1">
                        {move || lamports_to_sol(tip_max.get())}
                    </p>
                    <p class="mt-2 text-xs text-zinc-600 leading-snug">
                        "What the most aggressive bidders pay to almost always get in."
                    </p>
                </div>

            </div>

            // ── Leader window + live bundle flow ────────────────────────
            <div class="grid grid-cols-1 lg:grid-cols-2 gap-4">
                <LeaderWindow schedule=leaders/>
                <LiveBundleFlow flow=flow/>
            </div>

            // ── What people are tipping ─────────────────────────────────
            <div class="rounded-xl border border-[#2e2e2e] bg-[#222] overflow-hidden">
                <div class="px-6 pt-5 pb-4 border-b border-[#2e2e2e]">
                    <h2 class="text-sm font-semibold">"What people are tipping to get in line"</h2>
                    <p class="text-xs text-zinc-500 mt-0.5">
                        "A tip is a tiny amount of SOL paid to validators to prioritize a transaction. \
                         The longer the bar, the more competition at that level."
                    </p>
                </div>
                <div class="px-6 py-5 space-y-5">
                    <RateBar label="Floor"    caption="lowest observed auction clearing price" value=tip_min max_signal=tip_max/>
                    <RateBar label="Typical"  caption="median clearing price across last 20 slots" value=tip_med max_signal=tip_max/>
                    <RateBar label="Ceiling"  caption="highest observed auction clearing price" value=tip_max max_signal=tip_max/>
                </div>
            </div>

            // ── How to read this ────────────────────────────────────────
            <div class="rounded-xl border border-[#2a2a2a] bg-[#1f1f1f] px-6 py-5">
                <p class="text-[10px] uppercase tracking-widest text-zinc-600 mb-3">"How to read this"</p>
                <ul class="space-y-2 text-xs text-zinc-500 leading-relaxed">
                    <li>
                        <span class="text-zinc-300">"Network Pressure"</span>
                        " tells you, in one word, how crowded Solana is. Calm is cheap; Frenzy means you must bid high to land."
                    </li>
                    <li>
                        <span class="text-zinc-300">"Current Block"</span>
                        " is Solana's latest block. It moves to "<span class="text-amber-400">"Confirmed"</span>
                        " then "<span class="text-emerald-400">"Final"</span>" once it can never be reversed."
                    </li>
                    <li>
                        <span class="text-zinc-300">"Tips"</span>
                        " are shown in SOL. They're fractions of a cent — Styx's AI picks the smallest tip that still lands, instead of overpaying for the fast lane every time."
                    </li>
                </ul>
            </div>

        </div>
    }
}

// Plain-English read on how crowded the network is, derived from the median tip.
// Uses the same thresholds the AI uses to classify contention (Cold/Warm/Hot/Manic),
// so the word here always matches the regime Styx is bidding under.
// Returns (word, meaning, text-color-class).
fn pressure(median_tip: u64) -> (&'static str, &'static str, &'static str) {
    if median_tip < 1_500 {
        ("Calm", "Hardly any competition — transactions land cheaply.", "text-emerald-400")
    } else if median_tip < 5_000 {
        ("Steady", "Normal activity — a standard tip lands fine.", "text-amber-400")
    } else if median_tip < 20_000 {
        ("Busy", "Lots of competition — you need to tip up to land.", "text-orange-400")
    } else {
        ("Frenzy", "Fee spike — pay the fast-lane rate or you'll miss out.", "text-red-400")
    }
}

// Plain-English label + dot color for a block's commitment level.
fn block_state(status: &SlotStatus) -> (&'static str, &'static str) {
    match status {
        SlotStatus::Processed => ("Processing", "bg-zinc-500"),
        SlotStatus::Confirmed => ("Confirmed", "bg-amber-400"),
        SlotStatus::Finalized => ("Final · locked in", "bg-emerald-400"),
    }
}

#[component]
fn RateBar(
    label: &'static str,
    caption: &'static str,
    value: ReadSignal<u64>,
    max_signal: ReadSignal<u64>,
) -> impl IntoView {
    let pct = move || {
        let max = max_signal.get();
        if max == 0 { 0.0 } else { (value.get() as f64 / max as f64 * 100.0).min(100.0) }
    };
    view! {
        <div class="flex items-center gap-4">
            <div class="w-32 shrink-0">
                <p class="text-xs font-semibold text-zinc-300">{label}</p>
                <p class="text-[10px] text-zinc-600 leading-tight">{caption}</p>
            </div>
            <div class="flex-1 h-2.5 rounded-full bg-[#2a2a2a] overflow-hidden">
                <div
                    class="h-full rounded-full bg-gradient-to-r from-amber-700 to-amber-400 transition-[width] duration-700"
                    style=move || format!("width: {:.1}%", pct())
                ></div>
            </div>
            <span class="w-28 shrink-0 text-right text-xs font-mono text-zinc-300">
                {move || lamports_to_sol(value.get())}
            </span>
        </div>
    }
}

// ── Leader window ───────────────────────────────────────────────────────────
// A leader produces blocks for 4 consecutive slots. We group the upcoming slot
// leaders into those windows and show which validator is producing now and which
// are next, with an ETA (≈400 ms/slot). This is the window Styx submits into:
// Jito auctions every slot and forwards the bundle to the current leader.

struct LeaderRun { leader: String, start: u64, end: u64 }

fn group_windows(s: &LeaderSchedule) -> Vec<LeaderRun> {
    let mut out: Vec<LeaderRun> = Vec::new();
    for ls in &s.leaders {
        if let Some(last) = out.last_mut() {
            if last.leader == ls.leader && ls.slot == last.end + 1 {
                last.end = ls.slot;
                continue;
            }
        }
        out.push(LeaderRun { leader: ls.leader.clone(), start: ls.slot, end: ls.slot });
    }
    out
}

#[component]
fn LeaderWindow(schedule: ReadSignal<Option<LeaderSchedule>>) -> impl IntoView {
    view! {
        <div class="rounded-xl border border-[#2e2e2e] bg-[#222] overflow-hidden">
            <div class="px-6 py-3 border-b border-[#2e2e2e] bg-[#252525]">
                <span class="text-[10px] font-mono uppercase tracking-widest text-zinc-600">
                    "Leader window · who produces the next blocks"
                </span>
            </div>
            <div class="p-4">
                {move || match schedule.get() {
                    None => view! {
                        <p class="text-xs text-zinc-600 px-2 py-4">"Loading leader schedule…"</p>
                    }.into_any(),
                    Some(s) => {
                        let cur = s.current_slot;
                        let windows = group_windows(&s);
                        view! {
                            <div class="space-y-1.5">
                                {windows.into_iter().take(4).map(|w| {
                                    let is_now = cur >= w.start && cur <= w.end;
                                    let eta = if is_now {
                                        "leading now".to_string()
                                    } else if w.start > cur {
                                        format!("in ~{:.1}s", (w.start - cur) as f64 * 0.4)
                                    } else {
                                        "passed".to_string()
                                    };
                                    let row_cls = if is_now {
                                        "flex items-center justify-between rounded-lg border border-amber-700/60 bg-amber-950/40 px-4 py-2.5"
                                    } else {
                                        "flex items-center justify-between rounded-lg border border-[#2a2a2a] bg-[#1f1f1f] px-4 py-2.5"
                                    };
                                    view! {
                                        <div class=row_cls>
                                            <div class="flex items-center gap-2.5">
                                                <span class=if is_now {
                                                    "h-2 w-2 rounded-full bg-amber-400 animate-pulse"
                                                } else { "h-2 w-2 rounded-full bg-zinc-700" }></span>
                                                <span class="font-mono text-xs text-zinc-300">{short(&w.leader, 8)}{"…"}</span>
                                                <span class="font-mono text-[10px] text-zinc-600">
                                                    {format!("slots {}–{}", w.start, w.end)}
                                                </span>
                                            </div>
                                            <span class=if is_now {
                                                "text-xs font-semibold text-amber-400"
                                            } else { "text-xs font-mono text-zinc-500" }>{eta}</span>
                                        </div>
                                    }
                                }).collect_view()}
                            </div>
                        }.into_any()
                    }
                }}
                <p class="text-[10px] text-zinc-700 leading-relaxed mt-3 px-1">
                    "Jito bundles go through a per-slot auction to the current leader; priority-fee transactions go "
                    "straight to the leader over standard RPC. Either way, Styx submits continuously and retries across windows."
                </p>
            </div>
        </div>
    }
}

// ── Live bundle flow ────────────────────────────────────────────────────────
// Bundles moving through their lifecycle, streamed straight off the /ws feed.
#[component]
fn LiveBundleFlow(flow: ReadSignal<Vec<FlowEvt>>) -> impl IntoView {
    view! {
        <div class="rounded-xl border border-[#2e2e2e] bg-[#222] overflow-hidden">
            <div class="px-6 py-3 border-b border-[#2e2e2e] bg-[#252525] flex items-center gap-2">
                <span class="h-1.5 w-1.5 rounded-full bg-amber-400 animate-pulse"></span>
                <span class="text-[10px] font-mono uppercase tracking-widest text-zinc-600">
                    "Live execution flow"
                </span>
            </div>
            <div class="p-2">
                {move || {
                    let evs = flow.get();
                    if evs.is_empty() {
                        view! {
                            <p class="text-xs text-zinc-600 px-4 py-6 text-center">
                                "Nothing in flight. Fire one from Execute and watch it move here live."
                            </p>
                        }.into_any()
                    } else {
                        view! {
                            <div class="space-y-1">
                                {evs.into_iter().map(|e| {
                                    let (dot, cls) = flow_colors(&e.stage);
                                    let loud = matches!(e.stage.as_str(), "retrying" | "repriced" | "resubmitted");
                                    let tip = if e.tip_lamports > 0 { lamports_to_sol(e.tip_lamports) } else { String::new() };
                                    let row_cls = if loud {
                                        "flex items-center gap-2.5 rounded-lg px-3 py-2 bg-orange-950/20 border-l-2 border-orange-500"
                                    } else {
                                        "flex items-center gap-2.5 rounded-lg px-3 py-2 border-l-2 border-transparent"
                                    };
                                    view! {
                                        <div class=row_cls>
                                            <span class=format!("h-2 w-2 rounded-full shrink-0 {}", dot)></span>
                                            <span class=format!("text-[11px] font-mono font-bold uppercase w-20 shrink-0 {}", cls)>
                                                {e.stage}
                                            </span>
                                            <span class="font-mono text-[10px] text-zinc-600 shrink-0">{short(&e.bundle_id, 6)}{"…"}</span>
                                            <span class="text-[11px] text-zinc-400 flex-1 truncate">{e.message}</span>
                                            {(e.retry > 0).then(|| view! {
                                                <span class="text-[10px] font-mono text-orange-400/80 shrink-0">{format!("try {}", e.retry + 1)}</span>
                                            })}
                                            <span class="text-[10px] font-mono text-zinc-600 shrink-0">{tip}</span>
                                            <span class="text-[9px] font-mono text-zinc-700 shrink-0">{format_utc_hms(e.ts_ms)}</span>
                                        </div>
                                    }
                                }).collect_view()}
                            </div>
                        }.into_any()
                    }
                }}
            </div>
        </div>
    }
}

fn flow_colors(stage: &str) -> (&'static str, &'static str) {
    match stage {
        "submitted"   => ("bg-blue-400",    "text-blue-300"),
        "processed"   => ("bg-cyan-400",    "text-cyan-300"),
        "confirmed"   => ("bg-green-400",   "text-green-300"),
        "finalized"   => ("bg-emerald-400", "text-emerald-300"),
        "retrying" | "repriced" | "resubmitted" => ("bg-orange-400", "text-orange-300"),
        "exhausted" | "terminal" => ("bg-red-400", "text-red-300"),
        _             => ("bg-zinc-500",    "text-zinc-300"),
    }
}
