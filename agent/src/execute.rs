#![allow(unused_imports, unused_variables, dead_code)]

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;
use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    hash::Hash,
    instruction::Instruction,
    message::{v0, AddressLookupTableAccount, VersionedMessage},
    pubkey::Pubkey,
    signature::Signature,
    transaction::VersionedTransaction,
};
use solana_compute_budget_interface::ComputeBudgetInstruction;
use tokio::sync::Mutex;
use tracing::{debug, info};

use styx_core::{
    auction::AuctionWindow,
    bid::{BidContext, BidStrategy, BundleOutcome},
    compose::{build_bundle_unsigned, build_tip_tx_unsigned, encode_transaction, BundleSpec},
    compute_bid::{compute_tip, TxType},
    config::Config,
    jito_client::JitoClient,
    lane_router::{LaneChoice, LaneRouter},
    lifecycle::{wait_confirmed, LifecycleTracker},
    retry::{
        emit_exec, run_retry_loop, run_priority_fee_retry_loop,
        PriorityFeeRetryContext, RetryContext, RetryOutcome, SignerFn,
    },
};
use styx_ingest::bus::NetworkEvent;

use crate::baseline::OverpayerBaseline;
use crate::claude::LlmClassifier;

const OUTCOME_CAP: usize = 50;

// Everything needed to execute bundles. Cheaply cloneable.
#[derive(Clone)]
pub struct StyxClient {
    pub config: Arc<Config>,
    pub rpc: Arc<RpcClient>,
    pub jito: Arc<JitoClient>,
    pub tracker: Arc<Mutex<LifecycleTracker>>,
    pub claude: Arc<LlmClassifier>,
    pub baseline: Arc<OverpayerBaseline>,
    pub tip_account: Pubkey,
    // Updated live by a background task that ingests JitoTip bus events.
    pub auction_window: Arc<Mutex<AuctionWindow>>,
    // Rolling history of resolved bundles for Claude self-calibration.
    pub outcomes: Arc<Mutex<VecDeque<BundleOutcome>>>,
    pub log: Arc<Mutex<Vec<ExecutionRecord>>>,
    pub exec_bus: Option<tokio::sync::broadcast::Sender<NetworkEvent>>,
    pub leader: Option<styx_core::leader::LeaderClock>,
}

#[derive(Clone, Copy, Default, PartialEq)]
pub enum ExecuteLane {
    #[default]
    JitoBundle,
    PriorityFee,
}

pub struct ExecuteOpts {
    pub compute_unit_limit: u32,
    pub simulate: bool,
    pub address_lookup_tables: Vec<AddressLookupTableAccount>,
    pub lane: ExecuteLane,
    pub tip_ceiling_override: Option<u64>,
    pub inject_blockhash_expiry: bool,
    // Transaction type for value-cap computation.
    pub tx_type: TxType,
    // Economic value of this transaction in lamports (used for value cap).
    // 0 = unlimited cap (same as Memo).
    pub value_lamports: u64,
}

impl Default for ExecuteOpts {
    fn default() -> Self {
        ExecuteOpts {
            compute_unit_limit: 50_000,
            simulate: true,
            address_lookup_tables: Vec::new(),
            lane: ExecuteLane::default(),
            tip_ceiling_override: None,
            inject_blockhash_expiry: false,
            tx_type: TxType::Memo,
            value_lamports: 0,
        }
    }
}

pub struct ExecutionHandle {
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

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionRecord {
    pub bundle_id: String,
    pub lane: String,
    pub tip_lamports: u64,
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
    pub tx_signatures: Vec<String>,
}

pub fn rolling_landing_rate(log: &[ExecutionRecord]) -> Option<f64> {
    const WINDOW: usize = 50;
    const MIN_SAMPLES: usize = 5;

    let mut landed = 0usize;
    let mut total = 0usize;
    for r in log.iter().rev() {
        let is_landed = r.confirmed_at_ms.is_some();
        let is_failed = !is_landed && r.failure_kind.is_some();
        if is_landed || is_failed {
            total += 1;
            if is_landed { landed += 1; }
            if total >= WINDOW { break; }
        }
    }
    (total >= MIN_SAMPLES).then(|| landed as f64 / total as f64)
}

// ---- Internal lane state ----

enum BundleInner {
    Jito { spec: BundleSpec },
    PriorityFee {
        raw_instructions: Vec<Instruction>,
        compute_unit_limit: u32,
        address_lookup_tables: Vec<AddressLookupTableAccount>,
        micro_lamports_per_cu: u64,
        inject_blockhash_expiry: bool,
    },
    Jupiter {
        in_amount: u64,
        out_amount: u64,
        last_valid_block_height: u64,
        lane: ExecuteLane,
    },
}

pub struct PreparedBundle {
    pub transactions: Vec<VersionedTransaction>,
    pub tip_lamports: u64,
    pub baseline_tip_lamports: u64,
    pub delta_lamports: i64,
    pub regime: String,
    pub forward_multiplier: f64,
    pub reasoning: String,
    pub confidence: f64,
    pub lane: ExecuteLane,

