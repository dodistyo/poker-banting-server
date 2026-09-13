//! CORS tests for the production deployment: the frontend (Firebase Hosting
//! on poker-banting.dodistyo.com) talks to the API cross-origin, so the
//! server must answer CORS preflights and echo allowed origins.
//!
//! Local dev does NOT need CORS — dev-server.js proxies /api same-origin —
//! but we still allow localhost:3000 for direct testing against the API.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use poker_banting_server::build_app;
use poker_banting_server::rooms::RoomManager;
use poker_banting_server::store::InMemoryStore;
use std::sync::Arc;
use tower::util::ServiceExt;

fn app() -> axum::Router {
    build_app(Arc::new(RoomManager::new(
        Arc::new(InMemoryStore::new()),
        "test".to_string(),
        6,
        1000,
        0,
        10,
    )))
}

fn get_with_origin(origin: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri("/health")
        .header("origin", origin)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn cors_allows_the_firebase_hosting_origin() {
    let res = app().oneshot(get_with_origin("https://poker-banting.dodistyo.com")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let acao = res.headers().get("access-control-allow-origin").unwrap();
    assert_eq!(acao.to_str().unwrap(), "https://poker-banting.dodistyo.com");
}

#[tokio::test]
async fn cors_allows_local_dev_origin() {
    let res = app().oneshot(get_with_origin("http://localhost:3000")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let acao = res.headers().get("access-control-allow-origin").unwrap();
    assert_eq!(acao.to_str().unwrap(), "http://localhost:3000");
}

#[tokio::test]
async fn cors_preflight_returns_allow_methods_and_headers() {
    let req = Request::builder()
        .method("OPTIONS")
        .uri("/health")
        .header("origin", "https://poker-banting.dodistyo.com")
        .header("access-control-request-method", "GET")
        .header("access-control-request-headers", "content-type")
        .body(Body::empty())
        .unwrap();
    let res = app().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().get("access-control-allow-methods").is_some());
    assert!(res.headers().get("access-control-allow-headers").is_some());
}
