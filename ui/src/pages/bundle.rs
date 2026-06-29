use futures::StreamExt;
use gloo_net::websocket::Message;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;
use std::time::Duration;

use crate::api::{fetch_bundle_events, fetch_log, fetch_summary, format_utc_hms, ws_connect, ws_reconnect_delay, AiSummary, LogEntry, WsEvent};
use crate::utils::{delta_to_sol, humanize_failure, lamports_to_sol};

// One live lifecycle event streamed over /ws for this bundle.
// When `level` is non-empty this is a raw bridged log line (mirrors the server
// terminal); otherwise it's a curated stage event from emit_exec.
#[derive(Clone, Debug)]
struct LiveEvent {
    stage: String,
    message: String,
    tip_lamports: u64,
    retry: u32,
    ts_ms: u64,
    level: String,
    target: String,
}

#[component]
pub fn BundlePage() -> impl IntoView {
    let params = use_params_map();
    let id = move || params.read().get("id").unwrap_or_default();

    let (entry, set_entry)         = signal(None::<LogEntry>);
    let (loaded, set_loaded)       = signal(false);
    let (events, set_events)       = signal(Vec::<LiveEvent>::new());
    let (summary, set_summary)     = signal(None::<AiSummary>);
    let (sum_loading, set_sum_loading) = signal(false);

    // Initial + periodic fetch of the authoritative record. The lifecycle timestamps
    // are sourced from Yellowstone; the live WS feed below shows transitions as they fire.
    let do_fetch = move || {
        let target = id();
        spawn_local(async move {
            if let Ok(all) = fetch_log().await {
                let found = all.into_iter().find(|e| {
                    e.bundle_id == target || e.landed_bundle_id.as_deref() == Some(target.as_str())
                });
                set_entry.set(found);
                set_loaded.set(true);
            }
        });
    };
    do_fetch();
    set_interval(do_fetch, Duration::from_secs(3));

    // Fetch the server-side replay buffer immediately. This populates the event list
    // with everything that fired before the WS connected — AI decisions, retries, etc.
    // The WS loop below then appends events as they fire going forward.
    {
        let target = id();
        spawn_local(async move {
            if let Ok(history) = fetch_bundle_events(&target).await {
                set_events.update(|v| {
                    for ev in history {
                        match ev {
                            WsEvent::Execution { bundle_id: _, stage, message, tip_lamports, retry, ts_ms, .. } => {
                                v.push(LiveEvent { stage, message, tip_lamports, retry, ts_ms, level: String::new(), target: String::new() });
                            }
                            WsEvent::ExecLog { bundle_id: _, level, target, message, ts_ms } => {
                                v.push(LiveEvent { stage: "log".to_string(), message, tip_lamports: 0, retry: 0, ts_ms, level, target });
                            }
                            _ => {}
                        }
                    }
                    // Sort oldest→newest so history + live WS events stay in order.
                    v.sort_by_key(|e| e.ts_ms);
                });
            }
        });
    }

    // Live stage/retry events straight off the bus (driven by Yellowstone + retry loop).
    // Reconnects automatically if the socket drops so the stream never goes silent.
    spawn_local(async move {
        loop {
            let Some(ws) = ws_connect().await else {
                ws_reconnect_delay().await;
                continue;
            };
            let (_, mut read) = ws.split();
            while let Some(Ok(Message::Text(text))) = read.next().await {
                let cur = id();
                let landed = entry.get_untracked().and_then(|e| e.landed_bundle_id);
                let matches_this = |bid: &str| bid == cur || landed.as_deref() == Some(bid);
                match serde_json::from_str(&text) {
                    Ok(WsEvent::Execution { bundle_id, stage, message, tip_lamports, retry, ts_ms, .. })
                        if matches_this(&bundle_id) =>
                    {
                        set_events.update(|v| {
                            v.push(LiveEvent {
                                stage, message, tip_lamports, retry, ts_ms,
                                level: String::new(), target: String::new(),
                            });
                            v.sort_by_key(|e| e.ts_ms);
                            if v.len() > 200 { let d = v.len() - 200; v.drain(0..d); }
                        });
                    }
                    // Raw bridged log line — mirrors the server terminal for this tx.
                    Ok(WsEvent::ExecLog { bundle_id, level, target, message, ts_ms })
                        if matches_this(&bundle_id) =>
                    {
                        set_events.update(|v| {
                            v.push(LiveEvent {
                                stage: "log".to_string(), message, tip_lamports: 0, retry: 0, ts_ms,
                                level, target,
                            });
                            v.sort_by_key(|e| e.ts_ms);
                            if v.len() > 200 { let d = v.len() - 200; v.drain(0..d); }
                        });
                    }
                    _ => {}
                }
            }
            // Socket closed — wait, then reconnect.
            ws_reconnect_delay().await;
        }
    });

    view! {
        <div class="w-full space-y-4 px-4 pb-8">

            // ── Back link ────────────────────────────────────────────────
            <A href="/log" attr:class="inline-flex items-center gap-1.5 text-xs font-mono
                text-zinc-500 hover:text-zinc-300 transition-colors pt-4">
                "← back to log"
            </A>

            {move || {
                if !loaded.get() {
                    return view! {
                        <div class="w-full rounded-xl border border-[#2e2e2e] bg-[#222] px-6 py-16 text-center">
                            <p class="text-sm text-zinc-600">"Loading…"</p>
                        </div>
                    }.into_any();
                }
                let Some(e) = entry.get() else {
                    return view! {
                        <div class="w-full rounded-xl border border-[#2e2e2e] bg-[#222] px-6 py-16 text-center space-y-2">
                            <p class="text-zinc-500 text-2xl">"🔍"</p>
                            <p class="text-sm font-medium">"Not found"</p>
                            <p class="text-xs text-zinc-600">"It may have scrolled out of the in-memory log."</p>
                        </div>
                    }.into_any();
                };
                let id_for_summary = e.bundle_id.clone();

                let status      = derive_status(&e);
                let status_cls  = status_class(&status);
                let landed_id   = e.landed_bundle_id.clone().unwrap_or_else(|| e.bundle_id.clone());
                // Lane drives every label: a priority-fee submission is a single transaction
                // sent over standard RPC, not a Jito bundle — so "bundle"/"tip"/"Jito Explorer"
                // all become "transaction"/"priority fee"/"Solscan".
                let is_priority = e.lane == "PriorityFee";
                let reasoning_label = if is_priority { "Fee reasoning" } else { "Tip reasoning" };
                let solscan_url = format!("https://solscan.io/tx/{}", landed_id);
                let explorer_url = if is_priority {
                    solscan_url.clone()
                } else {
                    format!("https://explorer.jito.wtf/bundle/{}", landed_id)
                };
                let explorer_label = if is_priority {
                    "Solscan — transaction detail ↗"
                } else {
                    "Jito Explorer — regional auction playback ↗"
                };
                let explorer_caption = if is_priority {
                    "The timeline and event stream above are Styx's own view, updated live off the \
                     Yellowstone gRPC feed. This was sent over standard RPC with a priority fee — \
                     Solscan shows the on-chain execution, compute units, and fee paid."
                } else {
                    "The timeline and event stream above are Styx's own view, updated live off the \
                     Yellowstone gRPC feed. Jito's explorer adds the cross-region auction swimlane \
                     (proprietary block-engine data) for the bundle that landed."
                };
                let short_id    = format!("{}…", e.bundle_id.chars().take(20).collect::<String>());
                // The actual tip that landed (escalated on retry) vs the AI's first pick.
                let landed_tip  = e.landed_tip_lamports.unwrap_or(e.tip_lamports);
                let was_repriced = e.landed_tip_lamports.map(|t| t != e.tip_lamports).unwrap_or(false);
                // Fee label: priority-fee lane never reprices, so it's always "Priority fee".
                let fee_label   = if is_priority { "Priority fee" }
                                  else if was_repriced { "Tip landed" } else { "Tip paid" };
                // Lineage: the bundle that landed has a different Jito ID when a retry was needed.
                let landed_differs = e.landed_bundle_id.as_deref()
                    .map(|l| l != e.bundle_id).unwrap_or(false);
                let landed_short = format!("{}…", landed_id.chars().take(16).collect::<String>());
                let initial_tip_sol = lamports_to_sol(e.tip_lamports);
                let tip_sol     = lamports_to_sol(landed_tip);   // hero shows the amount actually paid
                let base_sol    = lamports_to_sol(e.baseline_tip_lamports);
                let saved_sol   = delta_to_sol(e.delta_lamports);
                let saved_pos   = e.delta_lamports >= 0;
                let slot_str    = e.landing_slot.map(|s| format!("{}", s)).unwrap_or_else(|| "—".to_string());
                let regime      = e.regime.clone();
                let regime_cls  = regime_badge_class(&regime);
                let retries     = e.retry_count;
                let reasoning   = e.reasoning.clone();

                view! {
                    <div class="w-full space-y-4">

                        // ══ HEADER ════════════════════════════════════════
                        <div class="w-full rounded-xl border border-[#2a2a2a] bg-[#181818] p-6">
                            <div class="flex items-start justify-between flex-wrap gap-4">
                                <div class="flex-1 min-w-0">
                                    <div class="flex items-center gap-2 mb-2 flex-wrap">
                                        <span class=format!(
                                            "inline-flex items-center rounded border px-2 py-0.5 text-[11px] font-bold font-mono tracking-wide {}",
                                            status_cls
                                        )>{status.clone()}</span>
                                        <span class=format!(
                                            "inline-flex items-center rounded border px-2 py-0.5 text-[11px] font-semibold {}",
                                            regime_cls
                                        )>{regime_human(&regime)}</span>
                                        {(retries > 0).then(|| view! {
                                            <span class="inline-flex items-center rounded border border-orange-800 bg-orange-950/50 px-2 py-0.5 text-[11px] font-mono text-orange-400">
                                                {if retries == 1 { "1 retry".to_string() } else { format!("{} retries", retries) }}
                                            </span>
                                        })}
                                        <span class="inline-flex items-center rounded border border-[#2e2e2e] bg-[#222] px-2 py-0.5 text-[11px] font-mono text-zinc-500">
                                            {if is_priority { "Priority Fee" } else { "Jito Bundle" }}
                                        </span>
                                    </div>
                                    <p class="font-mono text-base text-zinc-200 break-all">{e.bundle_id.clone()}</p>
                                    <p class="text-xs text-zinc-600 font-mono mt-1">
                                        {format_utc_hms(e.submitted_at_ms)}" UTC"
                                    </p>
                                </div>
                                <div class="text-right shrink-0">
                                    <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600 mb-1">
                                        {fee_label}
                                    </p>
                                    <p class="font-mono text-3xl font-bold text-amber-400">{tip_sol.clone()}</p>
                                    {was_repriced.then(|| view! {
                                        <p class="text-[11px] text-zinc-600 mt-0.5 font-mono">
                                            "was "{initial_tip_sol.clone()}
                                        </p>
                                    })}
                                    <p class="text-[11px] font-mono text-zinc-600 mt-1">
                                        "slot #"{slot_str.clone()}
                                    </p>
                                </div>
                            </div>
                        </div>

                        // ══ METADATA GRID (2-col Jito-style) ═════════════
                        <div class="w-full grid grid-cols-2 divide-x divide-y divide-[#1e1e1e] rounded-xl border border-[#2a2a2a] bg-[#181818] overflow-hidden">
                            // Row 1: Timestamp | Slot
                            <div class="px-6 py-4">
                                <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600 mb-1">"Timestamp"</p>
                                <p class="text-sm font-mono text-zinc-300">{format_utc_hms(e.submitted_at_ms)}" UTC"</p>
                            </div>
                            <div class="px-6 py-4">
                                <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600 mb-1">"Slot"</p>
                                <p class="text-sm font-mono text-zinc-300">
                                    {e.landing_slot.map(|s| format!("{}", s)).unwrap_or_else(|| "—".to_string())}
                                </p>
                            </div>

                            // Row 2: Fee | Saved vs market
                            <div class="px-6 py-4 border-amber-900/20">
                                <p class="text-[10px] font-mono uppercase tracking-widest text-amber-800 mb-1">
                                    {fee_label}
                                </p>
                                <p class="text-lg font-bold font-mono text-amber-400">{tip_sol.clone()}</p>
                                {was_repriced.then(|| view! {
                                    <p class="text-[11px] font-mono text-zinc-600 mt-0.5">"escalated from "{initial_tip_sol.clone()}</p>
                                })}
                            </div>
                            <div class="px-6 py-4">
                                <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600 mb-1">"Saved vs. market"</p>
                                <p class=format!("text-lg font-bold font-mono {}", if saved_pos { "text-emerald-400" } else { "text-zinc-400" })>
                                    {saved_sol}
                                </p>
                                <p class="text-[11px] font-mono text-zinc-600 mt-0.5">"market: "{base_sol}</p>
                            </div>

                            // Row 3: Network | AI strategy
                            <div class="px-6 py-4">
                                <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600 mb-2">"Network"</p>
                                <span class=format!("inline-flex items-center rounded border px-2 py-0.5 text-xs font-semibold mb-1.5 {}", regime_cls)>
                                    {regime_human(&regime)}
                                </span>
                                <p class="text-[11px] text-zinc-600">{regime_description_short(&regime)}</p>
                            </div>
                            <div class="px-6 py-4">
                                <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600 mb-2">"AI strategy"</p>
                                <p class="text-sm font-semibold text-zinc-200 mb-1.5">{bid_level_label(e.forward_multiplier)}</p>
                                <div class="flex items-center gap-2">
                                    <span class="text-[12px] font-mono text-amber-400 tracking-widest">{confidence_dots(e.confidence)}</span>
                                    <span class="text-[11px] text-zinc-600">{confidence_label(e.confidence)}</span>
                                </div>
                            </div>

                            // Row 4: Attempts | AI reasoning
                            <div class="px-6 py-4">
                                <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600 mb-1">"Attempts"</p>
                                <p class="text-sm font-mono text-zinc-300">
                                    {if retries == 0 { "1 — landed first try".to_string() }
                                     else { format!("{} — auto-retried by AI", retries + 1) }}
                                </p>
                            </div>
                            <div class="px-6 py-4">
                                <p class="text-[10px] font-mono uppercase tracking-widest text-amber-800 mb-1">
                                    {reasoning_label}
                                </p>
                                <p class="text-xs text-zinc-500 leading-relaxed">{reasoning}</p>
                            </div>

                            // Row 5 (conditional): Failure reason | Landed bundle ID
                            {e.failure_kind.as_ref().map(|fk| view! {
                                <div class="px-6 py-4 col-span-2">
                                    <p class="text-[10px] font-mono uppercase tracking-widest text-red-800 mb-1">"Failure reason"</p>
                                    <p class="text-sm text-red-400">{humanize_failure(fk)}</p>
                                </div>
                            })}

                            {landed_differs.then(|| view! {
                                <div class="px-6 py-4 col-span-2 bg-amber-950/10 border-t border-amber-900/20">
                                    <p class="text-[10px] font-mono uppercase tracking-widest text-amber-800 mb-1">
                                        "Re-priced — landed with a new bundle ID"
                                    </p>
                                    <div class="flex items-center gap-2 text-[11px] font-mono flex-wrap">
                                        <span class="text-zinc-600">"submitted"</span>
                                        <span class="text-zinc-500 break-all">{short_id.clone()}</span>
                                        <span class="text-zinc-700">"→"</span>
                                        <a href=explorer_url.clone() target="_blank"
                                           class="text-amber-500 hover:text-amber-300 transition-colors">
                                            "landed "{landed_short.clone()}" ↗"
                                        </a>
                                    </div>
                                </div>
                            })}
                        </div>

                        // ══ LIFECYCLE ════════════════════════════════════
                        <div class="w-full rounded-xl border border-[#2a2a2a] bg-[#181818] overflow-hidden">
                            <div class="px-6 py-3 border-b border-[#222] bg-[#1e1e1e] flex items-center gap-2.5">
                                <span class="h-1.5 w-1.5 rounded-full bg-amber-400 animate-pulse"></span>
                                <span class="text-[10px] font-mono uppercase tracking-widest text-zinc-600">
                                    "Lifecycle · live from Yellowstone"
                                </span>
                            </div>
                            <div class="grid grid-cols-2 gap-0">
                                <div class="p-6 border-r border-[#222]">
                                    <Stepper
                                        submitted=e.submitted_at_ms
                                        processed=e.processed_at_ms
                                        confirmed=e.confirmed_at_ms
                                        finalized=e.finalized_at_ms
                                        failure=e.failure_kind.clone()
                                    />
                                </div>
                                <div class="p-6">
                                    <LatencyTimeline
                                        submitted=e.submitted_at_ms
                                        processed=e.processed_at_ms
                                        confirmed=e.confirmed_at_ms
                                        finalized=e.finalized_at_ms
                                    />
                                </div>
                            </div>
                        </div>

                        // ══ AI TRANSACTION ANALYSIS ══════════════════════
                        {move || {
                            let loading = sum_loading.get();
                            let sum = summary.get();
                            let bid = id_for_summary.clone();

                            if let Some(s) = sum {
                                let verdict_cls = match s.verdict.as_str() {
                                    "Success" | "Confirmed" | "Finalized" =>
                                        "text-emerald-400 border-emerald-700 bg-emerald-950/60",
                                    "Failed" => "text-red-400 border-red-700 bg-red-950/60",
                                    _ => "text-zinc-300 border-zinc-600 bg-zinc-800",
                                };
                                return view! {
                                    <div class="w-full rounded-xl border border-amber-900/30 bg-[#111008] overflow-hidden">
                                        <div class="px-6 py-3 border-b border-amber-900/20 bg-amber-950/20 flex items-center justify-between">
                                            <div class="flex items-center gap-2.5">
                                                <div class="h-5 w-5 rounded bg-amber-500/20 border border-amber-700/40 flex items-center justify-center">
                                                    <span class="text-[10px] text-amber-400">"⬡"</span>
                                                </div>
                                                <span class="text-[10px] font-mono uppercase tracking-widest text-amber-700">
                                                    "AI Transaction Analysis"
                                                </span>
                                            </div>
                                            <span class=format!("inline-flex items-center rounded border px-2.5 py-0.5 text-[11px] font-bold font-mono {}", verdict_cls)>
                                                {s.verdict.clone()}
                                            </span>
                                        </div>
                                        <div class="p-6 space-y-0">
                                            {(!s.error.is_empty()).then(|| view! {
                                                <p class="text-sm text-red-400 mb-4">{s.error.clone()}</p>
                                            })}
                                            {(!s.transaction_analysis.is_empty()).then(|| view! {
                                                <div class="pb-5 border-b border-[#1e1a10]">
                                                    <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600 mb-2">
                                                        "Transaction analysis"
                                                    </p>
                                                    <p class="text-sm font-semibold text-zinc-200 leading-relaxed">
                                                        {s.transaction_analysis.clone()}
                                                    </p>
                                                </div>
                                            })}
                                            <div class="grid grid-cols-2 gap-0 divide-x divide-[#1e1a10]">
                                                <div class="pr-6 pt-5 space-y-5">
                                                    {(!s.what_happened.is_empty()).then(|| view! {
                                                        <div>
                                                            <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600 mb-1.5">"What happened"</p>
                                                            <p class="text-sm text-zinc-300 leading-relaxed">{s.what_happened.clone()}</p>
                                                        </div>
                                                    })}
                                                    {(!s.fee_analysis.is_empty()).then(|| view! {
                                                        <div>
                                                            <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600 mb-1.5">"Fee analysis"</p>
                                                            <p class="text-sm text-zinc-400 leading-relaxed">{s.fee_analysis.clone()}</p>
                                                        </div>
                                                    })}
                                                </div>
                                                <div class="pl-6 pt-5 space-y-5">
                                                    {(!s.performance.is_empty()).then(|| view! {
                                                        <div>
                                                            <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600 mb-1.5">"Styx performance"</p>
                                                            <p class="text-sm text-zinc-400 leading-relaxed">{s.performance.clone()}</p>
                                                        </div>
                                                    })}
                                                    {(!s.timing.is_empty()).then(|| view! {
                                                        <div>
                                                            <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600 mb-1.5">"Timing"</p>
                                                            <p class="text-sm text-zinc-400 leading-relaxed">{s.timing.clone()}</p>
                                                        </div>
                                                    })}
                                                </div>
                                            </div>
                                        </div>
                                    </div>
                                }.into_any();
                            }

                            view! {
                                <div class="w-full rounded-xl border border-dashed border-amber-900/30 bg-[#0f0d08] p-6">
                                    <div class="flex items-center gap-5">
                                        <div class="h-10 w-10 rounded-lg border border-amber-800/40 bg-amber-950/30 flex items-center justify-center shrink-0">
                                            <span class="text-xl text-amber-500">"⬡"</span>
                                        </div>
                                        <div class="flex-1">
                                            <p class="text-sm font-semibold text-zinc-300 mb-0.5">"AI Transaction Analysis"</p>
                                            <p class="text-xs text-zinc-600 leading-relaxed">
                                                "Describe what this transaction did, whether the bid was optimal, \
                                                 Styx performance, and a timing breakdown."
                                            </p>
                                        </div>
                                        <button
                                            disabled=loading
                                            class=move || format!(
                                                "shrink-0 inline-flex items-center gap-2 rounded-lg border px-5 py-2.5 text-xs font-semibold \
                                                 font-mono transition-all {}",
                                                if loading {
                                                    "border-zinc-700 bg-zinc-800/50 text-zinc-600 cursor-not-allowed"
                                                } else {
                                                    "border-amber-700 bg-amber-950/50 text-amber-300 \
                                                     hover:bg-amber-900/50 hover:border-amber-500 cursor-pointer shadow-amber-950/50 shadow-lg"
                                                }
                                            )
                                            on:click=move |_| {
                                                if !sum_loading.get_untracked() {
                                                    let bid2 = bid.clone();
                                                    set_sum_loading.set(true);
                                                    spawn_local(async move {
                                                        match fetch_summary(&bid2).await {
                                                            Ok(s)  => set_summary.set(Some(s)),
                                                            Err(e) => set_summary.set(Some(AiSummary {
                                                                error: e, ..Default::default()
                                                            })),
                                                        }
                                                        set_sum_loading.set(false);
                                                    });
                                                }
                                            }
                                        >
                                            {move || if sum_loading.get() { "Analysing…" } else { "Generate analysis" }}
                                        </button>
                                    </div>
                                </div>
                            }.into_any()
                        }}

                        // ══ LIVE EVENT STREAM ════════════════════════════
                        {move || {
                            let evs = events.get();
                            if evs.is_empty() {
                                return view! {
                                    <div class="w-full rounded-xl border border-dashed border-[#252525] bg-[#141414] px-8 py-10 text-center">
                                        <p class="text-[11px] font-mono text-zinc-700 uppercase tracking-widest">
                                            "Awaiting events · Styx streams execution activity here in real time"
                                        </p>
                                    </div>
                                }.into_any();
                            }
                            let had_retry = evs.iter().any(|e|
                                matches!(e.stage.as_str(), "repriced" | "resubmitted" | "retrying"));
                            view! {
                                <div class="w-full space-y-3">
                                    // Retry banner
                                    {had_retry.then(|| view! {
                                        <div class="w-full rounded-xl border border-orange-700/50 bg-gradient-to-r from-orange-950/50 via-amber-950/30 to-[#181818] px-6 py-4 flex items-start gap-4">
                                            <div class="shrink-0 h-9 w-9 rounded-lg border border-orange-700/60 bg-orange-950/60 flex items-center justify-center">
                                                <span class="text-lg text-orange-400">"⟳"</span>
                                            </div>
                                            <div>
                                                <p class="text-sm font-bold text-orange-300 tracking-wide uppercase mb-1">
                                                    "Styx engaged autonomous recovery"
                                                </p>
                                                <p class="text-xs text-orange-200/60 leading-relaxed">
                                                    "Bundle didn't land · AI re-priced the tip · fresh blockhash signed · resubmitted autonomously"
                                                </p>
                                            </div>
                                        </div>
                                    })}

                                    // Terminal panel
                                    <div class="w-full rounded-xl border border-[#1e1e1e] bg-[#0e0e0e] overflow-hidden shadow-2xl">
                                        // Terminal title bar
                                        <div class="px-4 py-2.5 border-b border-[#1a1a1a] bg-[#141414] flex items-center gap-3">
                                            <div class="flex gap-1.5">
                                                <span class="h-2.5 w-2.5 rounded-full bg-red-500/70"></span>
                                                <span class="h-2.5 w-2.5 rounded-full bg-amber-500/70"></span>
                                                <span class="h-2.5 w-2.5 rounded-full bg-emerald-500/70"></span>
                                            </div>
                                            <span class="text-[11px] font-mono text-zinc-600 flex-1 text-center">
                                                "styx · execution log · "{e.bundle_id.chars().take(24).collect::<String>()}"…"
                                            </span>
                                            <div class="flex items-center gap-1.5">
                                                <span class="h-1.5 w-1.5 rounded-full bg-emerald-400 animate-pulse"></span>
                                                <span class="text-[10px] font-mono text-emerald-600">"live"</span>
                                            </div>
                                        </div>
                                        // Event rows
                                        <div class="divide-y divide-[#141414]">
                                            {evs.into_iter().rev().map(|s| {
                                                let time = format_utc_hms(s.ts_ms);

                                                // ── AI decision card ──
                                                if s.stage == "ai_decision" || s.stage == "ai_retry_decision" {
                                                    let tip = if s.tip_lamports > 0 { lamports_to_sol(s.tip_lamports) } else { String::new() };
                                                    let is_retry = s.stage == "ai_retry_decision";
                                                    return view! {
                                                        <div class="px-5 py-4 bg-gradient-to-r from-amber-950/40 to-transparent border-l-2 border-amber-500">
                                                            <div class="flex items-center gap-2.5 mb-2">
                                                                <div class="h-5 w-5 rounded bg-amber-500/20 border border-amber-700/50 flex items-center justify-center shrink-0">
                                                                    <span class="text-[10px] text-amber-400">"⬡"</span>
                                                                </div>
                                                                <span class="text-[10px] font-bold uppercase tracking-widest text-amber-500">
                                                                    {if is_retry { "AI recovery decision" } else { "AI tip decision" }}
                                                                </span>
                                                                <span class="ml-auto text-[10px] font-mono text-zinc-700">{time}</span>
                                                            </div>
                                                            <p class="text-xs text-amber-100/70 leading-relaxed pl-7">{s.message}</p>
                                                            {(!tip.is_empty()).then(|| view! {
                                                                <p class="text-[11px] font-mono text-amber-400 mt-1.5 pl-7">
                                                                    "→ fee set to "{tip}
                                                                </p>
                                                            })}
                                                        </div>
                                                    }.into_any();
                                                }

                                                // ── Raw log line ──
                                                if s.stage == "log" {
                                                    let (lvl_cls, txt_cls) = log_level_colors(&s.level);
                                                    return view! {
                                                        <div class="flex items-baseline gap-3 px-5 py-1 font-mono hover:bg-[#121212] transition-colors">
                                                            <span class="text-[10px] text-zinc-800 shrink-0 w-16 text-right">{time}</span>
                                                            <span class=format!("text-[10px] font-bold uppercase shrink-0 w-9 {}", lvl_cls)>
                                                                {s.level.chars().take(4).collect::<String>()}
                                                            </span>
                                                            <span class="text-[10px] text-zinc-700 shrink-0 w-28 truncate">{s.target.clone()}</span>
                                                            <span class=format!("text-[11px] flex-1 break-all leading-relaxed {}", txt_cls)>
                                                                {s.message}
                                                            </span>
                                                        </div>
                                                    }.into_any();
                                                }

                                                // ── Stage event row ──
                                                let (dot_color, label_cls) = stage_colors(&s.stage);
                                                let loud = matches!(s.stage.as_str(), "retrying" | "repriced" | "resubmitted");
                                                let fault = s.stage == "fault_injected";
                                                let row_bg = if fault { "px-5 py-3 flex items-center gap-3 bg-red-950/20 border-l-2 border-red-500 hover:bg-red-950/30 transition-colors" }
                                                    else if loud { "px-5 py-3 flex items-center gap-3 bg-orange-950/15 border-l-2 border-orange-600 hover:bg-orange-950/25 transition-colors" }
                                                    else { "px-5 py-3 flex items-center gap-3 border-l-2 border-transparent hover:bg-[#111] transition-colors" };
                                                let tip_str = if s.tip_lamports > 0 { lamports_to_sol(s.tip_lamports) } else { String::new() };
                                                let retry_str = if s.retry > 0 { format!("#{}", s.retry + 1) } else { String::new() };
                                                let stage_lbl = stage_human_label(&s.stage);
                                                view! {
                                                    <div class=row_bg>
                                                        <span class=format!("h-2 w-2 rounded-full shrink-0 {}", dot_color)></span>
                                                        <span class=format!("text-[11px] font-semibold font-mono w-28 shrink-0 {}", label_cls)>
                                                            {stage_lbl}
                                                        </span>
                                                        <span class=format!("text-[11px] flex-1 {}", if loud || fault { "text-orange-200/80 font-medium" } else { "text-zinc-500" })>
                                                            {s.message}
                                                        </span>
                                                        {(!retry_str.is_empty()).then(|| view! {
                                                            <span class="text-[10px] font-mono text-orange-500/80 shrink-0 mr-1">{retry_str}</span>
                                                        })}
                                                        {(!tip_str.is_empty()).then(|| view! {
                                                            <span class="text-[11px] font-mono text-amber-500 shrink-0 w-24 text-right">{tip_str}</span>
                                                        })}
                                                        <span class="text-[10px] font-mono text-zinc-800 shrink-0 w-16 text-right">{time}</span>
                                                    </div>
                                                }.into_any()
                                            }).collect::<Vec<_>>()}
                                        </div>
                                    </div>
                                </div>
                            }.into_any()
                        }}

                        // ══ FOOTER LINKS ══════════════════════════════════
                        <div class="w-full flex flex-wrap gap-3 pt-2">
                            <a href=explorer_url target="_blank"
                               class="inline-flex items-center gap-2 rounded-lg border border-[#2a2a2a] bg-[#181818]
                                      px-4 py-2.5 text-xs font-mono text-amber-600 hover:text-amber-400
                                      hover:border-amber-800/60 transition-all">
                                {explorer_label}
                            </a>
                            {move || if !is_priority {
                                Some(view! {
                                    <a href=solscan_url.clone() target="_blank"
                                       class="inline-flex items-center gap-2 rounded-lg border border-[#2a2a2a] bg-[#181818]
                                              px-4 py-2.5 text-xs font-mono text-zinc-500 hover:text-zinc-300
                                              hover:border-zinc-600/50 transition-all">
                                        "Solscan — on-chain detail ↗"
                                    </a>
                                })
                            } else {
                                None
                            }}
                        </div>
                        <p class="text-[10px] text-zinc-700 leading-relaxed pb-4">
                            {explorer_caption}
                        </p>

                    </div>
                }.into_any()
            }}

        </div>
    }
}

