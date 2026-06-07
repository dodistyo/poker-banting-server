pub mod config;
pub mod game;
pub mod protocol;
pub mod rooms;
pub mod ws;

use actix_web::{web, App, HttpServer, Responder};
use std::sync::Arc;
use config::Config;
use rooms::RoomManager;

#[derive(serde::Serialize)]
struct HealthResponse {
    status: String,
    rooms: usize,
}

async fn health(rooms: web::Data<Arc<RoomManager>>) -> impl Responder {
    actix_web::HttpResponse::Ok().json(HealthResponse {
        status: "ok".to_string(),
        rooms: rooms.room_count(),
    })
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenv::dotenv().ok();

    let config = Config::new();
    let rooms = Arc::new(RoomManager::new(config.room_code_length));

    println!("Pocer server starting on {}:{}", config.host, config.port);

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(rooms.clone()))
            .route("/health", web::get().to(health))
            .route("/ws", web::get().to(ws::ws_index))
    })
    .bind((config.host, config.port))?
    .run()
    .await
}
