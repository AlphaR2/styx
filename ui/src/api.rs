// All types here mirror the backend exactly so serde can deserialise API
// responses directly into these structs without any manual parsing.

use gloo_net::http::Request;
use serde::Deserialize;

// ── Log ──────────────────────────────────────────────────────────────────

#[derive(Deserialize, Clone, Debug)]
pub struct LogEntry {
    pub bundle_id: String,
    #[serde(default = "default_lane")]
    pub lane: String,
    pub tip_lamports: u64,
    #[serde(default)]
    pub landed_tip_lamports: Option<u64>,
    pub baseline_tip_lamports: u64,
    pub delta_lamports: i64,
    pub regime: String,
    pub forward_multiplier: f64,
    pub reasoning: String,
    pub confidence: f64,
    pub submitted_at_ms: u64,
    pub landed_bundle_id: Option<String>,
    pub landing_slot: Option<u64>,
    pub processed_at_ms: Option<u64>,
    pub confirmed_at_ms: Option<u64>,
    pub finalized_at_ms: Option<u64>,
    pub failure_kind: Option<String>,
    pub retry_count: u32,
    #[serde(default)]
    pub tx_signatures: Vec<String>,
}

fn default_lane() -> String { "JitoBundle".to_string() }

// ── Execute ───────────────────────────────────────────────────────────────

#[derive(Deserialize, Clone, Debug)]
pub struct ExecuteResponse {
    pub bundle_id: String,
    pub tip_lamports: u64,
    pub baseline_tip_lamports: u64,
    pub delta_lamports: i64,
    pub regime: String,
    pub forward_multiplier: f64,
    pub reasoning: String,
    pub confidence: f64,
    pub solscan_url: String,
    pub lane: String,
}

// ── Session / deposit ─────────────────────────────────────────────────────

#[derive(Deserialize, Clone, Debug)]
pub struct SessionResponse {
    pub session_token: String,
    pub credits: u32,
    pub deposit_lamports: u64,
    pub created_at_ms: u64,
    pub deposit_address: String,
}

// ── WebSocket events ──────────────────────────────────────────────────────

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsEvent {
    SlotUpdate { slot: u64, parent: Option<u64>, status: SlotStatus },
    TxSeen     { sig: String, slot: u64 },
    JitoTip    { slot: u64, tip_lamports: u64, ts_ms: u64 },
    Execution {
        bundle_id: String,
        stage: String,
        tip_lamports: u64,
        retry: u32,
        regime: String,
        message: String,
        ts_ms: u64,
    },
    ExecLog {
        bundle_id: String,
        level: String,
        target: String,
        message: String,
        ts_ms: u64,
    },
    Lagged { dropped: u64 },
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SlotStatus { Processed, Confirmed, Finalized }

impl std::fmt::Display for SlotStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SlotStatus::Processed => write!(f, "processed"),
            SlotStatus::Confirmed => write!(f, "confirmed"),
            SlotStatus::Finalized => write!(f, "finalized"),
        }
    }
}

// ── HTTP helpers ──────────────────────────────────────────────────────────

pub async fn fetch_log() -> Result<Vec<LogEntry>, String> {
    Request::get("/api/log")
        .send().await.map_err(|e| e.to_string())?
        .json::<Vec<LogEntry>>().await.map_err(|e| e.to_string())
}

