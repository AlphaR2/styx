use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::hash::Hash;
use solana_sdk::instruction::Instruction;
use solana_sdk::message::{v0, VersionedMessage};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use solana_sdk::transaction::VersionedTransaction;
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{info, warn};

use solana_compute_budget_interface::ComputeBudgetInstruction;

use crate::auction::AuctionWindow;
use crate::bid::{RetryAction, RetryAdvisor, RetrySignal};
use crate::compose::{build_bundle_unsigned, encode_transaction, BundleSpec};
use crate::compute_bid::{compute_tip, TxType};
use crate::failure::{detect, FailureKind};
use crate::jito_client::JitoClient;
use crate::lifecycle::{
    bundle_status_watcher, rpc_confirm_watcher, wait_confirmed, LifecycleStage, LifecycleTracker,
};
use styx_ingest::bus::NetworkEvent;

pub type SignerFn = Arc<dyn Fn(Vec<VersionedTransaction>) -> anyhow::Result<Vec<VersionedTransaction>> + Send + Sync>;

pub fn emit_exec(
    bus: &Option<tokio::sync::broadcast::Sender<NetworkEvent>>,
    bundle_id: &str,
    stage: &str,
    tip_lamports: u64,
    retry: u32,
    regime: &str,
    message: &str,
) {
    if let Some(tx) = bus {
        let _ = tx.send(NetworkEvent::Execution {
            bundle_id: bundle_id.to_string(),
            stage: stage.to_string(),
            tip_lamports,
            retry,
            regime: regime.to_string(),
            message: message.to_string(),
            ts_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        });
    }
}

const MAX_RETRIES: u32 = 3;

#[derive(Debug)]
pub enum RetryOutcome {
    Confirmed { bundle_id: String, retries: u32, tip_lamports: u64 },
    Exhausted { retries: u32 },
    Terminal { reason: String },
    AlreadyLanded,
}

pub struct RetryContext {
    pub spec: BundleSpec,
    pub signer: SignerFn,
    pub payer: Pubkey,
    pub rpc: Arc<RpcClient>,
    pub jito: Arc<JitoClient>,
    pub tracker: Arc<Mutex<LifecycleTracker>>,
    pub current_bundle_id: String,
    pub advisor: Arc<dyn RetryAdvisor>,
    pub auction_window: Arc<Mutex<AuctionWindow>>,
    pub tx_type: TxType,
    pub value_lamports: u64,
    pub tip_ceiling: u64,
    pub exec_bus: Option<tokio::sync::broadcast::Sender<NetworkEvent>>,
    pub last_blockhash: Hash,
    pub last_multiplier: f64,
}

