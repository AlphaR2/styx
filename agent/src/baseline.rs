use std::future::Future;
use std::pin::Pin;

use styx_core::bid::{AgentOutput, BidContext, BidStrategy};

// The naive overpayer: always bids at 2x the baseline regardless of conditions.
// Models a fearful trader who overbids out of fear of missing a slot.
// Runs in parallel on every submission so its tip and delta are logged alongside Claude.
pub struct OverpayerBaseline;

impl OverpayerBaseline {
    pub fn new() -> Self { OverpayerBaseline }

    pub fn decide(&self, ctx: &BidContext) -> AgentOutput {
        AgentOutput {
            regime: ctx.window.regime.clone(),
            forward_multiplier: 2.0,
            reasoning: "baseline: always bid 2x clearing price, no contention analysis".to_string(),
            confidence: 1.0,
        }
    }
}

impl BidStrategy for OverpayerBaseline {
    fn bid<'a>(
        &'a self,
        ctx: BidContext,
    ) -> Pin<Box<dyn Future<Output = AgentOutput> + Send + 'a>> {
        Box::pin(async move { self.decide(&ctx) })
    }
}
