use std::net::SocketAddr;
use std::sync::Arc;

use poker_banting_server::config::Config;
use poker_banting_server::pubsub;
use poker_banting_server::rooms::RoomManager;
use poker_banting_server::store::build_store;
use poker_banting_server::build_app;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    dotenv::dotenv().ok();

    let config = Config::new();

    // Pod identity: Cloud Run injects POD_NAME when set; fall back to a
    // hostname. Stamped into cross-pod lock/pub envelopes so a pod can drop
    // its own echoes and locks auto-attribute to the owner.
    let pod_id = std::env::var("POD_NAME")
        .ok()
        .or_else(|| hostname())
        .unwrap_or_else(|| "pod".to_string());

    // LIVENESS identity: unique per PROCESS boot, not per pod name. A
    // `kill -9`'d container respawned under the SAME POD_NAME must not inherit
    // the dead boot's liveness key — its old seats are only reclaimable once
    // the OLD boot's key expires. (Advisory locks still use `pod_id`.)
    use rand::Rng;
    let boot_id = format!(
        "{}-{}",
        pod_id,
        rand::thread_rng().gen_range(0u64..u64::MAX)
    );

    let store = build_store(
        &config.storage,
        config.redis_url.as_deref(),
        pod_id.clone(),
        boot_id.clone(),
        config.store_lock_ttl_ms,
        (config.room_ttl_seconds * 1000).max(1000),
        (config.pod_alive_ttl_secs * 1000).max(1000),
    )
    .await
    .expect("failed to build room store");

    let rooms = Arc::new(RoomManager::new(
        store,
        pod_id.clone(),
        boot_id.clone(),
        config.room_code_length,
        config.bot_turn_delay_ms,
        config.room_orphan_timeout_secs,
        config.disconnect_timeout_sec,
    ));

    // Cross-pod fan-out: subscribe to `room:events:*` so mutations run by
    // another pod reach the clients sitting on THIS pod. No-op by design in
    // memory mode (nothing publishes), so gate on the actual store kind.
    if config.storage.trim().eq_ignore_ascii_case("redis") {
        if let Some(url) = config.redis_url.clone() {
            let subscriber_rooms = Arc::clone(&rooms);
            pubsub::run(subscriber_rooms, url, pod_id.clone());
        }
    }

    // Stateless driver + orphan reaper + seat-liveness reaper, in ONE tick loop
    // — but ALL of it now lives inside `RoomManager::tick_once()` and is GATED
    // on this pod having at least one local WebSocket session. An idle pod
    // (no clients) does ZERO Redis work: no heartbeat SET, no `room:codes`
    // scan, no locks — idle costs 0 Upstash commands, which is the whole point
    // of this refactor (a permanently-warm pod used to burn ~2.8 cmd/s).
    // The ~5s reaper cadence and the bot-driver tick ride on the same 1s loop,
    // so without clients nothing runs at all.
    // Spawn BEFORE the router below moves `rooms`.
    {
        let tick_rooms = Arc::clone(&rooms);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                tick.tick().await;
                let _ = tick_rooms.tick_once().await;
            }
        });
    }

    let app = build_app(rooms);

    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .expect("Invalid address");

    println!("Pocer server starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await
}

fn hostname() -> Option<String> {
    std::fs::read_to_string("/etc/hostname").ok().map(|h| h.trim().to_string())
}
