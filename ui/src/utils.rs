/// Format lamports as a human-readable SOL value with unit.
/// e.g. 450_348 → "0.000450 SOL"
pub fn lamports_to_sol(lamports: u64) -> String {
    if lamports == 0 {
        return "0 SOL".to_string();
    }
    let sol = lamports as f64 / 1_000_000_000.0;
    format!("{:.6} SOL", sol)
}

/// Numeric part only (no unit) — for large hero display where the unit is shown separately.
pub fn lamports_to_sol_num(lamports: u64) -> String {
    let sol = lamports as f64 / 1_000_000_000.0;
    format!("{:.6}", sol)
}

/// Signed delta — e.g. +450_348 → "+0.000450 SOL", -1000 → "-0.000001 SOL"
pub fn delta_to_sol(lamports: i64) -> String {
    if lamports == 0 {
        return "+0 SOL".to_string();
    }
    let sol = lamports as f64 / 1_000_000_000.0;
    format!("{:+.6} SOL", sol)
}

/// Truncate a long id/hash/pubkey to its first `n` characters for display.
pub fn short(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Turn an internal failure-kind code into a plain-English explanation.
pub fn humanize_failure(kind: &str) -> String {
    match kind {
        "Exhausted"        => "Didn't land — gave up after re-pricing the fee several times".to_string(),
        "Timeout"          => "Didn't confirm in time".to_string(),
        "BundleFailure"    => "Rejected by the network (a transaction failed)".to_string(),
        "ExpiredBlockhash" => "The transaction went stale before it landed".to_string(),
        "ComputeExceeded"  => "The transaction ran out of compute budget".to_string(),
        "FeeTooLow"        => "The fee wasn't high enough to win the slot".to_string(),
        "Dropped"          => "The network dropped the transaction".to_string(),
        other              => other.to_string(),
    }
}
