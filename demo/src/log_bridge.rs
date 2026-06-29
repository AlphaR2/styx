// Bridges the `tracing` subscriber to the WebSocket bus.
//
// Any log event that carries a `bundle_id` field is forwarded verbatim to the
// network bus as an `ExecLog` event, so the UI's per-transaction live stream
// mirrors exactly what shows up in the server terminal — message plus every
// other field — with zero per-call wiring. Events without a `bundle_id`
// (tip-floor ticks, Claude errors, socket connects) are global server noise and
// are intentionally left out of the per-transaction view.

use std::fmt::Write as _;

use styx_ingest::bus::NetworkEvent;
use tokio::sync::broadcast::Sender;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

pub struct BusLogLayer {
    tx: Sender<NetworkEvent>,
}

impl BusLogLayer {
    pub fn new(tx: Sender<NetworkEvent>) -> Self {
        Self { tx }
    }
}

// Collects the fields of a single tracing event. `bundle_id` and `message` are
// pulled out specially; everything else is appended as `key=value` so the UI
// line reads just like the terminal (e.g. "jito bundle status status=Invalid").
#[derive(Default)]
struct FieldVisitor {
    bundle_id: Option<String>,
    message: Option<String>,
    extras: String,
}

impl FieldVisitor {
    fn put(&mut self, name: &str, value: String) {
        match name {
            "bundle_id" => self.bundle_id = Some(value),
            // `sig` doubles as the bundle_id on the priority-fee lane; treat it as
            // the key only if no explicit bundle_id was recorded.
            "sig" if self.bundle_id.is_none() => self.bundle_id = Some(value),
            "message" => self.message = Some(value),
            other => {
                if !self.extras.is_empty() {
                    self.extras.push(' ');
                }
                let _ = write!(self.extras, "{}={}", other, value);
            }
        }
    }
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // Strip the surrounding quotes Debug adds to strings so ids stay clean.
        let v = format!("{:?}", value);
        let v = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(&v);
        self.put(field.name(), v.to_string());
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field.name(), value.to_string());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field.name(), value.to_string());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field.name(), value.to_string());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field.name(), value.to_string());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field.name(), value.to_string());
    }
}

impl<S: Subscriber> Layer<S> for BusLogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);

        // Only forward lines scoped to a specific transaction.
        let Some(bundle_id) = visitor.bundle_id.take() else {
            return;
        };

        // Compose "<message> <k=v k=v>" so the UI line matches the terminal.
        let mut message = visitor.message.unwrap_or_default();
        if !visitor.extras.is_empty() {
            if message.is_empty() {
                message = visitor.extras;
            } else {
                let _ = write!(message, " ({})", visitor.extras);
            }
        }

        let meta = event.metadata();
        let ts_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // Non-blocking: a closed/lagging bus just drops the line.
        let _ = self.tx.send(NetworkEvent::ExecLog {
            bundle_id,
            level: meta.level().to_string(),
            target: meta.target().to_string(),
            message,
            ts_ms,
        });
    }
}