    inner: BundleInner,
    payer: Pubkey,
    init_multiplier: f64,
    ceiling: u64,
    tx_type: TxType,
    value_lamports: u64,
    blockhash: Hash,
}

// ---- prepare ----

pub async fn prepare(
    payer: Pubkey,
    instructions: Vec<Instruction>,
    opts: ExecuteOpts,
    ctx: &StyxClient,
) -> Result<PreparedBundle> {
    let window = ctx.auction_window.lock().await.clone();
    let recent_outcomes: Vec<BundleOutcome> = {
        let o = ctx.outcomes.lock().await;
        o.iter().rev().take(10).cloned().collect()
    };

    let bid_ctx = BidContext {
        window: window.clone(),
        tx_type: opts.tx_type,
        value_lamports: opts.value_lamports,
        recent_outcomes,
    };

    let (agent_output, baseline_output) = tokio::join!(
        ctx.claude.decide(&bid_ctx),
        async { ctx.baseline.decide(&bid_ctx) }
    );

    let lane = LaneRouter::choose(&agent_output.regime);
    let ceiling = opts.tip_ceiling_override.unwrap_or(ctx.config.tip_ceiling_lamports);

    let tip_lamports = match std::env::var("TEST_TIP_LAMPORTS").ok().and_then(|v| v.parse::<u64>().ok()) {
        Some(forced) => {
            info!(forced_tip = forced, "TEST_TIP_LAMPORTS override active -- bypassing AI formula");
            forced
        }
        None => compute_tip(&window, agent_output.forward_multiplier, opts.tx_type, opts.value_lamports, ceiling),
    };
    let baseline_tip_lamports = compute_tip(&window, baseline_output.forward_multiplier, opts.tx_type, opts.value_lamports, ceiling);
    let delta_lamports = baseline_tip_lamports as i64 - tip_lamports as i64;
    let init_multiplier = agent_output.forward_multiplier;

    // Simulation: unsigned tx, sigVerify=false + replaceRecentBlockhash=true.
    let compute_unit_limit = if opts.simulate {
        let rpc_url = ctx.config.rpc_url.clone();
        debug!(rpc_url = %rpc_url, "simulating tx to size compute units");

        let msg = v0::Message::try_compile(
            &payer, &instructions, &opts.address_lookup_tables, Hash::default(),
        ).map_err(|e| anyhow::anyhow!("sim compile: {}", e))?;
        let versioned_msg = VersionedMessage::V0(msg);
        let n_sigs = versioned_msg.header().num_required_signatures as usize;
        let sim_tx = VersionedTransaction {
            signatures: vec![Signature::default(); n_sigs],
            message: versioned_msg,
        };
        let tx_b64 = encode_transaction(&sim_tx)?;
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "simulateTransaction",
            "params": [tx_b64, {
                "encoding": "base64",
                "sigVerify": false,
                "replaceRecentBlockhash": true
            }]
        });
        let resp = reqwest::Client::new()
            .post(&rpc_url)
            .json(&body)
            .send().await
            .map_err(|e| anyhow::anyhow!("simulate request: {}", e))?
            .json::<serde_json::Value>().await
            .map_err(|e| anyhow::anyhow!("simulate parse: {}", e))?;

        let sim_val = &resp["result"]["value"];
        if let Some(err) = sim_val.get("err").filter(|e| !e.is_null()) {
            let logs = sim_val["logs"]
                .as_array()
                .map(|l| l.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" | "))
                .unwrap_or_default();
            tracing::warn!(sim_err = %err, sim_logs = %logs, "pre-submission simulation failed -- tx may be invalid");
        }
        let units = sim_val["unitsConsumed"].as_u64();
        let limit = units.map(|u| ((u as f64) * 1.2) as u32).unwrap_or(opts.compute_unit_limit);
        match units {
            Some(u) => debug!(units_consumed = u, compute_unit_limit = limit, "simulation ok"),
            None    => tracing::warn!(fallback = opts.compute_unit_limit, "simulation returned no unitsConsumed -- using caller's CU limit"),
        }
        limit
    } else {
        opts.compute_unit_limit
    };

    let rpc = ctx.rpc.clone();
    let (blockhash, _) = tokio::task::spawn_blocking(move || {
        rpc.get_latest_blockhash_with_commitment(CommitmentConfig::confirmed())
    }).await??;

    // Priority-fee lane
    if opts.lane == ExecuteLane::PriorityFee || lane == LaneChoice::PriorityFee {
        let micro_lamports_per_cu = tip_lamports
            .saturating_mul(1_000_000)
            .checked_div(compute_unit_limit as u64)
            .unwrap_or(1_000);

        let raw_instructions = instructions.clone();

        let mut ixs = Vec::with_capacity(2 + instructions.len());
        ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit));
        ixs.push(ComputeBudgetInstruction::set_compute_unit_price(micro_lamports_per_cu));
        ixs.extend_from_slice(&instructions);

        let used_blockhash = if opts.inject_blockhash_expiry {
            tracing::warn!("fault injection: priority-fee tx built with a stale blockhash");
            Hash::default()
        } else {
            blockhash
        };

        let msg = v0::Message::try_compile(
            &payer, &ixs, &opts.address_lookup_tables, used_blockhash,
        ).map_err(|e| anyhow::anyhow!("priority-fee compile: {}", e))?;
        let versioned_msg = VersionedMessage::V0(msg);
        let n_sigs = versioned_msg.header().num_required_signatures as usize;
        let unsigned_tx = VersionedTransaction {
            signatures: vec![Signature::default(); n_sigs],
            message: versioned_msg,
        };

        return Ok(PreparedBundle {
            transactions: vec![unsigned_tx],
            tip_lamports,
            baseline_tip_lamports,
            delta_lamports,
            regime: format!("{:?}", agent_output.regime),
            forward_multiplier: agent_output.forward_multiplier,
            reasoning: agent_output.reasoning,
            confidence: agent_output.confidence,
            lane: ExecuteLane::PriorityFee,
            inner: BundleInner::PriorityFee {
                raw_instructions,
                compute_unit_limit,
                address_lookup_tables: opts.address_lookup_tables,
                micro_lamports_per_cu,
                inject_blockhash_expiry: opts.inject_blockhash_expiry,
            },
            payer,
            init_multiplier,
            ceiling,
            tx_type: opts.tx_type,
            value_lamports: opts.value_lamports,
            blockhash,
        });
    }

    // Jito lane
    let bundle_blockhash = if opts.inject_blockhash_expiry {
        tracing::warn!("fault injection: Jito bundle built with stale blockhash -- retry loop will detect ExpiredBlockhash and re-price");
        Hash::default()
    } else {
        blockhash
    };

    let spec = BundleSpec {
        user_instructions: instructions,
        tip_account: ctx.jito.random_tip_account(),
        tip_lamports,
        compute_unit_limit,
        address_lookup_tables: opts.address_lookup_tables,
    };

    let unsigned_txs = build_bundle_unsigned(&spec, &payer, bundle_blockhash)?;

    Ok(PreparedBundle {
        transactions: unsigned_txs,
        tip_lamports,
        baseline_tip_lamports,
        delta_lamports,
        regime: format!("{:?}", agent_output.regime),
        forward_multiplier: agent_output.forward_multiplier,
        reasoning: agent_output.reasoning,
        confidence: agent_output.confidence,
        lane: ExecuteLane::JitoBundle,
        inner: BundleInner::Jito { spec },
        payer,
        init_multiplier,
        ceiling,
        tx_type: opts.tx_type,
        value_lamports: opts.value_lamports,
        blockhash,
    })
}

