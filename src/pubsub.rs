//! Cross-pod broadcast fan-out (the SUBSCRIBE side of the pub/sub design in
//! `store.rs`).
//!
//! Every mutation is PUBLISHed by its authoring pod to `room:events:{code}`,
//! but a client's WebSocket lives in exactly one pod's memory. A pod therefore
//! must LISTEN on the same pattern and re-deliver to the sessions that happen
//! to sit here. Rooms are created dynamically, so this is one pattern
//! subscription for the whole pod: `PSUBSCRIBE room:events:*`.
//!
//! Privacy is inherited, not re-implemented: the envelope carries the RAW
//! (un-personalised) message, and [`RoomManager::broadcast_local`] re-masks
//! `State` per viewer exactly like the local path does. So cross-pod delivery
//! is card-privacy-safe by construction.

use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Duration;

use crate::protocol::ServerMsg;
use crate::rooms::RoomManager;
use crate::store::PubEnvelope;

/// Channel prefix for per-room event channels (`room:events:{code}`).
const EVENTS_PREFIX: &str = "room:events:";

/// Subscribe to `room:events:*` and forward every cross-pod envelope to this
/// pod's local sessions. Reconnects with capped exponential backoff (a blip in
/// Redis must not permanently kill this pod's fan-out).
///
/// Only meaningful when the store is a `RedisStore` — in memory mode there is
/// nothing to subscribe to (single process, `publish` is a no-op), so the
/// caller only spawns this in Redis mode. The subscriber opens its OWN
/// connection from `redis_url` (a pub/sub connection cannot share the
/// multiplexed command connection).
pub fn run(rooms: Arc<RoomManager>, redis_url: String, pod_id: String) {
    tokio::spawn(async move {
        let mut backoff = Duration::from_millis(500);
        loop {
            match run_once(rooms.clone(), redis_url.clone(), pod_id.clone()).await {
                Ok(()) => {
                    // Connection closed cleanly by Redis (or we were asked to
                    // stop). Reset backoff so a fresh connection isn't penalised.
                    backoff = Duration::from_millis(500);
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(15));
                }
                Err(e) => {
                    eprintln!("[pubsub] subscriber error: {e} — reconnecting in {backoff:?}");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(15));
                }
            }
        }
    });
}

/// One subscriber connection: psubscribe, then forward until the stream ends.
/// Returns `Ok(())` when the stream closed, `Err` on a mid-stream failure.
async fn run_once(
    rooms: Arc<RoomManager>,
    redis_url: String,
    pod_id: String,
) -> Result<(), String> {
    let client = redis::Client::open(redis_url.as_str())
        .map_err(|e| format!("failed to open pubsub client: {e}"))?;
    let mut pubsub = client
        .get_async_pubsub()
        .await
        .map_err(|e| e.to_string())?;
    pubsub
        .psubscribe(format!("{EVENTS_PREFIX}*"))
        .await
        .map_err(|e| e.to_string())?;
    eprintln!("[pubsub] {pod_id} subscribed to {EVENTS_PREFIX}*");

    let mut stream = pubsub.on_message();
    while let Some(msg) = stream.next().await {
        // Drop our OWN echo — this pod already delivered locally in `emit`.
        if let Some(env) = decode(&msg) {
            if !is_own_echo(&env, &pod_id) {
                // Lobby compaction: apply the seat renumbering to THIS pod's
                // session map before fanning out, so each survivor's stored
                // seat follows them into their new id (same as the local path).
                let renumbered = match &env.msg {
                    ServerMsg::SeatChanged { renumbered } => renumbered.clone(),
                    _ => Vec::new(),
                };
                let code = channel_code(&msg).to_string();
                if !renumbered.is_empty() {
                    rooms.apply_renumbering(&code, &renumbered);
                }
                rooms.broadcast_local(&code, &env.msg);
            }
        }
    }
    Ok(())
}

/// Decode a pub/sub `Msg` into a `PubEnvelope`. Subscription-ack messages and
/// undecodable payloads return `None` (logged, not fatal).
fn decode(msg: &redis::Msg) -> Option<PubEnvelope> {
    if !msg.from_pattern() {
        return None; // psubscribe ack, ignore.
    }
    let payload: String = msg.get_payload().ok()?;
    serde_json::from_str(&payload).ok()
}

fn is_own_echo(env: &PubEnvelope, pod_id: &str) -> bool {
    env.pod == pod_id
}

/// The room code is the channel name minus the `room:events:` prefix.
fn channel_code(msg: &redis::Msg) -> &str {
    msg.get_channel_name().strip_prefix(EVENTS_PREFIX).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pod: &str) -> PubEnvelope {
        PubEnvelope {
            pod: pod.to_string(),
            msg: ServerMsg::GameStarted,
        }
    }

    #[test]
    fn own_pod_echo_is_dropped() {
        assert!(is_own_echo(&env("pod-a"), "pod-a"));
        assert!(!is_own_echo(&env("pod-b"), "pod-a"));
    }

    #[test]
    fn code_prefix_is_the_channel_convention() {
        // The whole channel-name contract: `room:events:{code}` -> `{code}`.
        assert_eq!("room:events:AB12CD".strip_prefix(EVENTS_PREFIX), Some("AB12CD"));
        // A channel that does not match the prefix must degrade to empty, not panic.
        assert_eq!("something:else".strip_prefix(EVENTS_PREFIX), None);
    }
}
