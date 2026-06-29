use crate::bid::Regime;

#[derive(Debug, Clone, PartialEq)]
pub enum LaneChoice {
    JitoBundle,
    PriorityFee,
}

pub struct LaneRouter;

impl LaneRouter {
    pub fn choose(_regime: &Regime) -> LaneChoice {
        LaneChoice::JitoBundle
    }
}
