use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde_json::json;
use solana_sdk::pubkey::Pubkey;
use tracing::{info, warn};

static HTTP: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
fn http() -> &'static reqwest::Client {
    HTTP.get_or_init(|| reqwest::Client::new())
}

// Short human label for a block-engine URL (e.g. the "frankfurt" subdomain) for logs.
fn region_label(base_url: &str) -> String {
    base_url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('.').next())
        .unwrap_or(base_url)
        .to_string()
}

fn extract_jito_error(v: &serde_json::Value) -> String {
    v.get("error")
        .and_then(|e| e.get("message").and_then(|m| m.as_str()).map(str::to_string))
        .or_else(|| v.get("error").map(|e| e.to_string()))
        .unwrap_or_else(|| v.to_string())
}

/// The 8 Jito tip accounts (constant, published by Jito via getTipAccounts).
/// Jito recommends selecting one at random per submission to reduce write-lock
/// contention, which is what `random_tip_account` does.
pub const TIP_ACCOUNTS: [&str; 8] = [
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
];

/// Authoritative landing info from getBundleStatuses: the real slot and
/// commitment status straight from Jito (no get_slot() approximation needed).
#[derive(Debug, Clone)]
pub struct BundleCommitment {
    pub slot: u64,                   // slot the bundle landed in
    pub confirmation_status: String, // "processed" | "confirmed" | "finalized"
    pub signatures: Vec<String>,     // base58 tx signatures in the bundle
}

pub struct JitoClient {
    // All regional block-engine base URLs (e.g. ".../api/v1"). Bundles are fanned
    // out to every entry; getBundleStatuses queries the primary (first) one.
    block_engine_urls: Vec<String>,
}

impl JitoClient {
    pub fn new(block_engine_urls: Vec<String>) -> Self {
        let urls = if block_engine_urls.is_empty() {
            // Never run with zero endpoints — fall back to the global engine.
            vec!["https://mainnet.block-engine.jito.wtf/api/v1".to_string()]
        } else {
            block_engine_urls
        };
        info!(regions = urls.len(), "Jito client configured for multi-region submission");
        JitoClient { block_engine_urls: urls }
    }

    /// Primary endpoint, used for getBundleStatuses (bundle UUIDs are per-engine).
    fn primary_url(&self) -> &str {
        &self.block_engine_urls[0]
    }

    /// Returns the 8 Jito tip accounts.
    /// The SDK's get_tip_accounts sends to the wrong endpoint (/bundles instead of /getTipAccounts).
    /// Jito documents these as constant, so we return them directly rather than making a broken RPC call.
    pub async fn get_tip_accounts(&self) -> Result<Vec<String>> {
        Ok(TIP_ACCOUNTS.iter().map(|s| s.to_string()).collect())
    }