// ── Stage-latency timeline ─────────────────────────────────────────────────
// Visualises the time between commitment levels. The width of each segment is
// proportional to its latency, so the slowest stage is obvious at a glance.
// The processed→confirmed delta is the clearest read on cluster vote health.
#[component]
fn LatencyTimeline(
    submitted: u64,
    processed: Option<u64>,
    confirmed: Option<u64>,
    finalized: Option<u64>,
) -> impl IntoView {
    // Reached stages, in order, with absolute ms + bar colour.
    let mut stages: Vec<(&'static str, u64, &'static str)> =
        vec![("Submitted", submitted, "bg-blue-400")];
    if let Some(p) = processed { stages.push(("Processed", p, "bg-cyan-400")); }
    if let Some(c) = confirmed { stages.push(("Confirmed", c, "bg-amber-400")); }
    if let Some(f) = finalized { stages.push(("Finalized", f, "bg-emerald-400")); }

    let last_label = stages.last().map(|s| s.0).unwrap_or("Submitted");
    let total = stages.last().map(|l| l.1.saturating_sub(submitted)).unwrap_or(0).max(1);

    // (stage-reached label, delta ms, width %, colour) for each hop.
    let segments: Vec<(&'static str, u64, f64, &'static str)> = stages.windows(2).map(|w| {
        let delta = w[1].1.saturating_sub(w[0].1);
        let pct = (delta as f64 / total as f64 * 100.0).max(4.0);
        (w[1].0, delta, pct, w[1].2)
    }).collect();
    let only_submitted = segments.is_empty();

    // processed → confirmed = how fast the cluster is voting on the landing slot.
    let proc_to_conf = match (processed, confirmed) {
        (Some(p), Some(c)) => Some(c.saturating_sub(p)),
        _ => None,
    };
    let health = proc_to_conf.map(|d| if d < 800 {
        ("healthy — fast vote propagation", "text-emerald-400")
    } else if d < 2_000 {
        ("normal", "text-amber-400")
    } else {
        ("congested — slow voting", "text-red-400")
    });

    view! {
        <div class="space-y-4">
            <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Stage latency"</p>
            {if only_submitted {
                view! {
                    <p class="text-xs text-zinc-700">
                        "Waiting for first on-chain commitment…"
                    </p>
                }.into_any()
            } else {
                view! {
                    <div class="flex items-stretch gap-1.5">
                        {segments.into_iter().map(|(label, delta, pct, color)| view! {
                            <div class="flex flex-col gap-1.5" style=format!("width:{:.1}%", pct)>
                                <div class="flex items-center justify-between gap-1">
                                    <span class="text-[10px] text-zinc-600 truncate">{label}</span>
                                    <span class="text-[10px] font-mono text-amber-500 shrink-0">{format!("+{}ms", delta)}</span>
                                </div>
                                <div class=format!("h-2 rounded-full {}", color)></div>
                            </div>
                        }).collect_view()}
                    </div>
                    <div class="space-y-1.5 pt-1">
                        <p class="text-xs text-zinc-600">
                            "Total submit→"{last_label}": "
                            <span class="font-mono text-zinc-300">{format!("{} ms", total)}</span>
                        </p>
                        {health.map(|(txt, cls)| {
                            let d = proc_to_conf.unwrap_or(0);
                            view! {
                                <p class="text-xs text-zinc-600">
                                    "P→C: "
                                    <span class="font-mono text-zinc-300">{format!("{} ms", d)}</span>
                                    " · "<span class=cls>{txt}</span>
                                </p>
                            }
                        })}
                    </div>
                }.into_any()
            }}
        </div>
    }
}

