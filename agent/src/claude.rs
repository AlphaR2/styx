use std::future::Future;
use std::pin::Pin;

use anyhow::{Context, Result};
use tracing::{info, warn};

use styx_core::bid::{
    AgentOutput, BidContext, BidStrategy, Regime,
    RetryAction, RetryAdvice, RetryAdvisor, RetrySignal,
};
use styx_core::config::{LlmConfig, LlmKind};

use crate::baseline::OverpayerBaseline;

// System prompt for the initial tip decision. The model sees live auction data
// (empirical clearing prices from Jito tip account balance deltas) and its own
// recent win/loss history, and outputs a forward_multiplier.
//
// forward_multiplier semantics:
//   1.0 = bid at (clearing_price_min * safety_margin)
//   1.5 = bid 50% above that level
//   2.0 = double
//   0.8 = under-bid the baseline (risky, only in Cold regime)
//
// The model never emits lamports. compute_tip() converts the multiplier to lamports
// and applies value caps and ceiling, so the model's judgment shapes the bid
// within a pre-approved safe envelope.
const SYSTEM_PROMPT: &str = r#"
You are the tip strategist for a Solana MEV stack that submits Jito bundles. For each
submission you decide how aggressively to bid for block inclusion using a forward_multiplier
that scales the empirical baseline derived from live Jito tip auction data.

You receive:
- auction: live clearing prices (min/median/max) from the last 20 slots, bundles/slot, trend, regime
- tx: transaction type (Snipe/Swap/Arb/Memo), economic value in lamports, tip ceiling
- outcomes: your last 10 bundle results (tip paid, whether it landed, multiplier used, clearing price)

The compute_tip formula is: baseline * safety_margin * forward_multiplier, clamped to value cap.
Safety margins by regime: Cold=1.05, Warm=1.10, Hot=1.20, Manic=1.50.
So forward_multiplier=1.0 already includes the safety buffer -- you are deciding how far ABOVE
that level to bid based on competition signals.

Calibration guide:
- multiplier < 1.0 : under-bid the safety level (only safe in Cold with recent wins at low multipliers)
- multiplier 1.0   : match baseline+safety -- the minimum competitive bid
- multiplier 1.0-1.5: normal winning range for Warm/Hot
- multiplier 1.5-2.5: aggressive, appropriate for Manic or Rising trend
- multiplier > 2.5 : anomalous overpay (only if Manic + repeated drops at lower levels)

Use outcomes to self-calibrate: if recent bundles landed at 1.1x, you don't need 1.5x.
If recent bundles dropped at 1.3x, escalate.

Your PRIMARY objective is to land the bundle. Saving lamports is secondary.
For Snipe txs: landing is critical -- err high. For Memo: cost matters more.

Output ONLY valid JSON, no markdown, no extra text:
{"regime":"Hot","forward_multiplier":1.3,"reasoning":"...","confidence":0.85}
"#;

// System prompt for retry decisions.
const RETRY_PROMPT: &str = r#"
You are the recovery strategist for a Solana Jito bundle stack. A bundle just failed.
You receive: failure_kind, attempt, previous_tip_lamports, previous_forward_multiplier,
the live auction window (clearing prices, regime, trend), and the raw error.

