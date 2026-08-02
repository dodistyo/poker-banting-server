use serde::{Deserialize, Serialize};
use crate::game::state::GameState;

const fn default_is_public() -> bool { true }

// Client → Server messages

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ClientMsg {
    #[serde(rename = "create")]
    Create {
        name: String,
        #[serde(default = "default_is_public")]
        is_public: bool,
    },
    #[serde(rename = "join")]
    Join {
        code: String,
        name: String,
    },
    #[serde(rename = "rejoin")]
    Rejoin {
        code: String,
        name: String,
        token: String,
    },
    #[serde(rename = "play")]
    Play {
        cards: Vec<String>,
    },
    #[serde(rename = "pass")]
    Pass,
    #[serde(rename = "ping")]
    Ping,
}

// Server → Client messages

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ServerMsg {
    #[serde(rename = "created")]
    Created {
        code: String,
        player_id: usize,
        state: GameState,
        is_public: bool,
        token: String,
    },
    #[serde(rename = "joined")]
    Joined {
        player_id: usize,
        state: GameState,
        code: String,
        token: String,
    },
    #[serde(rename = "rejoined")]
    Rejoined {
        player_id: usize,
        state: GameState,
        code: String,
        token: String,
    },
    #[serde(rename = "state")]
    State {
        state: GameState,
    },
    #[serde(rename = "playerJoined")]
    PlayerJoined {
        player_id: usize,
        name: String,
    },
    #[serde(rename = "playerLeft")]
    PlayerLeft {
        player_id: usize,
        name: String,
    },
    #[serde(rename = "error")]
    Error {
        message: String,
    },
    #[serde(rename = "pong")]
    Pong,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::state::{GamePhase, TrickState};

    #[test]
    fn test_client_msg_create() {
        let json = r#"{"type":"create","name":"Alice"}"#;
        let msg: ClientMsg = serde_json::from_str(json).unwrap();
        match msg {
            ClientMsg::Create { name, is_public } => {
                assert_eq!(name, "Alice");
                assert!(is_public);
            }
            _ => panic!("Expected Create"),
        }
    }

    #[test]
    fn test_client_msg_join() {
        let json = r#"{"type":"join","code":"ABC123","name":"Bob"}"#;
        let msg: ClientMsg = serde_json::from_str(json).unwrap();
        match msg {
            ClientMsg::Join { code, name } => {
                assert_eq!(code, "ABC123");
                assert_eq!(name, "Bob");
            }
            _ => panic!("Expected Join"),
        }
    }

    #[test]
    fn test_client_msg_play() {
        let json = r#"{"type":"play","cards":["2:clubs","2:diamonds"]}"#;
        let msg: ClientMsg = serde_json::from_str(json).unwrap();
        match msg {
            ClientMsg::Play { cards } => assert_eq!(cards, vec!["2:clubs".to_string(), "2:diamonds".to_string()]),
            _ => panic!("Expected Play"),
        }
    }

    #[test]
    fn test_client_msg_pass() {
        let json = r#"{"type":"pass"}"#;
        let msg: ClientMsg = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, ClientMsg::Pass));
    }

    #[test]
    fn test_client_msg_ping() {
        let json = r#"{"type":"ping"}"#;
        let msg: ClientMsg = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, ClientMsg::Ping));
    }

    #[test]
    fn test_server_msg_created() {
        let state = GameState {
            phase: GamePhase::Lobby,
            players: vec![],
            current_player: 0,
            trick: TrickState::new(),
            finished_order: Vec::new(),
            scores: vec![],
            three_discard: None,
            log: Vec::new(),
        };
        let msg = ServerMsg::Created {
            code: "ABC123".to_string(),
            player_id: 0,
            state,
            is_public: true,
            token: "abc123".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"created\""));
        assert!(json.contains("\"code\":\"ABC123\""));
        assert!(json.contains("\"token\":\"abc123\""));
    }

    #[test]
    fn test_server_msg_error() {
        let msg = ServerMsg::Error {
            message: "Room not found".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"error\""));
        assert!(json.contains("Room not found"));
    }

    #[test]
    fn test_server_msg_state() {
        let state = GameState {
            phase: GamePhase::Playing,
            players: vec![],
            current_player: 0,
            trick: TrickState::new(),
            finished_order: Vec::new(),
            scores: vec![],
            three_discard: None,
            log: Vec::new(),
        };
        let msg = ServerMsg::State { state };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"state\""));
    }

    #[test]
    fn test_server_msg_player_joined() {
        let msg = ServerMsg::PlayerJoined {
            player_id: 1,
            name: "Bob".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"playerJoined\""));
        assert!(json.contains("Bob"));
    }

    #[test]
    fn test_server_msg_pong() {
        let msg = ServerMsg::Pong;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"pong\""));
    }

    #[test]
    fn test_client_msg_create_with_is_public() {
        let json = r#"{"type":"create","name":"Alice","isPublic":false}"#;
        let msg: ClientMsg = serde_json::from_str(json).unwrap();
        match msg {
            ClientMsg::Create { name, is_public } => {
                assert_eq!(name, "Alice");
                assert!(!is_public);
            }
            _ => panic!("Expected Create"),
        }
    }

    #[test]
    fn test_client_msg_create_default_is_public() {
        let json = r#"{"type":"create","name":"Alice"}"#;
        let msg: ClientMsg = serde_json::from_str(json).unwrap();
        match msg {
            ClientMsg::Create { name, is_public } => {
                assert_eq!(name, "Alice");
                assert!(is_public);
            }
            _ => panic!("Expected Create"),
        }
    }

    #[test]
    fn test_client_msg_rejoin() {
        let json = r#"{"type":"rejoin","code":"ABC123","name":"Alice","token":"abc123"}"#;
        let msg: ClientMsg = serde_json::from_str(json).unwrap();
        match msg {
            ClientMsg::Rejoin { code, name, token } => {
                assert_eq!(code, "ABC123");
                assert_eq!(name, "Alice");
                assert_eq!(token, "abc123");
            }
            _ => panic!("Expected Rejoin"),
        }
    }

    #[test]
    fn test_server_msg_rejoined() {
        let state = GameState {
            phase: GamePhase::Lobby,
            players: vec![],
            current_player: 0,
            trick: TrickState::new(),
            finished_order: Vec::new(),
            scores: vec![],
            three_discard: None,
            log: Vec::new(),
        };
        let msg = ServerMsg::Rejoined { player_id: 0, state, code: "ABC123".to_string(), token: "abc123".to_string() };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"rejoined\""));
        assert!(json.contains("\"code\":\"ABC123\""));
        assert!(json.contains("\"token\":\"abc123\""));
    }
}
