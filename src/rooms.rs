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

#[derive(Clone)]
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

        let mut seat_in_use = false;
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
                // Raw (un-masked) state on purpose: the caller serializes it
                // through personalise_for_viewer, which adds the public
                // handCount AND masks other hands in one JSON pass. Doing the
                // masking here on the GameState struct used to drop handCount
                // (the struct has no such field) -> opponents showed 0 cards.
                state,
                code: code.clone(),
                token: token.to_string(),
            }, false));
        }

        // Not a disconnected seat. Distinguish "another live connection is
        // sitting in this seat" (same token still in `players` as connected
        // — e.g. the user opened a second tab / PWA + mobile browser) from a
        // genuinely gone seat (expired / room closed). The client shows a
        // "close your other window" hint in the first case and must NOT drop
        // the session: the room is alive, the seat is just busy right now.
        seat_in_use = room
            .players
            .iter()
            .any(|p| p.token.as_deref() == Some(token) && p.connected);
        drop(room);

        Err(if seat_in_use {
            "Seat already in use by another window".to_string()
        } else {
            "No matching disconnected player found".to_string()
        })
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
                    room.transfer_crown_from(player_id);
                }
                let msg = ServerMsg::PlayerLeft {
                    player_id,
                    name: player_name,
                };
                let (removed, renumbered) = room.cleanup_disconnected_lobby_players(self.lobby_disconnect_timeout_secs);
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
                    self.remap_sessions(code, &renumbered);
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

            // Crown transfers to the next human in BOTH parallel vectors via
            // the shared helper (the client renders state.players; a flag on
            // room.players alone would leave everyone without a Start
            // button), and the new creator is auto-ready.
            if is_creator {
                room.transfer_crown_from(player_id);
            }

            let token = room.players.iter().find(|p| p.id == player_id).and_then(|p| p.token.clone());
            // An explicit Leave in the lobby is final: the seat is compacted
            // out of the room immediately, so the leaver's pending-rejoin
            // entry would point at a seat that no longer exists. Keeping it
            // lets a late Rejoin restore_seat() onto whoever now sits in
            // that slot — a silent seat hijack. Drop the entry here (the
            // socket-drop path keeps its entry: that seat is NOT compacted
            // yet, so a fast rejoin is legitimate there).
            if let Some(t) = token {
                room.disconnected_players.retain(|(_, tok, _)| tok != &t);
            }

            // Compact the seat out of every parallel vector (players,
            // state.players, ready, state.ready, scores) and renumber
            // survivors dense — the old two-line retain left the ready/score
            // arrays misaligned and id = players.len() reused dead ids.
            let renumbered = room.remove_lobby_seat(player_id);

            let state = room.state.clone();
            drop(room);

            self.broadcast(code, ServerMsg::PlayerLeft {
                player_id,
                name: player_name.clone(),
            });
            self.remap_sessions(code, &renumbered);
            self.broadcast(code, ServerMsg::State { state });
            return Some(ServerMsg::PlayerLeft {
                player_id,
                name: player_name,
            });
        }

        // Mid-game explicit leave (the client's Leave button in the menu
        // drawer): convert the seat to a bot so the game continues for the
        // remaining humans. Mirrors leave_room's socket-drop mid-game branch
        // — the two paths must stay in sync or a deliberate leave silently
        // strands the round (this used to return None here: no bot, no
        // broadcast, everyone stuck on the leaver's turn).
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
        Some(ServerMsg::PlayerLeft {
            player_id,
            name: player_name,
        })
    }

    /// Read-only rejoin probe (the client's "is my saved session still
    /// live?" check on connect). Runs the same expiry cleanup as rejoin
    /// so the answer matches what a real rejoin would do right now, but
    /// never mutates seats: the room is left exactly as found.
    /// Returns (found, rejoinable).
    pub fn check_room(&self, code: &str, token: &str) -> (bool, bool) {
        let code = code.to_uppercase();
        let Some(mut room) = self.rooms.get_mut(&code) else {
            return (false, false);
        };
        room.cleanup_expired_disconnected(Self::REJOIN_TIMEOUT_SECS);
        let rejoinable = room
            .disconnected_players
            .iter()
            .any(|(_, t, _)| t == token);
        (true, rejoinable)
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

    /// Remap session entries (and tell the clients) after a lobby compaction
    /// renumbered seats. Without this, every survivor's stored player_id keeps
    /// pointing at the OLD seat: their Ready/Play lands on whoever now sits in
    /// that slot (wrong-seat play = invalid rejection or, worse, acting for
    /// someone else) and personalise_for_viewer masks the wrong hand — a
    /// card-privacy leak. Returns nothing; broadcasts SeatChanged.
    pub fn remap_sessions(&self, code: &str, renumbered: &[(usize, usize)]) {
        if renumbered.is_empty() {
            return;
        }
        if let Some(mut entry) = self.sessions.get_mut(code) {
            for e in entry.iter_mut() {
                if let Some((_old, new)) = renumbered.iter().find(|(o, _)| *o == e.0) {
                    e.0 = *new;
                }
            }
        }
        self.broadcast(
            code,
            ServerMsg::SeatChanged { renumbered: renumbered.to_vec() },
        );
    }

    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    /// Periodic reaper. `is_orphaned()` is otherwise only consulted *at* a
    /// human's disconnect, when the disconnect timer is 0s old, so it never
    /// fires there — a room whose last human left was silently kept forever
    /// (memory leak, phantom "Browse Public Rooms" entries). This is called
    /// from a background tick: drop every room with no connected human whose
    /// last-disconnect timer has matured, plus any dangling session senders.
    pub fn reap_orphaned_rooms(&self) -> usize {
        let orphans: Vec<String> = self
            .rooms
            .iter()
            .filter(|e| e.value().is_orphaned(self.orphan_timeout_secs))
            .map(|e| e.key().clone())
            .collect();
        for code in &orphans {
            self.rooms.remove(code);
            self.sessions.remove(code);
        }
        orphans.len()
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

    /// Host-only room settings (play limit + winning point). Editable only
    /// while the room is in Lobby or GameOver; clamped to sane ranges.
    /// `None` leaves a setting unchanged. Broadcasts `RoomSettings` on success.
    pub fn set_room_settings(
        &self,
        code: &str,
        player_id: usize,
        play_limit_secs: Option<u32>,
        winning_point: Option<u32>,
    ) -> Result<ServerMsg, String> {
        let mut room = self.rooms.get_mut(code).ok_or("Room not found")?;
        if !room.players.iter().any(|p| p.id == player_id && p.is_creator) {
            return Err("Only the host can change room settings".to_string());
        }
        if room.started {
            // Settings are tuned once, at the very beginning of the room
            // (before the first round starts). After that the room is locked
            // for good — even in the between-rounds waiting room.
            return Err("Room settings are locked after the game has started".to_string());
        }
        if play_limit_secs.is_none() && winning_point.is_none() {
            return Err("Nothing to update".to_string());
        }
        let play_limit = match play_limit_secs {
            Some(v) if (1..=120).contains(&v) => v,
            Some(_) => return Err("Play limit must be 1-120 seconds".to_string()),
            None => room.state.play_limit_secs,
        };
        let point = match winning_point {
            Some(v) if (1..=9999).contains(&v) => v,
            Some(_) => return Err("Winning point must be 1-9999".to_string()),
            None => room.state.winning_point,
        };
        room.state.play_limit_secs = play_limit;
        room.state.winning_point = point;
        Ok(ServerMsg::RoomSettings {
            play_limit_secs: play_limit,
            winning_point: point,
        })
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
        if room.state.game_winner.is_some() {
            return Err("Match already won — a new game is locked".to_string());
        }
        // Only a Lobby can start a fresh game, or a GameOver can continue the
        // session into the next round. (Also prevents a stray StartGame from
        // re-dealing mid-round.)
        if room.state.phase != GamePhase::Lobby && room.state.phase != GamePhase::GameOver {
            return Err("Game already started".to_string());
        }
        if room.state.phase == GamePhase::GameOver {
            // Continuation: the waiting room auto-readies everyone so the
            // creator can start the next round without re-toggling Ready.
            for r in room.ready.iter_mut() { *r = true; }
            for r in room.state.ready.iter_mut() { *r = true; }
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
        Ok((
            ServerMsg::State {
                // Raw state: ws.rs serializes this via personalise_for_viewer
                // (one JSON pass -> handCount kept, other hands masked).
                state,
            },
            should_spawn,
        ))
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

    /// Arm the play-limit watchdog on the current turn if it belongs to a
    /// human (bots are exempt): after `play_limit_secs` of silence the seat
    /// is auto-moved (lowest legal single, else pass) and logged. The
    /// `turn_seq` captured at spawn cancels a stale timer once the turn
    /// moves on.
    pub fn spawn_turn_watchdog(&self, code: &str) {
        let state = match self.get_state(code) {
            Some(s) => s,
            None => return,
        };
        if state.phase != GamePhase::Playing {
            return;
        }
        let cp = state.current_player;
        if cp >= state.players.len() || state.players[cp].is_bot || state.players[cp].finished {
            return;
        }
        let limit = state.play_limit_secs.max(1);
        let seq = state.turn_seq;
        let mgr = self.clone();
        let code_owned = code.to_string();
        // Fire-and-forget: must NOT block the bot-turn driver for the whole
        // play limit. `turn_seq` mismatch cancels a stale timer.
        tokio::spawn(async move {
            mgr.run_watchdog(code_owned, seq, limit).await;
        });
    }

    /// The watchdog body, kept separate from `spawn_turn_watchdog` so the
    /// caller (async context) drives it with a direct `.await` — no
    /// `tokio::spawn` needed (and no escaping borrows).
    pub async fn run_watchdog(&self, code: String, seq: u32, limit_secs: u32) {
            tokio::time::sleep(std::time::Duration::from_secs(limit_secs as u64)).await;
            let state = match self.get_state(&code) {
                Some(s) => s,
                None => return,
            };
            if state.phase != GamePhase::Playing || state.turn_seq != seq {
                return;
            }
            let cp = state.current_player;
            if cp >= state.players.len() || state.players[cp].is_bot || state.players[cp].finished {
                return;
            }
            let move_ = crate::game::rules::idle_auto_move(&state, cp);
            let mut engine = crate::game::engine::GameEngine::new(state);
            match &move_ {
                Some(cards) => {
                    engine.apply_play(cp, cards);
                }
                None => {
                    engine.apply_pass(cp);
                }
            }
            let mut new_state = engine.state().clone();
            new_state.log.push(format!(
                "{} hit the time limit — auto-move {}",
                new_state.players[cp].name,
                move_.as_ref().map(|c| c.join(" ")).unwrap_or_else(|| "pass".to_string())
            ));
            self.update_state(&code, new_state.clone());
            self.broadcast(&code, ServerMsg::State { state: new_state.clone() });
            self.process_bot_turns_delayed(&code).await;
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
            // Continuation round: there is no three-discard, the game went
            // straight to Playing — drive bot turns directly.
            None => {
                self.enter_playing(code).await;
                return;
            }
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

        let state = match self.get_state(code) {
            Some(s) => s,
            None => return,
        };

        if state.phase == GamePhase::Playing {
            self.enter_playing(code).await;
        }
    }

    /// Shared "game is live" entry point: log who leads, broadcast, and hand
    /// over to the bot-turn driver. Used both after the three-discard phase
    /// (round 1) and directly for continuation rounds (round 2+).
    async fn enter_playing(&self, code: &str) {
        let mut state = match self.get_state(code) {
            Some(s) if s.phase == GamePhase::Playing => s,
            _ => return,
        };
        let lead = state.players[state.current_player].name.clone();
        if state.round > 1 {
            state.log.push(format!("Round {}! {} leads first trick.", state.round, lead));
        } else {
            state.log.push(format!("Game starts! {} leads first trick.", lead));
        }
        self.update_state(code, state.clone());
        self.broadcast(code, ServerMsg::State { state: state.clone() });
        self.process_bot_turns_delayed(code).await;
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
        self.spawn_turn_watchdog(code);
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
    fn test_rejoin_seat_still_connected_reports_in_use() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        let (joined_msg, _) = manager.join_room(&code, "Bob".to_string()).unwrap();
        let token = match &joined_msg {
            ServerMsg::Joined { token, .. } => token.clone(),
            _ => panic!("Expected Joined"),
        };
        // Bob's seat is still CONNECTED (he never left) — a second window
        // holding the same token must get the "in use" error, not the
        // generic "gone" one. The client uses that string to keep the
        // session alive and hint "close your other window".
        let err = manager.rejoin_room(&code, "Bob", &token).unwrap_err();
        assert!(err.contains("in use"), "expected in-use error, got: {err}");
        // The seat must be untouched: still connected, token intact.
        let room = manager.get_room(&code).unwrap();
        let seat = room.players.iter().find(|p| p.token.as_deref() == Some(&token)).unwrap();
        assert!(seat.connected);
    }

    // --- check_room (read-only rejoin probe) --------------------------------

    #[test]
    fn test_check_room_not_found() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (found, rejoinable) = manager.check_room("NOPE12", "token");
        assert!(!found);
        assert!(!rejoinable);
    }

    #[test]
    fn test_check_room_found_not_rejoinable() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        manager.join_room(&code, "Bob".to_string()).unwrap();
        // Room exists, but nobody is disconnected -> found=true, rejoinable=false.
        let (found, rejoinable) = manager.check_room(&code, "wrong-token");
        assert!(found);
        assert!(!rejoinable);
    }

    #[test]
    fn test_check_room_found_and_rejoinable() {
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

        let (found, rejoinable) = manager.check_room(&code, &token);
        assert!(found);
        assert!(rejoinable);

        // READ-ONLY: the probe must NOT have claimed the seat. A real rejoin
        // right after the probe still succeeds.
        let room = manager.get_room(&code).unwrap();
        assert!(room.disconnected_players.iter().any(|(_, t, _)| t == &token));
    }

    #[test]
    fn test_check_room_case_insensitive_code() {
        let manager = RoomManager::new(6, 2500, 30, 15);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        let lower = code.to_lowercase();
        let (found, _) = manager.check_room(&lower, "x");
        assert!(found);
    }

    #[test]
    fn test_check_room_expires_stale_seat() {
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

        // Expire the disconnected entry in-place so the probe's cleanup
        // removes it, matching what a real rejoin would see.
        manager.expire_disconnected_player(&code, &token);

        let (found, rejoinable) = manager.check_room(&code, &token);
        assert!(found);
        assert!(!rejoinable);
    }

    #[test]
    fn test_check_room_after_room_removed() {
        let manager = RoomManager::new(6, 2500, 0, 0);
        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        // Orphaned on creator leave (timeout 0) -> room removed.
        manager.leave_room(&code, 0);
        assert_eq!(manager.room_count(), 0);

        let (found, rejoinable) = manager.check_room(&code, "token");
        assert!(!found);
        assert!(!rejoinable);
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
    fn test_reaper_drops_orphaned_rooms() {
        // Short orphan timeout so the room is already "mature" by the time the
        // reaper runs. Two humans leave a private game: the seat becomes a
        // bot, last_human_disconnect_at is set, and no human stays connected.
        let manager = RoomManager::new(6, 2500, 1, 15);

        let (code, _, _, _) = manager.create_room("Alice".to_string(), false);
        manager.join_room(&code, "Bob".to_string()).unwrap();
        manager.ready_player(&code, 1, true).unwrap();
        manager.start_game(&code, 0).unwrap();

        manager.leave_room(&code, 1); // Bob -> bot, Alice still connected
        assert_eq!(manager.room_count(), 1);
        manager.leave_room(&code, 0); // Alice -> bot, room now orphaned

        // Not removed *at* the disconnect (timer is 0s old) — that's the bug
        // the reaper exists to fix.
        assert_eq!(manager.room_count(), 1);

        // Let the orphan timer mature (timeout = 1s).
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let dropped = manager.reap_orphaned_rooms();
        assert_eq!(dropped, 1);
        assert_eq!(manager.room_count(), 0);
        assert_eq!(manager.get_state(&code), None);
    }

    #[test]
    fn test_reaper_keeps_room_with_connected_human() {
        // One human still connected -> not orphaned, reaper must not touch it.
        let manager = RoomManager::new(6, 2500, 1, 15);

        let (code, _, _, _) = manager.create_room("Alice".to_string(), false);
        manager.join_room(&code, "Bob".to_string()).unwrap();
        manager.ready_player(&code, 1, true).unwrap();
        manager.start_game(&code, 0).unwrap();

        manager.leave_room(&code, 1); // Bob -> bot, Alice still connected
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let dropped = manager.reap_orphaned_rooms();
        assert_eq!(dropped, 0);
        assert_eq!(manager.room_count(), 1);
        assert!(manager.get_state(&code).is_some());
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
    fn test_remove_player_lobby_renumbers_all_parallel_arrays() {
        // MAJOR: creator leaves a public lobby. The room is NOT dissolved (only
        // a PUBLIC creator dissolves... actually public creators DO dissolve,
        // so use a PRIVATE room + non-creator leave first, then creator).
        // Non-creator Bob leaves; seat 1 is removed and Dave takes the freed
        // slot. EVERY parallel array (players, state.players, ready,
        // state.ready, scores, total_scores) must stay the same length and
        // seat ids must stay dense 0..n-1.
        let manager = RoomManager::new(6, 2500, 30, 15);

        let (code, _, _, _) = manager.create_room("Alice".to_string(), false);
        manager.join_room(&code, "Bob".to_string()).unwrap();
        manager.join_room(&code, "Charlie".to_string()).unwrap();

        manager.remove_player(&code, 1); // Bob (seat 1) leaves

        manager.join_room(&code, "Dave".to_string()).unwrap();

        let room = manager.get_room(&code).unwrap();
        // Same length across every player-shaped vector.
        assert_eq!(room.players.len(), room.state.players.len());
        assert_eq!(room.players.len(), room.ready.len());
        assert_eq!(room.players.len(), room.state.ready.len());
        assert_eq!(room.players.len(), room.state.scores.len());
        // Seat ids dense 0..n-1 in both vectors, and the two agree.
        let mut ids: Vec<usize> = room.players.iter().map(|p| p.id).collect();
        ids.sort();
        assert_eq!(ids, vec![0, 1, 2]);
        let mut sids: Vec<usize> = room.state.players.iter().map(|p| p.id).collect();
        sids.sort();
        assert_eq!(sids, vec![0, 1, 2]);
        assert!(room.players.iter().zip(room.state.players.iter()).all(|(a, b)| a.id == b.id));
        // Dave got the freed slot, and his ready bit is the NEW one (false),
        // not a stale leftover from a previous seat.
        let dave = room.players.iter().find(|p| p.name == "Dave").unwrap();
        assert!(!room.ready[dave.id]);
        assert!(!room.state.ready[dave.id]);

        // And the game must actually be startable afterwards.
        for p in room.players.iter() {
            if !p.is_bot {
                manager.ready_player(&code, p.id, true).unwrap();
            }
        }
        let creator = room.players.iter().find(|p| p.is_creator).unwrap();
        assert!(manager.start_game(&code, creator.id).is_ok());
    }

    #[test]
    fn test_remove_player_creator_transfer_reaches_state_players() {
        // MAJOR: when the creator leaves the lobby, the crown moves to the
        // next human. The client reads state.players (personalised broadcast),
        // so the flag MUST be set there too — not only on room.players.
        let manager = RoomManager::new(6, 2500, 30, 15);

        let (code, _, _, _) = manager.create_room("Alice".to_string(), false);
        manager.join_room(&code, "Bob".to_string()).unwrap();

        manager.remove_player(&code, 0); // creator Alice leaves (private room)

        let room = manager.get_room(&code).unwrap();
        assert_eq!(room.players.len(), 1);
        let bob_state = room.state.players.iter().find(|p| p.name == "Bob").unwrap();
        assert!(bob_state.is_creator, "crown must be visible in state.players (what the client renders)");
        // Ready arrays must have been compacted with the seat.
        assert_eq!(room.state.ready.len(), 1);
        assert_eq!(room.state.scores.len(), 1);
        assert!(room.state.ready[0]); // Bob keeps a usable ready state
    }

    #[test]
    fn test_lobby_disconnect_reap_frees_seat_without_stealing() {
        // MAJOR: Bob's lobby seat is reaped after the disconnect timeout and
        // Dave takes the freed slot. Bob's token is still inside
        // disconnected_players (5-min rejoin window). A late rejoin by Bob
        // must FAIL — restore_seat(1) would otherwise hijack Dave's seat.
        let manager = RoomManager::new(6, 2500, 30, 1); // 1s lobby timeout

        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        let bob_token = match manager.join_room(&code, "Bob".to_string()).unwrap() {
            (ServerMsg::Joined { token, .. }, _) => token,
            _ => panic!("Expected Joined"),
        };
        manager.leave_room(&code, 1); // socket drop: seat marked disconnected
        // Force the lobby reaper NOW (timeout 0 -> everything with a
        // disconnect_time gets reaped instantly).
        let reaped = manager
            .rooms_ref()
            .get_mut(&code)
            .map(|mut r| r.cleanup_disconnected_lobby_players(0))
            .unwrap_or_default();
        assert!(reaped.0.iter().any(|(id, _)| *id == 1), "Bob's seat should be reaped");

        manager.join_room(&code, "Dave".to_string()).unwrap();
        let room = manager.get_room(&code).unwrap();
        let dave = room.players.iter().find(|p| p.name == "Dave").unwrap();

        // Bob's token must not be able to reclaim Dave's seat.
        let rejoin = manager.rejoin_room(&code, "Bob", &bob_token);
        let room2 = manager.get_room(&code).unwrap();
        let dave2 = room2.players.iter().find(|p| p.id == dave.id).unwrap();
        assert_eq!(dave2.name, "Dave", "rejoin must not steal a compacted seat");
        // Either the rejoin failed, or (if a seat was found) it is Bob's OWN
        // still-present seat — never Dave's.
        if let Ok((ServerMsg::Rejoined { player_id, .. }, _)) = rejoin {
            assert_ne!(player_id, dave.id);
        }
    }

    #[test]
    fn test_leave_room_creator_drop_transfers_crown_to_state() {
        // MAJOR (same cluster, socket-drop variant): the creator CLOSES THE
        // TAB (the most common mobile disconnect) in a private lobby. The
        // crown must move to the next human in state.players too — otherwise,
        // once the 15s reaper removes the creator's seat, NO seat has
        // is_creator: the client renders no Start button for anyone and
        // start_game() rejects every caller -> room stuck forever.
        let manager = RoomManager::new(6, 2500, 30, 1); // 1s lobby timeout

        let (code, _, _, _) = manager.create_room("Alice".to_string(), false); // private: no dissolve
        manager.join_room(&code, "Bob".to_string()).unwrap();

        manager.leave_room(&code, 0); // creator's socket drops

        // Force the lobby reaper (timeout 0 -> instant).
        let reaped = manager
            .rooms_ref()
            .get_mut(&code)
            .map(|mut r| r.cleanup_disconnected_lobby_players(0))
            .unwrap_or_default();
        assert!(reaped.0.iter().any(|(_, n)| n == "Alice"), "creator seat should be reaped");

        let room = manager.get_room(&code).unwrap();
        let bob_state = room.state.players.iter().find(|p| p.name == "Bob").unwrap();
        assert!(
            bob_state.is_creator,
            "crown must reach state.players (what the client renders) after a creator socket drop"
        );
        // And the room must actually be startable afterwards.
        assert!(manager.start_game(&code, 0).is_ok());
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

    #[test]
    fn test_remove_player_mid_game_becomes_bot() {
        // The menu drawer's Leave button sends an explicit LeaveRoom ->
        // remove_player. Mid-game that must convert the seat to a bot so the
        // round continues (regression: this used to return None, stranding
        // everyone on the leaver's turn).
        let manager = RoomManager::new(6, 2500, 30, 15);

        let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
        manager.join_room(&code, "Bob".to_string()).unwrap();
        manager.ready_player(&code, 1, true).unwrap();
        manager.start_game(&code, 0).unwrap();

        let result = manager.remove_player(&code, 1);
        assert!(result.is_some());

        let room = manager.get_room(&code).unwrap();
        assert_eq!(room.players.len(), 4);
        let leaver = room.players.iter().find(|p| p.id == 1).unwrap();
        assert!(leaver.is_bot);
        assert_eq!(leaver.name, "Bot (Bob)");
        assert!(leaver.connected);
        let state_leaver = room.state.players.iter().find(|p| p.id == 1).unwrap();
        assert!(state_leaver.is_bot);
        assert_eq!(state_leaver.name, "Bot (Bob)");
    }
}
