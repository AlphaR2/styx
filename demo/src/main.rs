mod log_bridge;

use std::collections::{HashMap, VecDeque};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::{
    extract::{Path, State, WebSocketUpgrade},
    http::{HeaderValue, Method},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use axum::extract::ws::{Message, WebSocket};
use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair},
    signer::Signer,
};
use styx::{Config, StyxClient, ExecuteLane, ExecuteOpts, ExecutionRecord,
           prepare, prepare_jupiter, submit, NetworkEvent};
use styx_agent::{baseline::OverpayerBaseline, claude::LlmClassifier};
use styx_core::{
    auction::AuctionWindow,
    bid::BundleOutcome,
    compute_bid::TxType,
    jito_client::JitoClient,
    lifecycle::{run_event_loop, LifecycleStage, LifecycleTracker},
};
use styx_ingest::{bus, subscriber};
use tokio::sync::{broadcast, Mutex};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

// ── App state ──────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    ctx:           StyxClient,
    keypair:       Arc<Keypair>,
    bus:           broadcast::Sender<NetworkEvent>,
    bundle_events: Arc<Mutex<HashMap<String, Vec<NetworkEvent>>>>,
}

// ── API types ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AccountMetaRequest {
    pubkey:      String,
    is_signer:   bool,
    is_writable: bool,
}

#[derive(Deserialize)]
struct InstructionRequest {
    program_id: String,
    accounts:   Vec<AccountMetaRequest>,
    data:       String,
}

#[derive(Deserialize, Default)]
struct ExecuteRequest {
    #[serde(default)]
    instructions:        Option<Vec<InstructionRequest>>,
    #[serde(default)]
    compute_unit_limit:  Option<u32>,
    #[serde(default)]
    scenario:            Option<String>,
    #[serde(default)]
    sol_amount_lamports: Option<u64>,
    #[serde(default)]
    slippage_bps:        Option<u16>,
    #[serde(default)]
    lane:                Option<String>,
}

#[derive(Serialize)]
struct ExecuteResponse {
    bundle_id:             String,
    tip_lamports:          u64,
    baseline_tip_lamports: u64,
    delta_lamports:        i64,
    regime:                String,
    forward_multiplier:    f64,
    reasoning:             String,
    confidence:            f64,
    solscan_url:           String,
    lane:                  String,
}

#[derive(Serialize)]
struct StatusResponse {
    bundle_id:    String,
    stage:        String,
    landing_slot: Option<u64>,
}

#[derive(Serialize)]
struct LogEntry {
    bundle_id:             String,
    lane:                  String,
    tip_lamports:          u64,
    landed_tip_lamports:   Option<u64>,
    baseline_tip_lamports: u64,
    delta_lamports:        i64,
    regime:                String,
    forward_multiplier:    f64,
    reasoning:             String,
    confidence:            f64,
    submitted_at_ms:       u64,
    landed_bundle_id:      Option<String>,
    landing_slot:          Option<u64>,
    processed_at_ms:       Option<u64>,
    confirmed_at_ms:       Option<u64>,
    finalized_at_ms:       Option<u64>,
    failure_kind:          Option<String>,
    retry_count:           u32,
    tx_signatures:         Vec<String>,
}

