use dashmap::DashMap;
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use std::sync::Arc;
#[cfg(test)]
use std::time::Instant;
use axum::extract::ws::Message;
use tokio::sync::mpsc::UnboundedSender;
use crate::game::state::{Room, GameState, GamePhase};
use crate::game::rules::{process_three_discard, process_one_bot_turn};
use crate::protocol::ServerMsg;

#[derive(serde::Serialize, Clone)]
pub struct PublicRoomSummary {
    pub code: String,
    pub players: usize,
    pub max_players: usize,
    pub phase: String,
    pub host: String,
}

type SessionSender = Arc<UnboundedSender<Message>>;

pub struct RoomManager {
    rooms: Arc<DashMap<String, Room>>,
    // (player_id, sender): every connection knows which seat it belongs to,
    // so state broadcasts can be personalized per viewer (card privacy).
    sessions: Arc<DashMap<String, Vec<(usize, SessionSender)>>> ,
    code_length: usize,
    bot_turn_delay_ms: u64,
    orphan_timeout_secs: u64,
    lobby_disconnect_timeout_secs: u64,
}

impl RoomManager {
    pub fn new(code_length: usize, bot_turn_delay_ms: u64, orphan_timeout_secs: u64, lobby_disconnect_timeout_secs: u64) -> Self {
        RoomManager {
            rooms: Arc::new(DashMap::new()),
            sessions: Arc::new(DashMap::new()),
            code_length,
            bot_turn_delay_ms,
            orphan_timeout_secs,
            lobby_disconnect_timeout_secs,
        }
    }

    pub fn add_session(&self, code: String, player_id: usize, sender: SessionSender) {
        self.sessions.entry(code).or_insert_with(Vec::new).push((player_id, sender));
    }

    pub fn remove_session(&self, code: &str, sender: &Arc<UnboundedSender<Message>>) {
        if let Some(mut entry) = self.sessions.get_mut(code) {
            entry.retain(|s| !Arc::ptr_eq(&s.1, sender));
        }
        // Drop the entry guard BEFORE touching the map again. Re-locking the
        // shard while holding a get_mut() guard can deadlock when a writer is
        // queued on that shard (observed: a mid-game disconnect wedged the
        // whole server, /health stopped answering). remove_if is atomic:
        // it removes the key only if it is still empty, so a concurrent
        // add_session can't be clobbered.
        self.sessions.remove_if(code, |_, v| v.is_empty());
    }

    pub fn create_room(&self, host_name: String, is_public: bool) -> (String, usize, ServerMsg, bool) {
        let code = self.generate_code();
        let token = Self::generate_token();
        let mut room = Room::new(code.clone(), host_name.clone(), token);
        room.is_public = is_public;

        self.rooms.insert(code.clone(), room);

        let state = self.rooms.get(&code).unwrap().value().state.clone();

        let token = self.rooms.get(&code).unwrap().value().players[0].token.clone().unwrap();

        (
            code.clone(),
            0,
            ServerMsg::Created {
                code,
                player_id: 0,
                state,
                is_public,
                token,
            },
            false,
        )
    }

    pub fn join_room(&self, code: &str, name: String) -> Result<(ServerMsg, bool), String> {
        let code = code.to_uppercase();
        let token = Self::generate_token();
        let mut room = self.rooms.get_mut(&code).ok_or("Room not found")?;

        if room.state.phase != GamePhase::Lobby {
            return Err("Game already started".to_string());
        }

        if room.players.len() >= 4 {
            return Err("Room is full".to_string());
        }

        match room.add_player(name, token.clone()) {
            Ok(player_id) => {
                let state = room.state.clone();
                drop(room);

                let player_name = state.players.iter().find(|p| p.id == player_id).map(|p| p.name.clone());

                self.broadcast(&code, ServerMsg::PlayerJoined {
                    player_id,
                    name: player_name.unwrap_or_else(|| format!("Player {}", player_id)),
                });

                self.broadcast(&code, ServerMsg::State { state: state.clone() });

                Ok((ServerMsg::Joined {
                    player_id,
                    state,
                    code: code.clone(),
                    token,
                }, false))
            }
            Err(e) => Err(e),
        }
    }

    const REJOIN_TIMEOUT_SECS: u64 = 300;