/// Fetch the server-side event replay buffer for a bundle.
/// Returns all ExecLog and Execution events that fired before the WS connected.
pub async fn fetch_bundle_events(bundle_id: &str) -> Result<Vec<WsEvent>, String> {
    Request::get(&format!("/api/bundle/{}/events", bundle_id))
        .send().await.map_err(|e| e.to_string())?
        .json::<Vec<WsEvent>>().await.map_err(|e| e.to_string())
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct AiSummary {
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub transaction_analysis: String,
    #[serde(default)]
    pub what_happened: String,
    #[serde(default)]
    pub fee_analysis: String,
    #[serde(default)]
    pub performance: String,
    #[serde(default)]
    pub timing: String,
    // Present when the backend couldn't generate a summary.
    #[serde(default)]
    pub error: String,
}

pub async fn fetch_summary(bundle_id: &str) -> Result<AiSummary, String> {
    Request::get(&format!("/api/bundle/{}/summary", bundle_id))
        .send().await.map_err(|e| e.to_string())?
        .json::<AiSummary>().await.map_err(|e| e.to_string())
}

/// Submit the Execute smoke test. `scenario` is "memo" (always lands), "jupiter"
/// (a real SOL→USDC swap), or "fault" (priority-fee lane only: stale-blockhash recovery).
pub async fn post_execute(
    scenario: &str,
    sol_amount_lamports: Option<u64>,
    lane: &str,
) -> Result<ExecuteResponse, String> {
    let body = serde_json::json!({
        "scenario": scenario,
        "sol_amount_lamports": sol_amount_lamports,
        "lane": lane,
    });
    let resp = Request::post("/api/execute")
        .header("Content-Type", "application/json")
        .body(body.to_string()).map_err(|e| e.to_string())?
        .send().await.map_err(|e| e.to_string())?;
    if !resp.ok() {
        let msg = resp.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<serde_json::Value>(&msg)
            .ok()
            .and_then(|v| v["error"].as_str().map(String::from))
            .unwrap_or_else(|| format!("server returned an error: {}", msg));
        return Err(detail);
    }
    resp.json::<ExecuteResponse>().await.map_err(|e| e.to_string())
}

// ── Leader schedule ───────────────────────────────────────────────────────

#[derive(Deserialize, Clone, Debug)]
pub struct LeaderSlot {
    pub slot: u64,
    pub leader: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct LeaderSchedule {
    pub current_slot: u64,
    pub leaders: Vec<LeaderSlot>,
}

pub async fn fetch_leaders() -> Result<LeaderSchedule, String> {
    Request::get("/api/leader")
        .send().await.map_err(|e| e.to_string())?
        .json::<LeaderSchedule>().await.map_err(|e| e.to_string())
}

// ── Tip floor ─────────────────────────────────────────────────────────────

/// Current tip-floor snapshot, used to seed the fee tiles on load and as a poll
/// fallback so they stay populated even if a live WebSocket push is missed.
#[derive(Deserialize, Clone, Debug)]
pub struct TipFloorSnapshot {
    pub clearing_price_min: u64,
    pub clearing_price_median: u64,
    pub clearing_price_max: u64,
    #[serde(default)]
    pub bundles_per_slot: f64,
    #[serde(default)]
    pub is_bootstrapped: bool,
}

pub async fn fetch_tip_floor() -> Result<TipFloorSnapshot, String> {
    Request::get("/api/tip_floor")
        .send().await.map_err(|e| e.to_string())?
        .json::<TipFloorSnapshot>().await.map_err(|e| e.to_string())
}

pub async fn post_bypass(code: &str) -> Result<SessionResponse, String> {
    let body = serde_json::json!({"code": code});
    let resp = Request::post("/api/bypass")
        .header("Content-Type", "application/json")
        .body(body.to_string()).map_err(|e| e.to_string())?
        .send().await.map_err(|e| e.to_string())?;
    if !resp.ok() {
        return Err("Invalid bypass code".to_string());
    }
    resp.json::<SessionResponse>().await.map_err(|e| e.to_string())
}

pub async fn post_deposit_claim(session_token: &str) -> Result<SessionResponse, String> {
    let body = serde_json::json!({"session_token": session_token});
    let resp = Request::post("/api/deposit/claim")
        .header("Content-Type", "application/json")
        .body(body.to_string()).map_err(|e| e.to_string())?
        .send().await.map_err(|e| e.to_string())?;
    resp.json::<SessionResponse>().await.map_err(|e| e.to_string())
}

pub async fn fetch_session(token: &str) -> Result<SessionResponse, String> {
    let resp = Request::get(&format!("/api/session/{}", token))
        .send().await.map_err(|e| e.to_string())?;
    if !resp.ok() { return Err("session not found".to_string()); }
    resp.json::<SessionResponse>().await.map_err(|e| e.to_string())
}

// ── WebSocket ─────────────────────────────────────────────────────────────

/// Build the absolute WebSocket URL for the live event stream.
///
/// A relative `"/ws"` is NOT reliably resolved by the browser's WebSocket
/// constructor (unlike `fetch`), which is why the live pages silently failed to
/// connect. We derive `ws(s)://<host>/ws` from the current page location so the
/// scheme matches (https → wss) and the trunk/axum proxy on the same origin is hit.
pub fn ws_url() -> String {
    let loc = web_sys::window().expect("no window").location();
    let proto = loc.protocol().unwrap_or_else(|_| "http:".to_string());
    let host = loc.host().unwrap_or_default(); // host + port
    let scheme = if proto == "https:" { "wss" } else { "ws" };
    format!("{scheme}://{host}/ws")
}

/// Open the live `/ws` stream, retrying with a fixed backoff until it connects.
/// Returns a split read half once connected. Pair with [`ws_reconnect_delay`]
/// in the caller's loop to keep the stream alive across drops.
pub async fn ws_connect() -> Option<gloo_net::websocket::futures::WebSocket> {
    use gloo_net::websocket::futures::WebSocket;
    match WebSocket::open(&ws_url()) {
        Ok(ws) => Some(ws),
        Err(e) => {
            web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(&format!(
                "ws open failed: {e:?}"
            )));
            None
        }
    }
}

/// Wait before the next reconnect attempt (2s).
pub async fn ws_reconnect_delay() {
    gloo_timers::future::TimeoutFuture::new(2_000).await;
}

// ── Shared helpers ────────────────────────────────────────────────────────

/// Format a Unix-ms timestamp as UTC HH:MM:SS.
pub fn format_utc_hms(ms: u64) -> String {
    let secs = ms / 1000;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

/// Generate a client-side session token from current timestamp.
pub fn gen_session_token() -> String {
    // Use the current time as entropy — good enough for a demo.
    let ts = js_sys::Date::now() as u64;
    format!("styx_{:x}", ts ^ 0xc0ffee42c0ffee42u64)
}