impl From<ExecutionRecord> for LogEntry {
    fn from(r: ExecutionRecord) -> Self {
        LogEntry {
            bundle_id: r.bundle_id, lane: r.lane, tip_lamports: r.tip_lamports,
            landed_tip_lamports: r.landed_tip_lamports,
            baseline_tip_lamports: r.baseline_tip_lamports, delta_lamports: r.delta_lamports,
            regime: r.regime, forward_multiplier: r.forward_multiplier,
            reasoning: r.reasoning, confidence: r.confidence,
            submitted_at_ms: r.submitted_at_ms, landed_bundle_id: r.landed_bundle_id,
            landing_slot: r.landing_slot, processed_at_ms: r.processed_at_ms,
            confirmed_at_ms: r.confirmed_at_ms, finalized_at_ms: r.finalized_at_ms,
            failure_kind: r.failure_kind, retry_count: r.retry_count,
            tx_signatures: r.tx_signatures,
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn parse_instruction(req: InstructionRequest) -> Result<Instruction, String> {
    let program_id = Pubkey::from_str(&req.program_id)
        .map_err(|e| format!("invalid program_id '{}': {}", req.program_id, e))?;
    let accounts = req.accounts.into_iter().map(|a| {
        let pubkey = Pubkey::from_str(&a.pubkey)
            .map_err(|e| format!("invalid pubkey '{}': {}", a.pubkey, e))?;
        Ok(if a.is_writable {
            AccountMeta::new(pubkey, a.is_signer)
        } else {
            AccountMeta::new_readonly(pubkey, a.is_signer)
        })
    }).collect::<Result<Vec<_>, String>>()?;
    let data = general_purpose::STANDARD
        .decode(&req.data)
        .map_err(|e| format!("invalid instruction data (expected base64): {}", e))?;
    Ok(Instruction { program_id, accounts, data })
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn load_keypair() -> Result<Keypair> {
    if let Ok(b64) = std::env::var("KEYPAIR_JSON") {
        let json_bytes = general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|e| anyhow::anyhow!("KEYPAIR_JSON base64 decode failed: {}", e))?;
        let byte_vec: Vec<u8> = serde_json::from_slice(&json_bytes)
            .map_err(|e| anyhow::anyhow!("KEYPAIR_JSON is not valid keypair JSON: {}", e))?;
        if byte_vec.len() != 64 {
            anyhow::bail!("KEYPAIR_JSON must be exactly 64 bytes, got {}", byte_vec.len());
        }
        let secret: [u8; 32] = byte_vec[..32]
            .try_into()
            .map_err(|_| anyhow::anyhow!("KEYPAIR_JSON: could not extract secret key"))?;
        Ok(Keypair::new_from_array(secret))
    } else {
        let path = std::env::var("KEYPAIR_PATH").context("KEYPAIR_PATH not set (and KEYPAIR_JSON not set)")?;
        read_keypair_file(&path)
            .map_err(|e| anyhow::anyhow!("Failed to load keypair from {}: {}", path, e))
    }
}

fn make_signer(
    keypair: Arc<Keypair>,
) -> impl Fn(Vec<solana_sdk::transaction::VersionedTransaction>)
        -> anyhow::Result<Vec<solana_sdk::transaction::VersionedTransaction>>
     + Send + Sync + 'static
{
    move |mut txs| {
        let payer = keypair.pubkey();
        for tx in &mut txs {
            let msg_bytes = tx.message.serialize();
            let n_required = tx.message.header().num_required_signatures as usize;
            let keys = tx.message.static_account_keys();
            if let Some(slot) = keys.iter().take(n_required).position(|k| *k == payer) {
                tx.signatures[slot] = keypair.sign_message(&msg_bytes);
            }
        }
        Ok(txs)
    }
}

// ── Startup ────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let bus = bus::new_bus();

    {
        use tracing_subscriber::prelude::*;
        let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,hyper=warn,tonic=warn,h2=warn"));
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer().with_target(true).compact())
            .with(log_bridge::BusLogLayer::new(bus.clone()))
            .init();
    }

    let config = Arc::new(Config::from_env()?);
    let keypair = Arc::new(load_keypair()?);

    let lifecycle_bus = bus::new_lifecycle_bus();

    tokio::spawn(subscriber::run(
        config.yellowstone_endpoint.clone(),
        config.yellowstone_token.clone(),
        bus.clone(),
        lifecycle_bus.clone(),
        keypair.pubkey().to_string(),
    ));

    let tracker = Arc::new(Mutex::new(
        LifecycleTracker::new().with_exec_bus(bus.clone()),
    ));
    tokio::spawn(run_event_loop(tracker.clone(), lifecycle_bus.subscribe()));

    // Auction window: ingest live JitoTip events from Yellowstone account subscriptions.
    let auction_window: Arc<Mutex<AuctionWindow>> = Arc::new(Mutex::new(AuctionWindow::new()));
    {
        let window = auction_window.clone();
        let mut rx = bus.subscribe();
        tokio::spawn(async move {
            loop {
                if let Ok(NetworkEvent::JitoTip { slot, tip_lamports, .. }) = rx.recv().await {
                    window.lock().await.ingest(slot, tip_lamports);
                }
            }
        });
    }

    // Outcomes ring buffer for Claude self-calibration (cap 50, last 10 sent to Claude).
    let outcomes: Arc<Mutex<VecDeque<BundleOutcome>>> = Arc::new(Mutex::new(VecDeque::new()));

    // Per-bundle replay buffer.
    let bundle_events: Arc<Mutex<HashMap<String, Vec<NetworkEvent>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    {
        let map = bundle_events.clone();
        let mut rx = bus.subscribe();
        tokio::spawn(async move {
            loop {
                if let Ok(event) = rx.recv().await {
                    let bundle_id = match &event {
                        NetworkEvent::ExecLog   { bundle_id, .. } => Some(bundle_id.clone()),
                        NetworkEvent::Execution { bundle_id, .. } => Some(bundle_id.clone()),
                        _ => None,
                    };
                    if let Some(bid) = bundle_id {
                        let mut m = map.lock().await;
                        let entry = m.entry(bid).or_default();
                        entry.push(event);
                        if entry.len() > 500 {
                            let d = entry.len() - 500;
                            entry.drain(0..d);
                        }
                    }
                }
            }
        });
    }

    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    let jito = Arc::new(JitoClient::new(config.jito_block_engine_urls.clone()));
    let tip_accounts = jito.get_tip_accounts().await?;
    info!("fetched {} tip accounts", tip_accounts.len());
    let tip_account = Pubkey::from_str(&tip_accounts[0])?;

    let rpc = Arc::new(RpcClient::new_with_commitment(
        config.rpc_url.clone(),
        CommitmentConfig::confirmed(),
    ));

    {
        let rpc_b = rpc.clone();
        let payer = keypair.pubkey();
        match tokio::task::spawn_blocking(move || rpc_b.get_balance(&payer)).await {
            Ok(Ok(lamports)) => {
                let sol = lamports as f64 / 1_000_000_000.0;
                info!(balance_sol = sol, payer = %payer, "signer wallet balance");
                if lamports < 20_000_000 {
                    warn!(balance_sol = sol,
                        "signer wallet is low (<0.02 SOL) — sustained retries/snipes may exhaust \
                         it and cause bundles to be dropped; top up to land reliably");
                }
            }
            Ok(Err(e)) => warn!("could not fetch signer wallet balance: {}", e),
            Err(e) => warn!("balance check task failed: {}", e),
        }
    }

    let leader_clock = styx_core::leader::LeaderClock::new();
    tokio::spawn(styx_core::leader::run_slot_listener(leader_clock.clone(), bus.subscribe()));
    tokio::spawn(styx_core::leader::run_schedule_refresher(leader_clock.clone(), rpc.clone()));

    let ctx = StyxClient {
        claude: Arc::new(LlmClassifier::new(config.llm.clone())),
        baseline: Arc::new(OverpayerBaseline::new()),
        config: config.clone(),
        rpc,
        jito,
        tracker,
        tip_account,
        auction_window,
        outcomes,
        log: Arc::new(Mutex::new(Vec::new())),
        exec_bus: Some(bus.clone()),
        leader: Some(leader_clock),
    };

    let state = AppState { ctx, keypair, bus, bundle_events };

    let cors = CorsLayer::new()
        .allow_origin([
            "http://localhost:8080".parse::<HeaderValue>()?,
            "http://127.0.0.1:8080".parse::<HeaderValue>()?,
        ])
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([axum::http::header::CONTENT_TYPE]);

    let app = Router::new()
        .route("/execute",             post(execute_handler))
        .route("/status/{bundle_id}",  get(status_handler))
        .route("/ws",                  get(ws_handler))
        .route("/log",                 get(log_handler))
        .route("/bundle/{id}/events",  get(bundle_events_handler))
        .route("/bundle/{id}/summary", get(summary_handler))
        .route("/leader",              get(leader_handler))
        .route("/tip_floor",           get(tip_floor_handler))
        .route("/health",              get(health_handler))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state);

