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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomPlayer {
    pub id: usize,
    pub name: String,
    pub is_bot: bool,
    pub connected: bool,
    pub disconnect_time: Option<u64>,
}

impl Room {
    pub fn new(code: String, host_name: String) -> Self {
        let players = vec![RoomPlayer {
            id: 0,
            name: host_name.clone(),
            is_bot: false,
            connected: true,
            disconnect_time: None,
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
        }
    }

    pub fn add_player(&mut self, name: String) -> Result<usize, String> {
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
        let room = Room::new("ABC123".to_string(), "Host".to_string());
        assert_eq!(room.code, "ABC123");
        assert_eq!(room.players.len(), 1);
        assert_eq!(room.players[0].name, "Host");
        assert_eq!(room.state.phase, GamePhase::Lobby);
        assert!(!room.started);
    }

    #[test]
    fn test_room_add_player() {
        let mut room = Room::new("ABC123".to_string(), "Host".to_string());
        let id = room.add_player("Player1".to_string()).unwrap();
        assert_eq!(id, 1);
        assert_eq!(room.players.len(), 2);
        assert_eq!(room.state.players.len(), 2);
    }

    #[test]
    fn test_room_add_player_full() {
        let mut room = Room::new("ABC123".to_string(), "Host".to_string());
        room.add_player("P1".to_string()).unwrap();
        room.add_player("P2".to_string()).unwrap();
        room.add_player("P3".to_string()).unwrap();
        assert!(room.add_player("P4".to_string()).is_err());
    }

    #[test]
    fn test_room_add_bot() {
        let mut room = Room::new("ABC123".to_string(), "Host".to_string());
        let id = room.add_bot("Bot1".to_string());
        assert_eq!(id, 1);
        assert!(room.players[1].is_bot);
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
        let mut room = Room::new("ABC123".to_string(), "Host".to_string());
        room.started = true;
        assert!(room.add_player("Late".to_string()).is_err());
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
        let mut room = Room::new("ABC123".to_string(), "Host".to_string());
        assert_eq!(room.state.scores, vec![0]);
        room.add_player("P1".to_string()).unwrap();
        assert_eq!(room.state.scores, vec![0, 0]);
    }
}