// ── Lifecycle stepper ──────────────────────────────────────────────────────

#[component]
fn Stepper(
    submitted: u64,
    processed: Option<u64>,
    confirmed: Option<u64>,
    finalized: Option<u64>,
    failure: Option<String>,
) -> impl IntoView {
    let steps = vec![
        ("Submitted", Some(submitted)),
        ("Processed", processed),
        ("Confirmed", confirmed),
        ("Finalized", finalized),
    ];
    let n = steps.len();
    view! {
        <div class="space-y-0">
            {steps.into_iter().enumerate().map(|(i, (label, ts))| {
                let reached = ts.is_some();
                let last = i == n - 1;
                let time = ts.map(format_utc_hms).unwrap_or_else(|| "—".to_string());
                let latency = match ts {
                    Some(t) if t >= submitted => {
                        let d = t - submitted;
                        if d == 0 { String::new() } else { format!("+{} ms", d) }
                    }
                    _ => String::new(),
                };
                let (dot_cls, line_cls, label_cls) = if reached {
                    ("border-amber-500 bg-amber-500", "bg-amber-700/50", "text-zinc-200")
                } else {
                    ("border-zinc-700 bg-transparent", "bg-[#2a2a2a]", "text-zinc-600")
                };
                view! {
                    <div class="flex gap-4">
                        <div class="flex flex-col items-center">
                            <div class=format!("h-4 w-4 shrink-0 rounded-full border-2 {}", dot_cls)></div>
                            {(!last).then(|| view! {
                                <div class=format!("w-px flex-1 my-1 min-h-[28px] {}", line_cls)></div>
                            })}
                        </div>
                        <div class="pb-5 flex-1">
                            <div class="flex items-center justify-between">
                                <p class=format!("text-sm font-semibold {}", label_cls)>{label}</p>
                                <span class="text-[11px] font-mono text-zinc-700">{latency}</span>
                            </div>
                            <p class="text-xs font-mono text-zinc-600 mt-0.5">{time}" UTC"</p>
                        </div>
                    </div>
                }
            }).collect::<Vec<_>>()}

            {failure.map(|fk| view! {
                <div class="flex gap-4">
                    <div class="flex flex-col items-center">
                        <div class="h-4 w-4 shrink-0 rounded-full border-2 border-red-500 bg-red-500"></div>
                    </div>
                    <div class="flex-1">
                        <p class="text-sm font-semibold text-red-400">"Didn't land"</p>
                        <p class="text-xs text-red-500/80 mt-0.5">{humanize_failure(&fk)}</p>
                    </div>
                </div>
            })}
        </div>
    }
}

