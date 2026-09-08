use std::net::SocketAddr;
use std::sync::Arc;

use poker_banting_server::config::Config;
use poker_banting_server::rooms::RoomManager;
use poker_banting_server::build_app;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    dotenv::dotenv().ok();

    let config = Config::new();
    let rooms = Arc::new(RoomManager::new(
        config.room_code_length,
        config.bot_turn_delay_ms,
        config.room_orphan_timeout_secs,
        config.disconnect_timeout_sec,
    ));

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

    let app = build_app(rooms);

    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .expect("Invalid address");

    println!("Pocer server starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await
}
