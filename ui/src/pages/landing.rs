use leptos::prelude::*;
use leptos_router::components::A;

#[component]
pub fn LandingPage() -> impl IntoView {
    view! {
        <div class="space-y-14">

            // ── Hero ─────────────────────────────────────────────────────
            <div class="relative pt-6 pb-2">
                <div class="inline-flex items-center gap-2 px-3 py-1 rounded-full
                            border border-amber-800/60 bg-amber-950/50 mb-5">
                    <span class="h-1.5 w-1.5 rounded-full bg-amber-400 animate-pulse"></span>
                    <span class="text-[11px] font-mono tracking-wider text-amber-400/90">
                        "LIVE ON SOLANA MAINNET"
                    </span>
                </div>
                <h1 class="text-4xl sm:text-5xl font-bold tracking-tight leading-[1.05] max-w-2xl">
                    "Smart transaction execution for Solana"
                </h1>
                <p class="text-base text-zinc-400 mt-5 max-w-xl leading-relaxed">
                    "Styx observes live Jito tip auctions via Yellowstone, asks an AI to price the inclusion fee, "
                    "submits as a Jito bundle or priority-fee transaction, and retries with a fresh price until confirmed."
                </p>
                <div class="flex flex-wrap gap-3 mt-7">
                    <A href="/execute" attr:class="inline-flex items-center gap-2 rounded-lg px-5 py-2.5
                        bg-amber-500 text-black font-semibold text-sm hover:bg-amber-400
                        active:scale-[0.98] transition-all glow-box">
                        "Send a transaction"
                    </A>
                </div>
            </div>

            // ── Cards ─────────────────────────────────────────────────────
            <div>
                <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600 mb-4">
                    "Explore"
                </p>
                <div class="grid grid-cols-1 sm:grid-cols-3 gap-4">
                    <ScenarioCard
                        href="/execute" title="Execute"
                        desc="Submit a memo, a Jupiter swap, or a fault-injected bundle. Watch the AI price, submit, and retry live."
                    />
                    <ScenarioCard
                        href="/network" title="Mission Control"
                        desc="Live slots, leader schedule, tip auction stats, and bundle flow in real time."
                    />
                    <ScenarioCard
                        href="/log" title="Execution Log"
                        desc="Every submission: fee paid, savings vs. naive baseline, regime, retries, and landing slot."
                    />
                </div>
            </div>

            <div class="h-px bg-[#272727]"></div>

            // ── Pipeline ─────────────────────────────────────────────────
            <div>
                <h2 class="text-lg font-semibold mb-6">"How it works"</h2>
                <div class="space-y-0">
                    <PipelineStep num=1 title="Ingest"
                        desc="Yellowstone gRPC streams slot updates and Jito tip account balance deltas in real time. Observed deltas build an empirical auction window."
                        last=false/>
                    <PipelineStep num=2 title="Classify"
                        desc="The AI reads the live auction window — clearing prices, bundles per slot, trend — and classifies the regime: Cold, Warm, Hot, or Manic."
                        last=false/>
                    <PipelineStep num=3 title="Bid"
                        desc="The AI outputs a forward multiplier. The tip is: median_clearing_price * regime_safety * multiplier, clamped by transaction value type."
                        last=false/>
                    <PipelineStep num=4 title="Submit"
                        desc="Instructions are wrapped with a compute budget and the calculated tip, signed, and sent — as a Jito bundle fanned out to all four regions, or a priority-fee transaction."
                        last=false/>
                    <PipelineStep num=5 title="Track and retry"
                        desc="Lifecycle follows Submitted, Processed, Confirmed, Finalized via Yellowstone with RPC fallback. On timeout, the AI classifies the failure and reprices before resubmitting."
                        last=true/>
                </div>
            </div>

            <div class="h-px bg-[#272727]"></div>

            // ── Regime reference ─────────────────────────────────────────
            <div>
                <h2 class="text-lg font-semibold mb-1">"Network regimes"</h2>
                <p class="text-sm text-zinc-500 mb-5">
                    "Classified from the live median clearing price of the Jito tip auction."
                </p>
                <div class="h-2 w-full rounded-full overflow-hidden
                            bg-gradient-to-r from-zinc-700 via-amber-600 via-orange-500 to-red-600 mb-5">
                </div>
                <div class="grid grid-cols-2 sm:grid-cols-4 gap-3">
                    <RegimeCard label="Cold"  cutoff="below 10k lamports"
                        desc="Minimal competition. Conservative bid."
                        bg="bg-zinc-900" border="border-zinc-800"/>
                    <RegimeCard label="Warm"  cutoff="10k to 100k"
                        desc="Normal activity. Moderate safety margin."
                        bg="bg-amber-950" border="border-amber-900"/>
                    <RegimeCard label="Hot"   cutoff="100k to 1M"
                        desc="High demand. Aggressive bid required."
                        bg="bg-orange-950" border="border-orange-900"/>
                    <RegimeCard label="Manic" cutoff="above 1M lamports"
                        desc="Extreme congestion. Maximum pressure."
                        bg="bg-red-950" border="border-red-900"/>
                </div>
            </div>

            <div class="h-px bg-[#272727]"></div>

            // ── Stack ─────────────────────────────────────────────────────
            <div>
                <h2 class="text-lg font-semibold mb-4">"Built with"</h2>
                <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
                    <StackCard layer="Execution" items=vec![
                        ("Solana SDK",  "transaction construction and RPC"),
                        ("Jito SDK",    "bundle construction and submission"),
                        ("Jupiter v6",  "swap routing and lookup tables"),
                        ("Axum",        "HTTP API and WebSocket server"),
                    ]/>
                    <StackCard layer="Intelligence" items=vec![
                        ("Yellowstone gRPC", "live tip account observation, slot streaming"),
                        ("AuctionWindow",    "20-slot rolling clearing price statistics"),
                        ("LLM classifier",   "forward multiplier and retry advice"),
                        ("OverpayerBaseline","deterministic 2x benchmark for savings tracking"),
                    ]/>
                    <StackCard layer="Frontend" items=vec![
                        ("Leptos 0.8",     "Rust WASM reactive UI"),
                        ("Trunk",          "WASM bundler"),
                        ("Tailwind CSS 4", "utility-first styles"),
                    ]/>
                    <StackCard layer="Infrastructure" items=vec![
                        ("Cargo workspace",   "five crates: ingest, core, agent, styx, demo"),
                        ("Yellowstone",       "gRPC slot and account stream"),
                        ("Solana mainnet-beta", "Jito block engine, 4 regions"),
                    ]/>
                </div>
            </div>

        </div>
    }
}

