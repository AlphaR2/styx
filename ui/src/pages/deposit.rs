use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{
    fetch_session, format_utc_hms, gen_session_token,
    post_bypass, post_deposit_claim, SessionResponse,
};
use crate::utils::lamports_to_sol;

#[component]
pub fn DepositPage() -> impl IntoView {
    let session_token = use_context::<RwSignal<Option<String>>>()
        .expect("session_token context not provided");
    let credits = use_context::<RwSignal<u32>>()
        .expect("credits context not provided");

    let (session_info, set_info)  = signal(None::<SessionResponse>);
    let (bypass_val, set_bypass)  = signal(String::new());
    let (bypass_busy, set_bbsy)   = signal(false);
    let (bypass_err, set_berr)    = signal(None::<String>);
    let (claim_busy, set_cbsy)    = signal(false);
    let (copy_ok, set_copy)       = signal(false);

    // Fetch session info if token exists
    let refresh_session = move || {
        if let Some(tok) = session_token.get() {
            spawn_local(async move {
                if let Ok(s) = fetch_session(&tok).await {
                    set_info.set(Some(s));
                }
            });
        }
    };
    refresh_session();

    // ── bypass activation ──────────────────────────────────────────────
    let on_bypass = move |_| {
        let code = bypass_val.get();
        if code.is_empty() { return; }
        set_bbsy.set(true);
        set_berr.set(None);
        spawn_local(async move {
            match post_bypass(&code).await {
                Ok(s) => {
                    session_token.set(Some(s.session_token.clone()));
                    credits.set(s.credits);
                    set_info.set(Some(s));
                }
                Err(e) => { set_berr.set(Some(e)); }
            }
            set_bbsy.set(false);
        });
    };

    // ── deposit claim ──────────────────────────────────────────────────
    let on_claim = move |_| {
        let tok = session_token.get().unwrap_or_else(gen_session_token);
        set_cbsy.set(true);
        spawn_local(async move {
            if let Ok(s) = post_deposit_claim(&tok).await {
                session_token.set(Some(s.session_token.clone()));
                credits.set(s.credits);
                set_info.set(Some(s));
            }
            set_cbsy.set(false);
        });
    };

    // ── copy address ───────────────────────────────────────────────────
    let on_copy = move |_| {
        if let Some(ref s) = session_info.get() {
            let addr = s.deposit_address.clone();
            if let Some(window) = web_sys::window() {
                let clipboard = window.navigator().clipboard();
                let _ = clipboard.write_text(&addr);
                set_copy.set(true);
                set_timeout(move || set_copy.set(false), Duration::from_secs(2));
            }
        }
    };

    use std::time::Duration;

    view! {
        <div class="max-w-3xl space-y-8">

            // ── Page header ──────────────────────────────────────────────
            <div>
                <p class="text-xs font-mono uppercase tracking-widest text-zinc-600 mb-1">
                    "Dashboard / Deposit"
                </p>
                <h1 class="text-3xl font-bold tracking-tight">"Session & Credits"</h1>
                <p class="text-sm text-zinc-500 mt-1">
                    "Activate a session to unlock snipe. Judges: use the bypass code."
                </p>
            </div>

            // ── Two columns: bypass | deposit ────────────────────────────
            <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">

                // Bypass code card
                <div class="rounded-xl border border-[#2e2e2e] bg-[#222] p-6 space-y-4">
                    <div>
                        <p class="text-xs font-mono uppercase tracking-widest text-zinc-600">"Bypass Code"</p>
                        <p class="text-xs text-zinc-600 mt-1">"For judges and demo access"</p>
                    </div>
                    <div class="space-y-3">
                        <input
                            type="password"
                            placeholder="Enter code…"
                            prop:value=bypass_val
                            on:input=move |e| set_bypass.set(event_target_value(&e))
                            class="w-full rounded-lg border border-[#333] bg-[#1a1a1a]
                                   px-3 py-2.5 text-sm font-mono text-zinc-300
                                   placeholder:text-zinc-700 focus:outline-none focus:border-amber-700/60"
                        />
                        <button
                            on:click=on_bypass
                            disabled=move || bypass_busy.get() || bypass_val.get().is_empty()
                            class="w-full rounded-lg bg-amber-500 text-black py-2.5 text-sm font-semibold
                                   hover:bg-amber-400 disabled:opacity-40 disabled:cursor-not-allowed
                                   transition-colors"
                        >
                            {move || if bypass_busy.get() { "Activating…" } else { "Activate Session →" }}
                        </button>
                        {move || bypass_err.get().map(|e| view! {
                            <p class="text-xs text-red-400 font-mono">{e}</p>
                        })}
                    </div>
                    <div class="border-t border-[#2a2a2a] pt-3">
                        <p class="text-[10px] font-mono text-zinc-700 leading-relaxed">
                            "Bypass code grants 100 credits. "
                            "Set JUDGE_BYPASS_CODE in .env to customise."
                        </p>
                    </div>
                </div>

                // Deposit SOL card
                <div class="rounded-xl border border-[#2e2e2e] bg-[#222] p-6 space-y-4">
                    <div>
                        <p class="text-xs font-mono uppercase tracking-widest text-zinc-600">"Deposit SOL"</p>
                        <p class="text-xs text-zinc-600 mt-1">"Each claim adds 5 credits"</p>
                    </div>

                    // Deposit address display
                    {move || if let Some(ref s) = session_info.get() {
                        let addr = s.deposit_address.clone();
                        let short_addr = format!("{}…{}", &addr[..8], &addr[addr.len()-6..]);
                        view! {
                            <div class="space-y-3">
                                <div class="rounded-lg border border-[#333] bg-[#1a1a1a] px-3 py-2.5
                                            flex items-center justify-between gap-2">
                                    <span class="font-mono text-xs text-zinc-400 truncate">
                                        {short_addr}
                                    </span>
                                    <button
                                        on:click=on_copy
                                        class="shrink-0 text-xs font-mono text-zinc-600 hover:text-zinc-300
                                               transition-colors"
                                    >
                                        {move || if copy_ok.get() { "✓ copied" } else { "copy" }}
                                    </button>
                                </div>
                                <button
                                    on:click=on_claim
                                    disabled=move || claim_busy.get()
                                    class="w-full rounded-lg border border-amber-800/60 bg-amber-950/60
                                           text-amber-400 py-2.5 text-sm font-semibold
                                           hover:bg-amber-900/40 disabled:opacity-40 disabled:cursor-not-allowed
                                           transition-colors"
                                >
                                    {move || if claim_busy.get() { "Claiming…" } else { "Claim 5 Credits →" }}
                                </button>
                            </div>
                        }.into_any()
                    } else {
                        view! {
                            <div class="space-y-3">
                                <div class="rounded-lg border border-[#333] bg-[#1a1a1a] px-3 py-2.5
                                            text-xs font-mono text-zinc-700">
                                    "Activate a session first to see deposit address"
                                </div>
                                <button
                                    on:click=on_claim
                                    disabled=move || claim_busy.get()
                                    class="w-full rounded-lg border border-[#2e2e2e] bg-[#1a1a1a]
                                           text-zinc-400 py-2.5 text-sm font-semibold
                                           hover:bg-[#252525] disabled:opacity-40
                                           transition-colors"
                                >
                                    {move || if claim_busy.get() { "Creating session…" } else { "Create Session + 5 Credits →" }}
                                </button>
                            </div>
                        }.into_any()
                    }}

                    <div class="border-t border-[#2a2a2a] pt-3">
                        <p class="text-[10px] font-mono text-zinc-700 leading-relaxed">
                            "Demo mode: send any SOL amount, then click claim. "
                            "On-chain verification coming in production."
                        </p>
                    </div>
                </div>

            </div>

            // ── Session info card ────────────────────────────────────────
            {move || if let Some(s) = session_info.get() {
                let tok_short  = format!("{}…", s.session_token.chars().take(14).collect::<String>());
                let credits_s  = s.credits.to_string();
                let deposited  = lamports_to_sol(s.deposit_lamports);
                let created    = format_utc_hms(s.created_at_ms);
                view! {
                    <div class="rounded-xl border border-[#2e2e2e] bg-[#222] overflow-hidden">
                        <div class="px-6 py-3 border-b border-[#2e2e2e] bg-[#252525]">
                            <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600">
                                "Active Session"
                            </p>
                        </div>
                        <div class="grid grid-cols-2 sm:grid-cols-4 gap-px bg-[#2a2a2a]">
                            <SessionCell label="Token"     value=tok_short/>
                            <SessionCell label="Credits"   value=credits_s/>
                            <SessionCell label="Deposited" value=deposited/>
                            <SessionCell label="Created"   value=created/>
                        </div>
                    </div>
                }.into_any()
            } else {
                view! { <div></div> }.into_any()
            }}

        </div>
    }
}

#[component]
fn SessionCell(label: &'static str, value: String) -> impl IntoView {
    view! {
        <div class="bg-[#222] px-5 py-4">
            <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-700">{label}</p>
            <p class="text-sm font-mono text-zinc-300 mt-1">{value}</p>
        </div>
    }
}
