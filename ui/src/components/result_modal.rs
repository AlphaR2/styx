use leptos::prelude::*;
use leptos_router::hooks::use_navigate;

use crate::utils::short;

// A success modal shown after a bundle is submitted (execute or snipe).
// Two actions: "Watch real time" deep-links into the live bundle detail page;
// "Close" dismisses. Used by both the Execute and Snipe pages so the success
// affordance is identical everywhere.
#[component]
pub fn BundleModal(
    /// Bundle id to deep-link into the live detail page.
    bundle_id: String,
    /// Heading, e.g. "Bundle submitted" or "Snipe submitted".
    title: String,
    /// Regime chip text (e.g. "Hot"). Empty string hides the chip.
    #[prop(optional)] regime: String,
    /// Stat rows: (label, value, highlight). highlight=true tints the value amber.
    rows: Vec<(&'static str, String, bool)>,
    /// Optional explanatory note (the AI reasoning / a status message).
    #[prop(optional)] note: Option<String>,
    /// Called when the user dismisses the modal.
    on_close: Callback<()>,
) -> impl IntoView {
    let navigate = use_navigate();
    let bid = bundle_id.clone();
    let short_id = short(&bundle_id, 20);
    let regime_chip = (!regime.is_empty()).then(|| {
        let cls = regime_badge_class(&regime);
        view! {
            <span class=format!(
                "inline-flex items-center rounded-md border px-2.5 py-0.5 text-xs font-semibold {}", cls
            )>{regime}</span>
        }
    });

    let watch = {
        let navigate = navigate.clone();
        move |_| navigate(&format!("/bundle/{}", bid), Default::default())
    };

    view! {
        // Backdrop — click anywhere outside the card to close.
        <div
            class="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-sm p-4"
            on:click=move |_| on_close.run(())
        >
            // Card — stop propagation so inside clicks don't close it.
            <div
                class="w-full max-w-md rounded-2xl border border-[#2e2e2e] bg-[#1c1c1c] shadow-2xl overflow-hidden"
                on:click=|e| e.stop_propagation()
            >
                // Header
                <div class="flex items-center justify-between px-6 py-4 border-b border-[#2e2e2e] bg-[#1e1a10]">
                    <div class="flex items-center gap-3">
                        <span class="flex h-7 w-7 items-center justify-center rounded-full bg-amber-500/15 text-amber-400 text-sm">
                            "✓"
                        </span>
                        <div>
                            <p class="text-sm font-semibold text-zinc-100">{title}</p>
                            <p class="text-[11px] font-mono text-zinc-600">{short_id}{"…"}</p>
                        </div>
                    </div>
                    {regime_chip}
                </div>

                // Stat rows
                <div class="divide-y divide-[#262626]">
                    {rows.into_iter().map(|(label, value, hi)| {
                        let v_cls = if hi { "text-sm font-mono font-semibold text-amber-400" }
                                    else { "text-sm font-mono text-zinc-300" };
                        view! {
                            <div class="flex items-center justify-between px-6 py-2.5">
                                <span class="text-xs text-zinc-500">{label}</span>
                                <span class=v_cls>{value}</span>
                            </div>
                        }
                    }).collect_view()}
                </div>

                // Optional note
                {note.map(|n| view! {
                    <div class="px-6 py-3 border-t border-[#262626] bg-[#1a1a1a]">
                        <p class="text-xs text-zinc-500 leading-relaxed">{n}</p>
                    </div>
                })}

                // Actions
                <div class="flex gap-3 px-6 py-4 border-t border-[#2e2e2e]">
                    <button
                        on:click=watch
                        class="flex-1 inline-flex items-center justify-center gap-2 rounded-lg px-4 py-2.5
                               bg-amber-500 text-black font-semibold text-sm hover:bg-amber-400
                               active:scale-[0.98] transition-all glow-box"
                    >
                        <span class="text-base">"▶"</span>
                        "Watch real time"
                    </button>
                    <button
                        on:click=move |_| on_close.run(())
                        class="rounded-lg px-4 py-2.5 border border-[#2e2e2e] bg-[#222]
                               text-zinc-300 font-medium text-sm hover:border-zinc-600 transition-all"
                    >
                        "Close"
                    </button>
                </div>
            </div>
        </div>
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
