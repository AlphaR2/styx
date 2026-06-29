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
                    "The smart transaction sender for Solana"
                </h1>
                <p class="text-base text-zinc-400 mt-5 max-w-2xl leading-relaxed">
                    "Styx lands your transactions for the lowest competitive cost. It reads live "
                    "tip-floor contention, asks Claude to size the inclusion fee to the moment, submits — as a "
                    "Jito bundle or a priority-fee transaction — and re-prices on every retry until it confirms, "
                    "so you never overpay on a quiet slot or get left behind in a manic one."
                </p>
                <div class="flex flex-wrap gap-3 mt-7">
                    <A href="/execute" attr:class="inline-flex items-center gap-2 rounded-lg px-5 py-2.5
                        bg-amber-500 text-black font-semibold text-sm hover:bg-amber-400
                        active:scale-[0.98] transition-all glow-box">
                        <span class="font-mono">"⚡"</span>
                        "Send a transaction"
                    </A>
                </div>
            </div>

            // ── Scenario cards ───────────────────────────────────────────
            <div>
                <p class="text-[10px] font-mono uppercase tracking-widest text-zinc-600 mb-4">
                    "Try it"
                </p>
                <div class="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-4">
                    <ScenarioCard
                        href="/execute" icon="⚡" title="Execute a transaction"
                        desc="Run the full pipeline on a memo (always lands) or a real Jupiter SOL→USDC swap, via a Jito bundle or a priority fee. Watch fee selection and retries stream live."
                    />
                    <ScenarioCard
                        href="/network" icon="📡" title="Mission Control"
                        desc="Live slots, leader windows, fee pressure, and transactions moving through the network in real time — the command center."
                    />
                    <ScenarioCard
                        href="/log" icon="📑" title="Execution log"
                        desc="Every execution Styx has sent: fee paid, savings vs. a naive overpayer, regime, retries, and final landing slot."
                    />
                </div>
            </div>

            // Divider
            <div class="h-px bg-[#272727]"></div>

            // ── Pipeline ─────────────────────────────────────────────────
            <div>
                <h2 class="text-lg font-semibold mb-1">"How a transaction gets sent"</h2>
                <p class="text-sm text-zinc-500 mb-6">
                    "Every call — memo or swap — runs the same five stages."
                </p>
                <div class="space-y-0">
                    <PipelineStep num=1 title="Ingest"
                        desc="Yellowstone gRPC streams slot updates; the Jito REST API streams tip-floor percentiles, both in real time."
                        last=false/>
                    <PipelineStep num=2 title="Classify"
                        desc="Claude reads the live contention snapshot and labels the regime: Cold, Warm, Hot, or Manic."
                        last=false/>
                    <PipelineStep num=3 title="Bid"
                        desc="The regime maps to a fee percentile, then the lamport fee is clamped between the observed floor and a hard ceiling."
                        last=false/>
                    <PipelineStep num=4 title="Submit"
                        desc="Your instructions are wrapped with a compute budget and the fee, signed, and submitted — as a Jito bundle, or a priority-fee transaction over standard RPC."
                        last=false/>
                    <PipelineStep num=5 title="Track & retry"
                        desc="Lifecycle is followed Submitted → Processed → Confirmed → Finalized; if it stalls, Styx re-prices upward and resubmits."
                        last=true/>
                </div>
            </div>

            // Divider
            <div class="h-px bg-[#272727]"></div>

            // ── Regime reference ─────────────────────────────────────────
            <div>
                <h2 class="text-lg font-semibold mb-1">"Reading contention"</h2>
                <p class="text-sm text-zinc-500 mb-5">
                    "The tip is never hardcoded — it tracks how crowded the chain is right now."
                </p>
                <div class="h-2 w-full rounded-full overflow-hidden
                            bg-gradient-to-r from-zinc-700 via-amber-600 via-orange-500 to-red-600 mb-5">
                </div>
                <div class="grid grid-cols-2 sm:grid-cols-4 gap-3">
                    <RegimeCard regime="Quiet network"     label="Cold"  desc="Very little competition. Styx bids conservatively and saves the most here."
                        bg="bg-zinc-900"    border="border-zinc-800"/>
                    <RegimeCard regime="Normal traffic"    label="Warm"  desc="Typical on-chain activity. A moderate fee lands comfortably within a few blocks."
                        bg="bg-amber-950"   border="border-amber-900"/>
                    <RegimeCard regime="Heavy competition" label="Hot"   desc="High validator demand. Styx bids aggressively to stay ahead of the queue."
                        bg="bg-orange-950"  border="border-orange-900"/>
                    <RegimeCard regime="Extreme congestion" label="Manic" desc="Network overloaded. Maximum bid deployed — retries may still be needed."
                        bg="bg-red-950"     border="border-red-900"/>
                </div>
            </div>

            // Divider
            <div class="h-px bg-[#272727]"></div>

            // ── Stack ─────────────────────────────────────────────────────
            <div>
                <h2 class="text-lg font-semibold mb-4">"Built with"</h2>
                <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
                    <StackCard layer="Execution" items=vec![
                        ("Solana SDK",  "transaction construction + RPC"),
                        ("Jito SDK",    "bundle submission + tip floor"),
                        ("Jupiter v6",  "real swap routing + lookup tables"),
                        ("Axum",        "local HTTP API + WebSocket relay"),
                    ]/>
                    <StackCard layer="Intelligence" items=vec![
                        ("LLM classifier",   "reads live tip floor data, classifies network congestion"),
                        ("Bid optimizer",    "maps congestion level to an optimal fee percentile"),
                        ("Traffic levels",   "Quiet / Normal / Busy / Extreme"),
                    ]/>
                    <StackCard layer="Frontend" items=vec![
                        ("Leptos 0.8",     "Rust WASM reactive UI framework"),
                        ("Trunk",          "WASM bundler + live-reload server"),
                        ("Tailwind CSS 4", "utility-first design system"),
                    ]/>
                    <StackCard layer="Infrastructure" items=vec![
                        ("Cargo workspace", "shared types across all crates"),
                        ("Yellowstone",     "gRPC slot + transaction stream"),
                        ("Mainnet-Beta",    "Solana + Jito block engine"),
                    ]/>
                </div>
            </div>

        </div>
    }
}

#[component]
fn ScenarioCard(href: &'static str, icon: &'static str, title: &'static str, desc: &'static str) -> impl IntoView {
    view! {
        <A href=href attr:class="group block rounded-xl border border-[#2e2e2e] bg-[#222] p-5
            hover:border-amber-700/60 hover:bg-[#262320] transition-all">
            <div class="flex items-center gap-3 mb-2">
                <span class="text-xl">{icon}</span>
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
fn RegimeCard(regime: &'static str, label: &'static str, desc: &'static str, bg: &'static str, border: &'static str) -> impl IntoView {
    view! {
        <div class=format!("rounded-lg border p-4 space-y-1.5 {} {}", bg, border)>
            <p class="text-sm font-semibold">{regime}</p>
            <p class="text-[10px] font-mono uppercase tracking-wider text-zinc-600 -mt-0.5">{label}</p>
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
