use dashmap::DashMap;
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use std::sync::Arc;
use actix_ws::Session;
use crate::game::state::{Room, GameState, GamePhase};
use crate::game::rules::{deal_cards, start_three_discard, process_three_discard, process_bot_turns};
use crate::protocol::ServerMsg;

pub struct RoomManager {
    rooms: Arc<DashMap<String, Room>>,
    sessions: Arc<DashMap<String, Vec<Session>>>,
    code_length: usize,
}

impl RoomManager {
    pub fn new(code_length: usize) -> Self {
        RoomManager {
            rooms: Arc::new(DashMap::new()),
            sessions: Arc::new(DashMap::new()),
            code_length,
        }
    }

    pub fn add_session(&self, code: String, session: Session) {
        self.sessions.entry(code).or_insert_with(Vec::new).push(session);
    }

    pub fn remove_session(&self, code: &str) {
        self.sessions.remove(code);
    }

    pub fn create_room(&self, host_name: String) -> (String, usize, ServerMsg) {
        let code = self.generate_code();
        let mut room = Room::new(code.clone(), host_name.clone());

        // Fill bots to 4 players
        let mut bot_count = 0;
        while room.players.len() < 4 {
            room.add_bot(format!("Bot {}", bot_count));
            bot_count += 1;
        }

          // Start game immediately
        if room.players.len() >= 4 && !room.started {
            room.started = true;
            deal_cards(&mut room.state);
            start_three_discard(&mut room.state);
            if let Some(ref td) = room.state.three_discard {
                let order = td.order.clone();
                for pid in order {
                    process_three_discard(&mut room.state, pid);
                }
            }
            process_bot_turns(&mut room.state);
        }

        self.rooms.insert(code.clone(), room);

        let state = self.rooms.get(&code).unwrap().value().state.clone();

        (
            code.clone(),
            0,
            ServerMsg::Created {
                code,
                player_id: 0,
                state,
            },
        )
    }

    pub fn join_room(&self, code: &str, name: String) -> Result<ServerMsg, String> {
        let code = code.to_uppercase();
        let mut room = self.rooms.get_mut(&code).ok_or("Room not found")?;

        // If room is full but has bots, replace a bot with the human
        if room.players.len() >= 4 {
            if let Some(bot_idx) = room.players.iter().position(|p| p.is_bot) {
                room.players[bot_idx].name = name.clone();
                room.players[bot_idx].is_bot = false;
                room.players[bot_idx].connected = true;
                room.players[bot_idx].disconnect_time = None;
                // Also update game state
                if bot_idx < room.state.players.len() {
                    room.state.players[bot_idx].name = name.clone();
                    room.state.players[bot_idx].is_bot = false;
                    room.state.players[bot_idx].connected = true;
                }
                let player_id = bot_idx;

                let state = room.state.clone();
                drop(room);

                self.broadcast(&code, ServerMsg::PlayerJoined {
                    player_id,
                    name: name.clone(),
                });

                return Ok(ServerMsg::Joined {
                    player_id,
                    state,
                });
            }
            return Err("Room is full".to_string());
        }

        match room.add_player(name) {
            Ok(player_id) => {
                // Fill bots if room not full
                let mut bot_count = 0;
                while room.players.len() < 4 {
                    room.add_bot(format!("Bot {}", bot_count));
                    bot_count += 1;
                }

                // Start game if room is full and not started
                if room.players.len() >= 4 && !room.started {
                    room.started = true;
                    deal_cards(&mut room.state);
                    start_three_discard(&mut room.state);
                    if let Some(ref td) = room.state.three_discard {
                        let order = td.order.clone();
                        for pid in order {
                            process_three_discard(&mut room.state, pid);
                        }
                    }
                    process_bot_turns(&mut room.state);
                }

                let state = room.state.clone();
                drop(room);

                let player_name = state.players.iter().find(|p| p.id == player_id).map(|p| p.name.clone());

                self.broadcast(&code, ServerMsg::PlayerJoined {
                    player_id,
                    name: player_name.unwrap_or_else(|| format!("Player {}", player_id)),
                });

                if state.phase != GamePhase::Lobby {
                    self.broadcast(&code, ServerMsg::State { state: state.clone() });
                }

                Ok(ServerMsg::Joined {
                    player_id,
                    state,
                })
            }
            Err(e) => Err(e),
        }
    }

    pub fn leave_room(&self, code: &str, player_id: usize) -> Option<ServerMsg> {
        let mut room = self.rooms.get_mut(code)?;

        let player_name = room.players.iter()
            .find(|p| p.id == player_id)
            .map(|p| p.name.clone())?;

        // Mark as disconnected
        if let Some(player) = room.players.iter_mut().find(|p| p.id == player_id) {
            player.connected = false;
            player.disconnect_time = Some(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs());
        }

        let msg = ServerMsg::PlayerLeft {
            player_id,
            name: player_name,
        };

        Some(msg)
    }

    pub fn get_room(&self, code: &str) -> Option<Room> {
        self.rooms.get(code).map(|r| r.value().clone())
    }

    pub fn get_state(&self, code: &str) -> Option<GameState> {
        self.rooms.get(code).map(|r| r.value().state.clone())
    }

