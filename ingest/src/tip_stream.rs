use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

use crate::bus::NetworkEvent;

// Jito streams live tip-floor percentiles over this WebSocket. Pushed, not
// requested — so the tip floor arrives as a stream instead of a REST timer.
const DEFAULT_TIP_STREAM_URL: &str = "wss://bundles.jito.wtf/api/v1/bundles/tip_stream";

// One tip-floor sample. Same field shape as the REST tip_floor payload, so the
// stream and REST formats are interchangeable.
#[derive(Deserialize)]
struct TipEntry {
    landed_tips_25th_percentile: f64,
    landed_tips_50th_percentile: f64,
    landed_tips_75th_percentile: f64,
    landed_tips_95th_percentile: f64,
}

/// Subscribes to Jito's tip_stream WebSocket and republishes each update onto the
/// bus as a TipFloor event. Reconnects with exponential backoff. Until the first
/// message arrives, the system runs on ContentionSnapshot::default_warm().
pub async fn run(bus: broadcast::Sender<NetworkEvent>) {
    let url = std::env::var("TIP_STREAM_URL").unwrap_or_else(|_| DEFAULT_TIP_STREAM_URL.to_string());
    let mut backoff = Duration::from_secs(1);

    loop {
        match stream(&url, &bus).await {
            Ok(()) => {
                warn!("tip_stream closed, reconnecting");
                backoff = Duration::from_secs(1);
            }
            Err(e) => {
                warn!("tip_stream error: {} — reconnecting in {}s", e, backoff.as_secs());
                sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

// Opens one WebSocket connection and forwards tip updates until it closes/errors.
// Uses the unified stream (not split) so tokio-tungstenite keeps auto-responding
// to server pings — a split read half can't send pongs and would get dropped.
async fn stream(url: &str, bus: &broadcast::Sender<NetworkEvent>) -> Result<()> {
    let (mut ws, _resp) = connect_async(url).await?;
    info!("tip_stream connected to {}", url);

    const SOL_TO_LAMPORTS: f64 = 1_000_000_000.0;

    while let Some(msg) = ws.next().await {
        let text = match msg? {
            Message::Text(t) => t.to_string(),
            Message::Binary(b) => String::from_utf8_lossy(b.as_ref()).into_owned(),
            Message::Close(_) => break,
            // Ping/Pong/Frame: tokio-tungstenite handles keepalive for us.
            _ => continue,
        };

        // The stream sends the same shape as the REST tip_floor: an array of
        // entries (newest first) or a single entry. Accept either.
        let entry = match serde_json::from_str::<Vec<TipEntry>>(&text) {
            Ok(v) => v.into_iter().next(),
            Err(_) => serde_json::from_str::<TipEntry>(&text).ok(),
        };

        if let Some(e) = entry {
            // API reports SOL as f64 — convert to lamports.
            let _ = bus.send(NetworkEvent::TipFloor {
                p25: (e.landed_tips_25th_percentile * SOL_TO_LAMPORTS) as u64,
                p50: (e.landed_tips_50th_percentile * SOL_TO_LAMPORTS) as u64,
                p75: (e.landed_tips_75th_percentile * SOL_TO_LAMPORTS) as u64,
                p95: (e.landed_tips_95th_percentile * SOL_TO_LAMPORTS) as u64,
            });
        }
    }

    Ok(())
}
