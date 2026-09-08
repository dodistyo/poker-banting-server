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

/// Resolve the bind port with Cloud Run in mind.
///
/// Cloud Run injects `$PORT` (the port the ingress container must listen on).
/// It does NOT set `SERVER_PORT`. So the precedence is:
///   1. `SERVER_PORT` — explicit override for local dev / tests,
///   2. `PORT`        — Cloud Run's injected port,
///   3. `8080`        — sane default.
///
/// Takes `Option<&str>` for each so the precedence is a pure, testable
/// function (no process-global env mutation in tests).
fn resolve_port(server_port: Option<&str>, port: Option<&str>) -> u16 {
    for v in [server_port, port].into_iter().flatten() {
        if let Ok(p) = v.trim().parse::<u16>() {
            return p;
        }
    }
    8080
}

impl Config {
    pub fn new() -> Self {
        Config {
            host: env::var("SERVER_HOST").unwrap_or_else(|_| "0.0.0.0".to_string()),
            port: resolve_port(
                env::var("SERVER_PORT").ok().as_deref(),
                env::var("PORT").ok().as_deref(),
            ),
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

    // --- resolve_port: Cloud Run injects $PORT (NOT SERVER_PORT). Precedence:
    // SERVER_PORT (explicit dev override) > PORT (Cloud Run) > 8080. ---
    #[test]
    fn test_resolve_port_default_when_no_env() {
        assert_eq!(resolve_port(None, None), 8080);
    }

    #[test]
    fn test_resolve_port_cloud_run_port() {
        assert_eq!(resolve_port(None, Some("8080".into())), 8080);
        assert_eq!(resolve_port(None, Some("12345".into())), 12345);
    }

    #[test]
    fn test_resolve_port_server_port_wins_over_port() {
        assert_eq!(resolve_port(Some("9000".into()), Some("12345".into())), 9000);
    }

    #[test]
    fn test_resolve_port_invalid_values_fall_back() {
        assert_eq!(resolve_port(Some("not-a-number".into()), Some("8080".into())), 8080);
        assert_eq!(resolve_port(Some("not-a-number".into()), None), 8080);
        assert_eq!(resolve_port(None, Some("abc".into())), 8080);
    }

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
