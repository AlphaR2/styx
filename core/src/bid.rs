use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use crate::auction::AuctionWindow;
use crate::compute_bid::TxType;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Regime {
    Cold,
    Warm,
    Hot,
    Manic,
}

#[derive(Debug, Clone, Serialize)]
pub struct BidContext {
    pub window: AuctionWindow,
    pub tx_type: TxType,
    pub value_lamports: u64,
    pub recent_outcomes: Vec<BundleOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleOutcome {
    pub tip_lamports: u64,
    pub landed: bool,
    pub forward_multiplier: f64,
    pub clearing_price_at_submission: u64,
    pub ts_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOutput {
    pub regime: Regime,
    pub forward_multiplier: f64,
    pub reasoning: String,
    pub confidence: f64,
}

pub trait BidStrategy: Send + Sync {
    fn bid<'a>(
        &'a self,
        ctx: BidContext,
    ) -> Pin<Box<dyn Future<Output = AgentOutput> + Send + 'a>>;
}

#[derive(Debug, Clone, Serialize)]
pub struct BidDecision {
    pub tip_lamports: u64,
    pub baseline_tip_lamports: u64,
    pub delta_lamports: i64,
    pub agent_output: AgentOutput,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetrySignal {
    pub failure_kind: String,
    pub attempt: u32,
    pub previous_tip_lamports: u64,
    pub previous_forward_multiplier: f64,
    pub window: AuctionWindow,
    pub tx_type: TxType,
    pub value_lamports: u64,
    pub error: String,
    pub seconds_elapsed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryAdvice {
    pub action: RetryAction,
    pub forward_multiplier: f64,
    pub refresh_blockhash: bool,
    pub reasoning: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RetryAction {
    Retry,
    Abort,
}

pub trait RetryAdvisor: Send + Sync {
    fn advise<'a>(
        &'a self,
        signal: RetrySignal,
    ) -> Pin<Box<dyn Future<Output = RetryAdvice> + Send + 'a>>;
}
