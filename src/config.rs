use std::env;

pub struct Config {
    pub host: String,
    pub port: u16,
    pub room_code_length: usize,
    pub room_ttl_seconds: u64,
    pub max_players: usize,
    pub disconnect_timeout_sec: u64,
    pub bot_turn_delay_ms: u64,
    pub room_orphan_timeout_secs: u64,
  }

impl Config {
    pub fn new() -> Self {
        Config {
            host: env::var("SERVER_HOST").unwrap_or_else(|_| "0.0.0.0".to_string()),
            port: env::var("SERVER_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8080),
            room_code_length: env::var("ROOM_CODE_LENGTH")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(6),
            room_ttl_seconds: env::var("ROOM_TTL_SECONDS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1800),
            max_players: env::var("MAX_PLAYERS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(4),
            disconnect_timeout_sec: env::var("DISCONNECT_TIMEOUT_SEC")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(15),
            bot_turn_delay_ms: env::var("BOT_TURN_DELAY_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2500),
            room_orphan_timeout_secs: env::var("ROOM_ORPHAN_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = Config::new();
        assert_eq!(config.host, "0.0.0.0");
        assert_eq!(config.port, 8080);
        assert_eq!(config.room_code_length, 6);
        assert_eq!(config.room_ttl_seconds, 1800);
        assert_eq!(config.max_players, 4);
        assert_eq!(config.disconnect_timeout_sec, 15);
        assert_eq!(config.bot_turn_delay_ms, 2500);
        assert_eq!(config.room_orphan_timeout_secs, 30);
    }
}