#[component]
fn Cell(label: &'static str, value: String, accent: bool) -> impl IntoView {
    let cls = if accent { "text-sm font-mono font-semibold text-amber-400" }
              else { "text-sm font-mono text-zinc-300" };
    view! {
        <div class="bg-[#222] px-5 py-4">
            <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-700">{label}</p>
            <p class=format!("mt-1 {}", cls)>{value}</p>
        </div>
    }
}

// ── Shared helpers ──────────────────────────────────────────────────────────

// (level-tag colour, message colour) for a bridged log line.
fn log_level_colors(level: &str) -> (&'static str, &'static str) {
    match level {
        "ERROR" => ("text-red-400",    "text-red-300/90"),
        "WARN"  => ("text-amber-400",  "text-amber-200/90"),
        "INFO"  => ("text-sky-500",    "text-zinc-400"),
        "DEBUG" => ("text-zinc-600",   "text-zinc-600"),
        _       => ("text-zinc-600",   "text-zinc-500"),
    }
}

fn stage_colors(stage: &str) -> (&'static str, &'static str) {
    match stage {
        "ai_decision" => ("bg-amber-400",  "text-amber-300"),
        "ai_retry_decision" => ("bg-amber-400", "text-amber-300"),
        "fault_injected" => ("bg-red-400", "text-red-300"),
        "submitted" => ("bg-blue-400",   "text-blue-300"),
        "leader_window" => ("bg-sky-400", "text-sky-300"),
        "processed" => ("bg-cyan-400",   "text-cyan-300"),
        "retrying"  => ("bg-orange-400", "text-orange-300"),
        "repriced"  => ("bg-amber-400",  "text-amber-300"),
        "resubmitted" => ("bg-orange-400", "text-orange-300"),
        "confirmed" => ("bg-green-400",  "text-green-300"),
        "finalized" => ("bg-emerald-400","text-emerald-300"),
        "exhausted" | "terminal" => ("bg-red-400", "text-red-300"),
        _           => ("bg-zinc-500",   "text-zinc-300"),
    }
}

