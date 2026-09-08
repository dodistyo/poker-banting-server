use std::sync::Arc;

use axum::{
    extract::State,
    response::Json,
    routing::get,
    Router,
};
use tower_http::cors::{Any, CorsLayer};

use crate::rooms::RoomManager;

pub mod config;
pub mod game;
pub mod protocol;
pub mod rooms;
pub mod ws;

#[derive(serde::Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub rooms: usize,
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

/// Build the axum router with CORS. Lives in the lib (not the binary) so
/// integration tests can exercise it without binding a port.
///
/// CORS: the production frontend lives on Firebase Hosting
/// (https://poker-banting.dodistyo.com) and talks cross-origin; localhost
/// origins are allowed for direct dev testing. Local dev via dev-server.js
/// proxies /api same-origin, so it works with or without these headers.
/// `allowed_origins` is the production whitelist; pass the default in main.
pub fn build_app(rooms: Arc<RoomManager>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin([
            "https://poker-banting.dodistyo.com".parse().unwrap(),
            "http://localhost:3000".parse().unwrap(),
            "http://127.0.0.1:3000".parse().unwrap(),
        ])
        .allow_methods(Any)
        .allow_headers(Any);

    // Dual-mount: serve the API under BOTH `/api/*` (production — the
    // Firebase-Hosting frontend calls Cloud Run directly, prefix kept) and
    // the bare paths (local dev — dev-server.js proxies /api/* and STRIPS the
    // prefix before forwarding). Mounting the same handlers twice keeps every
    // existing caller working: prod hits /api/health, dev's proxy hits /health.
    let api = Router::new()
        .route("/health", get(health))
        .route("/ws", get(ws::ws_index))
        .route("/rooms", get(list_rooms));

    Router::new()
        .route("/", get(health))
        .nest("/api", api.clone())
        .merge(api)
        .layer(cors)
        .with_state(rooms)
}
