//! The substrate event feed behind `GET /api/events` — a
//! process-wide `tokio::sync::broadcast` of kind-tagged payloads that
//! integrations emit into and SSE connections drain. The shape is
//! deliberately generic (`kind: &'static str` + opaque `serde_json`
//! payload): producers name their own event kinds (`session`,
//! `meta`, `campaign`, …) and consumers branch on the SSE `event:`
//! field. Integrations bridge their domain events in — e.g. peregrin
//! maps session-plane events onto this feed.
//!
//! `broadcast` already implements the wire semantics: a bounded
//! per-subscriber queue (`MAX_QUEUED`), oldest-dropped under lag —
//! a lagged client resyncs through its own channel rather than
//! blocking the feed.

use serde_json::Value;
use tokio::sync::broadcast;

/// Bound per subscriber — a client this far behind is resyncing anyway.
pub const MAX_QUEUED: usize = 500;

/// One feed event: `kind` is the SSE `event:` field (`session`,
/// `meta`, `heartbeat`, or whatever an integration emits); `payload`
/// is the `data:` JSON verbatim.
#[derive(Clone, Debug)]
pub struct FeedEvent {
    pub kind: &'static str,
    pub payload: Value,
}

/// Process-wide emitter the routes write and SSE connections drain.
/// `Clone` shares the channel.
#[derive(Clone)]
pub struct EventFeed {
    tx: broadcast::Sender<FeedEvent>,
}

impl Default for EventFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl EventFeed {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(MAX_QUEUED);
        Self { tx }
    }

    /// Broadcast `kind`/`payload` to every subscriber. No active
    /// receivers is not an error.
    pub fn emit(&self, kind: &'static str, payload: Value) {
        let _ = self.tx.send(FeedEvent { kind, payload });
    }

    pub fn subscribe(&self) -> broadcast::Receiver<FeedEvent> {
        self.tx.subscribe()
    }

    /// Live SSE subscribers — background work gates on someone
    /// listening.
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn emit_reaches_subscribers_in_order() {
        let feed = EventFeed::new();
        let mut rx = feed.subscribe();
        feed.emit("alpha", json!({ "n": 1 }));
        feed.emit("beta", json!({ "n": 2 }));
        let first = rx.recv().await.unwrap();
        assert_eq!(first.kind, "alpha");
        assert_eq!(first.payload, json!({ "n": 1 }));
        let second = rx.recv().await.unwrap();
        assert_eq!(second.kind, "beta");
    }

    #[tokio::test]
    async fn emit_without_subscribers_is_not_an_error() {
        let feed = EventFeed::new();
        feed.emit("lonely", json!(null));
        assert_eq!(feed.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn lagged_subscriber_survives_the_flood() {
        let feed = EventFeed::new();
        let mut rx = feed.subscribe();
        for i in 0..(MAX_QUEUED * 3) {
            feed.emit("flood", json!(i));
        }
        // However far the subscriber lagged, a marker emitted after
        // the flood must still arrive — the stream never dies.
        feed.emit("marker", json!("after"));
        let mut saw_marker = false;
        let mut saw_lagged = false;
        for _ in 0..(MAX_QUEUED * 4) {
            match rx.recv().await {
                Ok(event) if event.kind == "marker" => {
                    saw_marker = true;
                    break;
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => saw_lagged = true,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        assert!(saw_marker, "marker event never arrived");
        assert!(saw_lagged, "flood never produced a lag signal");
    }
}
