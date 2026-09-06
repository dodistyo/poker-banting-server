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
    pub is_creator: bool,
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
    pub ready: Vec<bool>,
    pub current_player: usize,
    pub trick: TrickState,
    pub finished_order: Vec<usize>,
    pub scores: Vec<i32>,
    #[serde(default = "default_round")]
    pub round: usize,
    #[serde(default)]
    pub total_scores: Vec<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub three_discard: Option<ThreeDiscardState>,
    pub log: Vec<String>,
}

fn default_round() -> usize {
    1
}

impl GameState {
    /// Grow `total_scores` to cover every seat. Per-round `scores` and the
    /// cumulative `total_scores` are separate: `scores` is this round's
    /// points (10/5/0/-15), `total_scores` accumulates across rounds so the
    /// session scoreboard "berlanjut" from one game to the next.
    pub fn ensure_total_scores(&mut self) {
        while self.total_scores.len() < self.players.len() {
            self.total_scores.push(0);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Room {
    pub code: String,
    pub state: GameState,
    pub players: Vec<RoomPlayer>,
    pub started: bool,
    pub delay_task_spawned: bool,
    pub is_public: bool,
    pub ready: Vec<bool>,
    #[serde(skip, default)]
    pub disconnected_players: Vec<(usize, String, Instant)>, // (seat_id, token, disconnect_time)
    #[serde(skip, default)]
    pub last_human_disconnect_at: Option<Instant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomPlayer {
    pub id: usize,
    pub name: String,
    pub is_bot: bool,
    pub connected: bool,
    pub disconnect_time: Option<u64>,
    pub is_creator: bool,
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
            is_creator: true,
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
                is_creator: true,
            }],
            ready: vec![true],
            current_player: 0,
            trick: TrickState::new(),
            finished_order: Vec::new(),
            scores: vec![0],
            round: 1,
            total_scores: vec![0],
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
            ready: vec![true],
            disconnected_players: Vec::new(),
            last_human_disconnect_at: None,
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
            is_creator: false,
            token: Some(token),
        });
        self.ready.push(false);