Escalation guide for forward_multiplier (always relative to clearing_price_min * safety):
- ExpiredBlockhash: refresh blockhash; hold or slightly raise multiplier (competition hasn't changed).
- Dropped: de-prioritized in the auction -- raise by at least +0.3, more on consecutive drops.
- FeeTooLow: directly outbid -- raise aggressively toward 2.0+.
- ComputeExceeded / BundleFailure: payload broken -- abort.

On attempt >= 2, raise more aggressively than on attempt 1.
On attempt 3, go to 2.0+ or abort if clearly futile.

Output ONLY valid JSON:
{"action":"retry","forward_multiplier":1.7,"refresh_blockhash":true,"reasoning":"...","confidence":0.85}
"#;

const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct LlmClassifier {
    provider: Option<LlmConfig>,
    client: reqwest::Client,
    baseline: OverpayerBaseline,
}

#[derive(serde::Deserialize)]
struct ModelOutput {
    regime: String,
    forward_multiplier: f64,
    reasoning: String,
    confidence: f64,
}

impl LlmClassifier {
    pub fn new(provider: Option<LlmConfig>) -> Self {
        match &provider {
            Some(cfg) => {
                let kind = match cfg.kind {
                    LlmKind::Anthropic => "anthropic",
                    LlmKind::OpenAiCompatible => "openai-compatible",
                };
                info!(provider = kind, model = %cfg.model, "AI tip optimizer configured");
            }
            None => warn!("no LLM configured -- using baseline only"),
        }
        LlmClassifier {
            provider,
            client: reqwest::Client::new(),
            baseline: OverpayerBaseline::new(),
        }
    }

    pub async fn decide(&self, ctx: &BidContext) -> AgentOutput {
        let Some(provider) = &self.provider else {
            let mut out = self.baseline.decide(ctx);
            out.reasoning = "AI tip optimizer not configured -- used a safe default tip".to_string();
            out.confidence = 0.0;
            return out;
        };

        match self.call_api(provider, ctx).await {
            Ok(output) => output,
            Err(e) => {
                warn!("LLM classify failed ({}), falling back to baseline", e);
                let mut out = self.baseline.decide(ctx);
                let raw = e.to_string().to_lowercase();
                let friendly = if raw.contains("401") || raw.contains("authentication") || raw.contains("unauthorized") {
                    "AI tip optimizer offline (API key invalid) -- used a safe default tip"
                } else if raw.contains("timed out") || raw.contains("timeout") {
                    "AI tip optimizer timed out -- used a safe default tip"
                } else if raw.contains("429") || raw.contains("rate") {
                    "AI tip optimizer rate-limited -- used a safe default tip"
                } else {
                    "AI tip optimizer unavailable -- used a safe default tip"
                };
                out.reasoning = friendly.to_string();
                out.confidence = 0.0;
                out
            }
        }
    }
}

impl BidStrategy for LlmClassifier {
    fn bid<'a>(
        &'a self,
        ctx: BidContext,
    ) -> Pin<Box<dyn Future<Output = AgentOutput> + Send + 'a>> {
        Box::pin(async move { self.decide(&ctx).await })
    }
}

