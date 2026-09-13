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

    // The tick loop also owns the heartbeat, so keep a clone of the store
    // before RoomManager takes ownership below.
    let store_for_tick = Arc::clone(&store);

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

    // Orphan reaper + seat-liveness reaper + stateless driver, in ONE tick loop:
    //  - every 1s: `heartbeat` (refresh this pod's liveness key) + `drive_room`
    //    each room so deadline-based timing (bot turns, three-discard cascade,
    //    play-limit watchdog) fires. Without this tick the game would only
    //    advance when a HUMAN acted — on a multi-pod server there is no other
    //    clock, so bots would never move.
    //  - every 5s: drop rooms whose last human left (room_orphan_timeout_secs)
    //    AND reclaim seats whose owning pod died hard (kill -9 / OOM) — the
    //    seat-liveness reaper. A hard-killed pod stops heartbeating, its
    //    `pod:alive` key expires within pod_alive_ttl_secs, and these seats are
    //    freed so a reconnecting player can rejoin within ~15s instead of
    //    waiting for the slow orphan reaper.
    // Spawn BEFORE the router below moves `rooms`.
    {
        let tick_rooms = Arc::clone(&rooms);
        let tick_store = store_for_tick;
        let reaper_timeout = config.room_orphan_timeout_secs;
        let is_redis = config.storage.trim().eq_ignore_ascii_case("redis");
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
            let mut ticks = 0u64;
            loop {
                tick.tick().await;
                ticks += 1;
                // Keep our liveness key fresh (no-op in memory mode).
                tick_store.heartbeat().await;
                for code in tick_rooms.room_codes().await {
                    let _ = tick_rooms.drive_room(&code).await;
                }
                if ticks % 5 == 0 {
                    let dropped = tick_rooms.reap_orphaned_rooms().await;
                    if dropped > 0 {
                        println!("[reaper] dropped {} orphaned room(s) (last human gone >{}s)", dropped, reaper_timeout);
                    }
                    // Seat-level liveness: only meaningful in redis mode (a
                    // single in-memory process can't die out from under its
                    // own tick), and cheap enough to run on the 5s cadence.
                    if is_redis {
                        let seats = tick_rooms.reap_zombie_seats().await;
                        if seats > 0 {
                            println!("[reaper] reclaimed {} zombie seat(s) (owning pod stopped heartbeating)", seats);
                        }
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
