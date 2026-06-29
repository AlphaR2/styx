use anyhow::{Context, Result};

// Which wire protocol the configured LLM speaks.
#[derive(Clone, Copy, PartialEq)]
pub enum LlmKind {
    /// Anthropic Messages API (x-api-key header, top-level system field).
    Anthropic,
    /// Any OpenAI-compatible /chat/completions endpoint — Together, OpenAI,
    /// Groq, OpenRouter, Fireworks, DeepSeek, a local vLLM/Ollama, etc.
    OpenAiCompatible,
}

// "Bring your own LLM" config. Judges plug in whatever provider they have.
#[derive(Clone)]
pub struct LlmConfig {
    pub kind: LlmKind,
    pub base_url: String, // full endpoint URL
    pub api_key: String,
    pub model: String,
}

// All runtime configuration lives here. Nothing is hardcoded in the binary.
pub struct Config {
    pub rpc_url: String,                     // Solana RPC endpoint for blockhash and leader schedule
    pub yellowstone_endpoint: String,        // Yellowstone gRPC URL for slot and tx streaming
    pub yellowstone_token: String,           // Auth token for the Yellowstone endpoint
    pub llm: Option<LlmConfig>,             // AI tip optimizer; None = baseline-only
    pub tip_ceiling_lamports: u64,           // Hard cap so a broken agent can never overbid
    pub jito_block_engine_urls: Vec<String>, // Jito regional HTTP endpoints; bundles fanned out to all
}

// All Jito mainnet regional block engines. A bundle only lands if the leader for
// the window is reachable from the engine it was sent to, so we submit to all of
// them concurrently and let whichever engine feeds the leader include it.
const DEFAULT_JITO_ENGINES: [&str; 5] = [
    "https://amsterdam.mainnet.block-engine.jito.wtf/api/v1",
    "https://frankfurt.mainnet.block-engine.jito.wtf/api/v1",
    "https://ny.mainnet.block-engine.jito.wtf/api/v1",
    "https://tokyo.mainnet.block-engine.jito.wtf/api/v1",
    "https://slc.mainnet.block-engine.jito.wtf/api/v1",
];

impl Config {
    pub fn from_env() -> Result<Self> {
        // Load .env file if present. Silently ignored if missing (env vars may already be set).
        dotenv::dotenv().ok();

        // ── Bring-your-own-LLM ────────────────────────────────────────────────
        // Set LLM_BASE_URL + LLM_API_KEY + LLM_MODEL to enable the AI tip optimizer.
        // Any provider works — Together, OpenAI, Groq, Anthropic, a local model, etc.
        // Without these vars the optimizer runs baseline-only (no error at startup).
        //
        // LLM_KIND controls the wire protocol (default: openai-compatible):
        //   openai-compatible  — Bearer auth, /chat/completions shape (most providers)
        //   anthropic          — x-api-key auth, /v1/messages shape
        let llm = if let Ok(base_url) = std::env::var("LLM_BASE_URL") {
            let api_key = std::env::var("LLM_API_KEY").unwrap_or_default();
            let model = std::env::var("LLM_MODEL").unwrap_or_default();
            let kind = match std::env::var("LLM_KIND").as_deref() {
                Ok("anthropic") => LlmKind::Anthropic,
                _ => LlmKind::OpenAiCompatible,
            };
            Some(LlmConfig { kind, base_url, api_key, model })
        } else {
            None
        };

        Ok(Config {
            rpc_url: std::env::var("RPC_URL").context("RPC_URL not set")?,

            yellowstone_endpoint: std::env::var("YELLOWSTONE_ENDPOINT")
                .context("YELLOWSTONE_ENDPOINT not set")?,

            yellowstone_token: std::env::var("YELLOWSTONE_TOKEN")
                .context("YELLOWSTONE_TOKEN not set")?,

            llm,

            // Defaults to 500,000 lamports (0.0005 SOL) if not set in env.
            tip_ceiling_lamports: std::env::var("TIP_CEILING_LAMPORTS")
                .unwrap_or_else(|_| "500000".to_string())
                .parse()
                .context("TIP_CEILING_LAMPORTS must be a u64")?,

            // Bundle submission endpoints. Precedence:
            //   1. JITO_BLOCK_ENGINE_URLS — comma-separated list (fanned out to all).
            //   2. JITO_BLOCK_ENGINE_URL  — single region (back-compat override).
            //   3. default: all known mainnet regions (best landing probability).
            jito_block_engine_urls: {
                if let Ok(list) = std::env::var("JITO_BLOCK_ENGINE_URLS") {
                    list.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                } else if let Ok(one) = std::env::var("JITO_BLOCK_ENGINE_URL") {
                    vec![one]
                } else {
                    DEFAULT_JITO_ENGINES.iter().map(|s| s.to_string()).collect()
                }
            },
        })
    }
}
