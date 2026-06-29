use futures::StreamExt;
use gloo_net::websocket::Message;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{format_utc_hms, post_execute, ws_connect, ws_reconnect_delay, ExecuteResponse, WsEvent};
use crate::components::result_modal::BundleModal;
use crate::utils::{delta_to_sol, lamports_to_sol};

#[derive(Clone, Debug)]
struct FlowStep {
    stage: String,
    message: String,
    tip_lamports: u64,
    retry: u32,
    ts_ms: u64,
}

#[component]
pub fn ExecutePage() -> impl IntoView {
    let (loading, set_loading) = signal(false);
    let (result, set_result)   = signal(None::<ExecuteResponse>);
    let (error, set_error)     = signal(None::<String>);
    let (flow, set_flow)       = signal(Vec::<FlowStep>::new());
    let (mode, set_mode)       = signal("memo".to_string());
    // "priority" = standard RPC + CU price; "jito" = Jito bundle lane.
    // Fault injection is priority-lane only — when switching to jito we auto-reset to memo.
    let (lane, set_lane)       = signal("priority".to_string());

    spawn_local(async move {
        loop {
            let Some(ws) = ws_connect().await else {
                ws_reconnect_delay().await;
                continue;
            };
            let (_, mut read) = ws.split();
            while let Some(Ok(Message::Text(text))) = read.next().await {
                if let Ok(WsEvent::Execution { stage, message, tip_lamports, retry, ts_ms, .. })
                    = serde_json::from_str(&text)
                {
                    set_flow.update(|v| {
                        v.push(FlowStep { stage, message, tip_lamports, retry, ts_ms });
                        if v.len() > 60 { let drop = v.len() - 60; v.drain(0..drop); }
                    });
                }
            }
            ws_reconnect_delay().await;
        }
    });

    let on_execute = move |_| {
        if loading.get() { return; }
        set_loading.set(true);
        set_error.set(None);
        set_result.set(None);
        set_flow.set(Vec::new());
        let m = mode.get();
        let l = lane.get();
        spawn_local(async move {
            let amount = if m == "jupiter" { Some(1_000_000u64) } else { None };
            match post_execute(&m, amount, &l).await {
                Ok(r)  => { set_result.set(Some(r)); }
                Err(e) => { set_error.set(Some(e)); }
            }
            set_loading.set(false);
        });
    };

    view! {
        // Full-width two-column layout: controls left, live feed right
        <div class="w-full space-y-6">

            // ── Page header ──────────────────────────────────────────────────
            <div class="border-b border-[#252525] pb-6">
                <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600 mb-2">
                    "Dashboard / Execute"
                </p>
                <div class="flex flex-col lg:flex-row lg:items-end lg:justify-between gap-4">
                    <div>
                        <h1 class="text-4xl font-bold tracking-tight">"Execute"</h1>
                        <p class="text-sm text-zinc-500 mt-2 max-w-2xl leading-relaxed">
                            "Styx wraps your instructions with a live-priced compute budget and inclusion fee, "
                            "submits via the selected lane, and tracks to finality — auto-retrying with fresh "
                            "pricing on each missed slot."
                        </p>
                    </div>

                    // Lane toggle — prominent in header
                    <div class="flex flex-col gap-1 shrink-0">
                        <span class="text-[10px] font-mono uppercase tracking-widest text-zinc-600">
                            "Submission lane"
                        </span>
                        <div class="inline-flex items-center rounded-xl border border-[#2e2e2e] bg-[#1a1a1a] p-1 gap-1">
                            <button
                                on:click=move |_| set_lane.set("priority".to_string())
                                class=move || format!(
                                    "flex items-center gap-2 px-5 py-2 rounded-lg text-sm font-mono font-medium transition-all duration-150 {}",
                                    if lane.get() == "priority" {
                                        "bg-amber-500 text-black shadow-sm"
                                    } else {
                                        "text-zinc-500 hover:text-zinc-300"
                                    }
                                )
                            >
                                <span class="text-base">"⚡"</span>
                                "Priority Fee"
                            </button>
                            <button
                                on:click=move |_| {
                                    set_lane.set("jito".to_string());
                                    // Fault injection is priority-only — reset to memo on jito
                                    if mode.get() == "fault" { set_mode.set("memo".to_string()); }
                                }
                                class=move || format!(
                                    "flex items-center gap-2 px-5 py-2 rounded-lg text-sm font-mono font-medium transition-all duration-150 {}",
                                    if lane.get() == "jito" {
                                        "bg-amber-500 text-black shadow-sm"
                                    } else {
                                        "text-zinc-500 hover:text-zinc-300"
                                    }
                                )
                            >
                                <span class="text-base">"🎯"</span>
                                "Jito Bundle"
                            </button>
                        </div>
                    </div>
                </div>
            </div>

            // ── Main grid: controls | live feed ─────────────────────────────
            <div class="grid grid-cols-1 lg:grid-cols-5 gap-6 items-start">

                // ── LEFT: scenario + execute ─────────────────────────────────
                <div class="lg:col-span-2 space-y-5">

                    // Scenario cards
                    <div class="space-y-2">
                        <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600">
                            "Scenario"
                        </p>
                        <div class="space-y-2">
                            <ScenarioCard
                                selected=Signal::derive(move || mode.get() == "memo")
                                on_select=move |_| set_mode.set("memo".to_string())
                                icon="📝"
                                title="Proof of life"
                                tag="SPL Memo"
                                desc="A minimal on-chain memo. Always lands — the cleanest way to validate fee selection and the full lifecycle without trade risk."
                            />
                            <ScenarioCard
                                selected=Signal::derive(move || mode.get() == "jupiter")
                                on_select=move |_| set_mode.set("jupiter".to_string())
                                icon="🔄"
                                title="Real trade"
                                tag="Jupiter · 0.001 SOL → USDC"
                                desc="A genuine Jupiter-routed swap. Watch the AI bid for priority, routing, and confirmation in real time."
                            />
                            // Fault injection: only shown on priority lane
                            {move || (lane.get() == "priority").then(|| view! {
                                <ScenarioCard
                                    selected=Signal::derive(move || mode.get() == "fault")
                                    on_select=move |_| set_mode.set("fault".to_string())
                                    icon="💥"
                                    title="Fault injection"
                                    tag="Stale blockhash · Priority lane only"
                                    desc="Submits with an expired blockhash on purpose. The agent detects expiry, reasons about cause, refreshes, and resubmits — autonomously."
                                />
                            })}
                        </div>
                    </div>

                    // Execute panel
                    <div class="rounded-2xl border border-[#2e2e2e] bg-[#1e1e1e] overflow-hidden">
                        // Status bar
                        <div class="px-5 py-3 bg-[#252525] border-b border-[#2e2e2e] flex items-center gap-3">
                            {move || {
                                let l = lane.get();
                                let m = mode.get();
                                let (dot, txt) = match (m.as_str(), l.as_str()) {
                                    ("fault", _)            => ("bg-red-400",   "Fault injection · Priority Fee"),
                                    ("jupiter", "priority") => ("bg-blue-400",  "Real trade · Priority Fee"),
                                    ("jupiter", _)          => ("bg-purple-400","Real trade · Jito Bundle"),
                                    (_, "priority")         => ("bg-amber-400", "Proof of life · Priority Fee"),
                                    _                       => ("bg-amber-400", "Proof of life · Jito Bundle"),
                                };
                                view! {
                                    <span class=format!("h-2 w-2 rounded-full shrink-0 {}", dot)></span>
                                    <span class="text-xs font-mono text-zinc-500">{txt}</span>
                                }
                            }}
                        </div>

                        <div class="p-6 space-y-5">
                            // Pipeline description
                            <p class="text-sm text-zinc-400 leading-relaxed">
                                {move || if lane.get() == "priority" {
                                    "Classify contention → size CU price → send via RPC → poll for confirmation"
                                } else {
                                    "Classify contention → size tip → bundle all txs → fan-out to Jito leaders → re-price & retry until landed"
                                }}
                            </p>

                            // Execute button
                            <div class="relative">
                                <div class="absolute inset-0 scale-110 rounded-full
                                            bg-amber-500/10 blur-2xl pointer-events-none"></div>
                                <button
                                    class="relative w-full flex items-center justify-center gap-3 rounded-xl px-6 py-4
                                           bg-amber-500 text-black font-bold text-base
                                           hover:bg-amber-400 disabled:opacity-40 disabled:cursor-not-allowed
                                           active:scale-[0.98] transition-all duration-150"
                                    on:click=on_execute
                                    disabled=move || loading.get()
                                >
                                    {move || if loading.get() {
                                        let lbl = if lane.get() == "priority" { "Sending…" } else { "Submitting bundle…" };
                                        view! {
                                            <span class="h-4 w-4 rounded-full border-2 border-black/30 border-t-black animate-spin"></span>
                                            <span>{lbl}</span>
                                        }.into_any()
                                    } else {
                                        let lbl = match mode.get().as_str() {
                                            "jupiter" => "Execute Jupiter Swap",
                                            "fault"   => "Inject Fault & Recover",
                                            _         => "Execute Memo",
                                        };
                                        view! {
                                            <span class="text-xl">"⚡"</span>
                                            <span>{lbl}</span>
                                        }.into_any()
                                    }}
                                </button>
                            </div>
                        </div>
                    </div>

                    // Error display
                    {move || error.get().map(|e| view! {
                        <div class="rounded-xl border border-red-900/60 bg-red-950/40 px-5 py-4">
                            <p class="text-xs font-mono font-semibold text-red-400 mb-1">"Execution error"</p>
                            <p class="text-sm text-red-300">{e}</p>
                        </div>
                    })}

                    // Result modal (inline below execute on mobile, replaces error slot)
                    {move || result.get().map(|r| {
                        let saved_positive = r.delta_lamports >= 0;
                        let is_priority = r.lane == "PriorityFee";
                        let modal_title = if is_priority { "Transaction submitted" } else { "Bundle submitted" };
                        let lane_display = if is_priority { "Priority Fee".to_string() } else { "Jito Bundle".to_string() };
                        let fee_label = if is_priority { "Priority fee" } else { "Tip paid" };
                        let rows = vec![
                            (fee_label,               lamports_to_sol(r.tip_lamports),          false),
                            ("Market rate (typical)", lamports_to_sol(r.baseline_tip_lamports), false),
                            ("You saved",             delta_to_sol(r.delta_lamports),           saved_positive),
                            ("Lane",                  lane_display,                              false),
                            ("AI certainty",          format!("{:.0}%", r.confidence * 100.0),  false),
                        ];
                        view! {
                            <BundleModal
                                bundle_id=r.bundle_id.clone()
                                title=modal_title.to_string()
                                regime=r.regime.clone()
                                rows=rows
                                note=r.reasoning.clone()
                                on_close=Callback::new(move |_| set_result.set(None))
                            />
                        }
                    })}
                </div>

                // ── RIGHT: live execution feed ───────────────────────────────
                <div class="lg:col-span-3 space-y-4">

                    {move || {
                        let steps = flow.get();
                        let had_retry = steps.iter().any(|s|
                            matches!(s.stage.as_str(), "repriced" | "resubmitted" | "retrying"));

                        if steps.is_empty() {
                            return view! {
                                <div class="rounded-2xl border border-[#2e2e2e] bg-[#1a1a1a] flex flex-col items-center justify-center py-24 gap-4">
                                    <div class="h-12 w-12 rounded-2xl bg-[#252525] border border-[#303030] flex items-center justify-center">
                                        <span class="text-2xl">"⚡"</span>
                                    </div>
                                    <div class="text-center">
                                        <p class="text-sm font-medium text-zinc-400">"Waiting for execution"</p>
                                        <p class="text-xs text-zinc-600 mt-1">"Events stream here live as they happen"</p>
                                    </div>
                                </div>
                            }.into_any();
                        }

                        view! {
                            <div class="space-y-3">
                                // Retry banner
                                {had_retry.then(|| view! {
                                    <div class="rounded-xl border border-orange-700/50 bg-gradient-to-r from-orange-950/50 to-transparent px-5 py-4 flex items-start gap-3">
                                        <span class="text-orange-400 text-lg shrink-0 mt-0.5">"⟳"</span>
                                        <div>
                                            <p class="text-sm font-bold text-orange-300 tracking-wide">"Styx didn't give up"</p>
                                            <p class="text-xs text-orange-200/60 mt-0.5 leading-relaxed">
                                                "Bundle missed — Styx re-priced the tip, re-signed with a fresh blockhash, and resubmitted automatically."
                                            </p>
                                        </div>
                                    </div>
                                })}

                                // Timeline card
                                <div class="rounded-2xl border border-[#2e2e2e] bg-[#1a1a1a] overflow-hidden">
                                    // Header
                                    <div class="px-6 py-3.5 border-b border-[#252525] bg-[#1e1e1e] flex items-center justify-between">
                                        <div class="flex items-center gap-2">
                                            <span class="h-1.5 w-1.5 rounded-full bg-amber-400 animate-pulse"></span>
                                            <span class="text-[10px] font-mono uppercase tracking-widest text-zinc-500">
                                                "Live execution flow"
                                            </span>
                                        </div>
                                        <span class="text-[10px] font-mono text-zinc-700">
                                            {move || format!("{} events", flow.get().len())}
                                        </span>
                                    </div>

                                    // Steps — vertical timeline
                                    <div class="divide-y divide-[#1f1f1f]">
                                        {steps.into_iter().enumerate().map(|(idx, s)| {
                                            let (dot_cls, stage_cls) = match s.stage.as_str() {
                                                "submitted"         => ("bg-blue-400",    "text-blue-300"),
                                                "leader_window"     => ("bg-sky-400",     "text-sky-300"),
                                                "fault_injected"    => ("bg-red-500",     "text-red-300"),
                                                "ai_decision"       => ("bg-amber-400",   "text-amber-300"),
                                                "ai_retry_decision" => ("bg-amber-500",   "text-amber-200"),
                                                "retrying"          => ("bg-orange-400",  "text-orange-300"),
                                                "repriced"          => ("bg-amber-400",   "text-amber-300"),
                                                "resubmitted"       => ("bg-orange-500",  "text-orange-200"),
                                                "processed"         => ("bg-cyan-400",    "text-cyan-300"),
                                                "confirmed"         => ("bg-green-400",   "text-green-300"),
                                                "finalized"         => ("bg-green-500",   "text-green-300"),
                                                "exhausted"         => ("bg-red-400",     "text-red-300"),
                                                "terminal"          => ("bg-red-400",     "text-red-300"),
                                                _                   => ("bg-zinc-600",    "text-zinc-400"),
                                            };
                                            let stage_label = match s.stage.as_str() {
                                                "submitted"         => "Sent",
                                                "leader_window"     => "Leader window",
                                                "fault_injected"    => "Fault injected",
                                                "ai_decision"       => "AI decision",
                                                "ai_retry_decision" => "AI recovery",
                                                "retrying"          => "Auto-retry",
                                                "repriced"          => "Fee re-priced",
                                                "resubmitted"       => "Resubmitted",
                                                "processed"         => "Processed",
                                                "confirmed"         => "Confirmed ✓",
                                                "finalized"         => "Finalized ✓",
                                                "exhausted"         => "Retries exhausted",
                                                "terminal"          => "Failed",
                                                _                   => "—",
                                            };
                                            let is_loud = matches!(s.stage.as_str(),
                                                "retrying" | "repriced" | "resubmitted" |
                                                "fault_injected" | "ai_retry_decision" |
                                                "confirmed" | "finalized");
                                            let is_final = matches!(s.stage.as_str(),
                                                "confirmed" | "finalized");
                                            let is_error = matches!(s.stage.as_str(),
                                                "exhausted" | "terminal" | "fault_injected");

                                            let row_bg = if is_final {
                                                "bg-green-950/20 border-l-2 border-green-500"
                                            } else if is_error {
                                                "bg-red-950/20 border-l-2 border-red-700"
                                            } else if is_loud {
                                                "bg-orange-950/10 border-l-2 border-orange-600"
                                            } else {
                                                "border-l-2 border-transparent"
                                            };

                                            let tip = lamports_to_sol(s.tip_lamports);
                                            let time = format_utc_hms(s.ts_ms);
                                            let retry_lbl = if s.retry > 0 {
                                                format!("attempt {}", s.retry + 1)
                                            } else {
                                                String::new()
                                            };
                                            let _ = idx; // suppress unused warning

                                            view! {
                                                <div class=format!("flex items-start gap-4 px-6 py-3.5 {}", row_bg)>
                                                    // Timeline dot
                                                    <div class="flex flex-col items-center pt-0.5 shrink-0">
                                                        <span class=format!("h-2.5 w-2.5 rounded-full shrink-0 {}", dot_cls)></span>
                                                    </div>

                                                    // Content
                                                    <div class="flex-1 min-w-0 space-y-0.5">
                                                        <div class="flex items-center gap-2 flex-wrap">
                                                            <span class=format!("text-xs font-bold tracking-wide {}", stage_cls)>
                                                                {stage_label}
                                                            </span>
                                                            {(!retry_lbl.is_empty()).then(|| view! {
                                                                <span class="text-[10px] font-mono px-1.5 py-0.5 rounded bg-orange-900/40 text-orange-400 border border-orange-800/40">
                                                                    {retry_lbl}
                                                                </span>
                                                            })}
                                                        </div>
                                                        <p class=format!("text-xs leading-relaxed truncate {}",
                                                            if is_loud { "text-zinc-300" } else { "text-zinc-500" })>
                                                            {s.message}
                                                        </p>
                                                    </div>

                                                    // Right meta
                                                    <div class="flex flex-col items-end gap-0.5 shrink-0">
                                                        <span class="text-[11px] font-mono text-amber-400/70">{tip}</span>
                                                        <span class="text-[10px] font-mono text-zinc-700">{time}</span>
                                                    </div>
                                                </div>
                                            }
                                        }).collect::<Vec<_>>()}
                                    </div>
                                </div>
                            </div>
                        }.into_any()
                    }}
                </div>
            </div>
        </div>
    }
}

