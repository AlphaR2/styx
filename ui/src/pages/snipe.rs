use futures::StreamExt;
use gloo_net::websocket::Message;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;
use std::time::Duration;

use crate::api::{
    fetch_launches, format_utc_hms, post_snipe, ws_connect, ws_reconnect_delay, LaunchEntry,
    SnipeResponse,
};
use crate::components::result_modal::BundleModal;
use crate::utils::{lamports_to_sol, short};

#[component]
pub fn SnipePage() -> impl IntoView {
    let navigate = use_navigate();

    let (launches, set_launches)    = signal(Vec::<LaunchEntry>::new());
    let (loading, set_loading)      = signal(true);
    let (snipe_result, set_result)  = signal(None::<Result<SnipeResponse, String>>);
    let (sniping_mint, set_sniping) = signal(None::<String>);
    let (lane, set_lane)            = signal("jito".to_string());

    // ── refresh every 8 s ─────────────────────────────────────────────────
    let do_fetch = move || {
        spawn_local(async move {
            if let Ok(mut data) = fetch_launches().await {
                data.truncate(12);
                set_launches.set(data);
                set_loading.set(false);
            }
        });
    };
    do_fetch();
    set_interval(do_fetch, Duration::from_secs(8));

    // ── WebSocket: prepend new launches in real time ──────────────────────
    spawn_local(async move {
        loop {
            let Some(ws) = ws_connect().await else {
                ws_reconnect_delay().await;
                continue;
            };
            let (_, mut read) = ws.split();
            while let Some(Ok(Message::Text(text))) = read.next().await {
                if let Ok(crate::api::WsEvent::NewTokenLaunch {
                    mint, name, symbol, uri, creator, detected_at_ms,
                }) = serde_json::from_str(&text)
                {
                    set_launches.update(|v| {
                        v.insert(0, LaunchEntry {
                            mint, name, symbol, uri, creator, detected_at_ms,
                            snipe_bundle_id: None, snipe_status: None, snipe_tip_lamports: None,
                        });
                        v.truncate(12);
                    });
                }
            }
            ws_reconnect_delay().await;
        }
    });

    view! {
        <div class="space-y-6">

            // ── Page header ──────────────────────────────────────────────
            <div class="flex items-end justify-between flex-wrap gap-4">
                <div class="space-y-2">
                    <div>
                        <p class="text-xs font-mono uppercase tracking-widest text-zinc-600 mb-1">
                            "Dashboard / Snipe"
                        </p>
                        <h1 class="text-3xl font-bold tracking-tight">"Live Launches"</h1>
                        <p class="text-sm text-zinc-500 mt-1">
                            "pump.fun token feed · detected via Yellowstone · snipe with one click"
                        </p>
                    </div>
                    // Lane toggle
                    <div class="flex items-center gap-3 pt-1">
                        <span class="text-[10px] font-mono uppercase tracking-widest text-zinc-600 shrink-0">
                            "Submission lane"
                        </span>
                        <div class="inline-flex items-center rounded-lg border border-[#2e2e2e] bg-[#1a1a1a] p-0.5">
                            <button
                                on:click=move |_| set_lane.set("jito".to_string())
                                class=move || format!(
                                    "px-4 py-1.5 rounded-md text-xs font-mono font-medium transition-all duration-150 {}",
                                    if lane.get() == "jito" { "bg-amber-500 text-black shadow-sm" }
                                    else { "text-zinc-500 hover:text-zinc-300" }
                                )
                            >
                                "Jito Bundle"
                            </button>
                            <button
                                on:click=move |_| set_lane.set("priority".to_string())
                                class=move || format!(
                                    "px-4 py-1.5 rounded-md text-xs font-mono font-medium transition-all duration-150 {}",
                                    if lane.get() == "priority" { "bg-amber-500 text-black shadow-sm" }
                                    else { "text-zinc-500 hover:text-zinc-300" }
                                )
                            >
                                "Priority Fee"
                            </button>
                        </div>
                    </div>
                </div>
                <div class="flex items-center gap-2 px-3 py-1.5 rounded-full text-xs font-mono
                            border border-amber-800/60 bg-amber-950 text-amber-400">
                    <span class="h-1.5 w-1.5 rounded-full bg-amber-400 animate-pulse"></span>
                    "mainnet · live"
                </div>
            </div>

            // ── Snipe result modal / error ───────────────────────────────
            {move || snipe_result.get().map(|res| match res {
                Ok(r) => {
                    let fee_label = if lane.get() == "priority" { "Priority fee" } else { "Tip paid" };
                    let rows = vec![
                        (fee_label,   lamports_to_sol(r.tip_lamports), false),
                        ("Token mint", short(&r.mint, 12) + "…",       false),
                    ];
                    view! {
                        <BundleModal
                            bundle_id=r.bundle_id.clone()
                            title="Snipe submitted".to_string()
                            rows=rows
                            on_close=Callback::new(move |_| set_result.set(None))
                        />
                    }.into_any()
                }
                Err(e) => view! {
                    <div class="rounded-xl border border-red-900/60 bg-red-950/30 px-5 py-4
                                flex items-center justify-between">
                        <p class="text-xs font-mono text-red-400">"Snipe failed: "{e}</p>
                        <button
                            on:click=move |_| set_result.set(None)
                            class="text-zinc-700 hover:text-zinc-400 text-xs font-mono ml-4"
                        >
                            "✕"
                        </button>
                    </div>
                }.into_any()
            })}

            // ── Launch feed ──────────────────────────────────────────────
            {move || {
                let navigate = navigate.clone();
                let rows = launches.get();
                if loading.get() && rows.is_empty() {
                    return view! {
                        <div class="rounded-xl border border-[#2e2e2e] bg-[#222] px-6 py-16 text-center">
                            <p class="text-sm text-zinc-600">"Connecting to Yellowstone…"</p>
                        </div>
                    }.into_any();
                }
                if rows.is_empty() {
                    return view! {
                        <div class="rounded-xl border border-[#2e2e2e] bg-[#222] px-6 py-16 text-center space-y-3">
                            <p class="text-zinc-500 text-2xl">"👁"</p>
                            <p class="text-sm font-medium">"Watching for launches…"</p>
                            <p class="text-xs text-zinc-600">
                                "Yellowstone is monitoring the pump.fun program. "
                                "New token creates appear here instantly."
                            </p>
                        </div>
                    }.into_any();
                }
                view! {
                    <div class="rounded-xl border border-[#2e2e2e] overflow-hidden">
                        <div class="px-5 py-3 border-b border-[#2e2e2e] bg-[#252525] flex items-center justify-between">
                            <span class="text-[10px] font-mono uppercase tracking-widest text-zinc-600">
                                {move || {
                                let n = launches.get().len();
                                if n >= 12 { "latest 12 launches".to_string() }
                                else { format!("{} launches detected", n) }
                            }}
                            </span>
                            <span class="text-[10px] font-mono text-zinc-700">"refreshes every 8 s"</span>
                        </div>
                        <table class="w-full text-sm">
                            <thead>
                                <tr class="border-b border-[#2e2e2e] bg-[#222]">
                                    <th class="px-4 py-2.5 text-left text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Symbol"</th>
                                    <th class="px-4 py-2.5 text-left text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Name"</th>
                                    <th class="px-4 py-2.5 text-left text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Mint"</th>
                                    <th class="px-4 py-2.5 text-left text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Creator"</th>
                                    <th class="px-4 py-2.5 text-center text-[10px] font-mono uppercase tracking-widest text-zinc-600">"UTC"</th>
                                    <th class="px-4 py-2.5 text-right text-[10px] font-mono uppercase tracking-widest text-zinc-600">"Action"</th>
                                </tr>
                            </thead>
                            <tbody class="divide-y divide-[#272727] bg-[#1f1f1f]">
                                <For
                                    each=move || launches.get()
                                    key=|e| e.mint.clone()
                                    children=move |e| {
                                        let mint_short    = e.mint.chars().take(8).collect::<String>();
                                        let creator_short = e.creator.chars().take(8).collect::<String>();
                                        let solscan_url   = format!("https://solscan.io/token/{}", e.mint);
                                        let time          = format_utc_hms(e.detected_at_ms);
                                        let mint_c        = e.mint.clone();
                                        let creator_c     = e.creator.clone();
                                        let m_dis         = mint_c.clone();
                                        let m_lbl         = mint_c.clone();
                                        let is_sniping_dis = move || sniping_mint.get().as_deref() == Some(m_dis.as_str());
                                        let is_sniping_lbl = move || sniping_mint.get().as_deref() == Some(m_lbl.as_str());
                                        let already = e.snipe_bundle_id.is_some();

                                        let on_snipe = move |_| {
                                            if sniping_mint.get().is_some() { return; }
                                            let m  = mint_c.clone();
                                            let cr = creator_c.clone();
                                            let l  = lane.get();
                                            set_sniping.set(Some(m.clone()));
                                            spawn_local(async move {
                                                let res = post_snipe(&m, &cr, Some(100_000), &l).await;
                                                set_result.set(Some(res));
                                                set_sniping.set(None);
                                            });
                                        };

                                        view! {
                                            <tr class="hover:bg-[#242424] transition-colors">
                                                <td class="px-4 py-3">
                                                    <span class="inline-flex items-center rounded border border-amber-800/60 bg-amber-950/60 px-2 py-0.5 text-xs font-mono font-semibold text-amber-400">
                                                        {e.symbol.clone()}
                                                    </span>
                                                </td>
                                                <td class="px-4 py-3 text-sm text-zinc-300 max-w-[140px] truncate">
                                                    {e.name.clone()}
                                                </td>
                                                <td class="px-4 py-3">
                                                    <a href=solscan_url target="_blank"
                                                       class="font-mono text-xs text-zinc-500 hover:text-zinc-300 transition-colors">
                                                        {mint_short}{"…"}
                                                    </a>
                                                </td>
                                                <td class="px-4 py-3 font-mono text-xs text-zinc-600">
                                                    {creator_short}{"…"}
                                                </td>
                                                <td class="px-4 py-3 text-center font-mono text-[11px] text-zinc-600">
                                                    {time}
                                                </td>
                                                <td class="px-4 py-3 text-right">
                                                    {if already {
                                                        let bid = e.snipe_bundle_id.clone().unwrap_or_default();
                                                        let short_bid = short(&bid, 8);
                                                        let nav = navigate.clone();
                                                        view! {
                                                            <button
                                                                on:click=move |_| nav(&format!("/bundle/{}", bid), Default::default())
                                                                class="text-xs font-mono text-amber-600 hover:text-amber-400 transition-colors underline-offset-2 hover:underline">
                                                                "view · "{short_bid}{"…"}
                                                            </button>
                                                        }.into_any()
                                                    } else {
                                                        view! {
                                                            <button
                                                                on:click=on_snipe
                                                                disabled=move || is_sniping_dis()
                                                                class="inline-flex items-center gap-1.5 rounded-lg px-3 py-1.5
                                                                       bg-amber-500 text-black text-xs font-semibold
                                                                       hover:bg-amber-400 disabled:opacity-40 disabled:cursor-not-allowed
                                                                       transition-all active:scale-95"
                                                            >
                                                                {move || if is_sniping_lbl() { "⏳" } else { "⚡ Snipe" }}
                                                            </button>
                                                        }.into_any()
                                                    }}
                                                </td>
                                            </tr>
                                        }
                                    }
                                />
                            </tbody>
                        </table>
                    </div>
                }.into_any()
            }}

        </div>
    }
}