// ---- prepare_jupiter ----

pub async fn prepare_jupiter(
    payer: Pubkey,
    input_mint: &str,
    output_mint: &str,
    amount_lamports: u64,
    slippage_bps: u16,
    lane: ExecuteLane,
    ctx: &StyxClient,
) -> Result<PreparedBundle> {
    let window = ctx.auction_window.lock().await.clone();
    let recent_outcomes: Vec<BundleOutcome> = {
        let o = ctx.outcomes.lock().await;
        o.iter().rev().take(10).cloned().collect()
    };

    let bid_ctx = BidContext {
        window: window.clone(),
        tx_type: TxType::Swap,
        value_lamports: amount_lamports,
        recent_outcomes,
    };

    let (agent_output, baseline_output) = tokio::join!(
        ctx.claude.decide(&bid_ctx),
        async { ctx.baseline.decide(&bid_ctx) }
    );

    let ceiling = ctx.config.tip_ceiling_lamports;
    let tip_lamports = match std::env::var("TEST_TIP_LAMPORTS").ok().and_then(|v| v.parse::<u64>().ok()) {
        Some(forced) => {
            info!(forced_tip = forced, "TEST_TIP_LAMPORTS override active -- bypassing AI formula (jupiter)");
            forced
        }
        None => compute_tip(&window, agent_output.forward_multiplier, TxType::Swap, amount_lamports, ceiling),
    };
    let baseline_tip_lamports = compute_tip(&window, baseline_output.forward_multiplier, TxType::Swap, amount_lamports, ceiling);
    let delta_lamports = baseline_tip_lamports as i64 - tip_lamports as i64;

    let prioritization_fee = if lane == ExecuteLane::PriorityFee { tip_lamports } else { 0 };
    let exclude_vote_lock_dexes = lane == ExecuteLane::JitoBundle;

    let swap = styx_core::jupiter::build_swap(
        &payer, input_mint, output_mint, amount_lamports,
        slippage_bps, prioritization_fee, exclude_vote_lock_dexes,
    ).await?;

    let transactions = if lane == ExecuteLane::JitoBundle {
        let bundle_blockhash = styx_core::jupiter::recent_blockhash(&swap.tx);
        let tip_tx = build_tip_tx_unsigned(
            &payer, ctx.jito.random_tip_account(), tip_lamports, bundle_blockhash,
        )?;
        vec![swap.tx, tip_tx]
    } else {
        vec![swap.tx]
    };

    Ok(PreparedBundle {
        transactions,
        tip_lamports,
        baseline_tip_lamports,
        delta_lamports,
        regime: format!("{:?}", agent_output.regime),
        forward_multiplier: agent_output.forward_multiplier,
        reasoning: agent_output.reasoning.clone(),
        confidence: agent_output.confidence,
        lane,
        inner: BundleInner::Jupiter {
            in_amount: swap.in_amount,
            out_amount: swap.out_amount,
            last_valid_block_height: swap.last_valid_block_height,
            lane,
        },
        payer,
        init_multiplier: agent_output.forward_multiplier,
        ceiling,
        tx_type: TxType::Swap,
        value_lamports: amount_lamports,
        blockhash: Hash::default(),
    })
}

// ---- submit ----

pub async fn submit<F>(
    bundle: PreparedBundle,
    signer: F,
    ctx: &StyxClient,
) -> Result<ExecutionHandle>
where
    F: Fn(Vec<VersionedTransaction>) -> anyhow::Result<Vec<VersionedTransaction>> + Send + Sync + 'static,
{
    let signer: SignerFn = Arc::new(signer);

    match bundle.inner {
        BundleInner::PriorityFee {
            raw_instructions, compute_unit_limit, address_lookup_tables,
            micro_lamports_per_cu, inject_blockhash_expiry,
        } => {
            submit_priority_fee(
                bundle.payer, bundle.transactions, raw_instructions,
                compute_unit_limit, address_lookup_tables, micro_lamports_per_cu,
                inject_blockhash_expiry, bundle.tip_lamports, bundle.baseline_tip_lamports,
                bundle.delta_lamports, &bundle.regime, bundle.forward_multiplier,
                &bundle.reasoning, bundle.confidence, bundle.init_multiplier, bundle.ceiling,
                bundle.tx_type, bundle.value_lamports, bundle.blockhash, signer, ctx,
            ).await
        }
        BundleInner::Jito { spec } => {
            submit_jito(
                bundle.payer, bundle.transactions, spec, bundle.tip_lamports,
                bundle.baseline_tip_lamports, bundle.delta_lamports, &bundle.regime,
                bundle.forward_multiplier, &bundle.reasoning, bundle.confidence,
                bundle.init_multiplier, bundle.ceiling, bundle.tx_type, bundle.value_lamports,
                bundle.blockhash, signer, ctx,
            ).await
        }
        BundleInner::Jupiter { in_amount, out_amount, last_valid_block_height, lane } => {
            submit_jupiter(
                bundle.payer, bundle.transactions, in_amount, out_amount,
                last_valid_block_height, lane, bundle.tip_lamports, bundle.baseline_tip_lamports,
                bundle.delta_lamports, &bundle.regime, bundle.forward_multiplier,
                &bundle.reasoning, bundle.confidence, signer, ctx,
            ).await
        }
    }
}

