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

    let store = build_store(
        &config.storage,
        config.redis_url.as_deref(),
        pod_id.clone(),
        config.store_lock_ttl_ms,
        (config.room_ttl_seconds * 1000).max(1000),
    )
    .await
    .expect("failed to build room store");

    let rooms = Arc::new(RoomManager::new(
        store,
        pod_id.clone(),
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

    // Orphan reaper + stateless driver, in ONE tick loop:
    //  - every 1s: `drive_room` each room so deadline-based timing (bot turns,
    //    three-discard cascade, play-limit watchdog) fires. Without this tick
    //    the game would only advance when a HUMAN acted — on a multi-pod
    //    server there is no other clock, so bots would never move.
    //  - every 5s: drop rooms whose last human left (room_orphan_timeout_secs).
    // Spawn BEFORE the router below moves `rooms`.
    {
        let tick_rooms = Arc::clone(&rooms);
        let reaper_timeout = config.room_orphan_timeout_secs;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
            let mut ticks = 0u64;
            loop {
                tick.tick().await;
                ticks += 1;
                for code in tick_rooms.room_codes().await {
                    let _ = tick_rooms.drive_room(&code).await;
                }
                if ticks % 5 == 0 {
                    let dropped = tick_rooms.reap_orphaned_rooms().await;
                    if dropped > 0 {
                        println!("[reaper] dropped {} orphaned room(s) (last human gone >{}s)", dropped, reaper_timeout);
                    }
                }
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