fn stage_human_label(stage: &str) -> &'static str {
    match stage {
        "submitted"   => "Sent to network",
        "leader_window" => "Leader window",
        "fault_injected" => "Fault injected",
        "ai_retry_decision" => "AI recovery",
        "processed"   => "Picked up",
        "retrying"    => "Auto-retrying",
        "repriced"    => "Fee raised",
        "resubmitted" => "Resubmitted",
        "confirmed"   => "Confirmed ✓",
        "finalized"   => "Finalized ✓",
        "exhausted"   => "All retries used",
        "terminal"    => "Failed",
        _             => "—",
    }
}

fn derive_status(e: &LogEntry) -> String {
    if e.finalized_at_ms.is_some() { "Finalized".to_string() }
    else if e.confirmed_at_ms.is_some() { "Confirmed".to_string() }
    else if e.processed_at_ms.is_some() { "Processed".to_string() }
    else if e.failure_kind.is_some() { "Failed".to_string() }
    else { "Submitted".to_string() }
}

fn status_class(status: &str) -> &'static str {
    match status {
        "Finalized" => "border-emerald-800 bg-emerald-950 text-emerald-400",
        "Confirmed" => "border-green-800 bg-green-950 text-green-400",
        "Processed" => "border-cyan-800 bg-cyan-950 text-cyan-400",
        "Failed"    => "border-red-800 bg-red-950 text-red-400",
        _           => "border-zinc-700 bg-zinc-800 text-zinc-400",
    }
}