    let addr = "0.0.0.0:3000";
    info!("Styx demo listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

// ── Handlers ───────────────────────────────────────────────────────────────

async fn execute_handler(
    State(state): State<AppState>,
    body: Option<Json<ExecuteRequest>>,
) -> impl IntoResponse {
    let req = body.map(|b| b.0).unwrap_or_default();
    let payer = state.keypair.pubkey();

    let req_lane = if req.lane.as_deref() == Some("priority") {
        ExecuteLane::PriorityFee
    } else {
        ExecuteLane::JitoBundle
    };
    let fault = req.scenario.as_deref() == Some("fault");

    let (user_instructions, opts): (Vec<Instruction>, ExecuteOpts) =
        if let Some(list) = req.instructions.filter(|l| !l.is_empty()) {
            let mut parsed = Vec::with_capacity(list.len());
            for raw in list {
                match parse_instruction(raw) {
                    Ok(ix) => parsed.push(ix),
                    Err(e) => return Json(serde_json::json!({"error": e})).into_response(),
                }
            }
            (parsed, ExecuteOpts {
                compute_unit_limit: req.compute_unit_limit.unwrap_or(50_000),
                simulate: false,
                address_lookup_tables: Vec::new(),
                lane: req_lane,
                tip_ceiling_override: None,
                inject_blockhash_expiry: false,
                tx_type: TxType::Memo,
                value_lamports: 0,
            })
        } else if req.scenario.as_deref() == Some("jupiter") {
            let amount   = req.sol_amount_lamports.unwrap_or(1_000_000);
            let slippage = req.slippage_bps.unwrap_or(300);
            let bundle = match prepare_jupiter(
                payer,
                styx_core::jupiter::WSOL_MINT,
                styx_core::jupiter::USDC_MINT,
                amount, slippage, req_lane, &state.ctx,
            ).await {
                Ok(b) => b,
                Err(e) => {
                    warn!(error = %e, "prepare_jupiter failed");
                    return (axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({"error": format!("jupiter: {:#}", e)}))).into_response();
                }
            };
            return match submit(bundle, make_signer(state.keypair.clone()), &state.ctx).await {
                Ok(handle) => {
                    info!(bundle_id = %handle.bundle_id, tip = handle.tip_lamports,
                          lane = %handle.lane, "jupiter swap submitted");
                    Json(ExecuteResponse {
                        bundle_id: handle.bundle_id, tip_lamports: handle.tip_lamports,
                        baseline_tip_lamports: handle.baseline_tip_lamports,
                        delta_lamports: handle.delta_lamports, regime: handle.regime,
                        forward_multiplier: handle.forward_multiplier,
                        reasoning: handle.reasoning, confidence: handle.confidence,
                        solscan_url: handle.solscan_url, lane: handle.lane,
                    }).into_response()
                }
                Err(e) => {
                    warn!(error = %e, "submit (jupiter) failed");
                    (axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                     Json(serde_json::json!({"error": format!("jupiter: {:#}", e)}))).into_response()
                }
            };
        } else {
            let memo_program =
                Pubkey::from_str("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr")
                    .expect("valid memo program id");
            let data = format!("styx proof-of-life {}", now_ms()).into_bytes();
            (vec![Instruction {
                program_id: memo_program,
                accounts: vec![AccountMeta::new_readonly(payer, true)],
                data,
            }], ExecuteOpts {
                compute_unit_limit: 50_000,
                simulate: false,
                address_lookup_tables: Vec::new(),
                lane: req_lane,
                tip_ceiling_override: None,
                inject_blockhash_expiry: fault,
                tx_type: TxType::Memo,
                value_lamports: 0,
            })
        };

    let bundle = match prepare(payer, user_instructions, opts, &state.ctx).await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "prepare failed");
            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": e.to_string()}))).into_response();
        }
    };
    match submit(bundle, make_signer(state.keypair.clone()), &state.ctx).await {
        Ok(handle) => {
            info!(bundle_id = %handle.bundle_id, regime = %handle.regime,
                  tip = handle.tip_lamports, delta = handle.delta_lamports,
                  lane = %handle.lane, "bundle submitted");
            Json(ExecuteResponse {
                bundle_id: handle.bundle_id, tip_lamports: handle.tip_lamports,
                baseline_tip_lamports: handle.baseline_tip_lamports,
                delta_lamports: handle.delta_lamports, regime: handle.regime,
                forward_multiplier: handle.forward_multiplier,
                reasoning: handle.reasoning, confidence: handle.confidence,
                solscan_url: handle.solscan_url, lane: handle.lane,
            }).into_response()
        }
        Err(e) => {
            warn!(error = %e, "submit failed");
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR,
             Json(serde_json::json!({"error": e.to_string()}))).into_response()
        }
    }
}