    pub fn update_state(&self, code: &str, state: GameState) {
        if let Some(mut room) = self.rooms.get_mut(code) {
            room.state = state;
        }
    }

    pub fn broadcast(&self, code: &str, msg: ServerMsg) {
        let json = match serde_json::to_string(&msg) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("Failed to serialize message: {}", e);
                return;
            }
        };

        if let Some(entry) = self.sessions.get(code) {
            for session in entry.value() {
                actix_web::rt::spawn({
                    let mut session = session.clone();
                    let json = json.clone();
                    async move {
                        let _ = session.text(json).await;
                    }
                });
            }
        }
    }

    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    fn generate_code(&self) -> String {
        let mut rng = thread_rng();
        (0..self.code_length)
            .map(|_| rng.sample(Alphanumeric) as char)
            .collect::<String>()
            .to_uppercase()
    }

    pub fn rooms_ref(&self) -> Arc<DashMap<String, Room>> {
        self.rooms.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_room() {
        let manager = RoomManager::new(6);
        let (code, player_id, msg) = manager.create_room("Alice".to_string());

        assert_eq!(code.len(), 6);
        assert_eq!(player_id, 0);
        assert_eq!(manager.room_count(), 1);

        match msg {
            ServerMsg::Created { code: c, player_id: pid, state } => {
                assert_eq!(c, code);
                assert_eq!(pid, 0);
                assert_eq!(state.players.len(), 4); // Auto-filled with 3 bots
                assert_eq!(state.phase, GamePhase::Playing); // Game starts immediately
            }
            _ => panic!("Expected Created"),
        }
    }

    #[test]
    fn test_join_room() {
        let manager = RoomManager::new(6);
        let (code, _, _) = manager.create_room("Alice".to_string());

        let result = manager.join_room(&code, "Bob".to_string());
        assert!(result.is_ok());

        match result.unwrap() {
            ServerMsg::Joined { player_id, state } => {
                assert_eq!(player_id, 1); // Replaced Bot 0
                assert_eq!(state.players[player_id].name, "Bob");
                assert_eq!(state.players[player_id].is_bot, false);
                assert_eq!(state.players.len(), 4);
            }
            _ => panic!("Expected Joined"),
        }
    }

    #[test]
    fn test_join_room_not_found() {
        let manager = RoomManager::new(6);
        let result = manager.join_room("XXXXXX", "Bob".to_string());
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Room not found");
    }

    #[test]
    fn test_join_room_full() {
        let manager = RoomManager::new(6);
        let (code, _, _) = manager.create_room("Alice".to_string()); // 1 human + 3 bots
        manager.join_room(&code, "Bob".to_string()).unwrap(); // Replaces bot, now 2 humans + 2 bots
        manager.join_room(&code, "Charlie".to_string()).unwrap(); // 3 humans + 1 bot
        manager.join_room(&code, "Dave".to_string()).unwrap(); // 4 humans, no bots

        let result = manager.join_room(&code, "Eve".to_string());
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Room is full");
    }

    #[test]
    fn test_leave_room() {
        let manager = RoomManager::new(6);
        let (code, _, _) = manager.create_room("Alice".to_string());
        manager.join_room(&code, "Bob".to_string()).unwrap(); // Bob replaces Bot 0 at index 1

        let result = manager.leave_room(&code, 1);
        assert!(result.is_some());

        match result.unwrap() {
            ServerMsg::PlayerLeft { player_id, .. } => {
                assert_eq!(player_id, 1);
            }
            _ => panic!("Expected PlayerLeft"),
        }

        let room = manager.get_room(&code).unwrap();
        assert!(!room.players[1].connected);
    }

    #[test]
    fn test_get_room() {
        let manager = RoomManager::new(6);
        let (code, _, _) = manager.create_room("Alice".to_string());

        let room = manager.get_room(&code);
        assert!(room.is_some());
        assert_eq!(room.unwrap().code, code);
    }

    #[test]
    fn test_generate_code_length() {
        let manager = RoomManager::new(6);
        let (code, _, _) = manager.create_room("Alice".to_string());
        assert_eq!(code.len(), 6);

        let manager2 = RoomManager::new(8);
        let (code2, _, _) = manager2.create_room("Bob".to_string());
        assert_eq!(code2.len(), 8);
    }

    #[test]
    fn test_multiple_rooms() {
        let manager = RoomManager::new(6);
        let (code1, _, _) = manager.create_room("Alice".to_string());
        let (code2, _, _) = manager.create_room("Bob".to_string());

        assert_ne!(code1, code2);
        assert_eq!(manager.room_count(), 2);
    }

    #[test]
    fn test_update_state() {
        let manager = RoomManager::new(6);
        let (code, _, _) = manager.create_room("Alice".to_string());

        let mut state = manager.get_state(&code).unwrap();
        state.log.push("test".to_string());
        manager.update_state(&code, state);

        let updated = manager.get_state(&code).unwrap();
        assert_eq!(updated.log, vec!["test".to_string()]);
    }

    #[test]
    fn test_code_is_alphanumeric() {
        let manager = RoomManager::new(6);
        let (code, _, _) = manager.create_room("Alice".to_string());
        assert!(code.chars().all(|c| c.is_alphanumeric()));
    }
}
