//! Dual-mount routing tests.
//!
//! Two callers hit the backend:
//!  - PRODUCTION: the Firebase-Hosting frontend talks to Cloud Run DIRECTLY,
//!    so it calls `/api/health`, `/api/rooms`, `/api/ws` (prefix kept).
//!  - LOCAL DEV: dev-server.js proxies `/api/*` and STRIPS the prefix before
//!    forwarding, so the bare `/health`, `/rooms`, `/ws` must still work.
//!
//! The router must therefore serve BOTH the `/api/*` paths (production) and
//! the bare paths (dev proxy). Before the fix only the bare paths existed, so
//! the `/api/*` assertions below are RED.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use poker_banting_server::build_app;
use poker_banting_server::rooms::RoomManager;
use std::sync::Arc;
use tower::util::ServiceExt;

fn app() -> axum::Router {
    build_app(Arc::new(RoomManager::new(6, 1000, 0, 10)))
}

async fn get(path: &str) -> axum::response::Response {
    app()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

// --- platform health check ------------------------------------------------
//
// Cloud Run's default health check does GET / and requires a 2xx/3xx before
// it marks an instance ready. Without a `/` route the very first deploy fails
// with "service not ready". `/` serves the same body as `/health`.

#[tokio::test]
async fn root_health_check_is_served() {
    let res = get("/").await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "GET / should be 200 (Cloud Run default health check)"
    );
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("\"ok\""),
        "GET / should return the health JSON, got: {text}"
    );
}

// --- production path: /api/* (direct, prefix kept) -----------------------

#[tokio::test]
async fn prod_api_health_is_served() {
    let res = get("/api/health").await;
    assert_eq!(res.status(), StatusCode::OK, "GET /api/health should be 200");
}

#[tokio::test]
async fn prod_api_rooms_is_served() {
    let res = get("/api/rooms").await;
    assert_eq!(res.status(), StatusCode::OK, "GET /api/rooms should be 200");
    // Empty manager -> empty JSON array.
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert_eq!(text, "[]", "empty room list should be []");
}

// --- dev path: bare (prefix already stripped by dev-server.js) ------------

#[tokio::test]
async fn dev_bare_health_still_served() {
    let res = get("/health").await;
    assert_eq!(res.status(), StatusCode::OK, "GET /health should still be 200");
}

#[tokio::test]
async fn dev_bare_rooms_still_served() {
    let res = get("/rooms").await;
    assert_eq!(res.status(), StatusCode::OK, "GET /rooms should still be 200");
}
