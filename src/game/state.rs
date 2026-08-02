use std::time::Instant;
use serde::{Deserialize, Serialize};
use super::card::Card;
use super::combo::ComboType;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Player {
    pub id: usize,
    pub name: String,
    pub hand: Vec<Card>,
    pub finished: bool,
    pub is_bot: bool,
    pub connected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrickState {
    pub cards: Vec<Card>,
    pub combo_type: Option<ComboType>,
    pub combo_player: Option<usize>,
    pub passed: Vec<usize>,
    pub played: Vec<usize>,
}

impl TrickState {
    pub fn new() -> Self {
        TrickState {
            cards: Vec::new(),
            combo_type: None,
            combo_player: None,
            passed: Vec::new(),
            played: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThreeDiscardState {
    pub order: Vec<usize>,
    pub index: usize,
    pub player_cards: Vec<Vec<Card>>,
    pub discarded: Vec<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GamePhase {
    Lobby,
    ThreeDiscard,
    Playing,
    GameOver,
}

impl std::fmt::Display for GamePhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GamePhase::Lobby => write!(f, "lobby"),
            GamePhase::ThreeDiscard => write!(f, "three_discard"),
            GamePhase::Playing => write!(f, "playing"),
            GamePhase::GameOver => write!(f, "game_over"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GameState {
    pub phase: GamePhase,
    pub players: Vec<Player>,
    pub current_player: usize,
    pub trick: TrickState,
    pub finished_order: Vec<usize>,
    pub scores: Vec<i32>,
    pub three_discard: Option<ThreeDiscardState>,
    pub log: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    pub code: String,
    pub state: GameState,
    pub players: Vec<RoomPlayer>,
    pub started: bool,
    pub delay_task_spawned: bool,
    pub is_public: bool,
    #[serde(skip, default)]
    pub disconnected_players: Vec<(usize, String, Instant)>, // (seat_id, token, disconnect_time)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomPlayer {
    pub id: usize,
    pub name: String,
    pub is_bot: bool,
    pub connected: bool,
    pub disconnect_time: Option<u64>,
    #[serde(skip, default)]
    pub token: Option<String>,
}

impl Room {
    pub fn new(code: String, host_name: String, host_token: String) -> Self {
        let players = vec![RoomPlayer {
            id: 0,
            name: host_name.clone(),
            is_bot: false,
            connected: true,
            disconnect_time: None,
            token: Some(host_token),
        }];

        let state = GameState {
            phase: GamePhase::Lobby,
            players: vec![Player {
                id: 0,
                name: host_name.clone(),
                hand: Vec::new(),
                finished: false,
                is_bot: false,
                connected: true,
            }],
            current_player: 0,
            trick: TrickState::new(),
            finished_order: Vec::new(),
            scores: vec![0],
            three_discard: None,
            log: Vec::new(),
        };

        Room {
            code,
            state,
            players,
            started: false,
            delay_task_spawned: false,
            is_public: true,
            disconnected_players: Vec::new(),
        }
    }

    pub fn add_player(&mut self, name: String, token: String) -> Result<usize, String> {
        if self.players.len() >= 4 {
            return Err("Room is full".to_string());
        }
        if self.started {
            return Err("Game has already started".to_string());
        }

        let id = self.players.len();
        self.players.push(RoomPlayer {
            id,
            name: name.clone(),
            is_bot: false,
            connected: true,
            disconnect_time: None,
            token: Some(token),
        });

        self.state.players.push(Player {
            id,
            name,
            hand: Vec::new(),
            finished: false,
            is_bot: false,
            connected: true,
        });
        self.state.scores.push(0);

        Ok(id)
    }

    pub fn add_bot(&mut self, name: String) -> usize {
        let id = self.players.len();
        self.players.push(RoomPlayer {
            id,
            name: name.clone(),
            is_bot: true,
            connected: true,
            disconnect_time: None,
            token: None,
        });

        self.state.players.push(Player {
            id,
            name,
            hand: Vec::new(),
            finished: false,
            is_bot: true,
            connected: true,
        });
        self.state.scores.push(0);

        id
    }

    pub fn add_disconnected_player(&mut self, seat_id: usize, token: String) {
        self.disconnected_players.push((seat_id, token, Instant::now()));
    }

    pub fn deal_and_start_discard(&mut self) {
        let mut engine = crate::game::engine::GameEngine::new(self.state.clone());
        engine.deal_cards();
        engine.start_three_discard();
        self.state = engine.state().clone();
    }

    pub fn restore_seat(&mut self, seat_id: usize, name: &str, token: &str) {
        if seat_id < self.players.len() {
            self.players[seat_id].name = name.to_string();
            self.players[seat_id].is_bot = false;
            self.players[seat_id].connected = true;
            self.players[seat_id].disconnect_time = None;
            self.players[seat_id].token = Some(token.to_string());
        }
        if seat_id < self.state.players.len() {
            self.state.players[seat_id].name = name.to_string();
            self.state.players[seat_id].is_bot = false;
            self.state.players[seat_id].connected = true;
        }
    }

    pub fn cleanup_expired_disconnected(&mut self, timeout_secs: u64) {
        let cutoff = Instant::now() - std::time::Duration::from_secs(timeout_secs);
        self.disconnected_players.retain(|(_, _, t)| *t > cutoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trick_state_new() {
        let trick = TrickState::new();
        assert!(trick.cards.is_empty());
        assert_eq!(trick.combo_type, None);
        assert_eq!(trick.combo_player, None);
        assert!(trick.passed.is_empty());
    }

    #[test]
    fn test_room_new() {
        let room = Room::new("ABC123".to_string(), "Host".to_string(), "host-token".to_string());
        assert_eq!(room.code, "ABC123");
        assert_eq!(room.players.len(), 1);
        assert_eq!(room.players[0].name, "Host");
        assert_eq!(room.players[0].token, Some("host-token".to_string()));
        assert_eq!(room.state.phase, GamePhase::Lobby);
        assert!(!room.started);
    }

    #[test]
    fn test_room_add_player() {
        let mut room = Room::new("ABC123".to_string(), "Host".to_string(), "host-token".to_string());
        let id = room.add_player("Player1".to_string(), "p1-token".to_string()).unwrap();
        assert_eq!(id, 1);
        assert_eq!(room.players.len(), 2);
        assert_eq!(room.state.players.len(), 2);
        assert_eq!(room.players[1].token, Some("p1-token".to_string()));
    }

    #[test]
    fn test_room_add_player_full() {
        let mut room = Room::new("ABC123".to_string(), "Host".to_string(), "host-token".to_string());
        room.add_player("P1".to_string(), "t1".to_string()).unwrap();
        room.add_player("P2".to_string(), "t2".to_string()).unwrap();
        room.add_player("P3".to_string(), "t3".to_string()).unwrap();
        assert!(room.add_player("P4".to_string(), "t4".to_string()).is_err());
    }

    #[test]
    fn test_room_add_bot() {
        let mut room = Room::new("ABC123".to_string(), "Host".to_string(), "host-token".to_string());
        let id = room.add_bot("Bot1".to_string());
        assert_eq!(id, 1);
        assert!(room.players[1].is_bot);
        assert!(room.players[1].token.is_none());
    }

    #[test]
    fn test_game_state_serialization() {
        let state = GameState {
            phase: GamePhase::Playing,
            players: vec![Player {
                id: 0,
                name: "Test".to_string(),
                hand: Vec::new(),
                finished: false,
                is_bot: false,
                connected: true,
            }],
            current_player: 0,
            trick: TrickState::new(),
            finished_order: Vec::new(),
            scores: vec![0],
            three_discard: None,
            log: Vec::new(),
        };
        let json = serde_json::to_string(&state).unwrap();
        let deserialized: GameState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.phase, GamePhase::Playing);
        assert_eq!(deserialized.players[0].name, "Test");
    }

    #[test]
    fn test_trick_state_serialization() {
        let trick = TrickState {
            cards: Vec::new(),
            combo_type: Some(ComboType::Single),
            combo_player: Some(0),
            passed: vec![1, 2],
            played: Vec::new(),
        };
        let json = serde_json::to_string(&trick).unwrap();
        let deserialized: TrickState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.combo_type, Some(ComboType::Single));
        assert_eq!(deserialized.combo_player, Some(0));
        assert_eq!(deserialized.passed, vec![1, 2]);
    }

    #[test]
    fn test_room_started_no_join() {
        let mut room = Room::new("ABC123".to_string(), "Host".to_string(), "host-token".to_string());
        room.started = true;
        assert!(room.add_player("Late".to_string(), "late-token".to_string()).is_err());
    }

    #[test]
    fn test_game_phase_serialization() {
        assert_eq!(
            serde_json::to_string(&GamePhase::Lobby).unwrap(),
            "\"lobby\""
        );
        assert_eq!(
            serde_json::to_string(&GamePhase::ThreeDiscard).unwrap(),
            "\"threeDiscard\""
        );
        assert_eq!(
            serde_json::to_string(&GamePhase::Playing).unwrap(),
            "\"playing\""
        );
        assert_eq!(
            serde_json::to_string(&GamePhase::GameOver).unwrap(),
            "\"gameOver\""
        );
    }

    #[test]
    fn test_room_scores_initialized() {
        let mut room = Room::new("ABC123".to_string(), "Host".to_string(), "host-token".to_string());
        assert_eq!(room.state.scores, vec![0]);
        room.add_player("P1".to_string(), "p1-token".to_string()).unwrap();
        assert_eq!(room.state.scores, vec![0, 0]);
    }

    #[test]
    fn test_room_is_public_default() {
        let room = Room::new("ABC123".to_string(), "Host".to_string(), "host-token".to_string());
        assert!(room.is_public);
    }

    #[test]
    fn test_room_disconnected_player() {
        let mut room = Room::new("ABC123".to_string(), "Host".to_string(), "host-token".to_string());
        room.add_disconnected_player(0, "alice-token".to_string());
        assert_eq!(room.disconnected_players.len(), 1);
        assert_eq!(room.disconnected_players[0].0, 0);
        assert_eq!(room.disconnected_players[0].1, "alice-token");
    }

    #[test]
    fn test_room_cleanup_expired_disconnected() {
        let mut room = Room::new("ABC123".to_string(), "Host".to_string(), "host-token".to_string());
        room.add_disconnected_player(0, "alice-token".to_string());
        room.cleanup_expired_disconnected(0);
        assert!(room.disconnected_players.is_empty());
    }
}