        self.state.players.push(Player {
            id,
            name,
            hand: Vec::new(),
            finished: false,
            is_bot: false,
            connected: true,
            is_creator: false,
        });
        self.state.ready.push(false);
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
            is_creator: false,
            token: None,
        });
        self.ready.push(true);

        self.state.players.push(Player {
            id,
            name,
            hand: Vec::new(),
            finished: false,
            is_bot: true,
            connected: true,
            is_creator: false,
        });
        self.state.ready.push(true);
        self.state.scores.push(0);

        id
    }

    pub fn set_ready(&mut self, seat_id: usize, ready: bool) -> Result<(), String> {
        if seat_id >= self.ready.len() {
            return Err("Seat not found".to_string());
        }
        if self.players[seat_id].is_bot {
            return Err("Bots cannot be readied".to_string());
        }
        self.ready[seat_id] = ready;
        Ok(())
    }

    pub fn all_human_ready(&self) -> bool {
        self.players.iter().zip(self.ready.iter())
            .all(|(p, r)| p.is_bot || *r)
    }

    pub fn start_game(&mut self) {
        if self.state.phase == GamePhase::GameOver {
            self.start_next_round();
            return;
        }
        while self.players.len() < 4 {
            let bot_num = self.players.len() + 1;
            self.add_bot(format!("Bot {}", bot_num));
        }
        self.started = true;
        self.deal_and_start_discard();
    }

    /// Start the next round of a continuing session (the room persists after
    /// game over — no re-creation needed).
    ///
    /// The previous round's `scores` are already baked into `total_scores`
    /// by `finalize_game`; this only resets per-round state. Round 2+ has
    /// NO three-discard: cards are dealt straight into `Playing`, and the
    /// player who finished 1st in the previous round leads the first trick.
    /// Every seat is force-ready so the creator can start immediately.
    pub fn start_next_round(&mut self) {
        let prev_winner = self.state.finished_order.first().copied();

        self.state.round += 1;

        for p in self.state.players.iter_mut() {
            p.hand.clear();
            p.finished = false;
        }
        for r in self.ready.iter_mut() {
            *r = true;
        }
        for r in self.state.ready.iter_mut() {
            *r = true;
        }

        self.state.finished_order.clear();
        while self.state.scores.len() < self.state.players.len() {
            self.state.scores.push(0);
        }
        for s in self.state.scores.iter_mut() {
            *s = 0;
        }
        self.state.ensure_total_scores();
        self.state.trick = TrickState::new();
        self.state.three_discard = None;

        let mut engine = crate::game::engine::GameEngine::new(self.state.clone());
        engine.deal_cards_guarded();
        engine.state_mut().phase = GamePhase::Playing;
        engine.state_mut().current_player = prev_winner.unwrap_or(0);
        self.state = engine.state().clone();
    }

    pub fn add_disconnected_player(&mut self, seat_id: usize, token: String) {
        if !self.disconnected_players.iter().any(|(id, _, _)| *id == seat_id) {
            self.disconnected_players.push((seat_id, token, Instant::now()));
        }
    }

    pub fn deal_and_start_discard(&mut self) {
        let mut engine = crate::game::engine::GameEngine::new(self.state.clone());
        engine.deal_cards_guarded();
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

    pub fn has_connected_human(&self) -> bool {
        self.players.iter().any(|p| !p.is_bot && p.connected)
    }

    pub fn record_human_disconnect(&mut self) {
        if !self.has_connected_human() {
            self.last_human_disconnect_at = Some(Instant::now());
        } else {
            self.last_human_disconnect_at = None;
        }
    }

    pub fn human_reconnected(&mut self) {
        self.last_human_disconnect_at = None;
    }

    pub fn is_orphaned(&self, timeout_secs: u64) -> bool {
        if self.has_connected_human() {
            return false;
        }
        match self.last_human_disconnect_at {
            Some(t) => Instant::now().duration_since(t).as_secs() >= timeout_secs,
            None => false,
        }
    }

    /// Move the creator crown from the leaving seat to the next connected
    /// human, in BOTH parallel vectors (the client renders state.players),
    /// and auto-ready the new creator (the Ready button is hidden for the
    /// creator, so a non-ready creator could never satisfy all_human_ready()
    /// and the room would be stuck). Lobby only.
    pub fn transfer_crown_from(&mut self, leaving_id: usize) {
        if self.state.phase != GamePhase::Lobby {
            return;
        }
        if let Some(new_creator) = self
            .players
            .iter()
            .position(|p| !p.is_bot && p.connected && p.id != leaving_id)
        {
            let target_id = self.players[new_creator].id;
            self.players[new_creator].is_creator = true;
            if let Some(sp) = self.state.players.iter_mut().find(|p| p.id == target_id) {
                sp.is_creator = true;
            }
            if target_id < self.ready.len() {
                self.ready[target_id] = true;
            }
            if target_id < self.state.ready.len() {
                self.state.ready[target_id] = true;
            }
        }
    }

    /// Compact a seat out of EVERY lobby parallel vector and renumber the
    /// survivors to dense 0..n-1, so `players[i].id == i` stays true for BOTH
    /// `players` and `state.players`, and `ready` / `state.ready` / `scores`
    /// stay aligned with the seats. Lobby only — mid-game seats never leave
    /// (they become bots instead).
    ///
    /// Also drops the removed seat's pending rejoin token and shifts down any
    /// other pending disconnect entries: without that, a late Rejoin from a
    /// reaped seat would restore_seat() onto a compacted slot that now
    /// belongs to someone else (seat hijack).
    ///
    /// Returns the old→new seat map so callers can follow the survivors' ids
    /// into their session records — otherwise a survivor's stale player_id
    /// would point at someone else's hand/personalization and their plays
    /// would be rejected.
    pub fn remove_lobby_seat(&mut self, seat_id: usize) -> Vec<(usize, usize)> {
        if self.state.phase != GamePhase::Lobby {
            return Vec::new();
        }
        let mut renumbered: Vec<(usize, usize)> = Vec::new();
        self.players.retain(|p| p.id != seat_id);
        self.state.players.retain(|p| p.id != seat_id);
        if seat_id < self.ready.len() {
            self.ready.remove(seat_id);
        }
        // The leaver's ready bit is moot once they're gone — but the bit is
        // only removed in the lobby (mid-game seats become bots), so set the
        // survivor-side invariant cheaply: a seat that no longer exists
        // can't block all_human_ready().
        if seat_id < self.state.ready.len() {
            self.state.ready.remove(seat_id);
        }
        if seat_id < self.state.scores.len() {
            self.state.scores.remove(seat_id);
        }
        for (new_id, p) in self.players.iter_mut().enumerate() {
            if p.id != new_id {
                renumbered.push((p.id, new_id));
            }
            p.id = new_id;
        }
        for (new_id, p) in self.state.players.iter_mut().enumerate() {
            p.id = new_id;
        }
        self
            .disconnected_players
            .retain(|(id, _, _)| *id != seat_id);
        for (id, _, _) in self.disconnected_players.iter_mut() {
            if *id > seat_id {
                *id -= 1;
            }
        }
        renumbered
    }

    pub fn cleanup_disconnected_lobby_players(&mut self, timeout_secs: u64) -> (Vec<(usize, String)>, Vec<(usize, usize)>) {
        if self.state.phase != GamePhase::Lobby {
            return (Vec::new(), Vec::new());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // Collect the stale seats first (dense invariant: position == id).
        // Removing them via remove_lobby_seat() shifts later ids down by one
        // each, so walk ascending and subtract how many were already removed.
        // Every survivor renumber is recorded (old -> new) so the manager can
        // follow ids into the session map.
        let stale: Vec<(usize, String)> = self
            .players
            .iter()
            .filter(|p| p.disconnect_time.map_or(false, |dt| now - dt >= timeout_secs))
            .map(|p| (p.id, p.name.clone()))
            .collect();
        let mut removed = 0usize;
        let mut renumbered: Vec<(usize, usize)> = Vec::new();
        for (old_id, _) in stale.iter() {
            renumbered.extend(self.remove_lobby_seat(old_id - removed));
            removed += 1;
        }
        (stale, renumbered)
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
        assert!(room.players[0].is_creator);
        assert_eq!(room.state.phase, GamePhase::Lobby);
        assert!(!room.started);
        assert_eq!(room.ready, vec![true]);
    }

    #[test]
    fn test_room_add_player() {
        let mut room = Room::new("ABC123".to_string(), "Host".to_string(), "host-token".to_string());
        let id = room.add_player("Player1".to_string(), "p1-token".to_string()).unwrap();
        assert_eq!(id, 1);
        assert_eq!(room.players.len(), 2);
        assert_eq!(room.state.players.len(), 2);
        assert_eq!(room.players[1].token, Some("p1-token".to_string()));
        assert!(!room.players[1].is_creator);
        assert_eq!(room.ready, vec![true, false]);
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
        assert!(!room.players[1].is_creator);
        assert_eq!(room.ready, vec![true, true]);
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
                is_creator: false,
            }],
            ready: vec![true],
            current_player: 0,
            trick: TrickState::new(),
            finished_order: Vec::new(),
            scores: vec![0],
            round: 1,
            total_scores: vec![0],
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
    fn test_finalize_game_accumulates_total_scores_once() {
        use crate::game::rules::finalize_game;
        let mut room = Room::new("ABC123".to_string(), "Host".to_string(), "host-token".to_string());
        room.start_game(); // round 1, phase ThreeDiscard

        // Simulate a finished round: 0=1st, 1=2nd, 2=3rd, 3=last (loser).
        room.state.scores = vec![10, 5, 0, -15];
        room.state.finished_order = vec![0, 1, 2];
        for p in room.state.players.iter_mut() {
            p.finished = true;
        }
        // Undo the 3 finished flags on player 3 so finalize can score the loser.
        room.state.players[3].finished = false;

        assert!(finalize_game(&mut room.state));
        assert_eq!(room.state.phase, GamePhase::GameOver);
        assert_eq!(room.state.total_scores, vec![10, 5, 0, -15]);
        assert_eq!(room.state.finished_order, vec![0, 1, 2, 3]);

        // Idempotency: a second call must NOT double-accumulate.
        assert!(!finalize_game(&mut room.state));
        assert_eq!(room.state.total_scores, vec![10, 5, 0, -15]);
    }

    #[test]
    fn test_start_next_round_continues_session() {
        let mut room = Room::new("ABC123".to_string(), "Host".to_string(), "host-token".to_string());
        room.start_game();
        assert_eq!(room.state.round, 1);
        assert!(room.state.three_discard.is_some());

        // Finish round 1: player 0 wins, player 3 loses.
        room.state.scores = vec![10, 5, 0, -15];
        room.state.finished_order = vec![0, 1, 2, 3];
        room.state.phase = GamePhase::GameOver;
        room.state.ensure_total_scores();
        for i in 0..4 {
            room.state.total_scores[i] += room.state.scores[i];
            room.state.players[i].finished = true;
        }

        // Continue the session (creator presses Start Game again).
        room.start_game();

        assert_eq!(room.state.round, 2);
        assert_eq!(room.state.phase, GamePhase::Playing);
        // No three-discard on round 2+.
        assert!(room.state.three_discard.is_none());
        // Fresh hands, all 13 cards.
        for p in room.state.players.iter() {
            assert_eq!(p.hand.len(), 13);
            assert!(!p.finished);
        }
        // Per-round scores reset; cumulative scores preserved.
        assert_eq!(room.state.scores, vec![0, 0, 0, 0]);
        assert_eq!(room.state.total_scores, vec![10, 5, 0, -15]);
        // Previous round's winner leads the first trick.
        assert_eq!(room.state.current_player, 0);
        assert!(room.state.finished_order.is_empty());
        // Auto-ready so the waiting room can start immediately.
        assert!(room.ready.iter().all(|&r| r));
        assert!(room.state.ready.iter().all(|&r| r));
    }

    #[test]
    fn test_start_game_rejected_mid_round() {
        use crate::rooms::RoomManager;
        let mgr = RoomManager::new(6, 100, 60, 30);
        let (code, pid, _msg, _s) = mgr.create_room("Host".to_string(), true);
        // Start the round 1 (phase ThreeDiscard).
        let (msg, _spawn) = mgr.start_game(&code, pid).unwrap();
        assert!(matches!(msg, crate::protocol::ServerMsg::State { .. }));
        // A stray StartGame mid-round must be rejected, not re-deal.
        let result = mgr.start_game(&code, pid);
        assert!(result.is_err(), "mid-round StartGame must be rejected");
    }

    #[test]
    fn test_start_game_from_gameover_continues() {
        use crate::rooms::RoomManager;
        let mgr = RoomManager::new(6, 100, 60, 30);
        let (code, pid, _msg, _s) = mgr.create_room("Host".to_string(), true);
        let (_, _spawn) = mgr.start_game(&code, pid).unwrap();

        // Finish round 1 so the room sits in GameOver.
        {
            let map = mgr.rooms_ref();
            let mut entry = map.get_mut(&code).unwrap();
            let room: &mut Room = &mut *entry;
            room.state.scores = vec![10, 5, 0, -15];
            room.state.finished_order = vec![0, 1, 2, 3];
            room.state.phase = GamePhase::GameOver;
            room.state.ensure_total_scores();
            for i in 0..4 {
                room.state.total_scores[i] += room.state.scores[i];
                room.state.players[i].finished = true;
            }
        }

        // Creator starts again: round 2, no 3-discard, cumulative scores kept.
        let (msg, spawn) = mgr.start_game(&code, pid).unwrap();
        assert!(spawn, "continuation must drive bot turns");
        match msg {
            crate::protocol::ServerMsg::State { state } => {
                assert_eq!(state.round, 2);
                assert_eq!(state.phase, GamePhase::Playing);
                assert!(state.three_discard.is_none());
                assert_eq!(state.total_scores, vec![10, 5, 0, -15]);
                assert_eq!(state.scores, vec![0, 0, 0, 0]);
                assert_eq!(state.current_player, 0);
                assert!(state.ready.iter().all(|&r| r));
            }
            other => panic!("expected State, got {:?}", other),
        }
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
        let mut room = Room::new("ABC123".to_string(), "alice".to_string(), "alice-token".to_string());
        room.add_disconnected_player(0, "alice-token".to_string());
        room.cleanup_expired_disconnected(0);
        assert!(room.disconnected_players.is_empty());
    }

    #[test]
    fn test_set_ready() {
        let mut room = Room::new("ABC123".to_string(), "Host".to_string(), "h".to_string());
        room.add_player("P1".to_string(), "p1".to_string()).unwrap();
        room.set_ready(1, true).unwrap();
        assert_eq!(room.ready, vec![true, true]);
        room.set_ready(1, false).unwrap();
        assert_eq!(room.ready, vec![true, false]);
    }

    #[test]
    fn test_set_ready_invalid_seat() {
        let mut room = Room::new("ABC123".to_string(), "Host".to_string(), "h".to_string());
        assert!(room.set_ready(5, true).is_err());
    }

    #[test]
    fn test_all_human_ready() {
        let mut room = Room::new("ABC123".to_string(), "Host".to_string(), "h".to_string());
        assert!(room.all_human_ready());
        room.add_player("P1".to_string(), "p1".to_string()).unwrap();
        assert!(!room.all_human_ready());
        room.set_ready(1, true).unwrap();
        assert!(room.all_human_ready());
    }

    #[test]
    fn test_start_game() {
        let mut room = Room::new("ABC123".to_string(), "Host".to_string(), "h".to_string());
        room.add_player("P1".to_string(), "p1".to_string()).unwrap();
        room.start_game();
        assert!(room.started);
        assert_eq!(room.players.len(), 4);
        assert_eq!(room.state.phase, GamePhase::ThreeDiscard);
    }
}
