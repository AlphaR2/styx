use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;
use std::time::Duration;

use crate::api::{fetch_log, LogEntry};
use crate::utils::{delta_to_sol, humanize_failure, lamports_to_sol};

#[component]
pub fn LogPage() -> impl IntoView {
    let (entries, set_entries) = signal(Vec::<LogEntry>::new());
    let (loading, set_loading) = signal(true);
    let (error, set_error)     = signal(None::<String>);

    // Programmatic router navigation — makes the whole table row reliably clickable.
    let navigate = use_navigate();

    let do_fetch = move || {
        spawn_local(async move {
            match fetch_log().await {
                Ok(data) => { set_entries.set(data); set_loading.set(false); set_error.set(None); }
                Err(e)   => { set_error.set(Some(e)); set_loading.set(false); }
            }
        });
    };

    do_fetch();
    set_interval(do_fetch, Duration::from_secs(5));

    let total_saved = move || entries.get().iter().map(|e| e.delta_lamports).sum::<i64>();
    let landed_count = move || entries.get().iter().filter(|e| e.confirmed_at_ms.is_some() || e.finalized_at_ms.is_some()).count();
    let failed_count = move || entries.get().iter().filter(|e| e.failure_kind.is_some()).count();

    view! {
        <div class="space-y-6">

            // ── Page header ─────────────────────────────────────────────
            <div>
                <p class="text-xs font-mono uppercase tracking-widest text-zinc-600 mb-1">
                    "Dashboard / Log"
                </p>
                <h1 class="text-3xl font-bold tracking-tight">"Execution Log"</h1>
                <p class="text-sm text-zinc-500 mt-1">"Every submission · Jito bundles and priority-fee txs · refreshes every 5 s"</p>
            </div>

            // ── Aggregate stats ──────────────────────────────────────────
            <div class="grid grid-cols-2 sm:grid-cols-4 gap-3">
                <div class="rounded-lg border border-[#2a2a2a] bg-[#222] px-5 py-4">
                    <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Total Submitted"</p>
                    <p class="mt-1 text-2xl font-bold font-mono text-zinc-200">
                        {move || entries.get().len()}
                    </p>
                </div>
                <div class="rounded-lg border border-[#2a2a2a] bg-[#222] px-5 py-4">
                    <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Total Saved"</p>
                    <p class=move || format!(
                        "mt-1 text-xl font-bold font-mono {}",
                        if total_saved() >= 0 { "text-amber-400" } else { "text-red-400" }
                    )>
                        {move || delta_to_sol(total_saved())}
                    </p>
                </div>
                <div class="rounded-lg border border-[#2a2a2a] bg-[#222] px-5 py-4">
                    <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Landed"</p>
                    <p class="mt-1 text-2xl font-bold font-mono text-zinc-200">
                        {move || landed_count()}
                    </p>
                </div>
                <div class="rounded-lg border border-[#2a2a2a] bg-[#222] px-5 py-4">
                    <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Failed"</p>
                    <p class=move || format!(
                        "mt-1 text-2xl font-bold font-mono {}",
                        if failed_count() > 0 { "text-red-400" } else { "text-zinc-200" }
                    )>
                        {move || failed_count()}
                    </p>
                </div>
            </div>

            // Error
            {move || error.get().map(|e| view! {
                <p class="text-sm text-red-400 font-mono">{e}</p>
            })}

            // ── Table ────────────────────────────────────────────────────
            {move || {
                // Clone the navigator per render so the inner `For` closure moves the
                // clone (keeping this reactive closure `Fn`, not `FnOnce`).
                let navigate = navigate.clone();
                let rows = entries.get();

                if loading.get() {
                    view! {
                        <div class="rounded-xl border border-[#2e2e2e] bg-[#222] px-6 py-12 text-center">
                            <p class="text-sm text-zinc-600">"Loading…"</p>
                        </div>
                    }.into_any()
                } else if rows.is_empty() {
                    view! {
                        <div class="rounded-xl border border-[#2e2e2e] bg-[#222] px-6 py-16 text-center space-y-3">
                            <p class="text-zinc-500 text-2xl">"📭"</p>
                            <p class="text-sm font-medium">"No executions yet"</p>
                            <p class="text-xs text-zinc-600">
                                "Go to Execute and submit your first transaction."
                            </p>
                        </div>
                    }.into_any()
                } else {
                    view! {
                        <div class="rounded-xl border border-[#2e2e2e] overflow-hidden">
                            <table class="w-full text-sm">
                                <thead>
                                    <tr class="border-b border-[#2e2e2e] bg-[#252525]">
                                        <th class="px-5 py-3 text-left   text-[10px] font-mono uppercase tracking-widest text-zinc-600">"ID"</th>
                                        <th class="px-5 py-3 text-left   text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Lane"</th>
                                        <th class="px-5 py-3 text-left   text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Network"</th>
                                        <th class="px-5 py-3 text-right  text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Fee paid"</th>
                                        <th class="px-5 py-3 text-right  text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Market rate"</th>
                                        <th class="px-5 py-3 text-right  text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Saved"</th>
                                        <th class="px-5 py-3 text-center text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Tries"</th>
                                        <th class="px-5 py-3 text-right  text-[10px] font-mono uppercase tracking-widest text-zinc-600">"AI certainty"</th>
                                        <th class="px-5 py-3 text-left   text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Status"</th>
                                    </tr>
                                </thead>
                                <tbody class="divide-y divide-[#272727] bg-[#1f1f1f]">
                                    <For
                                        each=move || entries.get()
                                        key=|e| e.bundle_id.clone()
                                        children=move |e| {
                                            let short_id  = e.bundle_id.chars().take(12).collect::<String>();
                                            // Programmatic navigation to the live detail page on row click.
                                            let nav = navigate.clone();
                                            let bid = e.bundle_id.clone();
                                            let landed_for_jito = e.landed_bundle_id.clone().unwrap_or_else(|| e.bundle_id.clone());
                                            // Lane drives the explorer target: a priority-fee tx lives on Solscan,
                                            // a Jito bundle on the Jito block-engine explorer.
                                            let is_priority = e.lane == "PriorityFee";
                                            let (lane_short, lane_cls) = if is_priority {
                                                ("RPC", "border-sky-800 bg-sky-950/60 text-sky-300")
                                            } else {
                                                ("JITO", "border-amber-800 bg-amber-950/60 text-amber-300")
                                            };
                                            let row_url = if is_priority {
                                                format!("https://solscan.io/tx/{}", e.bundle_id)
                                            } else {
                                                format!("https://explorer.jito.wtf/bundle/{}", landed_for_jito)
                                            };
                                            let row_url_title = if is_priority { "Open on Solscan" } else { "Open on Jito Explorer" };
                                            let regime    = e.regime.clone();
                                            let status    = derive_status(&e);
                                            let status_cls = status_class(&status);
                                            let saved_pos = e.delta_lamports >= 0;
                                            let saved_str = delta_to_sol(e.delta_lamports);
                                            let tip_str   = lamports_to_sol(e.tip_lamports);
                                            let base_str  = lamports_to_sol(e.baseline_tip_lamports);
                                            let regime_cls  = regime_badge_class(&regime);
                                            let retry_count = e.retry_count;
                                            let retry_cls = if retry_count > 0 { "text-orange-400 font-semibold" } else { "text-zinc-700" };

                                            view! {
                                                // Main row — click anywhere to open the live detail page.
                                                <tr
                                                    class="group hover:bg-[#242424] transition-colors cursor-pointer"
                                                    on:click=move |_| nav(&format!("/bundle/{}", bid), Default::default())
                                                >
                                                    // Bundle ID (row opens detail), with a small ↗ to Jito's explorer.
                                                    <td class="px-5 py-3.5">
                                                        <div class="flex items-center gap-2">
                                                            <span class="font-mono text-xs text-amber-500 group-hover:text-amber-300 transition-colors underline-offset-2 group-hover:underline">
                                                                {short_id}{"…"}
                                                            </span>
                                                            <a
                                                                href=row_url
                                                                target="_blank"
                                                                title=row_url_title
                                                                on:click=|e| e.stop_propagation()
                                                                class="text-[10px] text-zinc-600 hover:text-amber-500 transition-colors"
                                                            >
                                                                "↗"
                                                            </a>
                                                        </div>
                                                    </td>
                                                    // Lane badge — JITO bundle vs RPC priority fee.
                                                    <td class="px-5 py-3.5">
                                                        <span class=format!(
                                                            "inline-flex items-center rounded border \
                                                             px-1.5 py-0.5 text-[10px] font-mono font-semibold tracking-wide {}",
                                                            lane_cls
                                                        )>{lane_short}</span>
                                                    </td>
                                                    <td class="px-5 py-3.5">
                                                        <span class=format!(
                                                            "inline-flex items-center rounded-md border \
                                                             px-2 py-0.5 text-xs font-semibold {}",
                                                            regime_cls
                                                        )>{regime_human(&regime)}</span>
                                                    </td>
                                                    <td class="px-5 py-3.5 text-right font-mono text-xs text-zinc-400">
                                                        {tip_str}
                                                    </td>
                                                    <td class="px-5 py-3.5 text-right font-mono text-xs text-zinc-600">
                                                        {base_str}
                                                    </td>
                                                    <td class=format!(
                                                        "px-5 py-3.5 text-right font-mono text-xs font-semibold {}",
                                                        if saved_pos { "text-amber-400" } else { "text-red-400" }
                                                    )>{saved_str}</td>
                                                    // Retry count — orange if retried
                                                    <td class="px-5 py-3.5 text-center font-mono text-xs">
                                                        <span class=retry_cls>{retry_count}</span>
                                                    </td>
                                                    <td class="px-5 py-3.5 text-right font-mono text-xs text-zinc-500">
                                                        {format!("{:.0}%", e.confidence * 100.0)}
                                                    </td>
                                                    <td class=format!("px-5 py-3.5 text-xs font-mono {}", status_cls)>
                                                        {status.clone()}
                                                    </td>
                                                </tr>
                                                // Reasoning sub-row
                                                <tr class="bg-[#1c1c1c]">
                                                    <td colspan="9" class="px-5 py-2 border-t border-[#252525]">
                                                        <p class="text-[11px] text-zinc-600 leading-relaxed">
                                                            <span class="text-zinc-700 mr-2">"AI:"</span>
                                                            {e.reasoning.clone()}
                                                        </p>
                                                        // Transaction signature links — visible even for failed bundles.
                                                        {if !e.tx_signatures.is_empty() {
                                                            let sig_links = e.tx_signatures.iter().enumerate().map(|(i, sig)| {
                                                                let short = format!("{}…{}", &sig[..8], &sig[sig.len()-6..]);
                                                                let url = format!("https://solscan.io/tx/{}", sig);
                                                                let label = if e.tx_signatures.len() > 1 {
                                                                    format!("tx[{}]", i)
                                                                } else {
                                                                    "tx sig".to_string()
                                                                };
                                                                view! {
                                                                    <span class="ml-4 text-[11px] text-zinc-700">
                                                                        {label}{": "}
                                                                        <a href=url target="_blank"
                                                                           on:click=|e| e.stop_propagation()
                                                                           class="font-mono text-zinc-500 hover:text-zinc-300 transition-colors">
                                                                            {short}
                                                                        </a>
                                                                    </span>
                                                                }
                                                            }).collect::<Vec<_>>();
                                                            Some(view! { <span>{sig_links}</span> })
                                                        } else {
                                                            None
                                                        }}
                                                        // Show landed_bundle_id if different (a retry landed)
                                                        {e.landed_bundle_id.clone().and_then(|lid| {
                                                            if lid != e.bundle_id {
                                                                let short_lid = lid.chars().take(12).collect::<String>();
                                                                let lid_url = format!("https://explorer.jito.wtf/bundle/{}", lid);
                                                                Some(view! {
                                                                    <span class="ml-4 text-[11px] text-zinc-700">
                                                                        "landed as: "
                                                                        <a href=lid_url target="_blank"
                                                                           class="text-amber-800 hover:text-amber-600 font-mono">
                                                                            {short_lid}{"…"}
                                                                        </a>
                                                                    </span>
                                                                })
                                                            } else {
                                                                None
                                                            }
                                                        })}
                                                        // Show failure reason if present, in plain English.
                                                        {e.failure_kind.clone().map(|fk| view! {
                                                            <span class="ml-4 text-[11px] text-red-500/80">
                                                                {humanize_failure(&fk)}
                                                            </span>
                                                        })}
                                                    </td>
                                                </tr>
                                            }
                                        }
                                    />
                                </tbody>
                            </table>
                        </div>
                    }.into_any()
                }
            }}

        </div>
    }
}

fn derive_status(e: &LogEntry) -> String {
    if e.failure_kind.is_some()            { "Didn't land".into() }
    else if e.finalized_at_ms.is_some()    { "Final".into() }
    else if e.confirmed_at_ms.is_some()    { "Confirmed".into() }
    else if e.processed_at_ms.is_some()    { "Processing".into() }
    else                                    { "Sending…".into()  }
}

fn status_class(status: &str) -> &'static str {
    match status {
        "Didn't land" => "text-red-400",
        "Final"       => "text-emerald-400",
        "Confirmed"   => "text-amber-400",
        "Processing"  => "text-zinc-400",
        _             => "text-zinc-600",
    }
}

fn regime_human(regime: &str) -> &'static str {
    match regime {
        "Cold"  => "Quiet",
        "Warm"  => "Normal",
        "Hot"   => "Busy",
        "Manic" => "Extreme",
        _       => "—",
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
