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
    #[serde(rename = "ready")]
    Ready {
        ready: bool,
    },
    #[serde(rename = "startGame")]
    StartGame,
    #[serde(rename = "leaveRoom")]
    LeaveRoom,
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
    #[serde(rename = "playerReady")]
    PlayerReady {
        player_id: usize,
        name: String,
        ready: bool,
    },
    #[serde(rename = "gameStarted")]
    GameStarted,
    #[serde(rename = "error")]
    Error {
        message: String,
    },
    #[serde(rename = "pong")]
    Pong,
}

/// Serialize a server message for one specific viewer (card privacy).
///
/// State-carrying variants (created/joined/rejoined/state) get personalized:
/// the viewer keeps their own hand and 3s, while every other player's hand
/// is replaced with an empty array plus a public `handCount`, and
/// `threeDiscard.playerCards` is emptied for everyone except the viewer
/// with a public `playerCounts` (counts stay visible so the client can
/// render "N 3s" and the discard order). Non-state variants serialize as-is.
pub fn personalise_for_viewer(msg: &ServerMsg, viewer: usize) -> String {
    let mut v = match serde_json::to_value(msg) {
        Ok(v) => v,
        Err(e) => panic!("Failed to serialize message: {}", e),
    };
    if let Some(state_v) = v.get_mut("state") {
        personalise_state_value(state_v, viewer);
    }
    v.to_string()
}

