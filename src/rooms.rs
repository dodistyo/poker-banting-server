//! Room management, multi-pod ready.
//!
//! Every state mutation goes through [`RoomManager::locked_step`], an atomic
//! read-modify-write under a per-room advisory lock held by the
//! [`RoomStore`](crate::store::RoomStore). The manager never touches a
//! `DashMap` or Redis directly — it holds an `Arc<dyn RoomStore>` — so the
//! same code runs against in-memory (single instance) or Redis (multi-pod).
//!
//! Timing (bot turns, the three-discard cascade, the play-limit watchdog) is
//! NOT driven by sleeping tasks. Instead each room carries a persistent
//! [`DriverState`](crate::game::state::DriverState) whose deadlines are
//! wall-clock unix millis. The stateless [`driver`](crate::driver) polls every
//! room and fires whatever is due through this same locked path. A pod that
//! dies mid-game just drops its lock; another pod's next tick re-acquires it
//! and continues from the persisted deadline. No room can be stranded.

use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;

use axum::extract::ws::Message;
use tokio::sync::mpsc::UnboundedSender;

use crate::game::engine::GameEngine;
use crate::game::rules::{
    idle_auto_move, process_one_bot_turn, process_three_discard, skip_finished,
};
use crate::game::state::{GamePhase, GameState, PendingAction, Room};
use crate::protocol::{personalise_for_viewer, ServerMsg};
use crate::store::{MutateOut, RoomLock, RoomStore};

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
    /// The shared room-state backend (in-memory or Redis). All CRUD goes
    /// through this; the manager never owns a `DashMap` of rooms itself.
    store: Arc<dyn RoomStore>,
    /// (player_id, sender) per room code: every connection knows which seat it
    /// belongs to, so state broadcasts can be personalized per viewer (card
    /// privacy). This is LOCAL to the pod — sessions live on the pod that
    /// accepted the WebSocket, and cross-pod fan-out re-delivers to each pod's
    /// own local sessions.
    sessions: Arc<DashMap<String, Vec<(usize, SessionSender)>>>,
    /// This pod's id — stamped into cross-pod publishes so receivers can drop
    /// their own echo.
    pod_id: String,
    code_length: usize,
    bot_turn_delay_ms: u64,
    orphan_timeout_secs: u64,
    lobby_disconnect_timeout_secs: u64,
}

impl RoomManager {
    pub fn new(
        store: Arc<dyn RoomStore>,
        pod_id: String,
        code_length: usize,
        bot_turn_delay_ms: u64,
        orphan_timeout_secs: u64,
        lobby_disconnect_timeout_secs: u64,
    ) -> Self {
        RoomManager {
            store,
            sessions: Arc::new(DashMap::new()),
            pod_id,
            code_length,
            bot_turn_delay_ms,
            orphan_timeout_secs,
            lobby_disconnect_timeout_secs,
        }
    }

    // ------------------------------------------------------------------
    // Local session registry (per-pod connections)
    // ------------------------------------------------------------------

    pub fn add_session(&self, code: String, player_id: usize, sender: SessionSender) {
        self.sessions
            .entry(code)
            .or_insert_with(Vec::new)
            .push((player_id, sender));
    }

    pub fn remove_session(&self, code: &str, sender: &Arc<UnboundedSender<Message>>) {
        if let Some(mut entry) = self.sessions.get_mut(code) {
            entry.retain(|s| !Arc::ptr_eq(&s.1, sender));
        }
        // Drop the entry guard BEFORE touching the map again (re-locking the
        // shard while holding a get_mut() guard can deadlock when a writer is
        // queued). remove_if is atomic: it removes the key only if it is still
        // empty, so a concurrent add_session can't be clobbered.
        self.sessions.remove_if(code, |_, v| v.is_empty());
    }

    /// Apply a lobby-compaction renumbering to this pod's session map so each
    /// survivor's stored seat follows them into their new id. (Purely local —
    /// other pods apply their own map when they receive `SeatChanged` via the
    /// pub/sub subscriber.)
    pub fn apply_renumbering(&self, code: &str, renumbered: &[(usize, usize)]) {
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
    }

    // ------------------------------------------------------------------
    // Broadcast / emit
    // ------------------------------------------------------------------