fn regime_badge_class(regime: &str) -> &'static str {
    match regime {
        "Cold"  => "border-zinc-700 bg-zinc-800 text-zinc-300",
        "Warm"  => "border-amber-800 bg-amber-950 text-amber-300",
        "Hot"   => "border-orange-800 bg-orange-950 text-orange-300",
        "Manic" => "border-red-800 bg-red-950 text-red-400",
        _       => "border-zinc-700 bg-zinc-800 text-zinc-400",
    }
}

fn regime_human(regime: &str) -> &'static str {
    match regime {
        "Cold"  => "Quiet network",
        "Warm"  => "Normal traffic",
        "Hot"   => "Heavy competition",
        "Manic" => "Extreme congestion",
        _       => "Unknown conditions",
    }
}

fn regime_text_class(regime: &str) -> &'static str {
    match regime {
        "Cold"  => "text-zinc-300",
        "Warm"  => "text-amber-300",
        "Hot"   => "text-orange-300",
        "Manic" => "text-red-400",
        _       => "text-zinc-400",
    }
}

fn regime_description(regime: &str) -> &'static str {
    match regime {
        "Cold"  => "Very few validators competing. Low inclusion fees — a conservative bid wins easily.",
        "Warm"  => "Moderate activity on-chain. Normal fees apply and most bids land within a block or two.",
        "Hot"   => "High validator competition. Styx needs to bid more aggressively to secure block inclusion.",
        "Manic" => "Extreme congestion — network is overwhelmed. Maximum bid may still require multiple retries.",
        _       => "Network conditions could not be determined.",
    }
}