pub async fn run_retry_loop(
    mut ctx: RetryContext,
    initial_error: &str,
) -> Result<RetryOutcome> {
    let mut retries = 0u32;
    let mut error_msg = initial_error.to_string();
    let mut prev_tip = ctx.spec.tip_lamports;
    let mut prev_multiplier = ctx.last_multiplier;
    let started = Instant::now();

    loop {
        let kind = detect(
            &ctx.current_bundle_id,
            &ctx.last_blockhash,
            &error_msg,
            ctx.rpc.clone(),
            ctx.tracker.clone(),
        )
        .await;
        info!(bundle_id = %ctx.current_bundle_id, kind = ?kind, "detected failure cause");

        if !kind.is_recoverable() {
            emit_exec(&ctx.exec_bus, &ctx.current_bundle_id, "terminal",
                prev_tip, retries, "", &format!("unrecoverable: {:?}", kind));
            return Ok(RetryOutcome::Terminal { reason: format!("{:?}", kind) });
        }

        {
            let t = ctx.tracker.lock().await;
            if let Some(handle) = t.get(&ctx.current_bundle_id) {
                if matches!(
                    handle.stage,
                    LifecycleStage::Confirmed { .. } | LifecycleStage::Finalized { .. }
                ) {
                    info!(bundle_id = %ctx.current_bundle_id, "already landed, aborting retry");
                    return Ok(RetryOutcome::AlreadyLanded);
                }
            }
        }

        if retries >= MAX_RETRIES {
            warn!(bundle_id = %ctx.current_bundle_id, "exhausted max retries");
            emit_exec(&ctx.exec_bus, &ctx.current_bundle_id, "exhausted",
                prev_tip, retries, "", "exhausted max retries");
            return Ok(RetryOutcome::Exhausted { retries });
        }

        retries += 1;

        let window = ctx.auction_window.lock().await.clone();
        let signal = RetrySignal {
            failure_kind: format!("{:?}", kind),
            attempt: retries,
            previous_tip_lamports: prev_tip,
            previous_forward_multiplier: prev_multiplier,
            window: window.clone(),
            tx_type: ctx.tx_type,
            value_lamports: ctx.value_lamports,
            error: error_msg.clone(),
            seconds_elapsed: started.elapsed().as_secs(),
        };
        let advice = ctx.advisor.advise(signal).await;
        info!(
            bundle_id = %ctx.current_bundle_id,
            action = ?advice.action,
            forward_multiplier = advice.forward_multiplier,
            refresh_blockhash = advice.refresh_blockhash,
            "agent retry decision: {}", advice.reasoning
        );
        emit_exec(&ctx.exec_bus, &ctx.current_bundle_id, "ai_retry_decision",
            prev_tip, retries, "",
            &format!("Agent reasoned about {:?}: {} ({:.0}% confident)",
                kind, advice.reasoning, advice.confidence * 100.0));

        if advice.action == RetryAction::Abort {
            emit_exec(&ctx.exec_bus, &ctx.current_bundle_id, "terminal",
                prev_tip, retries, "", &format!("agent chose to abort: {}", advice.reasoning));
            return Ok(RetryOutcome::Terminal { reason: format!("agent aborted: {}", advice.reasoning) });
        }

        emit_exec(&ctx.exec_bus, &ctx.current_bundle_id, "retrying",
            prev_tip, retries, "",
            &format!("Attempt {} -- applying the agent's recovery decision", retries));

        sleep(Duration::from_secs(2)).await;

        let mut tip = compute_tip(&window, advice.forward_multiplier, ctx.tx_type, ctx.value_lamports, ctx.tip_ceiling);
        if kind == FailureKind::FeeTooLow || kind == FailureKind::Dropped {
            tip = tip.max(prev_tip + 1).min(ctx.tip_ceiling);
        }
        let old_tip = prev_tip;
        ctx.spec.tip_lamports = tip;
        prev_tip = tip;
        prev_multiplier = advice.forward_multiplier;
        ctx.spec.tip_account = ctx.jito.random_tip_account();

        let pct_up = if old_tip > 0 {
            (((tip as f64 - old_tip as f64) / old_tip as f64) * 100.0).round() as i64
        } else { 0 };
        emit_exec(&ctx.exec_bus, &ctx.current_bundle_id, "repriced",
            tip, retries, "",
            &format!("Re-priced tip {} -> {} lamports ({}{}%){}",
                old_tip, tip, if pct_up >= 0 { "+" } else { "" }, pct_up,
                if advice.refresh_blockhash { "  | refreshing blockhash" } else { "" }));

        let rpc = ctx.rpc.clone();
        let (blockhash, _) = tokio::task::block_in_place(|| {
            rpc.get_latest_blockhash_with_commitment(CommitmentConfig::confirmed())
        })?;
        ctx.last_blockhash = blockhash;

        let unsigned_txs = build_bundle_unsigned(&ctx.spec, &ctx.payer, blockhash)?;
        let txs = (ctx.signer)(unsigned_txs)?;

        let sigs: Vec<String> = txs
            .iter()
            .filter_map(|tx| tx.signatures.first())
            .map(|s| s.to_string())
            .collect();

        let encoded: Vec<String> = txs.iter().map(encode_transaction).collect::<Result<_>>()?;

        let new_bundle_id = ctx.jito.send_bundle(encoded).await?;
        let new_sigs = sigs.clone();
        ctx.tracker.lock().await.register(new_bundle_id.clone(), sigs);
        ctx.current_bundle_id = new_bundle_id.clone();

        let short_new = new_bundle_id.chars().take(8).collect::<String>();
        emit_exec(&ctx.exec_bus, &new_bundle_id, "resubmitted",
            tip, retries, "",
            &format!("Resubmitted as a new bundle {}...", short_new));

        let rpc_watch = tokio::spawn(rpc_confirm_watcher(
            ctx.rpc.clone(),
            new_sigs.into_iter().next().unwrap_or_default(),
            ctx.tracker.clone(),
            35,
        ));
        let status_watch = tokio::spawn(bundle_status_watcher(
            ctx.jito.clone(),
            new_bundle_id.clone(),
            ctx.tracker.clone(),
            60,
        ));
        let confirm_result = wait_confirmed(ctx.tracker.clone(), &new_bundle_id, 30).await;
        rpc_watch.abort();
        status_watch.abort();
        match confirm_result {
            Ok(_) => {
                info!(bundle_id = %new_bundle_id, retries, "confirmed after retry");
                emit_exec(&ctx.exec_bus, &new_bundle_id, "confirmed",
                    tip, retries, "", "confirmed after retry");
                return Ok(RetryOutcome::Confirmed {
                    bundle_id: new_bundle_id,
                    retries,
                    tip_lamports: tip,
                });
            }
            Err(e) => {
                warn!(bundle_id = %new_bundle_id, error = %e, "retry did not confirm");
                error_msg = e.to_string();
            }
        }
    }
}