// ---- internal helpers ----

fn push_outcome(
    outcomes: &Arc<Mutex<VecDeque<BundleOutcome>>>,
    tip_lamports: u64,
    landed: bool,
    forward_multiplier: f64,
    clearing_price_at_submission: u64,
) {
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let outcome = BundleOutcome { tip_lamports, landed, forward_multiplier, clearing_price_at_submission, ts_ms };
    let outcomes = outcomes.clone();
    tokio::spawn(async move {
        let mut o = outcomes.lock().await;
        o.push_back(outcome);
        if o.len() > OUTCOME_CAP {
            o.pop_front();
        }
    });
}

// ---- submit_priority_fee ----

#[allow(clippy::too_many_arguments)]
async fn submit_priority_fee(
    payer: Pubkey,
    unsigned_txs: Vec<VersionedTransaction>,
    raw_instructions: Vec<Instruction>,
    compute_unit_limit: u32,
    address_lookup_tables: Vec<AddressLookupTableAccount>,
    micro_lamports_per_cu: u64,
    inject_blockhash_expiry: bool,
    tip_lamports: u64,
    baseline_tip_lamports: u64,
    delta_lamports: i64,
    regime: &str,
    forward_multiplier: f64,
    reasoning: &str,
    confidence: f64,
    init_multiplier: f64,
    ceiling: u64,
    tx_type: TxType,
    value_lamports: u64,
    blockhash: Hash,
    signer: SignerFn,
    ctx: &StyxClient,
) -> Result<ExecutionHandle> {
    let signed_txs = signer(unsigned_txs)?;
    let tx = signed_txs.into_iter().next().context("signer returned empty vec for priority-fee")?;

    let sig_str = tx.signatures.first().map(|s| s.to_string()).unwrap_or_default();
    let bundle_id = sig_str.clone();

    let tx_b64 = encode_transaction(&tx)?;
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "sendTransaction",
        "params": [tx_b64, {
            "encoding": "base64",
            "preflightCommitment": "processed",
            "skipPreflight": inject_blockhash_expiry,
        }]
    });
    let rpc_resp = reqwest::Client::new()
        .post(&ctx.config.rpc_url)
        .json(&body)
        .send().await
        .context("sendTransaction request")?
        .json::<serde_json::Value>().await
        .context("sendTransaction parse")?;
    if let Some(err) = rpc_resp.get("error").filter(|e| !e.is_null()) {
        tracing::error!(sig = %sig_str, error = %err, "sendTransaction rejected by RPC");
        anyhow::bail!("sendTransaction failed: {}", err);
    }
    info!(sig = %sig_str, micro_lamports_per_cu, tip_lamports, "priority-fee tx submitted");

    if inject_blockhash_expiry {
        emit_exec(&ctx.exec_bus, &bundle_id, "fault_injected", tip_lamports, 0, regime,
            "stale blockhash submitted intentionally -- retry loop should detect and recover");
    }

    emit_exec(&ctx.exec_bus, &bundle_id, "ai_decision", tip_lamports, 0, regime,
        &format!("{}  | {:.2}x multiplier  | {:.0}% confident  | {}",
            regime_human(regime), forward_multiplier, confidence * 100.0, reasoning));
    emit_exec(&ctx.exec_bus, &bundle_id, "submitted", tip_lamports, 0, regime,
        &format!("priority-fee tx {:.8}...", sig_str));

    ctx.tracker.lock().await.register(bundle_id.clone(), vec![sig_str.clone()]);

    let submitted_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

    ctx.log.lock().await.push(ExecutionRecord {
        bundle_id: bundle_id.clone(), lane: "PriorityFee".to_string(),
        tip_lamports, landed_tip_lamports: None, baseline_tip_lamports, delta_lamports,
        regime: regime.to_string(), forward_multiplier, reasoning: reasoning.to_string(),
        confidence, submitted_at_ms, landed_bundle_id: None, landing_slot: None,
        processed_at_ms: None, confirmed_at_ms: None, finalized_at_ms: None,
        failure_kind: None, retry_count: 0, tx_signatures: vec![sig_str.clone()],
    });

    let clearing_price = ctx.auction_window.lock().await.clearing_price_median;

    {
        let tracker = ctx.tracker.clone();
        let log_ref = ctx.log.clone();
        let outcomes_ref = ctx.outcomes.clone();
        let bid_id = bundle_id.clone();
        let tip2 = tip_lamports;
        let exec_bus2 = ctx.exec_bus.clone();
        let rpc2 = ctx.rpc.clone();
        let rpc_url2 = ctx.config.rpc_url.clone();
        let window2 = ctx.auction_window.clone();
        let advisor2 = ctx.claude.clone() as Arc<dyn styx_core::bid::RetryAdvisor>;

        tokio::spawn(async move {
            let rpc_watch = tokio::spawn(styx_core::lifecycle::rpc_confirm_watcher(
                rpc2.clone(), sig_str.clone(), tracker.clone(), 65,
            ));

            let (failure_kind, landed_tip) = match wait_confirmed(tracker.clone(), &bid_id, 60).await {
                Ok(_) => {
                    info!(sig = %bid_id, "priority-fee tx confirmed");
                    emit_exec(&exec_bus2, &bid_id, "confirmed", tip2, 0, "", "confirmed on-chain");
                    (None, Some(tip2))
                }
                Err(e) => {
                    tracing::warn!(sig = %bid_id, "priority-fee tx did not confirm: {}", e);
                    let retry_ctx = PriorityFeeRetryContext {
                        instructions: raw_instructions, compute_unit_limit, signer, payer,
                        rpc: rpc2, rpc_url: rpc_url2, address_lookup_tables,
                        tracker: tracker.clone(), current_sig: bid_id.clone(),
                        advisor: advisor2, auction_window: window2,
                        tx_type, value_lamports,
                        tip_ceiling: ceiling, exec_bus: exec_bus2.clone(),
                        last_blockhash: blockhash, last_multiplier: init_multiplier,
                    };
                    match run_priority_fee_retry_loop(retry_ctx, &e.to_string()).await {
                        Ok(RetryOutcome::Confirmed { tip_lamports: lt, .. }) => (None, Some(lt)),
                        Ok(RetryOutcome::Exhausted { .. }) => {
                            emit_exec(&exec_bus2, &bid_id, "exhausted", tip2, 0, "", "exhausted all retries");
                            (Some("Exhausted".to_string()), None)
                        }
                        Ok(RetryOutcome::Terminal { reason }) => (Some(reason), None),
                        Ok(RetryOutcome::AlreadyLanded) => (None, Some(tip2)),
                        Err(e) => (Some(e.to_string()), None),
                    }
                }
            };

            rpc_watch.abort();
            let landed = failure_kind.is_none();
            push_outcome(&outcomes_ref, landed_tip.unwrap_or(tip2), landed, forward_multiplier, clearing_price);

            styx_core::lifecycle::wait_finalized(tracker.clone(), &bid_id, 15).await;

            let handle = tracker.lock().await.get(&bid_id).cloned();
            let mut log = log_ref.lock().await;
            if let Some(record) = log.iter_mut().find(|r| r.bundle_id == bid_id) {
                record.failure_kind = failure_kind;
                record.landed_tip_lamports = landed_tip;
                if let Some(h) = handle {
                    record.landed_bundle_id = Some(bid_id.clone());
                    record.landing_slot = h.landing_slot;
                    record.processed_at_ms = h.processed_at_ms;
                    record.confirmed_at_ms = h.confirmed_at_ms;
                    record.finalized_at_ms = h.finalized_at_ms;
                }
            }
        });
    }

    let solscan_url = format!("https://solscan.io/tx/{}", bundle_id);
    Ok(ExecutionHandle {
        bundle_id, tip_lamports, baseline_tip_lamports, delta_lamports,
        regime: regime.to_string(), forward_multiplier, reasoning: reasoning.to_string(),
        confidence, solscan_url, lane: "PriorityFee".to_string(),
    })
}