    pub fn rejoin_room(&self, code: &str, name: &str, token: &str) -> Result<(ServerMsg, bool), String> {
        let code = code.to_uppercase();
        let mut room = self.rooms.get_mut(&code).ok_or("Room not found")?;

        room.cleanup_expired_disconnected(Self::REJOIN_TIMEOUT_SECS);

        if let Some(pos) = room.disconnected_players.iter().position(|(_, t, _)| t == token) {
            let (seat_id, _, _) = room.disconnected_players.remove(pos);

            room.restore_seat(seat_id, name, token);
            room.human_reconnected();

            let state = room.state.clone();
            drop(room);

            self.broadcast(&code, ServerMsg::PlayerJoined {
                player_id: seat_id,
                name: name.to_string(),
            });

            return Ok((ServerMsg::Rejoined {
                player_id: seat_id,
                state: crate::protocol::personalise_state(&state, seat_id),
                code: code.clone(),
                token: token.to_string(),
            }, false));
        }

        Err("No matching disconnected player found".to_string())
    }

    pub fn leave_room(&self, code: &str, player_id: usize) -> Option<ServerMsg> {
        let mut room = self.rooms.get_mut(code)?;

        let (player_name, is_bot, token, is_creator) = {
            let player = room.players.iter().find(|p| p.id == player_id)?;
            (player.name.clone(), player.is_bot, player.token.clone(), player.is_creator)
        };

        if room.state.phase == GamePhase::Lobby && is_creator && is_bot == false && room.is_public {
            drop(room);
            self.rooms.remove(code);
            self.sessions.remove(code);
            return Some(ServerMsg::PlayerLeft {
                player_id,
                name: player_name,
            });
        }

        if !is_bot {
            if let Some(t) = token {
                room.add_disconnected_player(player_id, t);
            }

            if room.state.phase == GamePhase::Lobby {
                if let Some(player) = room.players.iter_mut().find(|p| p.id == player_id) {
                    player.connected = false;
                    player.disconnect_time = Some(std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs());
                }
                if is_creator {
                    if let Some(new_creator) = room.players.iter().position(|p| !p.is_bot && p.connected && p.id != player_id) {
                        room.players[new_creator].is_creator = true;
                    }
                }
                let msg = ServerMsg::PlayerLeft {
                    player_id,
                    name: player_name,
                };
                let removed = room.cleanup_disconnected_lobby_players(self.lobby_disconnect_timeout_secs);
                for (rid, rname) in &removed {
                    self.broadcast(code, ServerMsg::PlayerLeft {
                        player_id: *rid,
                        name: rname.clone(),
                    });
                }
                room.record_human_disconnect();
                if room.is_orphaned(self.orphan_timeout_secs) {
                    drop(room);
                    self.rooms.remove(code);
                    self.sessions.remove(code);
                } else {
                    let state = room.state.clone();
                    drop(room);
                    self.broadcast(code, ServerMsg::State { state });
                }
                return Some(msg);
            }

            if let Some(player) = room.players.iter_mut().find(|p| p.id == player_id) {
                player.name = format!("Bot ({})", player_name);
                player.is_bot = true;
                player.connected = true;
                player.disconnect_time = None;
                player.token = None;
            }
            if let Some(player) = room.state.players.iter_mut().find(|p| p.id == player_id) {
                player.name = format!("Bot ({})", player_name);
                player.is_bot = true;
                player.connected = true;
            }

            let state = room.state.clone();
            room.record_human_disconnect();
            let orphaned = room.is_orphaned(self.orphan_timeout_secs);
            drop(room);

            if orphaned {
                self.rooms.remove(code);
                self.sessions.remove(code);
            } else {
                self.broadcast(&code, ServerMsg::PlayerLeft {
                    player_id,
                    name: player_name.clone(),
                });
                self.broadcast(&code, ServerMsg::State { state });
            }
            return Some(ServerMsg::PlayerLeft {
                player_id,
                name: player_name,
            });
        }

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

    pub fn remove_player(&self, code: &str, player_id: usize) -> Option<ServerMsg> {
        let mut room = self.rooms.get_mut(code)?;

        let (player_name, is_bot, is_creator) = {
            let player = room.players.iter().find(|p| p.id == player_id)?;
            (player.name.clone(), player.is_bot, player.is_creator)
        };

        if is_bot {
            return None;
        }

        if room.state.phase == GamePhase::Lobby {
            if is_creator {
                let is_public = room.is_public;
                if is_public {
                    drop(room);
                    self.rooms.remove(code);
                    self.sessions.remove(code);
                    return Some(ServerMsg::PlayerLeft {
                        player_id,
                        name: player_name,
                    });
                }
            }

            if let Some(new_creator) = room.players.iter().position(|p| !p.is_bot && p.id != player_id) {
                room.players[new_creator].is_creator = true;
            }

            let token = room.players.iter().find(|p| p.id == player_id).and_then(|p| p.token.clone());
            if let Some(t) = token {
                room.add_disconnected_player(player_id, t);
            }

            room.players.retain(|p| p.id != player_id);
            room.state.players.retain(|p| p.id != player_id);

            let state = room.state.clone();
            drop(room);

            self.broadcast(code, ServerMsg::PlayerLeft {
                player_id,
                name: player_name.clone(),
            });
            self.broadcast(code, ServerMsg::State { state });
            return Some(ServerMsg::PlayerLeft {
                player_id,
                name: player_name,
            });
        }

        None
    }

    pub fn get_room(&self, code: &str) -> Option<Room> {
        self.rooms.get(code).map(|r| r.value().clone())
    }

    #[cfg(test)]
    pub fn expire_disconnected_player(&self, code: &str, token: &str) {
        if let Some(mut room) = self.rooms.get_mut(code) {
            for entry in &mut room.disconnected_players {
                if entry.1 == token {
                    entry.2 = Instant::now() - std::time::Duration::from_secs(1);
                }
            }
            room.cleanup_expired_disconnected(0);
        }
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
        // State-carrying messages are personalized per viewer (card privacy):
        // every connection only receives the hands / 3s of its own seat.
        if matches!(msg, ServerMsg::State { .. }) {
            if let Some(entry) = self.sessions.get(code) {
                let senders: Vec<_> = entry.iter().map(|e| (e.0, e.1.clone())).collect();
                for (pid, sender) in senders {
                    let json = crate::protocol::personalise_for_viewer(&msg, pid);
                    let _ = sender.send(Message::Text(json.into()));
                }
            }
            return;
        }

        let json = match serde_json::to_string(&msg) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("Failed to serialize message: {}", e);
                return;
            }
        };

