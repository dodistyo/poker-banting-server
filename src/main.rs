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
    let rooms = Arc::new(RoomManager::new(config.room_code_length, config.bot_turn_delay_ms));

    let app = Router::new()
        .route("/health", get(health))
        .route("/ws", get(ws::ws_index))
        .route("/api/rooms", get(list_rooms))
        .with_state(rooms);

    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .expect("Invalid address");

    println!("Pocer server starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await
}