impl LlmClassifier {
    async fn call_api(&self, provider: &LlmConfig, ctx: &BidContext) -> Result<AgentOutput> {
        let user_content = serde_json::to_string(ctx).context("failed to serialize BidContext")?;

        let req = match provider.kind {
            LlmKind::Anthropic => self
                .client
                .post(&provider.base_url)
                .header("x-api-key", &provider.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
                .json(&serde_json::json!({
                    "model": provider.model,
                    "max_tokens": 256,
                    "system": SYSTEM_PROMPT.trim(),
                    "messages": [{ "role": "user", "content": user_content }],
                })),
            LlmKind::OpenAiCompatible => self
                .client
                .post(&provider.base_url)
                .header("authorization", format!("Bearer {}", provider.api_key))
                .header("content-type", "application/json")
                .json(&serde_json::json!({
                    "model": provider.model,
                    "max_tokens": 256,
                    "temperature": 0.2,
                    "response_format": { "type": "json_object" },
                    "messages": [
                        { "role": "system", "content": SYSTEM_PROMPT.trim() },
                        { "role": "user",   "content": user_content },
                    ],
                })),
        };

        info!(user_content = %user_content, "-> LLM tip-decision request");

        let resp = req.send().await.context("LLM API request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("LLM API returned {}: {}", status, text);
        }

        let value: serde_json::Value = resp.json().await.context("failed to parse API response")?;
        let content = match provider.kind {
            LlmKind::Anthropic => value["content"][0]["text"].as_str(),
            LlmKind::OpenAiCompatible => value["choices"][0]["message"]["content"].as_str(),
        }
        .context("no text content in API response")?;

        info!(response = %content, "<- LLM tip-decision response");

        let json_str = extract_json(content).unwrap_or(content);
        let parsed: ModelOutput =
            serde_json::from_str(json_str).context("failed to parse model JSON output")?;

        let regime = match parsed.regime.as_str() {
            "Cold"  => Regime::Cold,
            "Warm"  => Regime::Warm,
            "Hot"   => Regime::Hot,
            "Manic" => Regime::Manic,
            other => {
                warn!("unknown regime '{}' from model, using window regime", other);
                ctx.window.regime.clone()
            }
        };

        let forward_multiplier = parsed.forward_multiplier.clamp(0.1, 10.0);

        Ok(AgentOutput {
            regime,
            forward_multiplier,
            reasoning: parsed.reasoning,
            confidence: parsed.confidence.clamp(0.0, 1.0),
        })
    }
}

fn extract_json(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end > start { Some(&s[start..=end]) } else { None }
}

// ---- Transaction summary ----

const SUMMARY_SYSTEM: &str = r#"
You are a Solana transaction analyst. You will be given raw on-chain transaction data
and execution metrics from the Styx submission SDK. Analyse both and return a JSON object
with exactly these fields:

- verdict: "Success", "Failed", or "Pending"
- transaction_analysis: what this transaction actually did on-chain
- what_happened: submission outcome
- fee_analysis: fee paid vs market and bid optimality
- performance: Styx overall performance rating
- timing: on-chain commitment timeline

Respond with ONLY valid JSON, no markdown, no extra text:
{"verdict":"...","transaction_analysis":"...","what_happened":"...","fee_analysis":"...","performance":"...","timing":"..."}
"#;

impl LlmClassifier {
    pub async fn summarize(&self, context: &str) -> Result<String> {
        let Some(provider) = &self.provider else {
            anyhow::bail!("no LLM configured");
        };

        let req = match provider.kind {
            LlmKind::Anthropic => self
                .client
                .post(&provider.base_url)
                .header("x-api-key", &provider.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
                .json(&serde_json::json!({
                    "model": provider.model,
                    "max_tokens": 512,
                    "system": SUMMARY_SYSTEM.trim(),
                    "messages": [{ "role": "user", "content": context }],
                })),
            LlmKind::OpenAiCompatible => self
                .client
                .post(&provider.base_url)
                .header("authorization", format!("Bearer {}", provider.api_key))
                .header("content-type", "application/json")
                .json(&serde_json::json!({
                    "model": provider.model,
                    "max_tokens": 512,
                    "temperature": 0.3,
                    "response_format": { "type": "json_object" },
                    "messages": [
                        { "role": "system", "content": SUMMARY_SYSTEM.trim() },
                        { "role": "user",   "content": context },
                    ],
                })),
        };

        info!(context = %context, "-> LLM summary request");

        let resp = req.send().await.context("summary LLM request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("summary LLM returned {}: {}", status, text);
        }
        let value: serde_json::Value = resp.json().await.context("summary parse failed")?;
        let content = match provider.kind {
            LlmKind::Anthropic => value["content"][0]["text"].as_str(),
            LlmKind::OpenAiCompatible => value["choices"][0]["message"]["content"].as_str(),
        }
        .context("no content in summary response")?;

        info!(response = %content, "<- LLM summary response");
        Ok(extract_json(content).unwrap_or(content).to_string())
    }
}

// ---- Retry advisor ----

#[derive(serde::Deserialize)]
struct ModelRetryOutput {
    action: String,
    forward_multiplier: f64,
    #[serde(default)]
    refresh_blockhash: bool,
    reasoning: String,
    #[serde(default)]
    confidence: f64,
}

fn baseline_advice(signal: &RetrySignal) -> RetryAdvice {
    match signal.failure_kind.as_str() {
        "BundleFailure" | "ComputeExceeded" => RetryAdvice {
            action: RetryAction::Abort,
            forward_multiplier: signal.previous_forward_multiplier,
            refresh_blockhash: false,
            reasoning: "Bundle failed atomically; the same payload would fail again.".to_string(),
            confidence: 0.0,
        },
        "ExpiredBlockhash" | "Dropped" => RetryAdvice {
            action: RetryAction::Retry,
            forward_multiplier: (signal.previous_forward_multiplier + 0.2).min(3.0),
            refresh_blockhash: true,
            reasoning: "Stale or dropped -- refresh blockhash and raise multiplier slightly.".to_string(),
            confidence: 0.0,
        },
        "FeeTooLow" => RetryAdvice {
            action: RetryAction::Retry,
            forward_multiplier: (signal.previous_forward_multiplier + 0.5).min(3.0),
            refresh_blockhash: true,
            reasoning: "Outbid -- raise multiplier aggressively.".to_string(),
            confidence: 0.0,
        },
        _ => RetryAdvice {
            action: RetryAction::Retry,
            forward_multiplier: (signal.previous_forward_multiplier + 0.3).min(3.0),
            refresh_blockhash: true,
            reasoning: "Transient failure -- refresh and retry with a raised multiplier.".to_string(),
            confidence: 0.0,
        },
    }
}

impl RetryAdvisor for LlmClassifier {
    fn advise<'a>(
        &'a self,
        signal: RetrySignal,
    ) -> Pin<Box<dyn Future<Output = RetryAdvice> + Send + 'a>> {
        Box::pin(async move { self.advise_inner(signal).await })
    }
}