// ---- submit_jito ----

#[allow(clippy::too_many_arguments)]
async fn submit_jito(
    payer: Pubkey,
    unsigned_txs: Vec<VersionedTransaction>,
    spec: BundleSpec,
    tip_lamports: u64,
    baseline_tip_lamports: u64,
    delta_lamports: i64,
    regime: &str,
    forward_multiplier: f64,
    reasoning: &str,
    confidence: f64,
    init_multiplier: f64,
    ceiling: u64,
    tx_type: TxType,
    value_lamports: u64,
    blockhash: Hash,
    signer: SignerFn,
    ctx: &StyxClient,
) -> Result<ExecutionHandle> {
    let signed_txs = signer(unsigned_txs)?;
    let sigs: Vec<String> = signed_txs.iter()
        .filter_map(|tx| tx.signatures.first())
        .map(|s| s.to_string())
        .collect();
    let sig_str = sigs.first().cloned().unwrap_or_default();

    let leader_window = match &ctx.leader {
        Some(lc) => Some(lc.submission_window().await),
        None => None,
    };

    let encoded: Vec<String> = signed_txs.iter().map(encode_transaction).collect::<Result<_>>()?;
    let bundle_id = ctx.jito.send_bundle(encoded).await?;

    info!(
        bundle_id = %bundle_id, regime = ?regime, forward_multiplier, confidence,
        reasoning = %reasoning, "AI initial tip decision",
    );
    emit_exec(&ctx.exec_bus, &bundle_id, "ai_decision", tip_lamports, 0, regime,
        &format!("{}  | {:.2}x multiplier  | {:.0}% confident  | {}",
            regime_human(regime), forward_multiplier, confidence * 100.0, reasoning));
    emit_exec(&ctx.exec_bus, &bundle_id, "submitted", tip_lamports, 0, regime,
        &format!("Jito bundle submitted  | sig {:.8}...", sig_str));

    if let Some(w) = &leader_window {
        emit_exec(&ctx.exec_bus, &bundle_id, "leader_window", tip_lamports, 0, "",
            &leader_window_message(w));
    }

    ctx.tracker.lock().await.register(bundle_id.clone(), sigs.clone());

    let submitted_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

    ctx.log.lock().await.push(ExecutionRecord {
        bundle_id: bundle_id.clone(), lane: "JitoBundle".to_string(),
        tip_lamports, landed_tip_lamports: None, baseline_tip_lamports, delta_lamports,
        regime: regime.to_string(), forward_multiplier, reasoning: reasoning.to_string(),
        confidence, submitted_at_ms, landed_bundle_id: None, landing_slot: None,
        processed_at_ms: None, confirmed_at_ms: None, finalized_at_ms: None,
        failure_kind: None, retry_count: 0, tx_signatures: sigs.clone(),
    });

    let clearing_price = ctx.auction_window.lock().await.clearing_price_median;

    {
        let tracker = ctx.tracker.clone();
        let log_ref = ctx.log.clone();
        let outcomes_ref = ctx.outcomes.clone();
        let bid_id = bundle_id.clone();
        let tip2 = tip_lamports;
        let rpc2 = ctx.rpc.clone();
        let jito2 = ctx.jito.clone();
        let spec2 = spec.clone();
        let advisor2 = ctx.claude.clone() as Arc<dyn styx_core::bid::RetryAdvisor>;
        let window2 = ctx.auction_window.clone();
        let exec_bus2 = ctx.exec_bus.clone();

        tokio::spawn(async move {
            let rpc_watch = tokio::spawn(styx_core::lifecycle::rpc_confirm_watcher(
                rpc2.clone(), sigs[0].clone(), tracker.clone(), 65,
            ));
            let status_watch = tokio::spawn(styx_core::lifecycle::bundle_status_watcher(
                jito2.clone(), bid_id.clone(), tracker.clone(), 90,
            ));

            info!(bundle_id = %bid_id, sig = %sigs[0], "waiting for bundle confirmation (60s)");
            let (final_bundle_id, retry_count, failure_kind, landed_tip) =
                match wait_confirmed(tracker.clone(), &bid_id, 60).await {
                    Ok(_) => {
                        info!(bundle_id = %bid_id, "confirmed");
                        emit_exec(&exec_bus2, &bid_id, "confirmed", tip2, 0, "", "confirmed on first attempt");
                        (bid_id.clone(), 0u32, None, Some(tip2))
                    }
                    Err(e) => {
                        tracing::warn!(bundle_id = %bid_id, reason = %e, "bundle did not confirm -- handing off to retry loop");
                        let retry_ctx = RetryContext {
                            spec: spec2, signer, payer, rpc: rpc2, jito: jito2,
                            tracker: tracker.clone(), current_bundle_id: bid_id.clone(),
                            advisor: advisor2, auction_window: window2,
                            tx_type, value_lamports,
                            tip_ceiling: ceiling, exec_bus: exec_bus2.clone(),
                            last_blockhash: blockhash, last_multiplier: init_multiplier,
                        };
                        match run_retry_loop(retry_ctx, &e.to_string()).await {
                            Ok(RetryOutcome::Confirmed { bundle_id: landed, retries, tip_lamports: lt }) => {
                                (landed, retries, None, Some(lt))
                            }
                            Ok(RetryOutcome::Exhausted { retries }) => {
                                tracing::warn!(bundle_id = %bid_id, retries, "exhausted retries");
                                (bid_id.clone(), retries, Some("Exhausted".to_string()), None)
                            }
                            Ok(RetryOutcome::Terminal { reason }) => {
                                tracing::warn!(bundle_id = %bid_id, %reason, "terminal failure");
                                (bid_id.clone(), 0, Some(reason), None)
                            }
                            Ok(RetryOutcome::AlreadyLanded) => {
                                info!(bundle_id = %bid_id, "already landed");
                                (bid_id.clone(), 0, None, Some(tip2))
                            }
                            Err(e) => {
                                tracing::error!(bundle_id = %bid_id, "retry error: {}", e);
                                (bid_id.clone(), 0, Some(e.to_string()), None)
                            }
                        }
                    }
                };

            rpc_watch.abort();
            status_watch.abort();
            let landed = failure_kind.is_none();
            push_outcome(&outcomes_ref, landed_tip.unwrap_or(tip2), landed, forward_multiplier, clearing_price);

            styx_core::lifecycle::wait_finalized(tracker.clone(), &final_bundle_id, 15).await;

            let handle = tracker.lock().await.get(&final_bundle_id).cloned();
            let mut log = log_ref.lock().await;
            if let Some(record) = log.iter_mut().find(|r| r.bundle_id == bid_id) {
                record.retry_count = retry_count;
                record.failure_kind = failure_kind;
                record.landed_tip_lamports = landed_tip;
                if let Some(h) = handle {
                    record.landed_bundle_id = Some(final_bundle_id);
                    record.landing_slot = h.landing_slot;
                    record.processed_at_ms = h.processed_at_ms;
                    record.confirmed_at_ms = h.confirmed_at_ms;
                    record.finalized_at_ms = h.finalized_at_ms;
                }
            }
        });
    }

    Ok(ExecutionHandle {
        bundle_id: bundle_id.clone(), tip_lamports, baseline_tip_lamports, delta_lamports,
        regime: regime.to_string(), forward_multiplier, reasoning: reasoning.to_string(),
        confidence, solscan_url: format!("https://solscan.io/tx/{}", sig_str),
        lane: "JitoBundle".to_string(),
    })
}