// ---- Priority-fee retry ----

pub struct PriorityFeeRetryContext {
    pub instructions: Vec<Instruction>,
    pub compute_unit_limit: u32,
    pub signer: SignerFn,
    pub payer: Pubkey,
    pub rpc: Arc<RpcClient>,
    pub rpc_url: String,
    pub address_lookup_tables: Vec<solana_sdk::message::AddressLookupTableAccount>,
    pub tracker: Arc<Mutex<LifecycleTracker>>,
    pub current_sig: String,
    pub advisor: Arc<dyn RetryAdvisor>,
    pub auction_window: Arc<Mutex<AuctionWindow>>,
    pub tx_type: TxType,
    pub value_lamports: u64,
    pub tip_ceiling: u64,
    pub exec_bus: Option<tokio::sync::broadcast::Sender<NetworkEvent>>,
    pub last_blockhash: Hash,
    pub last_multiplier: f64,
}

pub async fn run_priority_fee_retry_loop(
    mut ctx: PriorityFeeRetryContext,
    initial_error: &str,
) -> Result<RetryOutcome> {
    let mut retries = 0u32;
    let mut error_msg = initial_error.to_string();
    let mut prev_tip = {
        let w = ctx.auction_window.lock().await.clone();
        compute_tip(&w, ctx.last_multiplier, ctx.tx_type, ctx.value_lamports, ctx.tip_ceiling)
    };
    let mut prev_multiplier = ctx.last_multiplier;
    let started = Instant::now();
    let http = reqwest::Client::new();

    let tracking_id = ctx.current_sig.clone();

    loop {
        let kind = detect(
            &tracking_id,
            &ctx.last_blockhash,
            &error_msg,
            ctx.rpc.clone(),
            ctx.tracker.clone(),
        )
        .await;
        info!(sig = %tracking_id, kind = ?kind, "priority-fee: detected failure cause");

        if !kind.is_recoverable() {
            emit_exec(&ctx.exec_bus, &tracking_id, "terminal", prev_tip, retries, "",
                &format!("unrecoverable: {:?}", kind));
            return Ok(RetryOutcome::Terminal { reason: format!("{:?}", kind) });
        }

        {
            let t = ctx.tracker.lock().await;
            if let Some(handle) = t.get(&tracking_id) {
                if matches!(handle.stage, LifecycleStage::Confirmed { .. } | LifecycleStage::Finalized { .. }) {
                    info!(sig = %tracking_id, "priority-fee: already confirmed, aborting retry");
                    return Ok(RetryOutcome::AlreadyLanded);
                }
            }
        }

        if retries >= MAX_RETRIES {
            warn!(sig = %tracking_id, "priority-fee: exhausted max retries");
            emit_exec(&ctx.exec_bus, &tracking_id, "exhausted", prev_tip, retries, "",
                "exhausted max retries");
            return Ok(RetryOutcome::Exhausted { retries });
        }

        retries += 1;

        let window = ctx.auction_window.lock().await.clone();
        let signal = RetrySignal {
            failure_kind: format!("{:?}", kind),
            attempt: retries,
            previous_tip_lamports: prev_tip,
            previous_forward_multiplier: prev_multiplier,
            window: window.clone(),
            tx_type: ctx.tx_type,
            value_lamports: ctx.value_lamports,
            error: error_msg.clone(),
            seconds_elapsed: started.elapsed().as_secs(),
        };
        let advice = ctx.advisor.advise(signal).await;
        info!(
            sig = %tracking_id,
            action = ?advice.action,
            forward_multiplier = advice.forward_multiplier,
            "priority-fee agent retry decision: {}", advice.reasoning
        );
        emit_exec(&ctx.exec_bus, &tracking_id, "ai_retry_decision", prev_tip, retries, "",
            &format!("Agent reasoned about {:?}: {} ({:.0}% confident)",
                kind, advice.reasoning, advice.confidence * 100.0));

        if advice.action == RetryAction::Abort {
            emit_exec(&ctx.exec_bus, &tracking_id, "terminal", prev_tip, retries, "",
                &format!("agent chose to abort: {}", advice.reasoning));
            return Ok(RetryOutcome::Terminal {
                reason: format!("agent aborted: {}", advice.reasoning),
            });
        }

        sleep(Duration::from_millis(500)).await;

        let mut tip = compute_tip(&window, advice.forward_multiplier, ctx.tx_type, ctx.value_lamports, ctx.tip_ceiling);
        if kind == FailureKind::FeeTooLow || kind == FailureKind::Dropped {
            tip = tip.max(prev_tip + 1).min(ctx.tip_ceiling);
        }
        let old_tip = prev_tip;
        prev_tip = tip;
        prev_multiplier = advice.forward_multiplier;

        let micro_lamports_per_cu = tip
            .saturating_mul(1_000_000)
            .checked_div(ctx.compute_unit_limit as u64)
            .unwrap_or(1_000);

        let pct_up = if old_tip > 0 {
            (((tip as f64 - old_tip as f64) / old_tip as f64) * 100.0).round() as i64
        } else { 0 };
        emit_exec(&ctx.exec_bus, &tracking_id, "repriced", tip, retries, "",
            &format!("Re-priced tip {} -> {} lamports ({}{}%)  | {}uL/CU",
                old_tip, tip,
                if pct_up >= 0 { "+" } else { "" }, pct_up,
                micro_lamports_per_cu));

        let rpc = ctx.rpc.clone();
        let (blockhash, _) = tokio::task::block_in_place(|| {
            rpc.get_latest_blockhash_with_commitment(CommitmentConfig::confirmed())
        })?;
        ctx.last_blockhash = blockhash;

        let mut ixs = Vec::with_capacity(2 + ctx.instructions.len());
        ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(ctx.compute_unit_limit));
        ixs.push(ComputeBudgetInstruction::set_compute_unit_price(micro_lamports_per_cu));
        ixs.extend_from_slice(&ctx.instructions);

        let msg = v0::Message::try_compile(&ctx.payer, &ixs, &ctx.address_lookup_tables, blockhash)
            .map_err(|e| anyhow::anyhow!("priority-fee retry compile: {}", e))?;
        let versioned_msg = VersionedMessage::V0(msg);
        let n_sigs = versioned_msg.header().num_required_signatures as usize;
        let unsigned_tx = VersionedTransaction {
            signatures: vec![Signature::default(); n_sigs],
            message: versioned_msg,
        };
        let signed_txs = (ctx.signer)(vec![unsigned_tx])?;
        let tx = signed_txs.into_iter().next()
            .ok_or_else(|| anyhow::anyhow!("signer returned empty vec for priority-fee retry"))?;

        let new_sig = tx.signatures.first().map(|s| s.to_string()).unwrap_or_default();

        let tx_b64 = encode_transaction(&tx)?;
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "sendTransaction",
            "params": [tx_b64, { "encoding": "base64", "preflightCommitment": "processed" }]
        });
        let rpc_resp = http
            .post(&ctx.rpc_url)
            .json(&body)
            .send().await
            .map_err(|e| anyhow::anyhow!("priority-fee retry send: {}", e))?
            .json::<serde_json::Value>().await
            .map_err(|e| anyhow::anyhow!("priority-fee retry parse: {}", e))?;

        if let Some(err) = rpc_resp.get("error").filter(|e| !e.is_null()) {
            error_msg = format!("sendTransaction: {}", err);
            warn!(sig = %new_sig, error = %err, "priority-fee retry: RPC rejected resubmission");
            continue;
        }

        ctx.tracker.lock().await.register(new_sig.clone(), vec![new_sig.clone()]);

        emit_exec(&ctx.exec_bus, &tracking_id, "resubmitted", tip, retries, "",
            &format!("Resubmitted as new tx {}...", new_sig.chars().take(8).collect::<String>()));

        let rpc_watch = tokio::spawn(rpc_confirm_watcher(
            ctx.rpc.clone(),
            new_sig.clone(),
            ctx.tracker.clone(),
            35,
        ));
        let confirm_result = wait_confirmed(ctx.tracker.clone(), &new_sig, 30).await;
        rpc_watch.abort();

        match confirm_result {
            Ok(_) => {
                info!(sig = %new_sig, retries, "priority-fee: confirmed after retry");
                emit_exec(&ctx.exec_bus, &tracking_id, "confirmed", tip, retries, "",
                    "confirmed after retry");
                return Ok(RetryOutcome::Confirmed {
                    bundle_id: new_sig,
                    retries,
                    tip_lamports: tip,
                });
            }
            Err(e) => {
                warn!(sig = %new_sig, error = %e, "priority-fee retry did not confirm");
                error_msg = e.to_string();
            }
        }
    }
}