impl LlmClassifier {
    async fn advise_inner(&self, signal: RetrySignal) -> RetryAdvice {
        let Some(provider) = &self.provider else {
            return baseline_advice(&signal);
        };
        match self.call_retry_api(provider, &signal).await {
            Ok(advice) => advice,
            Err(e) => {
                warn!("retry advisor LLM failed ({}), using baseline", e);
                baseline_advice(&signal)
            }
        }
    }

    async fn call_retry_api(&self, provider: &LlmConfig, signal: &RetrySignal) -> Result<RetryAdvice> {
        let user_content = serde_json::to_string(signal).context("failed to serialize RetrySignal")?;

        let req = match provider.kind {
            LlmKind::Anthropic => self
                .client
                .post(&provider.base_url)
                .header("x-api-key", &provider.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
                .json(&serde_json::json!({
                    "model": provider.model,
                    "max_tokens": 256,
                    "system": RETRY_PROMPT.trim(),
                    "messages": [{ "role": "user", "content": user_content }],
                })),
            LlmKind::OpenAiCompatible => self
                .client
                .post(&provider.base_url)
                .header("authorization", format!("Bearer {}", provider.api_key))
                .header("content-type", "application/json")
                .json(&serde_json::json!({
                    "model": provider.model,
                    "max_tokens": 256,
                    "temperature": 0.2,
                    "response_format": { "type": "json_object" },
                    "messages": [
                        { "role": "system", "content": RETRY_PROMPT.trim() },
                        { "role": "user",   "content": user_content },
                    ],
                })),
        };

        info!(user_content = %user_content, "-> LLM retry-decision request");

        let resp = req.send().await.context("retry LLM request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("retry LLM returned {}: {}", status, text);
        }

        let value: serde_json::Value =
            resp.json().await.context("failed to parse retry API response")?;
        let content = match provider.kind {
            LlmKind::Anthropic => value["content"][0]["text"].as_str(),
            LlmKind::OpenAiCompatible => value["choices"][0]["message"]["content"].as_str(),
        }
        .context("no text content in retry API response")?;

        info!(response = %content, "<- LLM retry-decision response");

        let json_str = extract_json(content).unwrap_or(content);
        let parsed: ModelRetryOutput =
            serde_json::from_str(json_str).context("failed to parse retry JSON output")?;

        let action = match parsed.action.to_lowercase().as_str() {
            "abort" => RetryAction::Abort,
            _ => RetryAction::Retry,
        };

        Ok(RetryAdvice {
            action,
            forward_multiplier: parsed.forward_multiplier.clamp(0.1, 10.0),
            refresh_blockhash: parsed.refresh_blockhash,
            reasoning: parsed.reasoning,
            confidence: parsed.confidence.clamp(0.0, 1.0),
        })
    }
}