fn personalise_state_value(v: &mut serde_json::Value, viewer: usize) {
    if let Some(players) = v.get_mut("players").and_then(|p| p.as_array_mut()) {
        for p in players {
            let id = p
                .get("id")
                .and_then(|x| x.as_u64())
                .map(|x| x as usize)
                .unwrap_or(usize::MAX);
            let n = p
                .get("hand")
                .and_then(|h| h.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            p["handCount"] = serde_json::Value::from(n);
            if id != viewer {
                p["hand"] = serde_json::Value::Array(Vec::new());
            }
        }
    }
    if let Some(td) = v.get_mut("threeDiscard") {
        let counts: Vec<usize> = td
            .get("playerCards")
            .and_then(|x| x.as_array())
            .map(|pc| {
                pc.iter()
                    .map(|c| c.as_array().map(|a| a.len()).unwrap_or(0))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(pc) = td.get_mut("playerCards").and_then(|x| x.as_array_mut()) {
            for (i, c) in pc.iter_mut().enumerate() {
                if i != viewer {
                    *c = serde_json::Value::Array(Vec::new());
                }
            }
        }
        td["playerCounts"] = serde_json::Value::from(counts);
    }
}

/// Return a copy of `state` as seen by `viewer` (card privacy):
/// other players' hands are emptied (public `handCount` is added) and
/// other players' 3s are emptied (public `threeDiscard.playerCounts` is
/// added). Used for direct sends (created/joined/rejoined/start_game)
/// where the message goes to a single client, not through broadcast.
pub fn personalise_state(state: &GameState, viewer: usize) -> GameState {
    let mut v = serde_json::to_value(state)
        .unwrap_or_else(|e| panic!("Failed to serialize state: {}", e));
    personalise_state_value(&mut v, viewer);
    serde_json::from_value(v)
        .unwrap_or_else(|e| panic!("Failed to deserialize personalized state: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::state::{GamePhase, Player, TrickState};

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
            ready: vec![],
            current_player: 0,
            trick: TrickState::new(),
            finished_order: Vec::new(),
            scores: vec![],
            round: 1,
            total_scores: vec![0],
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
            ready: vec![],
            current_player: 0,
            trick: TrickState::new(),
            finished_order: Vec::new(),
            scores: vec![],
            round: 1,
            total_scores: vec![0],
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
            ready: vec![],
            current_player: 0,
            trick: TrickState::new(),
            finished_order: Vec::new(),
            scores: vec![],
            round: 1,
            total_scores: vec![0],
            three_discard: None,
            log: Vec::new(),
        };
        let msg = ServerMsg::Rejoined { player_id: 0, state, code: "ABC123".to_string(), token: "abc123".to_string() };
        let json = personalise_for_viewer(&msg, 0);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["code"].as_str(), Some("ABC123"));
        assert_eq!(v["token"].as_str(), Some("abc123"));
        assert_eq!(v["state"]["phase"].as_str(), Some("lobby"));
    }

    #[test]
    fn test_personalise_state_hides_other_hands() {
        use crate::game::card::{Card, Rank, Suit};
        let mut state = GameState {
            phase: GamePhase::Playing,
            players: vec![],
            ready: vec![],
            current_player: 0,
            trick: TrickState::new(),
            finished_order: Vec::new(),
            scores: Vec::new(),
            round: 1,
            total_scores: vec![0],
            three_discard: None,
            log: Vec::new(),
        };
        for i in 0..4 {
            let mut p = Player {
                id: i,
                name: format!("P{}", i),
                hand: vec![
                    Card::new(Rank::King, Suit::Hearts),
                    Card::new(Rank::Three, Suit::Spades),
                ],
                finished: false,
                is_bot: i != 0,
                connected: true,
                is_creator: i == 0,
            };
            state.players.push(p);
        }
        let view = personalise_state(&state, 1);
        // Own hand survives, count is public
        assert_eq!(view.players[1].hand.len(), 2);
        // Other hands are empty
        assert!(view.players[0].hand.is_empty());
        assert!(view.players[2].hand.is_empty());
        assert!(view.players[3].hand.is_empty());
    }

    #[test]
    fn test_personalise_msg_hides_other_threes() {
        use crate::game::card::{Card, Rank, Suit};
        let state = GameState {
            phase: GamePhase::ThreeDiscard,
            players: (0..4)
                .map(|i| Player {
                    id: i,
                    name: format!("P{}", i),
                    hand: Vec::new(),
                    finished: false,
                    is_bot: i != 0,
                    connected: true,
                    is_creator: i == 0,
                })
                .collect(),
            ready: vec![true; 4],
            current_player: 0,
            trick: TrickState::new(),
            finished_order: Vec::new(),
            scores: vec![0; 4],
            round: 1,
            total_scores: vec![0; 4],
            three_discard: Some(crate::game::state::ThreeDiscardState {
                order: vec![0, 1, 2, 3],
                index: 0,
                player_cards: vec![
                    vec![Card::new(Rank::Three, Suit::Hearts), Card::new(Rank::Three, Suit::Spades)],
                    vec![Card::new(Rank::Three, Suit::Diamonds)],
                    Vec::new(),
                    vec![Card::new(Rank::Three, Suit::Clubs)],
                ],
                discarded: vec![false; 4],
            }),
            log: Vec::new(),
        };
        let json = personalise_for_viewer(&ServerMsg::State { state }, 0);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let td = &v["state"]["threeDiscard"];
        // Viewer keeps their own 3s
        assert_eq!(td["playerCards"][0].as_array().unwrap().len(), 2);
        // Others are masked
        assert!(td["playerCards"][1].as_array().unwrap().is_empty());
        assert!(td["playerCards"][2].as_array().unwrap().is_empty());
        assert!(td["playerCards"][3].as_array().unwrap().is_empty());
        // Counts stay public
        assert_eq!(td["playerCounts"][0].as_u64(), Some(2));
        assert_eq!(td["playerCounts"][1].as_u64(), Some(1));
        assert_eq!(td["playerCounts"][2].as_u64(), Some(0));
        assert_eq!(td["playerCounts"][3].as_u64(), Some(1));
    }

    #[test]
    fn test_personalise_non_state_msg_untouched() {
        let msg = ServerMsg::PlayerJoined {
            player_id: 1,
            name: "Bob".to_string(),
        };
        let json = personalise_for_viewer(&msg, 0);
        assert!(json.contains("Bob"));
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.get("state").is_none());
    }
}
