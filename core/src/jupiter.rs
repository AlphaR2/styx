// Jupiter v6 swap integration.
//
// Uses Jupiter's /swap endpoint to get a pre-built, pre-validated VersionedTransaction.
// We return it with the payer's signature slot zeroed so the caller can sign it.
// Jupiter's pre-signatures (ephemeral AMM keys) are preserved in all other slots.

use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine as _};
use solana_sdk::{pubkey::Pubkey, signature::Signature, transaction::VersionedTransaction};

pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

/// Everything the execute path needs after a successful swap build.
pub struct JupSwap {
    /// Pre-built VersionedTransaction from Jupiter, payer slot zeroed.
    /// All other signature slots (Jupiter pre-sigs for AMM ephemeral keys) are preserved.
    pub tx: VersionedTransaction,
    pub in_amount:   u64,
    pub out_amount:  u64,
    pub output_mint: String,
    pub last_valid_block_height: u64,
}

fn api_base() -> String {
    std::env::var("JUPITER_API_BASE")
        .unwrap_or_else(|_| "https://lite-api.jup.ag".to_string())
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("styx/0.1")
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .expect("reqwest client")
}

/// Quote → pre-built swap tx → zero payer slot (Jupiter pre-sigs are preserved).
///
/// `prioritization_fee_lamports` controls how the swap pays for inclusion:
///   - `0`  → Jito lane: zero CU price. The swap carries NO tip; the caller submits it
///            as tx[0] of a 2-tx Jito bundle whose tx[1] is a separate tip transfer.
///            We do not use Jupiter's `jitoTipLamports` because the free lite-api
///            endpoint ignores it, leaving the bundle with no tip-account write —
///            Jito then rejects it ("must write lock a tip account").
///   - `>0` → priority-fee lane: Jupiter bakes a real priority fee into the swap's own
///            compute-budget instructions so the tx can be sent over standard RPC.
///
/// Returns a `JupSwap` with `tx.signatures[payer_slot] == Signature::default()`.
/// The caller fills that slot before submission.
pub async fn build_swap(
    payer: &Pubkey,
    input_mint:  &str,
    output_mint: &str,
    amount_lamports: u64,
    slippage_bps: u16,
    prioritization_fee_lamports: u64,
    // Jito bundles cannot touch vote accounts. When true we exclude the small set of AMMs
    // known to write-lock vote accounts (GooseFX, Crema, Lifinity, Saros) while keeping
    // multi-hop enabled — so Raydium/Orca/Meteora/Phoenix paths still compete for best price.
    // Priority-fee lane passes false to allow all routes.
    exclude_vote_lock_dexes: bool,
) -> Result<JupSwap> {
    let http  = http_client();
    let base  = api_base();
    let user  = payer.to_string();

    // 1. Quote — raw JSON so we're not tied to the SDK's struct definition.
    // AMMs that write-lock vote accounts — excluded on the Jito lane to prevent bundle
    // rejection. Multi-hop through safe AMMs (Raydium, Orca, Meteora, Phoenix) is still
    // allowed, giving better pricing than onlyDirectRoutes=true would.
    const VOTE_LOCK_DEXES: &str = "GooseFX,Crema,Lifinity,Saros";

    let exclude_param = if exclude_vote_lock_dexes {
        format!("&excludeDexes={VOTE_LOCK_DEXES}")
    } else {
        String::new()
    };

    let quote_url = format!(
        "{base}/swap/v1/quote\
         ?inputMint={input_mint}&outputMint={output_mint}\
         &amount={amount_lamports}&slippageBps={slippage_bps}\
         &restrictIntermediateTokens=true\
         {exclude_param}"
    );
    let quote_resp = http.get(&quote_url)
        .header("accept", "application/json")
        .send().await
        .with_context(|| format!("jupiter quote GET failed: {quote_url}"))?;

    let quote_status = quote_resp.status();
    let quote_body   = quote_resp.text().await.context("jupiter quote: read body")?;
    if !quote_status.is_success() {
        anyhow::bail!("jupiter quote HTTP {quote_status}: {quote_body}");
    }
    let quote: serde_json::Value = serde_json::from_str(&quote_body)
        .with_context(|| format!("jupiter quote parse: {quote_body}"))?;

    if quote.get("error").is_some() {
        anyhow::bail!("jupiter quote error: {}", quote);
    }

    let in_amount  = quote["inAmount"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0u64);
    let out_amount = quote["outAmount"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0u64);

    // 2. Swap transaction — pass the raw quote JSON straight back.
    //    dynamic_compute_unit_limit: Jupiter simulates to size CU precisely.
    //    wrap_and_unwrap_sol: Jupiter handles WSOL wrapping.
    let mut swap_body = serde_json::json!({
        "quoteResponse": quote,
        "userPublicKey":  user,
        "payer":          user,
        "wrapAndUnwrapSol": true,
        "dynamicComputeUnitLimit": true,
    });
    if prioritization_fee_lamports > 0 {
        swap_body["prioritizationFeeLamports"] = serde_json::json!(prioritization_fee_lamports);
    } else {
        swap_body["computeUnitPriceMicroLamports"] = serde_json::json!(0);
    }
    let swap_url = format!("{base}/swap/v1/swap");
    let swap_resp = http.post(&swap_url)
        .header("accept", "application/json")
        .json(&swap_body)
        .send().await
        .with_context(|| format!("jupiter swap POST failed: {swap_url}"))?;

    let swap_status = swap_resp.status();
    let swap_body_text = swap_resp.text().await.context("jupiter swap: read body")?;
    if !swap_status.is_success() {
        anyhow::bail!("jupiter swap HTTP {swap_status}: {swap_body_text}");
    }
    let swap_json: serde_json::Value = serde_json::from_str(&swap_body_text)
        .with_context(|| format!("jupiter swap parse: {swap_body_text}"))?;

    if let Some(err) = swap_json.get("error") {
        anyhow::bail!("jupiter swap error: {}", err);
    }

    let tx_b64 = swap_json["swapTransaction"]
        .as_str()
        .context("jupiter swap: missing swapTransaction field")?;

    let last_valid_block_height = swap_json["lastValidBlockHeight"]
        .as_u64()
        .unwrap_or(0);

    // 3. Decode → zero our slot → preserve all other signatures.
    //
    // try_new would wipe Jupiter's pre-signatures (e.g. ephemeral keys for some AMM routes).
    // Instead we find our slot by pubkey and zero only that slot. The caller fills it in
    // before submission; all other pre-sigs remain intact.
    let raw = general_purpose::STANDARD
        .decode(tx_b64)
        .context("jupiter tx base64 decode")?;
    let mut tx: VersionedTransaction =
        bincode::deserialize(&raw).context("jupiter tx bincode deserialize")?;

    let num_required = tx.message.header().num_required_signatures as usize;
    let static_keys = tx.message.static_account_keys();
    let our_slot = static_keys
        .iter()
        .take(num_required)
        .position(|k| k == payer)
        .with_context(|| format!(
            "payer {} is not in the required signers list (first {} of {} accounts)",
            payer, num_required, static_keys.len()
        ))?;

    tx.signatures[our_slot] = Signature::default();

    tracing::info!(
        in_amount, out_amount, last_valid_block_height,
        num_required_signatures = num_required,
        our_sig_slot = our_slot,
        total_signatures = tx.signatures.len(),
        "jupiter swap tx built — payer slot zeroed"
    );

    Ok(JupSwap {
        tx,
        in_amount,
        out_amount,
        output_mint: output_mint.to_string(),
        last_valid_block_height,
    })
}

/// Encode a VersionedTransaction to base64 for Jito submission.
pub fn encode_jup_tx(tx: &VersionedTransaction) -> Result<String> {
    let bytes = bincode::serialize(tx).context("jup tx serialize")?;
    Ok(general_purpose::STANDARD.encode(bytes))
}

/// First signature from a transaction as a base58 string.
pub fn first_sig(tx: &VersionedTransaction) -> String {
    tx.signatures.first().map(|s| s.to_string()).unwrap_or_default()
}

/// Extract the recentBlockhash from a VersionedTransaction.
/// Used so the tip tx in a 2-tx Jito bundle can share the same blockhash as the
/// Jupiter swap tx — Jito requires all transactions in a bundle to use the same one.
pub fn recent_blockhash(tx: &VersionedTransaction) -> solana_sdk::hash::Hash {
    match &tx.message {
        solana_sdk::message::VersionedMessage::Legacy(m) => m.recent_blockhash,
        solana_sdk::message::VersionedMessage::V0(m) => m.recent_blockhash,
        solana_sdk::message::VersionedMessage::V1(_) => solana_sdk::hash::Hash::default(),
    }
}