    /// Deliver a message to THIS pod's local sessions. State-carrying messages
    /// are personalized per viewer (card privacy). Sync — safe to call from
    /// the pub/sub subscriber.
    pub fn broadcast_local(&self, code: &str, msg: &ServerMsg) {
        if matches!(msg, ServerMsg::State { .. }) {
            if let Some(entry) = self.sessions.get(code) {
                let senders: Vec<_> = entry.iter().map(|e| (e.0, e.1.clone())).collect();
                for (pid, sender) in senders {
                    let json = personalise_for_viewer(msg, pid);
                    let _ = sender.send(Message::Text(json.into()));
                }
            }
            return;
        }

        let json = match serde_json::to_string(msg) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("Failed to serialize message: {e}");
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

    /// Broadcast to local sessions AND publish to the other pods (Redis mode;
    /// a no-op publish in memory mode). Awaited by `locked_step` after a
    /// mutation commits.
    async fn emit(&self, code: &str, msg: &ServerMsg) {
        self.broadcast_local(code, msg);
        self.store.publish(code, &self.pod_id, msg).await;
    }

    // ------------------------------------------------------------------
    // The locked read-modify-write
    // ------------------------------------------------------------------

    /// Run an atomic mutation on a room under its advisory lock.
    ///
    /// The closure mutates `&mut Room` and returns:
    /// - `Ok(out)` → the mutation committed: the room is saved (or deleted if
    ///   `out.removed`), `out.renumbered` is applied to the local session
    ///   map, and every `out.broadcast` message is emitted (local + other
    ///   pods) in order.
    /// - `Err(msg)` → the mutation was rejected (validation failed): nothing
    ///   is saved, the lock is released, and `msg` is returned to the caller.
    ///
    /// If the lock can't be claimed (another task/pod is mid-mutation) the
    /// step retries briefly — the lock is only held for the duration of a
    /// synchronous mutation + save, so contention is short-lived. A truly
    /// empty `out` (no broadcast, no reply, no renumber, no delete) is treated
    /// as a no-op and neither saved nor emitted.
    pub async fn locked_step(
        &self,
        code: &str,
        mutate: impl FnOnce(&mut Room) -> Result<MutateOut, String>,
    ) -> Result<MutateOut, String> {
        // Claim the lock with a short retry loop.
        let lock = self.acquire_lock(code).await;
        self.with_lock(code, lock, mutate).await
    }

    /// Non-blocking variant of [`Self::locked_step`]: if the room's lock is
    /// held by another actor right now (a concurrent human move, or another
    /// pod's driver), fail immediately instead of waiting. The stateless
    /// driver uses this — its deadlines are wall-clock, so the next tick
    /// re-fires whatever this tick skipped.
    pub async fn try_locked_step(
        &self,
        code: &str,
        mutate: impl FnOnce(&mut Room) -> Result<MutateOut, String>,
    ) -> Result<MutateOut, String> {
        match self.store.try_lock(code).await {
            Some(lock) => self.with_lock(code, lock, mutate).await,
            None => Err("room is locked by another actor".to_string()),
        }
    }

    /// Claim a room lock, retrying while another actor holds it. The lock is
    /// only held for a synchronous mutation + save, so a few tens of ms of
    /// waiting always clears.
    async fn acquire_lock(&self, code: &str) -> RoomLock {
        loop {
            if let Some(l) = self.store.try_lock(code).await {
                return l;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// The shared read-modify-write core, under an already-held lock.
    async fn with_lock<F: FnOnce(&mut Room) -> Result<MutateOut, String>>(
        &self,
        code: &str,
        lock: RoomLock,
        mutate: F,
    ) -> Result<MutateOut, String> {
        let mut room = match self.store.get(code).await {
            Some(r) => r,
            None => {
                self.store.unlock(code, lock).await;
                return Err("Room not found".to_string());
            }
        };
        // Snapshot for the post-mutation diff: a mutation may change room
        // state WITHOUT producing a broadcast (expire_disconnected,
        // finalize_game, a direct state edit) — the diff, not the
        // broadcast, decides whether a store write is needed. A true no-op
        // (e.g. a driver tick with nothing due) writes nothing.
        let before = room.clone();

        let result = mutate(&mut room);

        match result {
            Ok(out) => {
                if out.removed {
                    self.store.delete(code).await;
                    self.sessions.remove(code);
                } else if room != before {
                    self.store.save(code, &room).await;
                }
                self.apply_renumbering(code, &out.renumbered);
                for msg in &out.broadcast {
                    self.emit(code, msg).await;
                }
                self.store.unlock(code, lock).await;
                Ok(out)
            }
            Err(e) => {
                self.store.unlock(code, lock).await;
                Err(e)
            }
        }
    }

    // ------------------------------------------------------------------
    // Room lifecycle
    // ------------------------------------------------------------------

    pub async fn create_room(
        &self,
        host_name: String,
        is_public: bool,
    ) -> (String, usize, ServerMsg) {
        // A brand-new code can't contend with an existing lock. On the
        // astronomically-rare collision, bump the code and retry a few times
        // rather than failing the user.
        let mut code = self.generate_code();
        let mut attempt = 0;
        let room = loop {
            let token = Self::generate_token();
            let mut r = Room::new(code.clone(), host_name.clone(), token);
            r.is_public = is_public;
            match self.store.create(&code, &r).await {
                Ok(()) => break r,
                Err(e) => {
                    attempt += 1;
                    if attempt > 8 {
                        return (
                            code,
                            0,
                            ServerMsg::Error {
                                message: format!("failed to allocate a room code: {e}"),
                            },
                        );
                    }
                    code = self.generate_code();
                }
            }
        };

        // The creator is seat 0 by construction — `Room::new` pushes
        // `host_name` as the first player, and `create_room` is the only
        // call site of `Room::new`.
        let code = room.code.clone();
        let state = room.state.clone();
        let token = room.players[0].token.clone().unwrap();
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
        )
    }

    pub async fn join_room(&self, code: &str, name: String) -> Result<ServerMsg, String> {
        let code = code.to_uppercase();
        let token = Self::generate_token();
        let reply_code = code.clone();
        let out = self
            .locked_step(&code, move |room| {
                if room.state.phase != GamePhase::Lobby {
                    return Err("Game already started".to_string());
                }
                if room.players.len() >= 4 {
                    return Err("Room is full".to_string());
                }
                let player_id = room.add_player(name, token.clone())?;
                let state = room.state.clone();
                let player_name = state
                    .players
                    .iter()
                    .find(|p| p.id == player_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| format!("Player {player_id}"));

                let mut out = MutateOut::default();
                out.broadcast.push(ServerMsg::PlayerJoined {
                    player_id,
                    name: player_name,
                });
                out.broadcast.push(ServerMsg::State {
                    state: state.clone(),
                });
                out.reply = Some(ServerMsg::Joined {
                    player_id,
                    state,
                    code: reply_code,
                    token,
                });
                Ok(out)
            })
            .await?;
        Ok(out.reply.unwrap())
    }

    const REJOIN_TIMEOUT_SECS: u64 = 300;

    pub async fn rejoin_room(
        &self,
        code: &str,
        name: &str,
        token: &str,
    ) -> Result<ServerMsg, String> {
        let code = code.to_uppercase();
        let reply_code = code.clone();
        let out = self
            .locked_step(&code, move |room| {
                room.cleanup_expired_disconnected(Self::REJOIN_TIMEOUT_SECS);

                if let Some(pos) = room.disconnected_players.iter().position(|(_, t, _)| t == token)
                {
                    let (seat_id, _, _) = room.disconnected_players.remove(pos);
                    room.restore_seat(seat_id, name, token);
                    room.human_reconnected();

                    let state = room.state.clone();
                    let mut out = MutateOut::default();
                    out.broadcast.push(ServerMsg::PlayerJoined {
                        player_id: seat_id,
                        name: name.to_string(),
                    });
                    // Raw (un-masked) state: the caller serializes it through
                    // personalise_for_viewer, which adds handCount AND masks
                    // other hands in one JSON pass.
                    out.reply = Some(ServerMsg::Rejoined {
                        player_id: seat_id,
                        state,
                        code: reply_code,
                        token: token.to_string(),
                    });
                    return Ok(out);
                }

                // Not a disconnected seat. Distinguish "another live connection
                // holds this seat" (same token still connected — second tab /
                // PWA + mobile) from a genuinely gone seat. The client shows a
                // "close your other window" hint in the first case and must NOT
                // drop the session.
                let seat_in_use = room
                    .players
                    .iter()
                    .any(|p| p.token.as_deref() == Some(token) && p.connected);

                Err(if seat_in_use {
                    "Seat already in use by another window".to_string()
                } else {
                    "No matching disconnected player found".to_string()
                })
            })
            .await?;
        Ok(out.reply.unwrap())
    }

    pub async fn leave_room(&self, code: &str, player_id: usize) -> Option<ServerMsg> {
        let code = code.to_uppercase();
        let lobby_timeout = self.lobby_disconnect_timeout_secs;
        let orphan_timeout = self.orphan_timeout_secs;
        let out = self
            .locked_step(&code, move |room| {
                let player = room
                    .players
                    .iter()
                    .find(|p| p.id == player_id)
                    .cloned()
                    .ok_or_else(|| "Player not found".to_string())?;
                let (player_name, is_bot, token, is_creator) = (
                    player.name.clone(),
                    player.is_bot,
                    player.token.clone(),
                    player.is_creator,
                );

                // Public room, lobby, creator drops the tab: dissolve.
                if room.state.phase == GamePhase::Lobby && is_creator && !is_bot && room.is_public {
                    let mut out = MutateOut::default();
                    out.removed = true;
                    out.broadcast.push(ServerMsg::PlayerLeft {
                        player_id,
                        name: player_name,
                    });
                    return Ok(out);
                }

                if !is_bot {
                    if let Some(t) = token {
                        room.add_disconnected_player(player_id, t);
                    }

                    if room.state.phase == GamePhase::Lobby {
                        if let Some(player) = room.players.iter_mut().find(|p| p.id == player_id) {
                            player.connected = false;
                            player.disconnect_time = Some(crate::game::state::now_secs());
                        }
                        if is_creator {
                            room.transfer_crown_from(player_id);
                        }
                        let mut out = MutateOut::default();
                        out.broadcast.push(ServerMsg::PlayerLeft {
                            player_id,
                            name: player_name.clone(),
                        });
                        let (removed, renumbered) =
                            room.cleanup_disconnected_lobby_players(lobby_timeout);
                        for (rid, rname) in &removed {
                            out.broadcast.push(ServerMsg::PlayerLeft {
                                player_id: *rid,
                                name: rname.clone(),
                            });
                        }
                        room.record_human_disconnect();
                        if room.is_orphaned(orphan_timeout) {
                            out.removed = true;
                            return Ok(out);
                        }
                        out.renumbered = renumbered;
                        out.broadcast.push(ServerMsg::State {
                            state: room.state.clone(),
                        });
                        out.reply = Some(ServerMsg::PlayerLeft {
                            player_id,
                            name: player_name,
                        });
                        return Ok(out);
                    }

                    // Mid-game socket drop: become a bot so the round continues.
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
                    let orphaned = room.is_orphaned(orphan_timeout);
                    let mut out = MutateOut::default();
                    if orphaned {
                        out.removed = true;
                    } else {
                        out.broadcast.push(ServerMsg::PlayerLeft {
                            player_id,
                            name: player_name.clone(),
                        });
                        out.broadcast.push(ServerMsg::State { state });
                    }
                    out.reply = Some(ServerMsg::PlayerLeft {
                        player_id,
                        name: player_name,
                    });
                    return Ok(out);
                }

                // Bot (or a bot-ified seat) leaving: just mark disconnected.
                if let Some(player) = room.players.iter_mut().find(|p| p.id == player_id) {
                    player.connected = false;
                    player.disconnect_time = Some(crate::game::state::now_secs());
                }
                let mut out = MutateOut::default();
                out.reply = Some(ServerMsg::PlayerLeft {
                    player_id,
                    name: player_name,
                });
                Ok(out)
            })
            .await
            .ok()?;
        out.reply
    }

    pub async fn remove_player(&self, code: &str, player_id: usize) -> Option<ServerMsg> {
        let code = code.to_uppercase();
        let orphan_timeout = self.orphan_timeout_secs;
        let out = self
            .locked_step(&code, move |room| {
                let player = room
                    .players
                    .iter()
                    .find(|p| p.id == player_id)
                    .cloned()
                    .ok_or_else(|| "Player not found".to_string())?;
                let (player_name, is_bot, is_creator) = (
                    player.name.clone(),
                    player.is_bot,
                    player.is_creator,
                );
                if is_bot {
                    // No reply, no change.
                    return Ok(MutateOut::default());
                }

                if room.state.phase == GamePhase::Lobby {
                    if is_creator && room.is_public {
                        let mut out = MutateOut::default();
                        out.removed = true;
                        out.reply = Some(ServerMsg::PlayerLeft {
                            player_id,
                            name: player_name,
                        });
                        return Ok(out);
                    }
                    if is_creator {
                        room.transfer_crown_from(player_id);
                    }
                    // An explicit lobby Leave is final: drop the leaver's
                    // pending-rejoin entry so a late Rejoin can't restore onto a
                    // compacted slot (seat hijack).
                    if let Some(t) = room
                        .players
                        .iter()
                        .find(|p| p.id == player_id)
                        .and_then(|p| p.token.clone())
                    {
                        room.disconnected_players.retain(|(_, tok, _)| tok != &t);
                    }
                    let renumbered = room.remove_lobby_seat(player_id);
                    let mut out = MutateOut::default();
                    out.broadcast.push(ServerMsg::PlayerLeft {
                        player_id,
                        name: player_name.clone(),
                    });
                    out.broadcast.push(ServerMsg::SeatChanged {
                        renumbered: renumbered.clone(),
                    });
                    out.broadcast.push(ServerMsg::State {
                        state: room.state.clone(),
                    });
                    out.renumbered = renumbered;
                    out.reply = Some(ServerMsg::PlayerLeft {
                        player_id,
                        name: player_name,
                    });
                    return Ok(out);
                }

                // Mid-game explicit leave: become a bot so the game continues.
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
                let orphaned = room.is_orphaned(orphan_timeout);
                let mut out = MutateOut::default();
                if orphaned {
                    out.removed = true;
                } else {
                    out.broadcast.push(ServerMsg::PlayerLeft {
                        player_id,
                        name: player_name.clone(),
                    });
                    out.broadcast.push(ServerMsg::State { state });
                }
                out.reply = Some(ServerMsg::PlayerLeft {
                    player_id,
                    name: player_name,
                });
                Ok(out)
            })
            .await
            .ok()?;
        out.reply
    }

    /// Read-only rejoin probe (the client's "is my saved session still live?"
    /// check). Runs the same expiry cleanup as rejoin so the answer matches
    /// what a real rejoin would do, but never claims a seat.
    pub async fn check_room(&self, code: &str, token: &str) -> (bool, bool) {
        let code = code.to_uppercase();
        let lock = match self.store.try_lock(&code).await {
            Some(l) => l,
            None => return (false, false),
        };
        let mut room = match self.store.get(&code).await {
            Some(r) => r,
            None => {
                self.store.unlock(&code, lock).await;
                return (false, false);
            }
        };
        room.cleanup_expired_disconnected(Self::REJOIN_TIMEOUT_SECS);
        let rejoinable = room
            .disconnected_players
            .iter()
            .any(|(_, t, _)| t == token);
        self.store.save(&code, &room).await;
        self.store.unlock(&code, lock).await;
        (true, rejoinable)
    }

    pub async fn get_room(&self, code: &str) -> Option<Room> {
        self.store.get(code).await
    }

    #[cfg(test)]
    pub async fn expire_disconnected_player(&self, code: &str, token: &str) {
        let code = code.to_uppercase();
        let _ = self
            .locked_step(&code, move |room| {
                for entry in &mut room.disconnected_players {
                    if entry.1 == token {
                        entry.2 = crate::game::state::now_secs().saturating_sub(1);
                    }
                }
                room.cleanup_expired_disconnected(0);
                Ok(MutateOut::default())
            })
            .await;
    }

    pub async fn get_state(&self, code: &str) -> Option<GameState> {
        self.store.get(code).await.map(|r| r.state)
    }

    /// Test-only: force the lobby disconnect reaper to run now (timeout 0).
    /// Replaces the old `rooms_ref()` map-poke. Returns the removed seats.
    #[cfg(test)]
    pub async fn force_lobby_reap(&self, code: &str) -> Vec<(usize, String)> {
        let code = code.to_uppercase();
        let removed = std::sync::Arc::new(std::cell::RefCell::new(Vec::new()));
        let removed_for_map = removed.clone();
        self.locked_step(&code, move |room| {
            let (removed_seats, renumbered) = room.cleanup_disconnected_lobby_players(0);
            removed.borrow_mut().extend(removed_seats);
            let mut out = MutateOut::default();
            out.renumbered = renumbered;
            out.broadcast.push(ServerMsg::State {
                state: room.state.clone(),
            });
            Ok(out)
        })
        .await
        .map(|_| removed_for_map.borrow().clone())
        .unwrap_or_default()
    }

    pub async fn room_count(&self) -> usize {
        self.store.codes().await.len()
    }

    /// All known room codes — the driver tick loop scans this each interval.
    pub async fn room_codes(&self) -> Vec<String> {
        self.store.codes().await
    }

    /// Drop every room with no connected human whose last-disconnect timer has
    /// matured, plus its dangling local session senders.
    pub async fn reap_orphaned_rooms(&self) -> usize {
        let codes = self.store.codes().await;
        let timeout = self.orphan_timeout_secs;
        let mut dropped = 0usize;
        for code in codes {
            // Skip rooms another task is mid-mutation on (retry next tick).
            let lock = match self.store.try_lock(&code).await {
                Some(l) => l,
                None => continue,
            };
            let room = match self.store.get(&code).await {
                Some(r) => r,
                None => {
                    self.store.unlock(&code, lock).await;
                    continue;
                }
            };
            let orphaned = room.is_orphaned(timeout);
            if orphaned {
                self.store.delete(&code).await;
                self.sessions.remove(&code);
                dropped += 1;
            }
            self.store.unlock(&code, lock).await;
        }
        dropped
    }

    pub async fn list_public_rooms(&self) -> Vec<PublicRoomSummary> {
        let codes = self.store.codes().await;
        let mut result = Vec::new();
        for code in codes {
            let Some(room) = self.store.get(&code).await else {
                continue;
            };
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

    pub async fn set_room_settings(
        &self,
        code: &str,
        player_id: usize,
        play_limit_secs: Option<u32>,
        winning_point: Option<u32>,
    ) -> Result<ServerMsg, String> {
        let code = code.to_uppercase();
        let out = self
            .locked_step(&code, move |room| {
                if !room.players.iter().any(|p| p.id == player_id && p.is_creator) {
                    return Err("Only the host can change room settings".to_string());
                }
                if room.started {
                    return Err(
                        "Room settings are locked after the game has started".to_string(),
                    );
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
                let msg = ServerMsg::RoomSettings {
                    play_limit_secs: play_limit,
                    winning_point: point,
                };
                let mut out = MutateOut::default();
                out.broadcast.push(msg.clone());
                out.reply = Some(msg);
                Ok(out)
            })
            .await?;
        Ok(out.reply.unwrap())
    }

    pub async fn ready_player(
        &self,
        code: &str,
        player_id: usize,
        ready: bool,
    ) -> Result<ServerMsg, String> {
        let code = code.to_uppercase();
        let out = self
            .locked_step(&code, move |room| {
                room.set_ready(player_id, ready)?;
                let player_name = room
                    .players
                    .iter()
                    .find(|p| p.id == player_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                let msg = ServerMsg::PlayerReady {
                    player_id,
                    name: player_name,
                    ready,
                };
                let mut out = MutateOut::default();
                out.broadcast.push(msg.clone());
                out.reply = Some(msg);
                Ok(out)
            })
            .await?;
        Ok(out.reply.unwrap())
    }

    pub async fn start_game(&self, code: &str, player_id: usize) -> Result<ServerMsg, String> {
        let code = code.to_uppercase();
        let out = self
            .locked_step(&code, move |room| {
                if !room.players.iter().any(|p| p.id == player_id && p.is_creator) {
                    return Err("Only the room creator can start the game".to_string());
                }
                if room.state.game_winner.is_some() {
                    return Err("Match already won — a new game is locked".to_string());
                }
                if room.state.phase != GamePhase::Lobby && room.state.phase != GamePhase::GameOver {
                    return Err("Game already started".to_string());
                }
                if room.state.phase == GamePhase::GameOver {
                    for r in room.ready.iter_mut() {
                        *r = true;
                    }
                    for r in room.state.ready.iter_mut() {
                        *r = true;
                    }
                }
                if !room.all_human_ready() {
                    return Err("Not all players are ready".to_string());
                }
                room.start_game();

                // Arm the driver: round 1 goes through the three-discard
                // cascade, a continuation round goes straight to Playing.
                if room.state.phase == GamePhase::ThreeDiscard {
                    room.driver.arm(PendingAction::StepThreeDiscard, 600);
                } else {
                    room.driver.arm(PendingAction::EnterPlaying, 0);
                }

                let state = room.state.clone();
                let mut out = MutateOut::default();
                out.broadcast.push(ServerMsg::GameStarted);
                out.broadcast.push(ServerMsg::State {
                    state: state.clone(),
                });
                out.reply = Some(ServerMsg::State { state });
                Ok(out)
            })
            .await?;
        Ok(out.reply.unwrap())
    }

    // ------------------------------------------------------------------
    // Human moves (the old ws.rs inline driving, now one locked step each)
    // ------------------------------------------------------------------

    /// A human plays cards (`Some`) or passes (`None`). Applies the move
    /// atomically, broadcasts, and arms the driver for whatever comes next
    /// (the next bot's turn, or the play-limit watchdog if a human is up).
    pub async fn apply_human_move(
        &self,
        code: &str,
        player_id: usize,
        cards: Option<Vec<String>>,
    ) -> Result<(), String> {
        let code = code.to_uppercase();
        let delay = self.bot_turn_delay_ms;
        self.locked_step(&code, move |room| {
            let old_seq = room.state.turn_seq;
            let mut engine = GameEngine::new(room.state.clone());
            match &cards {
                Some(cs) => {
                    engine.apply_play(player_id, cs);
                }
                None => {
                    engine.apply_pass(player_id);
                }
            }
            let new_state = engine.state().clone();
            // No-op (invalid / wrong turn): the engine didn't advance the
            // turn, so nothing to persist or drive.
            if new_state.turn_seq == old_seq && new_state.phase == room.state.phase {
                return Ok(MutateOut::default());
            }
            room.state = new_state;

            let st = &room.state;
            if st.phase == GamePhase::Playing {
                let is_bot = st.players.get(st.current_player).map(|p| p.is_bot).unwrap_or(false);
                if is_bot {
                    room.driver.arm(PendingAction::BotTurn, delay);
                } else {
                    room.driver.arm_watchdog(st);
                    room.driver.clear_pending();
                }
            } else {
                room.driver.clear_pending();
            }

            let mut out = MutateOut::default();
            out.broadcast.push(ServerMsg::State {
                state: room.state.clone(),
            });
            Ok(out)
        })
        .await?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // The stateless driver's per-room step
    // ------------------------------------------------------------------

    /// Fire whatever the room's persistent driver timing says is due: a
    /// pending one-shot action (enter-playing / three-discard / bot turn) and
    /// the play-limit watchdog. Runs under the room lock, so it can't race a
    /// human move. No-op (no save, no broadcast) if nothing is due. Called by
    /// the background [`driver`](crate::driver) every tick, for every room —
    /// this is the ONLY thing that advances the game when no human is acting.
    pub async fn drive_room(&self, code: &str) -> bool {
        let code = code.to_uppercase();
        let delay = self.bot_turn_delay_ms;
        let out = match self.try_locked_step(&code, move |room| {
            let mut out = MutateOut::default();
            // Chain immediately-due actions (e.g. the last three-discard flips
            // the phase to Playing, arming an EnterPlaying at 0ms — fire it in
            // the same tick). Bounded to avoid a runaway loop; each fire either
            // advances the state or arms a future-delayed action.
            for _ in 0..8 {
                if room.driver.is_due() {
                    let action = match room.driver.pending.clone() {
                        Some(a) => a,
                        None => break,
                    };
                    self.fire_action(room, action, delay, &mut out);
                } else if room.driver.watchdog_due(&room.state) {
                    self.fire_watchdog(room, delay, &mut out);
                } else {
                    break;
                }
            }
            Ok(out)
        }).await {
            Ok(o) => o,
            Err(_) => return false,
        };
        !out.broadcast.is_empty()
    }

    /// Execute one due pending action, mutating `room` and appending its
    /// broadcasts to `out`. Sync — no I/O, no sleeping (all delays are
    /// re-expressed as deadlines).
    fn fire_action(
        &self,
        room: &mut Room,
        action: PendingAction,
        delay_ms: u64,
        out: &mut MutateOut,
    ) {
        use PendingAction as PA;
        match action {
            PA::EnterPlaying => {
                if room.state.phase != GamePhase::Playing {
                    room.driver.clear_pending();
                    return;
                }
                let lead = room.state.players[room.state.current_player].name.clone();
                if room.state.round > 1 {
                    room.state
                        .log
                        .push(format!("Round {}! {lead} leads first trick.", room.state.round));
                } else {
                    room.state.log.push(format!("Game starts! {lead} leads first trick."));
                }
                out.broadcast.push(ServerMsg::State {
                    state: room.state.clone(),
                });
                self.arm_after_playing(room, delay_ms, out);
            }
            PA::StepThreeDiscard => {
                let next_pid = room
                    .state
                    .three_discard
                    .as_ref()
                    .and_then(|td| td.order.iter().find(|id| !td.discarded[**id]));
                match next_pid {
                    Some(&pid) => {
                        let (player_name, count) = {
                            let st = &room.state;
                            let name = st.players[pid].name.clone();
                            let count = st
                                .three_discard
                                .as_ref()
                                .map(|td| td.player_cards[pid].len())
                                .unwrap_or(0);
                            (name, count)
                        };
                        process_three_discard(&mut room.state, pid);
                        // Privacy: the log is broadcast to every seat — only
                        // the COUNT is public, never the exact 3s.
                        let log_msg = if count > 0 {
                            format!("{player_name} discarded their 3s ({count} 3s)")
                        } else {
                            format!("{player_name} has no 3s")
                        };
                        room.state.log.push(log_msg);
                        out.broadcast.push(ServerMsg::State {
                            state: room.state.clone(),
                        });
                        if room.state.phase == GamePhase::Playing {
                            room.driver.arm(PendingAction::EnterPlaying, 0);
                        } else {
                            room.driver.arm(PendingAction::StepThreeDiscard, 500);
                        }
                    }
                    None => {
                        // Phase already flipped (idempotent re-fire): enter playing.
                        room.driver.arm(PendingAction::EnterPlaying, 0);
                    }
                }
            }
            PA::BotTurn => {
                if room.state.phase != GamePhase::Playing {
                    room.driver.clear_pending();
                    return;
                }
                let needs_more = process_one_bot_turn(&mut room.state);
                out.broadcast.push(ServerMsg::State {
                    state: room.state.clone(),
                });
                if needs_more {
                    room.driver.arm(PendingAction::BotTurn, delay_ms);
                } else {
                    let cp_before = room.state.current_player;
                    skip_finished(&mut room.state);
                    if room.state.current_player != cp_before {
                        out.broadcast.push(ServerMsg::State {
                            state: room.state.clone(),
                        });
                    }
                    room.driver.arm_watchdog(&room.state);
                    room.driver.clear_pending();
                }
            }
        }
    }

    /// Fire the play-limit watchdog: auto-move the idle human seat (lowest
    /// legal single, else pass), log it, and hand off to the next turn.
    fn fire_watchdog(&self, room: &mut Room, delay_ms: u64, out: &mut MutateOut) {
        let st = &room.state;
        if st.phase != GamePhase::Playing {
            self.disarm_watchdog(room);
            return;
        }
        let cp = st.current_player;
        let Some(p) = st.players.get(cp) else {
            self.disarm_watchdog(room);
            return;
        };
        if p.is_bot || p.finished {
            self.disarm_watchdog(room);
            return;
        }
        let move_ = idle_auto_move(st, cp);
        let mut engine = GameEngine::new(st.clone());
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
            move_
                .as_ref()
                .map(|c| c.join(" "))
                .unwrap_or_else(|| "pass".to_string())
        ));
        room.state = new_state;
        self.disarm_watchdog(room);
        out.broadcast.push(ServerMsg::State {
            state: room.state.clone(),
        });
        self.arm_after_playing(room, delay_ms, out);
    }

    fn disarm_watchdog(&self, room: &mut Room) {
        room.driver.watchdog_deadline_ms = None;
        room.driver.watchdog_turn_seq = None;
    }

    /// After the turn is a live human's (or a bot's), arm the right driver
    /// state: a BotTurn if a bot is up, else the play-limit watchdog.
    fn arm_after_playing(&self, room: &mut Room, delay_ms: u64, _out: &mut MutateOut) {
        let st = &room.state;
        if st.phase != GamePhase::Playing {
            room.driver.clear_pending();
            self.disarm_watchdog(room);
            return;
        }
        let is_bot = st.players.get(st.current_player).map(|p| p.is_bot).unwrap_or(false);
        if is_bot {
            room.driver.arm(PendingAction::BotTurn, delay_ms);
        } else {
            room.driver.arm_watchdog(st);
            room.driver.clear_pending();
        }
    }

    pub fn get_bot_turn_delay_ms(&self) -> u64 {
        self.bot_turn_delay_ms
    }

    /// The shared room-state backend (exposed for the cross-pod pub/sub
    /// subscriber, which needs the channel naming convention + client).
    pub fn store(&self) -> &Arc<dyn RoomStore> {
        &self.store
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::InMemoryStore;
    use std::sync::Arc;

    /// Build a single-instance manager over an in-memory store. `bot_delay` is
    /// set small (0) in tests so the driver's chained fires resolve without
    /// waiting; timing-sensitive tests that need real delays build their own.
    fn mem(code_len: usize, bot: u64, orphan: u64, lobby: u64) -> Arc<RoomManager> {
        Arc::new(RoomManager::new(
            Arc::new(InMemoryStore::new()),
            "test".to_string(),
            code_len,
            bot,
            orphan,
            lobby,
        ))
    }

    #[tokio::test]
    async fn test_create_room() {
        let manager = mem(6, 0, 30, 15);
        let (code, player_id, msg) = manager.create_room("Alice".to_string(), true).await;

        assert_eq!(code.len(), 6);
        assert_eq!(player_id, 0);
        assert_eq!(manager.room_count().await, 1);

        match msg {
            ServerMsg::Created {
                code: c,
                player_id: pid,
                state,
                ..
            } => {
                assert_eq!(c, code);
                assert_eq!(pid, 0);
                assert_eq!(state.players.len(), 1);
                assert_eq!(state.phase, GamePhase::Lobby);
            }
            _ => panic!("Expected Created"),
        }
    }

    #[tokio::test]
    async fn test_join_room() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;

        let result = manager.join_room(&code, "Bob".to_string()).await;
        assert!(result.is_ok());

        match result.unwrap() {
            ServerMsg::Joined {
                player_id,
                state,
                ..
            } => {
                assert_eq!(player_id, 1);
                assert_eq!(state.players[player_id].name, "Bob");
                assert_eq!(state.players[player_id].is_bot, false);
                assert_eq!(state.players.len(), 2);
                assert_eq!(state.phase, GamePhase::Lobby);
            }
            _ => panic!("Expected Joined"),
        }
    }

    #[tokio::test]
    async fn test_join_room_not_found() {
        let manager = mem(6, 0, 30, 15);
        let result = manager.join_room("XXXXXX", "Bob".to_string()).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Room not found");
    }

    #[tokio::test]
    async fn test_join_room_full() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();
        manager.join_room(&code, "Charlie".to_string()).await.unwrap();
        manager.join_room(&code, "Dave".to_string()).await.unwrap();

        let result = manager.join_room(&code, "Eve".to_string()).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Room is full");
    }

    #[tokio::test]
    async fn test_start_game() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();

        // Not all ready, should fail
        let result = manager.start_game(&code, 0).await;
        assert!(result.is_err());

        // Ready Bob
        manager.ready_player(&code, 1, true).await.unwrap();

        // Start game as creator
        let result = manager.start_game(&code, 0).await;
        assert!(result.is_ok());

        let room = manager.get_room(&code).await.unwrap();
        assert!(room.started);
        assert_eq!(room.players.len(), 4);
        assert_eq!(room.state.phase, GamePhase::ThreeDiscard);
    }

    #[tokio::test]
    async fn test_start_game_non_creator() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();
        manager.ready_player(&code, 1, true).await.unwrap();

        let result = manager.start_game(&code, 1).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_leave_room_creator_dissolves_public() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();

        manager.leave_room(&code, 0).await;
        assert_eq!(manager.room_count().await, 0);
    }

    #[tokio::test]
    async fn test_public_room_listing_only_lobby() {
        let manager = mem(6, 0, 30, 15);
        let (code1, _, _) = manager.create_room("Alice".to_string(), true).await;
        manager.create_room("Bob".to_string(), false).await;

        let room_list = manager.list_public_rooms().await;
        assert_eq!(room_list.len(), 1);
        assert_eq!(room_list[0].code, code1);
        assert_eq!(room_list[0].players, 1);
        assert_eq!(room_list[0].phase, "lobby");

        // Start the game, should no longer appear
        manager.start_game(&code1, 0).await.unwrap();
        let room_list = manager.list_public_rooms().await;
        assert_eq!(room_list.len(), 0);
    }

    #[tokio::test]
    async fn test_leave_room() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();
        manager.ready_player(&code, 1, true).await.unwrap();
        manager.start_game(&code, 0).await.unwrap();

        let result = manager.leave_room(&code, 1).await;
        assert!(result.is_some());

        match result.unwrap() {
            ServerMsg::PlayerLeft { player_id, name } => {
                assert_eq!(player_id, 1);
                assert_eq!(name, "Bob");
            }
            _ => panic!("Expected PlayerLeft"),
        }

        let room = manager.get_room(&code).await.unwrap();
        // Human player is auto-replaced by a bot
        assert!(room.players[1].is_bot);
        assert!(room.players[1].connected);
        assert_eq!(room.players[1].name, "Bot (Bob)");
    }

    #[tokio::test]
    async fn test_get_room() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;

        let room = manager.get_room(&code).await;
        assert!(room.is_some());
        assert_eq!(room.unwrap().code, code);
    }

    #[tokio::test]
    async fn test_generate_code_length() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        assert_eq!(code.len(), 6);

        let manager2 = mem(8, 0, 30, 15);
        let (code2, _, _) = manager2.create_room("Bob".to_string(), true).await;
        assert_eq!(code2.len(), 8);
    }