// ---- submit_jupiter ----

#[allow(clippy::too_many_arguments)]
async fn submit_jupiter(
    payer: Pubkey,
    unsigned_txs: Vec<VersionedTransaction>,
    in_amount: u64,
    out_amount: u64,
    last_valid_block_height: u64,
    lane: ExecuteLane,
    tip_lamports: u64,
    baseline_tip_lamports: u64,
    delta_lamports: i64,
    regime: &str,
    forward_multiplier: f64,
    reasoning: &str,
    confidence: f64,
    signer: SignerFn,
    ctx: &StyxClient,
) -> Result<ExecutionHandle> {
    let signed_txs = signer(unsigned_txs)?;

    if lane == ExecuteLane::PriorityFee {
        let tx = signed_txs.into_iter().next().context("signer returned empty vec for jupiter priority-fee")?;
        let sig_str = tx.signatures.first().map(|s| s.to_string()).unwrap_or_default();
        let bundle_id = sig_str.clone();

        let tx_b64 = styx_core::jupiter::encode_jup_tx(&tx)?;
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "sendTransaction",
            "params": [tx_b64, { "encoding": "base64", "preflightCommitment": "processed" }]
        });
        let rpc_resp = reqwest::Client::new()
            .post(&ctx.config.rpc_url)
            .json(&body)
            .send().await
            .context("jupiter sendTransaction request")?
            .json::<serde_json::Value>().await
            .context("jupiter sendTransaction parse")?;
        if let Some(err) = rpc_resp.get("error").filter(|e| !e.is_null()) {
            anyhow::bail!("jupiter sendTransaction failed: {}", err);
        }

        info!(sig = %sig_str, tip_lamports, in_amount, out_amount, "jupiter priority-fee tx submitted");
        emit_exec(&ctx.exec_bus, &bundle_id, "submitted", tip_lamports, 0, regime,
            &format!("Jupiter swap {:.4} SOL -> {:.6} output via priority-fee",
                in_amount as f64 / 1e9, out_amount as f64 / 1e6));

        ctx.tracker.lock().await.register(bundle_id.clone(), vec![sig_str.clone()]);

        let submitted_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

        ctx.log.lock().await.push(ExecutionRecord {
            bundle_id: bundle_id.clone(), lane: "PriorityFee".to_string(),
            tip_lamports, landed_tip_lamports: None, baseline_tip_lamports, delta_lamports,
            regime: regime.to_string(), forward_multiplier, reasoning: reasoning.to_string(),
            confidence, submitted_at_ms, landed_bundle_id: None, landing_slot: None,
            processed_at_ms: None, confirmed_at_ms: None, finalized_at_ms: None,
            failure_kind: None, retry_count: 0, tx_signatures: vec![sig_str.clone()],
        });

        let clearing_price = ctx.auction_window.lock().await.clearing_price_median;
        let outcomes_ref = ctx.outcomes.clone();

        {
            let tracker = ctx.tracker.clone();
            let log_ref = ctx.log.clone();
            let bid_id = bundle_id.clone();
            let tip2 = tip_lamports;
            let exec_bus2 = ctx.exec_bus.clone();
            tokio::spawn(async move {
                let (failure_kind, landed_tip) = match wait_confirmed(tracker.clone(), &bid_id, 60).await {
                    Ok(_) => {
                        info!(sig = %bid_id, "jupiter priority-fee tx confirmed");
                        emit_exec(&exec_bus2, &bid_id, "confirmed", tip2, 0, "", "confirmed on-chain");
                        (None, Some(tip2))
                    }
                    Err(e) => {
                        tracing::warn!(sig = %bid_id, "jupiter priority-fee tx did not confirm: {}", e);
                        emit_exec(&exec_bus2, &bid_id, "exhausted", tip2, 0, "",
                            "jupiter swap did not confirm -- re-quote to retry");
                        (Some("Timeout".to_string()), None)
                    }
                };
                let landed = failure_kind.is_none();
                push_outcome(&outcomes_ref, landed_tip.unwrap_or(tip2), landed, forward_multiplier, clearing_price);
                styx_core::lifecycle::wait_finalized(tracker.clone(), &bid_id, 15).await;
                let handle = tracker.lock().await.get(&bid_id).cloned();
                let mut log = log_ref.lock().await;
                if let Some(record) = log.iter_mut().find(|r| r.bundle_id == bid_id) {
                    record.failure_kind = failure_kind;
                    record.landed_tip_lamports = landed_tip;
                    if let Some(h) = handle {
                        record.landed_bundle_id = Some(bid_id.clone());
                        record.landing_slot = h.landing_slot;
                        record.processed_at_ms = h.processed_at_ms;
                        record.confirmed_at_ms = h.confirmed_at_ms;
                        record.finalized_at_ms = h.finalized_at_ms;
                    }
                }
            });
        }

        return Ok(ExecutionHandle {
            bundle_id, tip_lamports, baseline_tip_lamports, delta_lamports,
            regime: regime.to_string(), forward_multiplier, reasoning: reasoning.to_string(),
            confidence, solscan_url: format!("https://solscan.io/tx/{}", sig_str),
            lane: "PriorityFee".to_string(),
        });
    }

    // Jito lane: 2-tx bundle [swap, tip].
    let jup_tx = signed_txs.get(0).context("signer returned no swap tx")?;
    let tip_tx = signed_txs.get(1).context("signer returned no tip tx")?;
    let jup_sig = styx_core::jupiter::first_sig(jup_tx);

    let swap_b64 = styx_core::jupiter::encode_jup_tx(jup_tx)?;
    let tip_b64  = encode_transaction(tip_tx)?;

    let leader_window = match &ctx.leader {
        Some(lc) => Some(lc.submission_window().await),
        None => None,
    };

    let bundle_id = ctx.jito.send_bundle(vec![swap_b64, tip_b64]).await?;

    info!(bundle_id = %bundle_id, "Jupiter Jito bundle submitted");
    emit_exec(&ctx.exec_bus, &bundle_id, "submitted", tip_lamports, 0, regime,
        &format!("Jupiter swap {:.4} SOL -> {:.6} output via Jito",
            in_amount as f64 / 1e9, out_amount as f64 / 1e6));

    if let Some(w) = &leader_window {
        emit_exec(&ctx.exec_bus, &bundle_id, "leader_window", tip_lamports, 0, "",
            &leader_window_message(w));
    }

    ctx.tracker.lock().await.register(bundle_id.clone(), vec![jup_sig.clone()]);

    let submitted_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

    ctx.log.lock().await.push(ExecutionRecord {
        bundle_id: bundle_id.clone(), lane: "JitoBundle".to_string(),
        tip_lamports, landed_tip_lamports: None, baseline_tip_lamports, delta_lamports,
        regime: regime.to_string(), forward_multiplier, reasoning: reasoning.to_string(),
        confidence, submitted_at_ms, landed_bundle_id: None, landing_slot: None,
        processed_at_ms: None, confirmed_at_ms: None, finalized_at_ms: None,
        failure_kind: None, retry_count: 0, tx_signatures: vec![jup_sig.clone()],
    });

    let clearing_price = ctx.auction_window.lock().await.clearing_price_median;
    let outcomes_ref = ctx.outcomes.clone();

    {
        let tracker = ctx.tracker.clone();
        let log_ref = ctx.log.clone();
        let bid_id = bundle_id.clone();
        let tip2 = tip_lamports;
        let exec_bus2 = ctx.exec_bus.clone();
        let rpc_jup = ctx.rpc.clone();
        let jup_sig2 = jup_sig.clone();
        let jito2 = ctx.jito.clone();
        let lvbh = last_valid_block_height;

        tokio::spawn(async move {
            if lvbh > 0 {
                if let Ok(Ok(h)) = tokio::task::spawn_blocking({
                    let rpc = rpc_jup.clone();
                    move || rpc.get_block_height()
                }).await {
                    if h > lvbh {
                        tracing::warn!(bundle_id = %bid_id, "Jupiter blockhash already expired");
                        emit_exec(&exec_bus2, &bid_id, "exhausted", tip2, 0, "",
                            "jupiter blockhash expired -- re-quote to retry");
                        push_outcome(&outcomes_ref, tip2, false, forward_multiplier, clearing_price);
                        let mut log = log_ref.lock().await;
                        if let Some(record) = log.iter_mut().find(|r| r.bundle_id == bid_id) {
                            record.failure_kind = Some("BlockhashExpired".to_string());
                        }
                        return;
                    }
                }
            }

            let rpc_watch = tokio::spawn(styx_core::lifecycle::rpc_confirm_watcher(
                rpc_jup, jup_sig2, tracker.clone(), 65,
            ));
            let status_watch = tokio::spawn(styx_core::lifecycle::bundle_status_watcher(
                jito2, bid_id.clone(), tracker.clone(), 90,
            ));

            let (failure_kind, landed_tip) = match wait_confirmed(tracker.clone(), &bid_id, 60).await {
                Ok(_) => {
                    info!(bundle_id = %bid_id, "jupiter jito tx confirmed");
                    emit_exec(&exec_bus2, &bid_id, "confirmed", tip2, 0, "", "confirmed");
                    (None, Some(tip2))
                }
                Err(e) => {
                    tracing::warn!(bundle_id = %bid_id, "jupiter jito tx did not land: {}", e);
                    emit_exec(&exec_bus2, &bid_id, "exhausted", tip2, 0, "",
                        "jupiter swap did not land -- re-quote to retry");
                    (Some("Exhausted".to_string()), None)
                }
            };

            rpc_watch.abort();
            status_watch.abort();
            let landed = failure_kind.is_none();
            push_outcome(&outcomes_ref, landed_tip.unwrap_or(tip2), landed, forward_multiplier, clearing_price);

            styx_core::lifecycle::wait_finalized(tracker.clone(), &bid_id, 15).await;

            let handle = tracker.lock().await.get(&bid_id).cloned();
            let mut log = log_ref.lock().await;
            if let Some(record) = log.iter_mut().find(|r| r.bundle_id == bid_id) {
                record.failure_kind = failure_kind;
                record.landed_tip_lamports = landed_tip;
                if let Some(h) = handle {
                    record.landed_bundle_id = Some(bid_id.clone());
                    record.landing_slot = h.landing_slot;
                    record.processed_at_ms = h.processed_at_ms;
                    record.confirmed_at_ms = h.confirmed_at_ms;
                    record.finalized_at_ms = h.finalized_at_ms;
                }
            }
        });
    }

    Ok(ExecutionHandle {
        bundle_id: bundle_id.clone(), tip_lamports, baseline_tip_lamports, delta_lamports,
        regime: regime.to_string(), forward_multiplier, reasoning: reasoning.to_string(),
        confidence, solscan_url: format!("https://solscan.io/tx/{}", jup_sig),
        lane: "JitoBundle".to_string(),
    })
}

// ---- helpers ----

fn leader_window_message(w: &styx_core::leader::SubmissionWindow) -> String {
    let leader = w.leader.as_deref()
        .map(|l| l.chars().take(8).collect::<String>())
        .unwrap_or_else(|| "unknown".to_string());
    format!(
        "slot {}  | {}/4 into leader {}...'s window  | {} slot(s) left",
        w.current_slot, w.slot_in_window, leader, w.slots_left_in_window
    )
}

fn regime_human(regime: &str) -> &'static str {
    match regime {
        "Cold"  => "Quiet network",
        "Warm"  => "Normal traffic",
        "Hot"   => "Heavy competition",
        "Manic" => "Extreme congestion",
        _       => "Unknown conditions",
    }
}
