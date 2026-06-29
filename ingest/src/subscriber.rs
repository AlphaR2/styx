use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;
use tokio::sync::broadcast;
use tokio::time::sleep;
use tracing::{error, info, warn};
use tonic::transport::ClientTlsConfig;
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::prelude::{
    subscribe_update::UpdateOneof,
    CommitmentLevel,
    SubscribeRequest,
    SubscribeRequestFilterAccounts,
    SubscribeRequestFilterSlots,
    SubscribeRequestFilterTransactions,
};

use crate::bus::{LifecycleEvent, NetworkEvent, SlotStatus};


// The 8 Jito tip accounts. Monitoring their lamport balance changes gives us
// empirical clearing prices for the tip auction without polling the REST API.
const JITO_TIP_ACCOUNTS: &[&str] = &[
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1sTaC4qCK38",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6Lv",
];

/// Connects to Yellowstone and publishes to both buses.
/// Market events go to the lossy network bus; lifecycle events go to the dedicated bus.
/// Runs forever -- reconnects with exponential backoff on any error.
pub async fn run(
    endpoint: String,
    token: String,
    bus: broadcast::Sender<NetworkEvent>,
    lifecycle_bus: broadcast::Sender<LifecycleEvent>,
    payer_pubkey: String,
) {
    let mut backoff = Duration::from_secs(1);
    // Whether we've ever reached an active subscription. Until we have, a "transport error"
    // almost always means the endpoint/token is wrong rather than a transient network blip,
    // so we surface a louder, actionable hint instead of the cryptic tonic message.
    let mut ever_connected = false;

    loop {
        info!("connecting to Yellowstone at {}", endpoint);
        match stream_events(&endpoint, &token, &bus, &lifecycle_bus, &mut ever_connected, &payer_pubkey).await {
            Ok(()) => {
                warn!("Yellowstone stream closed, reconnecting");
                backoff = Duration::from_secs(1);
            }
            Err(e) if is_resource_exhausted(&e) || is_gateway_protocol_error(&e) => {
                // Two failure modes that both leave a phantom stream registered server-side, so
                // a fast retry just blocks itself -- fatal on a 1-concurrent-stream tier:
                //   * ResourceExhausted: the provider still counts our PREVIOUS session as alive
                //     because its server-side timeout is longer than a fast reconnect.
                //   * A non-gRPC HTTP error (e.g. 400 from the edge gateway, surfaced as an
                //     "invalid compression flag" protocol error): the aborted attempt can still
                //     occupy the slot until it times out.
                // The old client has already been dropped (stream_events returned), so wait a
                // fixed, longer interval to let the slot free up before trying again.
                error!(
                    "Yellowstone stream slot unavailable ({}). Waiting {}s for the previous \
                     session to expire before reconnecting. If this persists, another instance is \
                     likely still running on the same token.",
                    e,
                    STREAM_LIMIT_DELAY.as_secs()
                );
                sleep(STREAM_LIMIT_DELAY).await;
                backoff = Duration::from_secs(1);
            }
            Err(e) => {
                error!("Yellowstone error: {}. Reconnecting in {}s", e, backoff.as_secs());
                // A transport error before the first successful subscription is the classic
                // signature of bad credentials: the gateway resets the HTTP/2 connection on
                // auth rejection, which tonic reports as a generic "transport error".
                if !ever_connected && is_transport_error(&e) {
                    warn!(
                        "Could not establish a Yellowstone subscription. The network path is up, \
                         so this is most likely an auth problem. Check that YELLOWSTONE_ENDPOINT \
                         and YELLOWSTONE_TOKEN are correct and the key is active (some providers \
                         expect the token without any prefix). Verify the sibling RPC key with: \
                         curl -X POST \"$RPC_URL\" -H 'content-type: application/json' \
                         -d '{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getHealth\"}}'"
                    );
                }
                sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

/// How long to wait after hitting the per-tier stream limit before re-dialing.
/// Must exceed the provider's server-side session timeout so the stale stream is
/// released first; ~15s is comfortably above the typical Yellowstone idle timeout.
const STREAM_LIMIT_DELAY: Duration = Duration::from_secs(15);

/// Returns true if the error chain contains a tonic/h2 transport-level failure.
/// These surface as the opaque "transport error" and, before a first successful
/// subscription, usually mean bad credentials rather than a flaky connection.
fn is_transport_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause.is::<tonic::transport::Error>()
            || cause.to_string().to_lowercase().contains("transport error")
    })
}

/// Returns true if the error is a gRPC ResourceExhausted status -- i.e. the provider's
/// per-tier concurrent-stream limit. Matched on the tonic Status code so it's robust to
/// wording changes in the human-readable message.
fn is_resource_exhausted(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<tonic::Status>()
            .is_some_and(|s| s.code() == tonic::Code::ResourceExhausted)
    })
}