        if let Some(entry) = self.sessions.get(code) {
            let message = Message::Text(json.into());
            let senders: Vec<_> = entry.iter().map(|e| e.1.clone()).collect();
            for sender in senders {
                let _ = sender.send(message.clone());
            }
        }
    }

    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    pub fn list_public_rooms(&self) -> Vec<PublicRoomSummary> {
        let mut result = Vec::new();
        for entry in self.rooms.iter() {
            let room = entry.value();
            if room.is_public && room.state.phase == GamePhase::Lobby {
                result.push(PublicRoomSummary {
                    code: room.code.clone(),
                    players: room.players.iter().filter(|p| !p.is_bot).count(),
                    max_players: 4,
                    phase: room.state.phase.to_string(),
                    host: room.players.first().map(|p| p.name.clone()).unwrap_or_default(),
                });
            }
        }
        result
    }

    pub fn ready_player(&self, code: &str, player_id: usize, ready: bool) -> Result<ServerMsg, String> {
        let mut room = self.rooms.get_mut(code).ok_or("Room not found")?;
        room.set_ready(player_id, ready)?;
        let player_name = room.players.iter().find(|p| p.id == player_id)
            .map(|p| p.name.clone()).unwrap_or_default();
        drop(room);
        Ok(ServerMsg::PlayerReady {
            player_id,
            name: player_name,
            ready,
        })
    }

    pub fn start_game(&self, code: &str, player_id: usize) -> Result<(ServerMsg, bool), String> {
        let mut room = self.rooms.get_mut(code).ok_or("Room not found")?;
        if !room.players.iter().any(|p| p.id == player_id && p.is_creator) {
            return Err("Only the room creator can start the game".to_string());
        }
        if !room.all_human_ready() {
            return Err("Not all players are ready".to_string());
        }
        room.start_game();
        let should_spawn = true;
        let state = room.state.clone();
        drop(room);
        self.broadcast(code, ServerMsg::GameStarted);
        self.broadcast(code, ServerMsg::State { state: state.clone() });
        Ok((ServerMsg::State {
            state: crate::protocol::personalise_state(&state, player_id),
        }, should_spawn))
    }

    fn generate_code(&self) -> String {
        let mut rng = thread_rng();
        (0..self.code_length)
            .map(|_| rng.sample(Alphanumeric) as char)
            .collect::<String>()
            .to_uppercase()
    }

    fn generate_token() -> String {
        let mut rng = thread_rng();
        (0..32)
            .map(|_| rng.sample(Alphanumeric) as char)
            .collect::<String>()
            .to_lowercase()
    }

    pub fn get_bot_turn_delay_ms(&self) -> u64 {
        self.bot_turn_delay_ms
    }

    pub fn rooms_ref(&self) -> Arc<DashMap<String, Room>> {
        self.rooms.clone()
    }

    pub async fn process_three_discard_delayed(&self, code: &str) {
        let state = match self.get_state(code) {
            Some(s) => s,
            None => return,
        };
        let td = match &state.three_discard {
            Some(td) => td.clone(),
            None => return,
        };

        self.broadcast(code, ServerMsg::State { state: state.clone() });
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;

        for &pid in &td.order {
            let mut state = match self.get_state(code) {
                Some(s) if s.three_discard.is_some() => s,
                _ => return,
            };

            let player_name = state.players[pid].name.clone();
            let cards: Vec<String> = state.three_discard.as_ref().unwrap().player_cards[pid]
                .iter().map(|c| c.to_string()).collect();
            let count = cards.len();

            process_three_discard(&mut state, pid);

            let log_msg = if count > 0 {
                // Privacy: log must not name the exact 3s, it is broadcast to
                // every seat. Only the count is public information.
                format!("{} discarded their 3s ({} 3s)", player_name, count)
            } else {
                format!("{} has no 3s", player_name)
            };
            state.log.push(log_msg);

            self.update_state(code, state.clone());
            self.broadcast(code, ServerMsg::State { state });

            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        let mut state = match self.get_state(code) {
            Some(s) => s,
            None => return,
        };

        if state.phase == GamePhase::Playing {
            state.log.push(format!("Game starts! {} leads first trick.", state.players[state.current_player].name));
            self.update_state(code, state.clone());
            self.broadcast(code, ServerMsg::State { state: state.clone() });
            self.process_bot_turns_delayed(code).await;
        }
    }

    async fn process_bot_turns_delayed(&self, code: &str) {
        loop {
            let mut state = match self.get_state(code) {
                Some(s) if s.phase == GamePhase::Playing => s,
                _ => return,
            };
            let needs_more = process_one_bot_turn(&mut state);
            self.update_state(code, state.clone());
            self.broadcast(code, ServerMsg::State { state });
            if !needs_more {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(self.bot_turn_delay_ms)).await;
        }
        let mut state = match self.get_state(code) {
            Some(s) if s.phase == GamePhase::Playing => s,
            _ => return,
        };
        crate::game::rules::skip_finished(&mut state);
        self.update_state(code, state.clone());
        self.broadcast(code, ServerMsg::State { state });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_room() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, player_id, msg, should_spawn) = manager.create_room("Alice".to_string(), true);

        assert_eq!(code.len(), 6);
        assert_eq!(player_id, 0);
        assert_eq!(manager.room_count(), 1);
        assert!(!should_spawn);

        match msg {
            ServerMsg::Created { code: c, player_id: pid, state, .. } => {
                assert_eq!(c, code);
                assert_eq!(pid, 0);
                assert_eq!(state.players.len(), 1);
                assert_eq!(state.phase, GamePhase::Lobby);
            }
            _ => panic!("Expected Created"),
        }
    }

    #[test]
    fn test_join_room() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);

        let result = manager.join_room(&code, "Bob".to_string());
        assert!(result.is_ok());

        match result.unwrap() {
            (ServerMsg::Joined { player_id, state, .. }, _should_spawn) => {
                assert_eq!(player_id, 1);
                assert_eq!(state.players[player_id].name, "Bob");
                assert_eq!(state.players[player_id].is_bot, false);
                assert_eq!(state.players.len(), 2);
                assert_eq!(state.phase, GamePhase::Lobby);
            }
            _ => panic!("Expected Joined"),
        }
    }

    #[test]
    fn test_join_room_not_found() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let result = manager.join_room("XXXXXX", "Bob".to_string());
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Room not found");
    }

    #[test]
    fn test_join_room_full() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        manager.join_room(&code, "Bob".to_string()).unwrap();
        manager.join_room(&code, "Charlie".to_string()).unwrap();
        manager.join_room(&code, "Dave".to_string()).unwrap();

        let result = manager.join_room(&code, "Eve".to_string());
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Room is full");
    }

    #[test]
    fn test_start_game() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        manager.join_room(&code, "Bob".to_string()).unwrap();

        // Not all ready, should fail
        let result = manager.start_game(&code, 0);
        assert!(result.is_err());

        // Ready Bob
        manager.ready_player(&code, 1, true).unwrap();

        // Start game as creator
        let result = manager.start_game(&code, 0);
        assert!(result.is_ok());

        let room = manager.get_room(&code).unwrap();
        assert!(room.started);
        assert_eq!(room.players.len(), 4);
        assert_eq!(room.state.phase, GamePhase::ThreeDiscard);
    }

    #[test]
    fn test_start_game_non_creator() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        manager.join_room(&code, "Bob".to_string()).unwrap();
        manager.ready_player(&code, 1, true).unwrap();

        let result = manager.start_game(&code, 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_leave_room_creator_dissolves_public() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        manager.join_room(&code, "Bob".to_string()).unwrap();

        manager.leave_room(&code, 0);
        assert_eq!(manager.room_count(), 0);
    }

    #[test]
    fn test_public_room_listing_only_lobby() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code1, _, _, _) = manager.create_room("Alice".to_string(), true);
        manager.create_room("Bob".to_string(), false);

        let room_list = manager.list_public_rooms();
        assert_eq!(room_list.len(), 1);
        assert_eq!(room_list[0].code, code1);
        assert_eq!(room_list[0].players, 1);
        assert_eq!(room_list[0].phase, "lobby");

        // Start the game, should no longer appear
        manager.start_game(&code1, 0).unwrap();
        let room_list = manager.list_public_rooms();
        assert_eq!(room_list.len(), 0);
    }

    #[test]
    fn test_leave_room() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        manager.join_room(&code, "Bob".to_string()).unwrap();
        manager.ready_player(&code, 1, true).unwrap();
        manager.start_game(&code, 0).unwrap();

        let result = manager.leave_room(&code, 1);
        assert!(result.is_some());

        match result.unwrap() {
            ServerMsg::PlayerLeft { player_id, name } => {
                assert_eq!(player_id, 1);
                assert_eq!(name, "Bob");
            }
            _ => panic!("Expected PlayerLeft"),
        }

        let room = manager.get_room(&code).unwrap();
        // Human player is auto-replaced by a bot
        assert!(room.players[1].is_bot);
        assert!(room.players[1].connected);
        assert_eq!(room.players[1].name, "Bot (Bob)");
    }

    #[test]
    fn test_get_room() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);

        let room = manager.get_room(&code);
        assert!(room.is_some());
        assert_eq!(room.unwrap().code, code);
    }

    #[test]
    fn test_generate_code_length() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        assert_eq!(code.len(), 6);

        let manager2 = RoomManager::new(8, 2500, 30, 15);
        let (code2, _, _, _) = manager2.create_room("Bob".to_string(), true);
        assert_eq!(code2.len(), 8);
    }

    #[test]
    fn test_multiple_rooms() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code1, _, _, _) = manager.create_room("Alice".to_string(), true);
        let (code2, _, _, _) = manager.create_room("Bob".to_string(), true);

        assert_ne!(code1, code2);
        assert_eq!(manager.room_count(), 2);
    }

    #[test]
    fn test_update_state() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);

        let mut state = manager.get_state(&code).unwrap();
        state.log.push("test".to_string());
        manager.update_state(&code, state);

        let updated = manager.get_state(&code).unwrap();
        assert_eq!(updated.log, vec!["test".to_string()]);
    }

    #[test]
    fn test_code_is_alphanumeric() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        assert!(code.chars().all(|c| c.is_alphanumeric()));
    }

    #[test]
    fn test_rejoin_restores_seat() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        let (joined_msg, _) = manager.join_room(&code, "Bob".to_string()).unwrap();
        let token = match &joined_msg {
            ServerMsg::Joined { token, .. } => token.clone(),
            _ => panic!("Expected Joined"),
        };

        manager.ready_player(&code, 1, true).unwrap();
        manager.start_game(&code, 0).unwrap();

        manager.leave_room(&code, 1);
        let room = manager.get_room(&code).unwrap();
        assert_eq!(room.disconnected_players.len(), 1);

        let result = manager.rejoin_room(&code, "Bob", &token);
        assert!(result.is_ok());
        match result.unwrap().0 {
            ServerMsg::Rejoined { player_id, state, .. } => {
                assert_eq!(player_id, 1);
                assert_eq!(state.players[1].name, "Bob");
                assert!(!state.players[1].is_bot);
                assert!(state.players[1].connected);
            }
            _ => panic!("Expected Rejoined"),
        }

        let room = manager.get_room(&code).unwrap();
        assert!(!room.disconnected_players.iter().any(|(_, t, _)| t == &token));
    }

    #[test]
    fn test_rejoin_not_found() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        let result = manager.rejoin_room(&code, "Unknown", "nonexistent-token");
        assert!(result.is_err());
    }

    #[test]
    fn test_leave_room_tracks_disconnected() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        let (joined_msg, _) = manager.join_room(&code, "Bob".to_string()).unwrap();
        let token = match &joined_msg {
            ServerMsg::Joined { token, .. } => token.clone(),
            _ => panic!("Expected Joined"),
        };

        manager.ready_player(&code, 1, true).unwrap();
        manager.start_game(&code, 0).unwrap();
        manager.leave_room(&code, 1);

        let room = manager.get_room(&code).unwrap();
        assert_eq!(room.disconnected_players.len(), 1);
        assert_eq!(room.disconnected_players[0].0, 1);
        assert_eq!(room.disconnected_players[0].1, token);
    }

    #[test]
    fn test_leave_room_bot_not_tracked() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        manager.join_room(&code, "Bob".to_string()).unwrap();
        manager.ready_player(&code, 1, true).unwrap();
        manager.start_game(&code, 0).unwrap();

        manager.leave_room(&code, 2);

        let room = manager.get_room(&code).unwrap();
        assert!(room.disconnected_players.is_empty());
    }

    #[test]
    fn test_public_room_listing() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        manager.create_room("Alice".to_string(), true);
        manager.create_room("Bob".to_string(), false);

        let room_list = manager.list_public_rooms();
        assert_eq!(room_list.len(), 1);
        assert_eq!(room_list[0].players, 1);
        assert_eq!(room_list[0].max_players, 4);
        assert_eq!(room_list[0].host, "Alice");
        assert_eq!(room_list[0].phase, "lobby");
    }

    #[test]
    fn test_rejoin_timeout_expired() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        let (joined_msg, _) = manager.join_room(&code, "Bob".to_string()).unwrap();
        let token = match &joined_msg {
            ServerMsg::Joined { token, .. } => token.clone(),
            _ => panic!("Expected Joined"),
        };

        manager.ready_player(&code, 1, true).unwrap();
        manager.start_game(&code, 0).unwrap();
        manager.leave_room(&code, 1);
        let room = manager.get_room(&code).unwrap();
        assert_eq!(room.disconnected_players.len(), 1);

        // Expire the disconnected entry in-place so cleanup removes it
        manager.expire_disconnected_player(&code, &token);
        let room = manager.get_room(&code).unwrap();
        assert!(room.disconnected_players.is_empty());

        // Rejoin should fail because entry was cleaned up
        let result = manager.rejoin_room(&code, "Bob", &token);
        assert!(result.is_err());
    }

    #[test]
    fn test_full_rejoin_flow() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        let (joined_msg, _) = manager.join_room(&code, "Bob".to_string()).unwrap();
        let token = match &joined_msg {
            ServerMsg::Joined { token, .. } => token.clone(),
            _ => panic!("Expected Joined"),
        };

        manager.ready_player(&code, 1, true).unwrap();
        manager.start_game(&code, 0).unwrap();
        manager.leave_room(&code, 1);

        let result = manager.rejoin_room(&code, "Bob", &token);
        assert!(result.is_ok());
        match result.unwrap().0 {
            ServerMsg::Rejoined { player_id, state, .. } => {
                assert_eq!(player_id, 1);
                assert_eq!(state.players[1].name, "Bob");
                assert!(state.players[1].connected);
            }
            _ => panic!("Expected Rejoined"),
        }

        let room = manager.get_room(&code).unwrap();
        assert!(!room.disconnected_players.iter().any(|(_, t, _)| t == &token));
    }

    #[test]
    fn test_orphaned_room_removed_immediate() {
        let manager = RoomManager::new(6, 2500, 0, 0);

        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        manager.join_room(&code, "Bob".to_string()).unwrap();
        manager.ready_player(&code, 1, true).unwrap();
        manager.start_game(&code, 0).unwrap();

        manager.leave_room(&code, 1);
        assert_eq!(manager.room_count(), 1);

        manager.leave_room(&code, 0);
        assert_eq!(manager.room_count(), 0);
    }

    #[test]
    fn test_orphaned_room_not_removed_within_timeout() {
        let manager = RoomManager::new(6, 2500, 60, 15);

        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        manager.join_room(&code, "Bob".to_string()).unwrap();
        manager.ready_player(&code, 1, true).unwrap();
        manager.start_game(&code, 0).unwrap();

        manager.leave_room(&code, 1);
        assert_eq!(manager.room_count(), 1);

        manager.leave_room(&code, 0);
        assert_eq!(manager.room_count(), 1);
    }

    #[test]
    fn test_rejoin_resets_orphan_timer() {
        let manager = RoomManager::new(6, 2500, 0, 0);

        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        let (joined_msg, _) = manager.join_room(&code, "Bob".to_string()).unwrap();
        let token = match &joined_msg {
            ServerMsg::Joined { token, .. } => token.clone(),
            _ => panic!("Expected Joined"),
        };

        manager.ready_player(&code, 1, true).unwrap();
        manager.start_game(&code, 0).unwrap();

        manager.leave_room(&code, 1);
        assert_eq!(manager.room_count(), 1);

        let result = manager.rejoin_room(&code, "Bob", &token);
        assert!(result.is_ok());
        assert_eq!(manager.room_count(), 1);
    }

    #[test]
    fn test_orphaned_room_removed_in_lobby() {
        let manager = RoomManager::new(6, 2500, 0, 0);

        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        manager.join_room(&code, "Bob".to_string()).unwrap();

        manager.leave_room(&code, 1);
        assert_eq!(manager.room_count(), 1);

        manager.leave_room(&code, 0);
        assert_eq!(manager.room_count(), 0);
    }

    #[test]
    fn test_lobby_disconnected_player_removed_after_timeout() {
        let manager = RoomManager::new(6, 2500, 60, 0);

        let (code, _, _, _) = manager.create_room("Alice".to_string(), false);
        manager.join_room(&code, "Bob".to_string()).unwrap();
        manager.join_room(&code, "Charlie".to_string()).unwrap();

        assert_eq!(manager.get_room(&code).unwrap().players.len(), 3);

        manager.leave_room(&code, 0);

        assert_eq!(manager.room_count(), 1);
        let room = manager.get_room(&code).unwrap();
        assert!(!room.players.iter().any(|p| p.name == "Alice"));
        assert_eq!(room.players.len(), 2);
    }

    #[test]
    fn test_remove_player_instant_from_lobby() {
        let manager = RoomManager::new(6, 2500, 30, 15);

        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        manager.join_room(&code, "Bob".to_string()).unwrap();

        let result = manager.remove_player(&code, 1);
        assert!(result.is_some());

        let room = manager.get_room(&code).unwrap();
        assert_eq!(room.players.len(), 1);
        assert_eq!(room.players[0].name, "Alice");
    }

    #[test]
    fn test_remove_player_creator_dissolves_public() {
        let manager = RoomManager::new(6, 2500, 30, 15);

        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        manager.join_room(&code, "Bob".to_string()).unwrap();

        let result = manager.remove_player(&code, 0);
        assert!(result.is_some());

        assert_eq!(manager.room_count(), 0);
    }

    #[test]
    fn test_remove_player_transfers_creator_private() {
        let manager = RoomManager::new(6, 2500, 30, 15);

        let (code, _, _, _) = manager.create_room("Alice".to_string(), false);
        manager.join_room(&code, "Bob".to_string()).unwrap();

        let result = manager.remove_player(&code, 0);
        assert!(result.is_some());

        let room = manager.get_room(&code).unwrap();
        assert_eq!(room.players.len(), 1);
        assert_eq!(room.players[0].name, "Bob");
        assert!(room.players[0].is_creator);
    }

    #[test]
    fn test_remove_player_bot_returns_none() {
        let manager = RoomManager::new(6, 2500, 30, 15);

        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        manager.join_room(&code, "Bob".to_string()).unwrap();
        manager.ready_player(&code, 1, true).unwrap();
        manager.start_game(&code, 0).unwrap();

        let room = manager.get_room(&code).unwrap();
        let bot_id = room.players.iter().position(|p| p.is_bot).unwrap();

        assert!(manager.remove_player(&code, bot_id).is_none());
    }
}