    #[tokio::test]
    async fn test_multiple_rooms() {
        let manager = mem(6, 0, 30, 15);
        let (code1, _, _) = manager.create_room("Alice".to_string(), true).await;
        let (code2, _, _) = manager.create_room("Bob".to_string(), true).await;

        assert_ne!(code1, code2);
        assert_eq!(manager.room_count().await, 2);
    }

    #[tokio::test]
    async fn test_update_state() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;

        let mut state = manager.get_state(&code).await.unwrap();
        state.log.push("test".to_string());
        manager
            .locked_step(&code, move |room| {
                room.state = state;
                Ok(MutateOut::default())
            })
            .await
            .unwrap();

        let updated = manager.get_state(&code).await.unwrap();
        assert_eq!(updated.log, vec!["test".to_string()]);
    }

    #[tokio::test]
    async fn test_code_is_alphanumeric() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        assert!(code.chars().all(|c| c.is_alphanumeric()));
    }

    #[tokio::test]
    async fn test_rejoin_restores_seat() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        let joined_msg = manager.join_room(&code, "Bob".to_string()).await.unwrap();
        let token = match &joined_msg {
            ServerMsg::Joined { token, .. } => token.clone(),
            _ => panic!("Expected Joined"),
        };

        manager.ready_player(&code, 1, true).await.unwrap();
        manager.start_game(&code, 0).await.unwrap();

        manager.leave_room(&code, 1).await;
        let room = manager.get_room(&code).await.unwrap();
        assert_eq!(room.disconnected_players.len(), 1);

        let result = manager.rejoin_room(&code, "Bob", &token).await;
        assert!(result.is_ok());
        match result.unwrap() {
            ServerMsg::Rejoined {
                player_id, state, ..
            } => {
                assert_eq!(player_id, 1);
                assert_eq!(state.players[1].name, "Bob");
                assert!(!state.players[1].is_bot);
                assert!(state.players[1].connected);
            }
            _ => panic!("Expected Rejoined"),
        }

        let room = manager.get_room(&code).await.unwrap();
        assert!(!room.disconnected_players.iter().any(|(_, t, _)| t == &token));
    }

    #[tokio::test]
    async fn test_rejoin_not_found() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        let result = manager.rejoin_room(&code, "Unknown", "nonexistent-token").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_rejoin_seat_still_connected_reports_in_use() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        let joined_msg = manager.join_room(&code, "Bob".to_string()).await.unwrap();
        let token = match &joined_msg {
            ServerMsg::Joined { token, .. } => token.clone(),
            _ => panic!("Expected Joined"),
        };
        let err = manager.rejoin_room(&code, "Bob", &token).await.unwrap_err();
        assert!(err.contains("in use"), "expected in-use error, got: {err}");
        let room = manager.get_room(&code).await.unwrap();
        let seat = room
            .players
            .iter()
            .find(|p| p.token.as_deref() == Some(&token))
            .unwrap();
        assert!(seat.connected);
    }

    // --- check_room (read-only rejoin probe) --------------------------------

    #[tokio::test]
    async fn test_check_room_not_found() {
        let manager = mem(6, 0, 30, 15);
        let (found, rejoinable) = manager.check_room("NOPE12", "token").await;
        assert!(!found);
        assert!(!rejoinable);
    }

    #[tokio::test]
    async fn test_check_room_found_not_rejoinable() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();
        let (found, rejoinable) = manager.check_room(&code, "wrong-token").await;
        assert!(found);
        assert!(!rejoinable);
    }

    #[tokio::test]
    async fn test_check_room_found_and_rejoinable() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        let joined_msg = manager.join_room(&code, "Bob".to_string()).await.unwrap();
        let token = match &joined_msg {
            ServerMsg::Joined { token, .. } => token.clone(),
            _ => panic!("Expected Joined"),
        };

        manager.ready_player(&code, 1, true).await.unwrap();
        manager.start_game(&code, 0).await.unwrap();
        manager.leave_room(&code, 1).await;

        let (found, rejoinable) = manager.check_room(&code, &token).await;
        assert!(found);
        assert!(rejoinable);

        // READ-ONLY: the probe must NOT have claimed the seat.
        let room = manager.get_room(&code).await.unwrap();
        assert!(room.disconnected_players.iter().any(|(_, t, _)| t == &token));
    }

    #[tokio::test]
    async fn test_check_room_case_insensitive_code() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        let lower = code.to_lowercase();
        let (found, _) = manager.check_room(&lower, "x").await;
        assert!(found);
    }

    #[tokio::test]
    async fn test_check_room_expires_stale_seat() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        let joined_msg = manager.join_room(&code, "Bob".to_string()).await.unwrap();
        let token = match &joined_msg {
            ServerMsg::Joined { token, .. } => token.clone(),
            _ => panic!("Expected Joined"),
        };

        manager.ready_player(&code, 1, true).await.unwrap();
        manager.start_game(&code, 0).await.unwrap();
        manager.leave_room(&code, 1).await;

        manager.expire_disconnected_player(&code, &token).await;

        let (found, rejoinable) = manager.check_room(&code, &token).await;
        assert!(found);
        assert!(!rejoinable);
    }

    #[tokio::test]
    async fn test_check_room_after_room_removed() {
        let manager = mem(6, 0, 0, 0);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        manager.leave_room(&code, 0).await;
        assert_eq!(manager.room_count().await, 0);

        let (found, rejoinable) = manager.check_room(&code, "token").await;
        assert!(!found);
        assert!(!rejoinable);
    }

    #[tokio::test]
    async fn test_leave_room_tracks_disconnected() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        let joined_msg = manager.join_room(&code, "Bob".to_string()).await.unwrap();
        let token = match &joined_msg {
            ServerMsg::Joined { token, .. } => token.clone(),
            _ => panic!("Expected Joined"),
        };

        manager.ready_player(&code, 1, true).await.unwrap();
        manager.start_game(&code, 0).await.unwrap();
        manager.leave_room(&code, 1).await;

        let room = manager.get_room(&code).await.unwrap();
        assert_eq!(room.disconnected_players.len(), 1);
        assert_eq!(room.disconnected_players[0].0, 1);
        assert_eq!(room.disconnected_players[0].1, token);
    }

    #[tokio::test]
    async fn test_leave_room_bot_not_tracked() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();
        manager.ready_player(&code, 1, true).await.unwrap();
        manager.start_game(&code, 0).await.unwrap();

        manager.leave_room(&code, 2).await;

        let room = manager.get_room(&code).await.unwrap();
        assert!(room.disconnected_players.is_empty());
    }

    #[tokio::test]
    async fn test_public_room_listing() {
        let manager = mem(6, 0, 30, 15);
        manager.create_room("Alice".to_string(), true).await;
        manager.create_room("Bob".to_string(), false).await;

        let room_list = manager.list_public_rooms().await;
        assert_eq!(room_list.len(), 1);
        assert_eq!(room_list[0].players, 1);
        assert_eq!(room_list[0].max_players, 4);
        assert_eq!(room_list[0].host, "Alice");
        assert_eq!(room_list[0].phase, "lobby");
    }

    #[tokio::test]
    async fn test_rejoin_timeout_expired() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        let joined_msg = manager.join_room(&code, "Bob".to_string()).await.unwrap();
        let token = match &joined_msg {
            ServerMsg::Joined { token, .. } => token.clone(),
            _ => panic!("Expected Joined"),
        };

        manager.ready_player(&code, 1, true).await.unwrap();
        manager.start_game(&code, 0).await.unwrap();
        manager.leave_room(&code, 1).await;
        let room = manager.get_room(&code).await.unwrap();
        assert_eq!(room.disconnected_players.len(), 1);

        manager.expire_disconnected_player(&code, &token).await;
        let room = manager.get_room(&code).await.unwrap();
        assert!(room.disconnected_players.is_empty());

        let result = manager.rejoin_room(&code, "Bob", &token).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_full_rejoin_flow() {
        let manager = mem(6, 0, 30, 15);
        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        let joined_msg = manager.join_room(&code, "Bob".to_string()).await.unwrap();
        let token = match &joined_msg {
            ServerMsg::Joined { token, .. } => token.clone(),
            _ => panic!("Expected Joined"),
        };

        manager.ready_player(&code, 1, true).await.unwrap();
        manager.start_game(&code, 0).await.unwrap();
        manager.leave_room(&code, 1).await;

        let result = manager.rejoin_room(&code, "Bob", &token).await;
        assert!(result.is_ok());
        match result.unwrap() {
            ServerMsg::Rejoined { player_id, state, .. } => {
                assert_eq!(player_id, 1);
                assert_eq!(state.players[1].name, "Bob");
                assert!(state.players[1].connected);
            }
            _ => panic!("Expected Rejoined"),
        }

        let room = manager.get_room(&code).await.unwrap();
        assert!(!room.disconnected_players.iter().any(|(_, t, _)| t == &token));
    }

    #[tokio::test]
    async fn test_orphaned_room_removed_immediate() {
        let manager = mem(6, 0, 0, 0);

        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();
        manager.ready_player(&code, 1, true).await.unwrap();
        manager.start_game(&code, 0).await.unwrap();

        manager.leave_room(&code, 1).await;
        assert_eq!(manager.room_count().await, 1);

        manager.leave_room(&code, 0).await;
        assert_eq!(manager.room_count().await, 0);
    }

    #[tokio::test]
    async fn test_orphaned_room_not_removed_within_timeout() {
        let manager = mem(6, 0, 60, 15);

        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();
        manager.ready_player(&code, 1, true).await.unwrap();
        manager.start_game(&code, 0).await.unwrap();

        manager.leave_room(&code, 1).await;
        assert_eq!(manager.room_count().await, 1);

        manager.leave_room(&code, 0).await;
        assert_eq!(manager.room_count().await, 1);
    }

    #[tokio::test]
    async fn test_reaper_drops_orphaned_rooms() {
        let manager = mem(6, 0, 1, 15);

        let (code, _, _) = manager.create_room("Alice".to_string(), false).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();
        manager.ready_player(&code, 1, true).await.unwrap();
        manager.start_game(&code, 0).await.unwrap();

        manager.leave_room(&code, 1).await; // Bob -> bot, Alice still connected
        assert_eq!(manager.room_count().await, 1);
        manager.leave_room(&code, 0).await; // Alice -> bot, room now orphaned

        // Not removed *at* the disconnect (timer is 0s old).
        assert_eq!(manager.room_count().await, 1);

        // Let the orphan timer mature (timeout = 1s).
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let dropped = manager.reap_orphaned_rooms().await;
        assert_eq!(dropped, 1);
        assert_eq!(manager.room_count().await, 0);
        assert_eq!(manager.get_state(&code).await, None);
    }

    #[tokio::test]
    async fn test_reaper_keeps_room_with_connected_human() {
        let manager = mem(6, 0, 1, 15);

        let (code, _, _) = manager.create_room("Alice".to_string(), false).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();
        manager.ready_player(&code, 1, true).await.unwrap();
        manager.start_game(&code, 0).await.unwrap();

        manager.leave_room(&code, 1).await; // Bob -> bot, Alice still connected
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let dropped = manager.reap_orphaned_rooms().await;
        assert_eq!(dropped, 0);
        assert_eq!(manager.room_count().await, 1);
        assert!(manager.get_state(&code).await.is_some());
    }

    #[tokio::test]
    async fn test_rejoin_resets_orphan_timer() {
        let manager = mem(6, 0, 0, 0);

        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        let joined_msg = manager.join_room(&code, "Bob".to_string()).await.unwrap();
        let token = match &joined_msg {
            ServerMsg::Joined { token, .. } => token.clone(),
            _ => panic!("Expected Joined"),
        };

        manager.ready_player(&code, 1, true).await.unwrap();
        manager.start_game(&code, 0).await.unwrap();

        manager.leave_room(&code, 1).await;
        assert_eq!(manager.room_count().await, 1);

        let result = manager.rejoin_room(&code, "Bob", &token).await;
        assert!(result.is_ok());
        assert_eq!(manager.room_count().await, 1);
    }

    #[tokio::test]
    async fn test_orphaned_room_removed_in_lobby() {
        let manager = mem(6, 0, 0, 0);

        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();

        manager.leave_room(&code, 1).await;
        assert_eq!(manager.room_count().await, 1);

        manager.leave_room(&code, 0).await;
        assert_eq!(manager.room_count().await, 0);
    }

    #[tokio::test]
    async fn test_lobby_disconnected_player_removed_after_timeout() {
        let manager = mem(6, 0, 60, 0);

        let (code, _, _) = manager.create_room("Alice".to_string(), false).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();
        manager.join_room(&code, "Charlie".to_string()).await.unwrap();

        assert_eq!(manager.get_room(&code).await.unwrap().players.len(), 3);

        // lobby timeout 0: Alice's socket drop reaps her seat immediately.
        manager.leave_room(&code, 0).await;

        assert_eq!(manager.room_count().await, 1);
        let room = manager.get_room(&code).await.unwrap();
        assert!(!room.players.iter().any(|p| p.name == "Alice"));
        assert_eq!(room.players.len(), 2);
    }

    #[tokio::test]
    async fn test_remove_player_instant_from_lobby() {
        let manager = mem(6, 0, 30, 15);

        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();

        let result = manager.remove_player(&code, 1).await;
        assert!(result.is_some());

        let room = manager.get_room(&code).await.unwrap();
        assert_eq!(room.players.len(), 1);
        assert_eq!(room.players[0].name, "Alice");
    }

    #[tokio::test]
    async fn test_remove_player_creator_dissolves_public() {
        let manager = mem(6, 0, 30, 15);

        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();

        let result = manager.remove_player(&code, 0).await;
        assert!(result.is_some());

        assert_eq!(manager.room_count().await, 0);
    }

    #[tokio::test]
    async fn test_remove_player_transfers_creator_private() {
        let manager = mem(6, 0, 30, 15);

        let (code, _, _) = manager.create_room("Alice".to_string(), false).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();

        let result = manager.remove_player(&code, 0).await;
        assert!(result.is_some());

        let room = manager.get_room(&code).await.unwrap();
        assert_eq!(room.players.len(), 1);
        assert_eq!(room.players[0].name, "Bob");
        assert!(room.players[0].is_creator);
    }

    #[tokio::test]
    async fn test_remove_player_lobby_renumbers_all_parallel_arrays() {
        let manager = mem(6, 0, 30, 15);

        let (code, _, _) = manager.create_room("Alice".to_string(), false).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();
        manager.join_room(&code, "Charlie".to_string()).await.unwrap();

        manager.remove_player(&code, 1).await; // Bob (seat 1) leaves

        manager.join_room(&code, "Dave".to_string()).await.unwrap();

        let room = manager.get_room(&code).await.unwrap();
        assert_eq!(room.players.len(), room.state.players.len());
        assert_eq!(room.players.len(), room.ready.len());
        assert_eq!(room.players.len(), room.state.ready.len());
        assert_eq!(room.players.len(), room.state.scores.len());
        let mut ids: Vec<usize> = room.players.iter().map(|p| p.id).collect();
        ids.sort();
        assert_eq!(ids, vec![0, 1, 2]);
        let mut sids: Vec<usize> = room.state.players.iter().map(|p| p.id).collect();
        sids.sort();
        assert_eq!(sids, vec![0, 1, 2]);
        assert!(room
            .players
            .iter()
            .zip(room.state.players.iter())
            .all(|(a, b)| a.id == b.id));
        let dave = room.players.iter().find(|p| p.name == "Dave").unwrap();
        assert!(!room.ready[dave.id]);
        assert!(!room.state.ready[dave.id]);

        for p in room.players.iter() {
            if !p.is_bot {
                manager.ready_player(&code, p.id, true).await.unwrap();
            }
        }
        let creator = room.players.iter().find(|p| p.is_creator).unwrap();
        assert!(manager.start_game(&code, creator.id).await.is_ok());
    }

    #[tokio::test]
    async fn test_remove_player_creator_transfer_reaches_state_players() {
        let manager = mem(6, 0, 30, 15);

        let (code, _, _) = manager.create_room("Alice".to_string(), false).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();

        manager.remove_player(&code, 0).await; // creator Alice leaves (private room)

        let room = manager.get_room(&code).await.unwrap();
        assert_eq!(room.players.len(), 1);
        let bob_state = room
            .state
            .players
            .iter()
            .find(|p| p.name == "Bob")
            .unwrap();
        assert!(
            bob_state.is_creator,
            "crown must be visible in state.players (what the client renders)"
        );
        assert_eq!(room.state.ready.len(), 1);
        assert_eq!(room.state.scores.len(), 1);
        assert!(room.state.ready[0]); // Bob keeps a usable ready state
    }

    #[tokio::test]
    async fn test_lobby_disconnect_reap_frees_seat_without_stealing() {
        let manager = mem(6, 0, 30, 1); // 1s lobby timeout

        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        let bob_token = match manager.join_room(&code, "Bob".to_string()).await.unwrap() {
            ServerMsg::Joined { token, .. } => token,
            _ => panic!("Expected Joined"),
        };
        manager.leave_room(&code, 1).await; // socket drop: seat marked disconnected
        // Force the lobby reaper NOW (timeout 0 -> everything reaped).
        manager.force_lobby_reap(&code).await;

        manager.join_room(&code, "Dave".to_string()).await.unwrap();
        let room = manager.get_room(&code).await.unwrap();
        let dave = room.players.iter().find(|p| p.name == "Dave").unwrap();

        // Bob's token must not be able to reclaim Dave's seat.
        let rejoin = manager.rejoin_room(&code, "Bob", &bob_token).await;
        let room2 = manager.get_room(&code).await.unwrap();
        let dave2 = room2
            .players
            .iter()
            .find(|p| p.id == dave.id)
            .unwrap();
        assert_eq!(dave2.name, "Dave", "rejoin must not steal a compacted seat");
        if let Ok(ServerMsg::Rejoined { player_id, .. }) = rejoin {
            assert_ne!(player_id, dave.id);
        }
    }

    #[tokio::test]
    async fn test_leave_room_creator_drop_transfers_crown_to_state() {
        let manager = mem(6, 0, 30, 1); // 1s lobby timeout

        let (code, _, _) = manager.create_room("Alice".to_string(), false).await; // private
        manager.join_room(&code, "Bob".to_string()).await.unwrap();

        manager.leave_room(&code, 0).await; // creator's socket drops

        // Force the lobby reaper (timeout 0 -> instant).
        manager.force_lobby_reap(&code).await;

        let room = manager.get_room(&code).await.unwrap();
        let bob_state = room
            .state
            .players
            .iter()
            .find(|p| p.name == "Bob")
            .unwrap();
        assert!(
            bob_state.is_creator,
            "crown must reach state.players (what the client renders) after a creator socket drop"
        );
        // And the room must actually be startable afterwards.
        assert!(manager.start_game(&code, 0).await.is_ok());
    }

    #[tokio::test]
    async fn test_remove_player_bot_returns_none() {
        let manager = mem(6, 0, 30, 15);

        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();
        manager.ready_player(&code, 1, true).await.unwrap();
        manager.start_game(&code, 0).await.unwrap();

        let room = manager.get_room(&code).await.unwrap();
        let bot_id = room.players.iter().position(|p| p.is_bot).unwrap();

        assert!(manager.remove_player(&code, bot_id).await.is_none());
    }

    #[tokio::test]
    async fn test_remove_player_mid_game_becomes_bot() {
        let manager = mem(6, 0, 30, 15);

        let (code, _, _) = manager.create_room("Alice".to_string(), true).await;
        manager.join_room(&code, "Bob".to_string()).await.unwrap();
        manager.ready_player(&code, 1, true).await.unwrap();
        manager.start_game(&code, 0).await.unwrap();

        let result = manager.remove_player(&code, 1).await;
        assert!(result.is_some());

        let room = manager.get_room(&code).await.unwrap();
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