/// Returns true if the edge gateway returned a non-gRPC HTTP error (e.g. a 400 page) and
/// tonic tried to parse the HTML body as a gRPC frame -- surfaced as an "invalid compression
/// flag" protocol error. On a 1-stream tier this aborted attempt can still hold the slot, so
/// we treat it like a limit hit and wait it out rather than hammering 1s retries.
fn is_gateway_protocol_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        let msg = cause.to_string().to_lowercase();
        msg.contains("invalid compression flag") || msg.contains("400 bad request")
    })
}

/// Opens one connection and streams events until the stream ends or errors.
/// This is where we get the yellowstone streams of value
async fn stream_events(
    endpoint: &str,
    token: &str,
    bus: &broadcast::Sender<NetworkEvent>,
    lifecycle_bus: &broadcast::Sender<LifecycleEvent>,
    ever_connected: &mut bool,
    payer_pubkey: &str,
) -> Result<()> {
    // Build the gRPC client with explicit TLS config.
    // tonic does not enable TLS from the https:// scheme alone -- .tls_config() is required
    // or the TCP connection is established but TLS is never initiated, causing transport error.
    //
    // .tls_config() requires at least one trust anchor; ClientTlsConfig::new() starts with
    // none, so certificate verification fails for every server without with_native_roots().
    let mut client = GeyserGrpcClient::build_from_shared(endpoint.to_string())?
        .x_token(Some(token.to_string()))?
        // with_native_roots() uses the OS certificate store, which covers enterprise
        // Yellowstone providers (Helius, Triton, Shyft) that may use certs not in the
        // bundled Mozilla webpki list. solana-streamer uses this for the same reason.
        .tls_config(ClientTlsConfig::new().with_native_roots())?
        // Tonic's default is 4 MiB. A densely-packed Solana block can exceed this,
        // causing the stream to error and disconnect mid-slot. 16 MiB is generous
        // enough to avoid spurious disconnects (solana-streamer uses 10 MiB).
        .max_decoding_message_size(16 * 1024 * 1024)
        // HTTP/2 PING frames every 10s keep the stream alive through NAT timeouts.
        // Without this, a long-lived gRPC stream dies silently: the TCP connection
        // appears open but stream.next() hangs forever and no error fires, so the
        // reconnection loop never triggers (richat defaults to 15s; 10s is safer).
        .http2_keep_alive_interval(std::time::Duration::from_secs(10))
        .keep_alive_while_idle(true)
        // TCP-level keepalive as a second layer against NAT eviction.
        .tcp_keepalive(Some(std::time::Duration::from_secs(15)))
        .tcp_nodelay(true)
        .connect()
        .await?;

    let request = SubscribeRequest {
        slots: HashMap::from([(
            "slots".to_string(),
            SubscribeRequestFilterSlots {
                filter_by_commitment: None,     // receive all commitment levels
                interslot_updates: Some(false), // only emit on slot boundary, not mid-slot
            },
        )]),
        accounts: HashMap::from([(
            "tips".to_string(),
            SubscribeRequestFilterAccounts {
                account: JITO_TIP_ACCOUNTS.iter().map(|s| s.to_string()).collect(),
                owner: vec![],
                ..Default::default()
            },
        )]),
        transactions: HashMap::from([(
            "payer".to_string(),
            SubscribeRequestFilterTransactions {
                vote: Some(false),
                failed: Some(false),
                account_include: vec![payer_pubkey.to_string()],
                ..Default::default()
            },
        )]),
        commitment: Some(CommitmentLevel::Processed as i32),
        ..Default::default()
    };

    // subscribe_once sends the request as the FIRST message when the stream opens, atomically.
    // The two-step alternative (subscribe() then sink.send(request)) races on a 1-stream tier:
    // the server can tear down the request-less subscription before our send lands, producing
    // "send failed because receiver is gone" and leaving a half-open stream that then trips
    // "max concurrent streams" on the next attempt.
    let mut stream = client.subscribe_once(request).await?;

    info!("Yellowstone subscription active");
    // Mark that we've reached a healthy subscription at least once. From here on,
    // a transport error is treated as a transient reconnect rather than a credential issue.
    *ever_connected = true;

    // Track previous Jito tip account balances so we can emit deltas, not raw balances.
    let mut prev_tip_lamports: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

    let payer_bytes = match bs58::decode(payer_pubkey).into_vec() {
        Ok(b) if b.len() == 32 => b,
        Ok(b) => {
            warn!(payer = %payer_pubkey, len = b.len(), "payer pubkey decoded to wrong length — TxSeen events will not fire");
            b
        }
        Err(e) => {
            warn!(payer = %payer_pubkey, error = %e, "payer pubkey base58 decode failed — TxSeen events will not fire");
            Vec::new()
        }
    };
    info!(payer = %payer_pubkey, payer_bytes_len = payer_bytes.len(), "Yellowstone payer filter ready");

    // Read events from the stream and publish them to the bus.
    while let Some(msg) = stream.next().await {
        let update = msg?;

        match update.update_oneof {
            Some(UpdateOneof::Slot(s)) => {
                // Map proto slot status (i32) to our enum.
                // 0 = Processed, 1 = Confirmed, 2 = Rooted (treated as Finalized).
                let status = match s.status {
                    0 => SlotStatus::Processed,
                    1 => SlotStatus::Confirmed,
                    2 => SlotStatus::Finalized,
                    _ => continue,
                };
                // Publish to both buses: network bus for UI/WS, lifecycle bus for the tracker.
                let _ = bus.send(NetworkEvent::SlotUpdate {
                    slot: s.slot,
                    parent: s.parent,
                    status: status.clone(),
                });
                let _ = lifecycle_bus.send(LifecycleEvent::SlotUpdate {
                    slot: s.slot,
                    parent: s.parent,
                    status,
                });
            }
            Some(UpdateOneof::Transaction(t)) => {
                if let Some(info) = t.transaction {
                    // Build the FULL account list in canonical resolution order:
                    // static keys, then ALT-loaded writable, then loaded readonly.
                    // Versioned (v0) transactions reference these via the same index
                    // space, so without the loaded addresses the indices are wrong.
                    let msg_opt = info.transaction.as_ref().and_then(|tx| tx.message.as_ref());
                    let mut full_keys: Vec<Vec<u8>> = Vec::new();
                    if let Some(msg) = msg_opt {
                        full_keys = msg.account_keys.clone();
                        if let Some(meta) = &info.meta {
                            full_keys.extend(meta.loaded_writable_addresses.iter().cloned());
                            full_keys.extend(meta.loaded_readonly_addresses.iter().cloned());
                        }
                    }

                    let is_payer_tx = full_keys.iter().any(|k| k == &payer_bytes);
                    if is_payer_tx {
                        let sig = bs58::encode(&info.signature).into_string();
                        info!(sig = %sig, slot = t.slot, "Yellowstone: payer tx seen on-chain");
                        let _ = bus.send(NetworkEvent::TxSeen { sig: sig.clone(), slot: t.slot });
                        let _ = lifecycle_bus.send(LifecycleEvent::TxSeen { sig, slot: t.slot });
                    }
                }
            }
            Some(UpdateOneof::Account(t)) => {
                if let Some(info) = t.account {
                    let pubkey = bs58::encode(&info.pubkey).into_string();
                    if JITO_TIP_ACCOUNTS.contains(&pubkey.as_str()) {
                        let ts_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        match prev_tip_lamports.get(&pubkey).copied() {
                            None => {
                                // First time seeing this account: establish baseline.
                                prev_tip_lamports.insert(pubkey, info.lamports);
                            }
                            Some(prev) if info.lamports > prev => {
                                let delta = info.lamports - prev;
                                prev_tip_lamports.insert(pubkey.clone(), info.lamports);
                                tracing::trace!(
                                    pubkey = %pubkey, slot = t.slot,
                                    tip_lamports = delta, "Jito tip account balance increase"
                                );
                                let _ = bus.send(crate::bus::NetworkEvent::JitoTip {
                                    slot: t.slot,
                                    tip_lamports: delta,
                                    ts_ms,
                                });
                            }
                            Some(_) => {
                                // Balance unchanged or decreased (validator withdrawal).
                                prev_tip_lamports.insert(pubkey, info.lamports);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}