#[component]
fn ScenarioCard(href: &'static str, title: &'static str, desc: &'static str) -> impl IntoView {
    view! {
        <A href=href attr:class="group block rounded-xl border border-[#2e2e2e] bg-[#222] p-5
            hover:border-amber-700/60 hover:bg-[#262320] transition-all">
            <div class="mb-2">
                <span class="text-sm font-semibold text-zinc-100 group-hover:text-amber-300 transition-colors">
                    {title}
                </span>
            </div>
            <p class="text-xs text-zinc-500 leading-relaxed">{desc}</p>
            <p class="text-xs font-mono text-amber-700 group-hover:text-amber-400 mt-3 transition-colors">
                "open →"
            </p>
        </A>
    }
}

#[component]
fn PipelineStep(num: u8, title: &'static str, desc: &'static str, last: bool) -> impl IntoView {
    view! {
        <div class="flex gap-4">
            <div class="flex flex-col items-center">
                <div class="h-8 w-8 shrink-0 rounded-full border border-amber-800 bg-amber-950
                            flex items-center justify-center">
                    <span class="text-xs font-mono font-bold text-amber-400">{num}</span>
                </div>
                {if !last { Some(view! {
                    <div class="w-px flex-1 my-1 bg-[#2a2a2a]"></div>
                }) } else { None }}
            </div>
            <div class="pb-6">
                <p class="text-sm font-semibold text-zinc-200">{title}</p>
                <p class="text-xs text-zinc-500 mt-1 leading-relaxed">{desc}</p>
            </div>
        </div>
    }
}

#[component]
fn RegimeCard(label: &'static str, cutoff: &'static str, desc: &'static str, bg: &'static str, border: &'static str) -> impl IntoView {
    view! {
        <div class=format!("rounded-lg border p-4 space-y-1.5 {} {}", bg, border)>
            <p class="text-sm font-semibold">{label}</p>
            <p class="text-[10px] font-mono text-zinc-600">{cutoff}</p>
            <p class="text-xs text-zinc-500 leading-relaxed">{desc}</p>
        </div>
    }
}

#[component]
fn StackCard(layer: &'static str, items: Vec<(&'static str, &'static str)>) -> impl IntoView {
    view! {
        <div class="rounded-xl border border-[#2e2e2e] bg-[#222] p-5 space-y-4">
            <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600">{layer}</p>
            <ul class="space-y-2.5">
                {items.into_iter().map(|(name, desc)| view! {
                    <li class="flex items-start gap-2.5">
                        <span class="mt-1.5 h-1 w-1 rounded-full bg-amber-600 shrink-0"></span>
                        <span class="text-xs text-zinc-500 leading-relaxed">
                            <span class="text-zinc-300 font-medium">{name}</span>
                            {" — "}{desc}
                        </span>
                    </li>
                }).collect_view()}
            </ul>
        </div>
    }
}