async fn status_handler(
    State(state): State<AppState>,
    Path(bundle_id): Path<String>,
) -> impl IntoResponse {
    let tracker = state.ctx.tracker.lock().await;
    match tracker.get(&bundle_id) {
        None => {
            warn!(bundle_id = %bundle_id, "status: not found");
            Json(serde_json::json!({"error": "bundle not found"})).into_response()
        }
        Some(handle) => {
            let (stage_str, landing_slot) = match &handle.stage {
                LifecycleStage::Submitted              => ("Submitted".to_string(), None),
                LifecycleStage::Pending                => ("Pending".to_string(), None),
                LifecycleStage::Processed { landing_slot } => ("Processed".to_string(), Some(*landing_slot)),
                LifecycleStage::Confirmed { landing_slot } => ("Confirmed".to_string(), Some(*landing_slot)),
                LifecycleStage::Finalized { landing_slot } => ("Finalized".to_string(), Some(*landing_slot)),
                LifecycleStage::Failed    { reason }       => (format!("Failed: {}", reason), None),
            };
            Json(StatusResponse { bundle_id: handle.bundle_id.clone(), stage: stage_str, landing_slot }).into_response()
        }
    }
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| stream_bus(socket, state.bus.subscribe()))
}

async fn stream_bus(mut socket: WebSocket, mut rx: broadcast::Receiver<NetworkEvent>) {
    loop {
        match rx.recv().await {
            Ok(event) => {
                let text = match serde_json::to_string(&event) { Ok(s) => s, Err(_) => continue };
                if socket.send(Message::Text(text.into())).await.is_err() { break; }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!(dropped = n, "WebSocket client lagged");
                let _ = socket.send(Message::Text(
                    format!("{{\"type\":\"lagged\",\"dropped\":{}}}", n).into()
                )).await;
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn bundle_events_handler(
    State(state): State<AppState>,
    Path(bundle_id): Path<String>,
) -> impl IntoResponse {
    let map = state.bundle_events.lock().await;
    let events = map.get(&bundle_id).cloned().unwrap_or_default();
    Json(events)
}

async fn summary_handler(
    State(state): State<AppState>,
    Path(bundle_id): Path<String>,
) -> impl IntoResponse {
    use solana_commitment_config::CommitmentConfig;
    use solana_transaction_status_client_types::UiTransactionEncoding;
    use solana_rpc_client_api::config::RpcTransactionConfig;
    use solana_sdk::signature::Signature;
    use std::str::FromStr;

    let record = {
        let log = state.ctx.log.lock().await;
        log.iter().find(|r| {
            r.bundle_id == bundle_id
                || r.landed_bundle_id.as_deref() == Some(bundle_id.as_str())
        }).cloned()
    };
    let Some(r) = record else {
        return (axum::http::StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "bundle not found"}))).into_response();
    };

    let tx_config = RpcTransactionConfig {
        encoding: Some(UiTransactionEncoding::JsonParsed),
        commitment: Some(CommitmentConfig::confirmed()),
        max_supported_transaction_version: Some(0),
    };
    let rpc = state.ctx.rpc.clone();
    let sigs = r.tx_signatures.clone();
    let tx_json: Vec<serde_json::Value> = {
        let mut out = Vec::new();
        for sig_str in &sigs {
            if let Ok(sig) = Signature::from_str(sig_str) {
                match rpc.get_transaction_with_config(&sig, tx_config.clone()) {
                    Ok(tx) => {
                        if let Ok(v) = serde_json::to_value(&tx) {
                            out.push(v);
                        }
                    }
                    Err(e) => warn!("could not fetch tx {sig_str}: {e}"),
                }
            }
        }
        out
    };

    let sub_to_proc = r.processed_at_ms.map(|p| p.saturating_sub(r.submitted_at_ms));
    let proc_to_conf = match (r.processed_at_ms, r.confirmed_at_ms) {
        (Some(p), Some(c)) => Some(c.saturating_sub(p)),
        _ => None,
    };
    let total_ms = r.confirmed_at_ms.or(r.finalized_at_ms)
        .map(|t| t.saturating_sub(r.submitted_at_ms));

    let status = if r.finalized_at_ms.is_some() { "Finalized" }
        else if r.confirmed_at_ms.is_some() { "Confirmed" }
        else if r.processed_at_ms.is_some() { "Processed" }
        else if r.failure_kind.is_some() { "Failed" }
        else { "Pending" };

    let events_snapshot = {
        let map = state.bundle_events.lock().await;
        map.get(&bundle_id).cloned().unwrap_or_default()
    };
    let event_lines: Vec<String> = events_snapshot.iter()
        .filter_map(|ev| match ev {
            styx_ingest::bus::NetworkEvent::Execution { stage, message, .. } =>
                Some(format!("[{}] {}", stage, message)),
            styx_ingest::bus::NetworkEvent::ExecLog { level, message, .. } =>
                Some(format!("[{}] {}", level, message)),
            _ => None,
        })
        .take(20)
        .collect();

    let context = format!(
        "## Styx execution record\n\
         bundle_id: {bundle_id}\n\
         lane: {lane}\n\
         status: {status}\n\
         fee_lamports: {tip_lam}\n\
         market_baseline_lamports: {base_lam}\n\
         savings_lamports: {delta_lam}\n\
         network_regime: {regime}\n\
         ai_forward_multiplier: {mult:.2}\n\
         ai_confidence: {conf:.2}\n\
         ai_reasoning: {reasoning}\n\
         retry_count: {retries}\n\
         failure_kind: {failure}\n\
         submit_to_processed_ms: {s2p}\n\
         processed_to_confirmed_ms: {p2c}\n\
         total_ms: {total}\n\
         landing_slot: {slot}\n\
         \n\
         ## Styx lifecycle events\n\
         {events}\n\
         \n\
         ## Solana units\n\
         SOL = 9 decimals (1 SOL = 1,000,000,000 lamports). \
         USDC (EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v) = 6 decimals. \
         Use uiAmount/uiAmountString for human-readable token amounts. \
         preBalances/postBalances are raw lamports; divide by 1e9 for SOL.\n\
         \n\
         ## Raw on-chain transaction data (JSON-parsed)\n\
         {tx_data}",
        lane      = r.lane,
        tip_lam   = r.tip_lamports,
        base_lam  = r.baseline_tip_lamports,
        delta_lam = r.delta_lamports,
        regime    = r.regime,
        mult      = r.forward_multiplier,
        conf      = r.confidence,
        reasoning = r.reasoning,
        retries   = r.retry_count,
        failure   = r.failure_kind.as_deref().unwrap_or("none"),
        s2p       = sub_to_proc.map(|d| format!("{d}ms")).unwrap_or_else(|| "n/a".to_string()),
        p2c       = proc_to_conf.map(|d| format!("{d}ms")).unwrap_or_else(|| "n/a".to_string()),
        total     = total_ms.map(|d| format!("{d}ms")).unwrap_or_else(|| "n/a".to_string()),
        slot      = r.landing_slot.map(|s| s.to_string()).unwrap_or_else(|| "n/a".to_string()),
        events    = event_lines.join("\n"),
        tx_data   = if tx_json.is_empty() {
            "not yet available (transaction may not be finalized)".to_string()
        } else {
            serde_json::to_string_pretty(&tx_json).unwrap_or_default()
        },
    );

    match state.ctx.claude.summarize(&context).await {
        Ok(json_str) => {
            match serde_json::from_str::<serde_json::Value>(&json_str) {
                Ok(v) => Json(v).into_response(),
                Err(_) => Json(serde_json::json!({"verdict": status, "what_happened": json_str,
                    "fee_analysis": "", "performance": "", "timing": ""})).into_response(),
            }
        }
        Err(e) => (axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": format!("AI summary unavailable: {}", e)}))).into_response(),
    }
}

async fn log_handler(State(state): State<AppState>) -> impl IntoResponse {
    let entries: Vec<LogEntry> = state.ctx.log.lock().await
        .iter().cloned().map(LogEntry::from).collect();
    Json(entries)
}

async fn leader_handler(State(state): State<AppState>) -> impl IntoResponse {
    let rpc = state.ctx.rpc.clone();
    let fetched = tokio::task::spawn_blocking(move || {
        let slot = rpc.get_slot()?;
        let leaders = rpc.get_slot_leaders(slot, 16)?;
        Ok::<_, anyhow::Error>((slot, leaders))
    })
    .await;

    match fetched {
        Ok(Ok((slot, leaders))) => {
            let list: Vec<_> = leaders
                .iter()
                .enumerate()
                .map(|(i, pk)| serde_json::json!({
                    "slot": slot + i as u64,
                    "leader": pk.to_string(),
                }))
                .collect();
            Json(serde_json::json!({ "current_slot": slot, "leaders": list })).into_response()
        }
        Ok(Err(e)) => {
            warn!(error = %e, "leader schedule fetch failed");
            (axum::http::StatusCode::BAD_GATEWAY,
             Json(serde_json::json!({"error": format!("leader fetch: {}", e)}))).into_response()
        }
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR,
             Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}

async fn tip_floor_handler(State(state): State<AppState>) -> impl IntoResponse {
    let w = state.ctx.auction_window.lock().await.clone();
    Json(serde_json::json!({
        "clearing_price_min":    w.clearing_price_min,
        "clearing_price_median": w.clearing_price_median,
        "clearing_price_max":    w.clearing_price_max,
        "bundles_per_slot":      w.bundles_per_slot,
        "trend":                 format!("{:?}", w.trend),
        "regime":                format!("{:?}", w.regime),
        "is_bootstrapped":       w.is_bootstrapped,
    }))
}

async fn health_handler() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}
