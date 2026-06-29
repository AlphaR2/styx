use leptos::prelude::*;
use leptos_router::components::{A, Route, Router, Routes};
use leptos_router::path;

use crate::pages::{
    bundle::BundlePage,
    execute::ExecutePage,
    landing::LandingPage,
    log::LogPage,
    network::NetworkPage,
};

#[component]
pub fn App() -> impl IntoView {
    // Global session state — shared across Snipe and Deposit pages.
    let session_token: RwSignal<Option<String>> = RwSignal::new(None);
    let credits: RwSignal<u32> = RwSignal::new(0);
    provide_context(session_token);
    provide_context(credits);

    view! {
        <Router>
            <div class="relative min-h-screen bg-background">

                // Ambient amber radial
                <div class="pointer-events-none fixed inset-0 z-0
                            bg-[radial-gradient(ellipse_80%_35%_at_50%_-5%,rgba(245,158,11,0.1)_0%,transparent_100%)]">
                </div>

                // Grid texture
                <div class="pointer-events-none fixed inset-0 z-0 bg-grid opacity-100"></div>

                <TopNav session_token=session_token credits=credits/>

                <main class="relative z-10 mx-auto max-w-7xl px-6 py-10">
                    <Routes fallback=|| view! {
                        <p class="text-muted-foreground text-sm">"404 — page not found"</p>
                    }>
                        <Route path=path!("/")        view=LandingPage/>
                        <Route path=path!("/execute") view=ExecutePage/>
                        <Route path=path!("/log")     view=LogPage/>
                        <Route path=path!("/bundle/:id") view=BundlePage/>
                        <Route path=path!("/network") view=NetworkPage/>
                    </Routes>
                </main>

                <footer class="relative z-10 border-t border-[#252525] mt-16">
                    <div class="mx-auto max-w-7xl px-6 py-4 flex items-center justify-between">
                        <span class="text-xs text-zinc-600 font-mono">"STYX · Solana execution SDK"</span>
                        <span class="text-xs text-zinc-700 font-mono">"v0.1.0"</span>
                    </div>
                </footer>

            </div>
        </Router>
    }
}

#[component]
fn TopNav(
    session_token: RwSignal<Option<String>>,
    credits: RwSignal<u32>,
) -> impl IntoView {
    view! {
        <header class="sticky top-0 z-50 w-full">
            <div class="absolute inset-0 bg-[#1a1a1a]"></div>
            <div class="absolute bottom-0 left-0 right-0 h-px
                        bg-gradient-to-r from-transparent via-amber-500/40 to-transparent"></div>

            <div class="relative mx-auto max-w-7xl px-6 flex h-14 items-center gap-6">

                // Brand
                <div class="flex items-center gap-3 mr-2 shrink-0">
                    <div class="h-5 w-5 rotate-45 rounded-sm bg-amber-900/60 border border-amber-700/50
                                flex items-center justify-center glow-box">
                        <div class="h-1.5 w-1.5 rotate-45 rounded-[1px] bg-amber-400"></div>
                    </div>
                    <span class="font-mono font-bold text-sm tracking-widest text-amber-400 glow-text">
                        "STYX"
                    </span>
                    <span class="hidden md:block h-4 w-px bg-[#303030]"></span>
                    <span class="hidden md:block text-xs text-zinc-500 font-mono">"Solana execution SDK"</span>
                </div>

                // Nav links
                <nav class="flex items-center gap-0.5">
                    <NavLink href="/"        label="Home"/>
                    <NavLink href="/network" label="Mission Control"/>
                    <NavLink href="/execute" label="Execute"/>
                    <NavLink href="/log"     label="Log"/>
                </nav>

                // Right side: credits pill + live indicator
                <div class="ml-auto flex items-center gap-3">
                    {move || if session_token.get().is_some() {
                        view! {
                            <div class=move || format!(
                                "flex items-center gap-2 px-2.5 py-1 rounded-full text-xs font-mono border {}",
                                if credits.get() > 0 {
                                    "border-amber-800/60 bg-amber-950/60 text-amber-400"
                                } else {
                                    "border-red-900/60 bg-red-950/60 text-red-400"
                                }
                            )>
                                {move || format!("{} credit{}", credits.get(), if credits.get() == 1 { "" } else { "s" })}
                            </div>
                        }.into_any()
                    } else {
                        view! { <div></div> }.into_any()
                    }}
                    <div class="flex items-center gap-2 px-3 py-1 rounded-full border border-amber-800/60 bg-amber-950/80">
                        <span class="h-1.5 w-1.5 rounded-full bg-amber-400 animate-pulse"></span>
                        <span class="text-xs text-amber-400/80 font-mono tracking-wider">"LIVE"</span>
                    </div>
                </div>

            </div>
        </header>
    }
}

#[component]
fn NavLink(href: &'static str, label: &'static str) -> impl IntoView {
    view! {
        <A
            href=href
            attr:class="px-3 py-1.5 text-sm text-zinc-400 hover:text-zinc-100 hover:bg-[#262626] rounded-md transition-all duration-150"
        >
            {label}
        </A>
    }
}