    /// Pick one of the 8 tip accounts at random per submission (Jito's
    /// recommendation) to spread write-lock contention across them. Uses
    /// nanosecond entropy so we don't pull in an rng dependency on the hot path.
    pub fn random_tip_account(&self) -> Pubkey {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize)
            .unwrap_or(0)
            % TIP_ACCOUNTS.len();
        Pubkey::from_str(TIP_ACCOUNTS[n]).expect("hardcoded tip account is a valid pubkey")
    }

    /// Send a single transaction to Jito's `/api/v1/transactions` endpoint.
    ///
    /// This is a proxied `sendTransaction` — Jito forwards it to validators with MEV
    /// protection but there is NO bundle atomicity and NO bundle UUID. The return value
    /// is the transaction signature (not a bundle ID), which doubles as the tracking ID.
    ///
    /// When to use this lane instead of `send_bundle`:
    ///   - You want the simplest possible submission path (no bundle overhead).
    ///   - Your transaction already has the tip embedded as a transfer instruction.
    ///   - You do NOT need atomic pairing with a second transaction.
    ///   - You are not competing in a high-contention auction where atomicity matters.
    ///
    /// Why we currently use `send_bundle` instead:
    ///   The bundle lane (`/api/v1/bundles`) gives us a bundle UUID for `getBundleStatuses`,
    ///   atomic multi-tx execution (swap + tip in one unit), and multi-region fan-out.
    ///   This function exists as a lighter alternative for future use cases where those
    ///   guarantees are not needed (e.g. a simple tip-embedded transfer or memo tx).
    #[allow(dead_code)]
    pub async fn send_jito_transaction(&self, encoded_tx: String) -> Result<String> {
        let url = format!("{}/transactions", self.primary_url().trim_end_matches('/'));
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "sendTransaction",
            "params": [encoded_tx, { "encoding": "base64" }]
        });
        let v: serde_json::Value = http()
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Jito sendTransaction request failed")?
            .json()
            .await
            .context("Jito sendTransaction parse failed")?;
        match v["result"].as_str() {
            Some(sig) => {
                info!(sig = %sig, "Jito transaction submitted");
                Ok(sig.to_string())
            }
            None => {
                let reason = extract_jito_error(&v);
                anyhow::bail!("Jito rejected transaction: {}", reason);
            }
        }
    }

    /// Submit a bundle (1-5 base64 transactions) to EVERY configured region
    /// concurrently. A bundle only lands if it reaches the engine that feeds the
    /// current Jito leader, so fanning out across regions is what makes bundles land
    /// reliably instead of only when the leader happens to sit behind one region.
    ///
    /// Each engine returns its own bundle UUID for the same transactions; we return
    /// the first acceptance (used for getBundleStatuses). Authoritative landing
    /// confirmation is by transaction signature (Yellowstone + getSignatureStatuses),
    /// which is region-agnostic. Succeeds if at least one region accepts.
    pub async fn send_bundle(&self, encoded_txs: Vec<String>) -> Result<String> {
        info!(tx_count = encoded_txs.len(), regions = self.block_engine_urls.len(), "→ sendBundle");
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "sendBundle",
            "params": [encoded_txs, { "encoding": "base64" }],
        });

        // Fire all regions at once.
        let futures = self.block_engine_urls.iter().map(|base| {
            let url = format!("{}/bundles", base.trim_end_matches('/'));
            let region = region_label(base);
            let body = body.clone();
            async move {
                info!(region = %region, url = %url, "→ sendBundle region");
                match http()
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(resp) => {
                        let status = resp.status();
                        match resp.json::<serde_json::Value>().await {
                            Ok(v) => {
                                info!(region = %region, http_status = %status, response = %v, "← sendBundle region");
                                match v["result"].as_str() {
                                    Some(id) => Ok((region, id.to_string())),
                                    None => Err(format!("{}: {}", region, extract_jito_error(&v))),
                                }
                            }
                            Err(e) => Err(format!("{}: parse failed: {}", region, e)),
                        }
                    }
                    Err(e) => {
                        warn!(region = %region, error = %e, "✗ sendBundle region request failed");
                        Err(format!("{}: request failed: {}", region, e))
                    }
                }
            }
        });

        let results = futures::future::join_all(futures).await;

        let mut accepted: Option<String> = None;
        let mut errors: Vec<String> = Vec::new();
        for r in results {
            match r {
                Ok((region, id)) => {
                    info!(region = %region, bundle_id = %id, "✓ bundle accepted by region");
                    if accepted.is_none() {
                        accepted = Some(id);
                    }
                }
                Err(e) => {
                    warn!(reason = %e, "✗ region rejected bundle");
                    errors.push(e);
                }
            }
        }

        match accepted {
            Some(id) => {
                info!(bundle_id = %id, "bundle submitted");
                Ok(id)
            }
            None => anyhow::bail!("Jito rejected bundle in all regions: {}", errors.join("; ")),
        }
    }

    /// Query getBundleStatuses for one bundle id. Returns None until the bundle
    /// has landed (Jito returns a null/empty value before then). Gives the real
    /// landed slot and commitment status ("processed"|"confirmed"|"finalized")
    /// straight from Jito — the authoritative source for the lifecycle log.
    pub async fn get_bundle_statuses(&self, bundle_id: &str) -> Result<Option<BundleCommitment>> {
        let be = self.primary_url().trim_end_matches('/');
        let url = format!("{}/getBundleStatuses", be);
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "getBundleStatuses",
            "params": [[bundle_id]]
        });
        let v: serde_json::Value = http()
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("getBundleStatuses request failed")?
            .json()
            .await
            .context("getBundleStatuses parse failed")?;

        info!(bundle_id = %bundle_id, response = %v, "← getBundleStatuses");

        // result.value is an array; the entry is null until the bundle lands.
        let entry = match v["result"]["value"].as_array().and_then(|a| a.first()) {
            Some(e) if !e.is_null() => e.clone(),
            _ => return Ok(None),
        };

        // Jito serializes success as `"err":{"Ok":null}` (Rust Result style).
        // A real failure looks like `"err":{"Err":{"BundleDropped":...}}` or similar.
        // We only bail when err is present, non-null, AND not the success sentinel.
        let err_field = &entry["err"];
        let is_success_sentinel = err_field.get("Ok").map(|v| v.is_null()).unwrap_or(false);
        if !err_field.is_null() && !is_success_sentinel {
            anyhow::bail!("Jito bundle dropped: {}", err_field);
        }

        let (slot, confirmation_status) =
            match (entry["slot"].as_u64(), entry["confirmation_status"].as_str()) {
                (Some(s), Some(c)) => (s, c.to_string()),
                _ => return Ok(None),
            };
        let signatures = entry["transactions"]
            .as_array()
            .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        info!(bundle_id = %bundle_id, slot, status = %confirmation_status, "getBundleStatuses landed");
        Ok(Some(BundleCommitment { slot, confirmation_status, signatures }))
    }
}