#[component]
fn ScenarioCard(
    selected: Signal<bool>,
    on_select: impl Fn(leptos::ev::MouseEvent) + 'static,
    icon: &'static str,
    title: &'static str,
    tag: &'static str,
    desc: &'static str,
) -> impl IntoView {
    view! {
        <button
            on:click=on_select
            class=move || format!(
                "w-full text-left rounded-xl border p-4 transition-all duration-150 {}",
                if selected.get() {
                    "border-amber-600/60 bg-amber-950/30 ring-1 ring-amber-600/30"
                } else {
                    "border-[#2e2e2e] bg-[#1e1e1e] hover:border-zinc-600 hover:bg-[#222]"
                }
            )
        >
            <div class="flex items-start gap-3">
                // Icon + radio
                <div class=move || format!(
                    "h-9 w-9 rounded-lg border flex items-center justify-center text-lg shrink-0 {}",
                    if selected.get() {
                        "border-amber-600/50 bg-amber-950/40"
                    } else {
                        "border-[#2e2e2e] bg-[#252525]"
                    }
                )>
                    {icon}
                </div>

                <div class="flex-1 min-w-0">
                    <div class="flex items-center justify-between gap-2 mb-0.5">
                        <span class=move || format!(
                            "text-sm font-semibold {}",
                            if selected.get() { "text-amber-300" } else { "text-zinc-200" }
                        )>{title}</span>
                        <span class=move || format!(
                            "h-3.5 w-3.5 rounded-full border-2 shrink-0 {}",
                            if selected.get() { "border-amber-400 bg-amber-400" } else { "border-zinc-600" }
                        )></span>
                    </div>
                    <p class="text-[10px] font-mono uppercase tracking-wider text-zinc-600 mb-1">{tag}</p>
                    <p class="text-xs text-zinc-500 leading-relaxed">{desc}</p>
                </div>
            </div>
        </button>
    }
}
