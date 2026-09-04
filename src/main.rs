pub mod config;
pub mod game;
pub mod protocol;
pub mod rooms;
pub mod ws;

use axum::{routing::get, Router, response::Json, extract::State};
use std::net::SocketAddr;
use std::sync::Arc;
use config::Config;
use rooms::RoomManager;

#[derive(serde::Serialize)]
struct HealthResponse {
    status: String,
    rooms: usize,
}

async fn health(State(rooms): State<Arc<RoomManager>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        rooms: rooms.room_count(),
    })
}

async fn list_rooms(
    State(rooms): State<Arc<RoomManager>>,
) -> Json<Vec<rooms::PublicRoomSummary>> {
    Json(rooms.list_public_rooms())
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    dotenv::dotenv().ok();

    let config = Config::new();
    let rooms = Arc::new(RoomManager::new(config.room_code_length, config.bot_turn_delay_ms, config.room_orphan_timeout_secs, config.disconnect_timeout_sec));

    // Orphan reaper: rooms whose last human left get dropped after
    // room_orphan_timeout_secs. Without this tick the orphan check only ever
    // ran at disconnect time (timer = 0s) and dead rooms accumulated forever.
    // Spawn BEFORE the router below moves `rooms`.
    {
        let reaper_rooms = Arc::clone(&rooms);
        let reaper_timeout = config.room_orphan_timeout_secs;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tick.tick().await;
                let dropped = reaper_rooms.reap_orphaned_rooms();
                if dropped > 0 {
                    println!("[reaper] dropped {} orphaned room(s) (last human gone >{}s)", dropped, reaper_timeout);
                }
            }
        });
    }

    let app = Router::new()
        .route("/health", get(health))
        .route("/ws", get(ws::ws_index))
        // NOTE: no /api prefix here — the client's dev proxy (dev-server.js)
        // strips the /api prefix before forwarding, so routes must be
        // prefix-free like /ws and /health. (This was why "Browse Public
        // Rooms" always came back empty: /api/rooms -> /rooms -> 404.)
        .route("/rooms", get(list_rooms))
        .with_state(rooms);

    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .expect("Invalid address");

    println!("Pocer server starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await
}