fn regime_description_short(regime: &str) -> &'static str {
    match regime {
        "Cold"  => "Low competition",
        "Warm"  => "Moderate activity",
        "Hot"   => "High congestion",
        "Manic" => "Extreme congestion",
        _       => "Unknown",
    }
}

fn bid_level_label(multiplier: f64) -> &'static str {
    if multiplier < 0.8       { "Below baseline" }
    else if multiplier < 1.2  { "Match clearing price" }
    else if multiplier < 1.8  { "Moderately aggressive" }
    else if multiplier < 2.5  { "Aggressive" }
    else                      { "Maximum pressure" }
}

fn bid_level_desc(pct: f64) -> &'static str {
    if pct < 0.25      { "Bidding below most validators — fine for non-urgent transactions." }
    else if pct < 0.50 { "Bidding around the median — good landing chance within a few blocks." }
    else if pct < 0.75 { "Bidding above most validators — strong chance of landing quickly." }
    else if pct < 0.90 { "Bidding in the top 25% — very high inclusion probability." }
    else               { "Top of the market — virtually guaranteed inclusion." }
}

fn confidence_dots(conf: f64) -> String {
    let filled = (conf * 5.0).round() as usize;
    let empty = 5usize.saturating_sub(filled);
    format!("{}{}", "●".repeat(filled.min(5)), "○".repeat(empty))
}

fn confidence_label(conf: f64) -> &'static str {
    if conf >= 0.80    { "High confidence" }
    else if conf >= 0.55 { "Moderate confidence" }
    else               { "Low confidence" }
}
