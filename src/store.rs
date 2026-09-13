//! Storage backend for room state.
//!
//! The whole point of this module is that the game server can run as EITHER
//! a single in-memory instance (dev / `just dev`, no Redis) OR a fleet of
//! stateless pods sharing one Redis (production multi-pod). The `RoomManager`
//! talks to a `RoomStore` — it never touches a `DashMap` or Redis directly —
//! so swapping backends is a one-line config change (`STORAGE=memory|redis`).
//!
//! ## The two backends
//! - [`InMemoryStore`]: the original `DashMap`. Single-instance ONLY.
//! - [`RedisStore`]: room state as JSON in Redis + a `room:codes` set +
//!   per-room advisory locks (`SET NX PX`) + a pub/sub channel per room for
//!   cross-pod fan-out.
//!
//! ## Concurrency / failover model
//! Every state mutation goes through `RoomManager::locked_step`, which is an
//! atomic read-modify-write under a per-room lock:
//!
//!   1. `try_lock(code)`   — non-blocking claim (`SET NX PX` in Redis, a
//!                           semaphore permit in memory mode)
//!   2. `get(code)`        — read the current room
//!   3. `f(&mut room)`     — the mutation (pure in-memory, no I/O)
//!   4. `save` / `delete`  — persist
//!   5. `emit`             — broadcast locally + publish to other pods
//!   6. `unlock(code)`     — release (compare-and-delete in Redis)
//!
//! The lock is an OWNED token ([`RoomLock`]) so it can be held across the
//! `await`s without borrowing `self` — and so a dead pod's claim simply
//! expires via the Redis TTL: another pod's driver tick re-acquires it and
//! the room is never stranded. The "driver" (see `driver.rs`) is a stateless
//! poller that only ever mutates through this same locked path, so two pods
//! can never advance the same room at once.
//!
//! Note on the trait shape: the mutation CLOSURE deliberately does NOT live
//! in the trait (an `async_trait` method can't take a `FnOnce` that borrows
//! a local guard — the expansion breaks the lifetimes). `RoomManager` owns
//! the read-modify-write loop and calls the small primitive methods below.

use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::game::state::Room;
use crate::protocol::ServerMsg;

/// What a mutation reports back to its caller.
///
/// The mutation closure has no access to the local WebSocket sessions (those
/// live in `RoomManager`, not the store), so it can't broadcast itself.
/// Instead it records what SHOULD be broadcast and whether the room was
/// removed; `RoomManager` fans the message out locally (and, in Redis mode,
/// the store also publishes it to the other pods).
#[derive(Debug, Clone)]
pub struct MutateOut {
    /// Messages to broadcast to the room after the mutation commits (in
    /// order). A single mutation can emit several (e.g. join = PlayerJoined +
    /// State). Delivered locally by `RoomManager` AND published to other pods
    /// (Redis mode) so cross-pod clients stay in sync.
    pub broadcast: Vec<ServerMsg>,
    /// Message to return to the CALLER of the mutation (sent over the actor's
    /// own socket, never broadcast — e.g. `Joined`/`Rejoined`/`Created`).
    pub reply: Option<ServerMsg>,
    /// Set to `true` to delete the room from the store after the mutation.
    pub removed: bool,
    /// Seat renumbering produced by a lobby compaction, `(old_seat, new_seat)`.
    /// `RoomManager` applies it to its LOCAL session map after the step (the
    /// store has no notion of connections) and broadcasts `SeatChanged`.
    pub renumbered: Vec<(usize, usize)>,
}

impl Default for MutateOut {
    fn default() -> Self {
        MutateOut {
            broadcast: Vec::new(),
            reply: None,
            removed: false,
            renumbered: Vec::new(),
        }
    }
}

/// Owned handle to a room's advisory lock.
///
/// - [`RoomLock::Memory`] — a semaphore permit; the permit itself IS the
///   lock, and dropping it releases it. (In-process mutual exclusion for
///   single-instance mode; there is no TTL because a permit can't be lost
///   without its owning task finishing.)
/// - [`RoomLock::Redis`] — the claim lives in Redis itself
///   (`room:lock:{code}` = this pod's id, `SET NX PX <ttl>`). The token
///   carries nothing; `unlock` does a compare-and-delete by pod id.
#[derive(Debug)]
pub enum RoomLock {
    Memory(SemaphorePermit),
    Redis,
}

type SemaphorePermit = tokio::sync::OwnedSemaphorePermit;

/// A room-state backend. Object-safe (no generic methods) so it can sit in an
/// `Arc<dyn RoomStore>` and be swapped at startup by config.
#[async_trait]
pub trait RoomStore: Send + Sync {
    /// Read-only fetch of a room.
    async fn get(&self, code: &str) -> Option<Room>;

    /// All known room codes (the driver scans this every tick).
    async fn codes(&self) -> Vec<String>;

    /// Create a brand-new room. Fails if the code already exists.
    async fn create(&self, code: &str, room: &Room) -> Result<(), String>;

    /// Persist a room (full JSON overwrite + register the code).
    async fn save(&self, code: &str, room: &Room);

    /// Delete a room (state + code registration).
    async fn delete(&self, code: &str);

    /// Non-blocking attempt to acquire the room's advisory lock.
    /// `None` = someone else (another pod, or another task in memory mode)
    /// holds it right now.
    async fn try_lock(&self, code: &str) -> Option<RoomLock>;

    /// Release a lock previously obtained from [`RoomStore::try_lock`].
    async fn unlock(&self, code: &str, lock: RoomLock);

    /// Cross-pod fan-out of a broadcast message.
    ///
    /// Memory mode: no-op (the mutation already ran in-process and
    /// `RoomManager` delivered to local sessions directly).
    /// Redis mode: `PUBLISH room:events:{code}` so every other connected pod
    /// re-broadcasts to ITS local sessions.
    async fn publish(&self, code: &str, pod: &str, msg: &ServerMsg);
}

// ---------------------------------------------------------------------------
// In-memory backend (single instance)
// ---------------------------------------------------------------------------

/// The original `DashMap`-based store. Correct for ONE process only.
///
/// Locks here are REAL per-room semaphores, not no-ops: even in single
/// instance mode, a human action and the driver tick can interleave on the
/// same room, and the read-modify-write must stay atomic or a bot turn can
/// be silently overwritten.
pub struct InMemoryStore {
    rooms: DashMap<String, Room>,
    /// One permit per room: exactly one task may be inside a locked step for
    /// this room at a time.
    locks: DashMap<String, Arc<Semaphore>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        InMemoryStore {
            rooms: DashMap::new(),
            locks: DashMap::new(),
        }
    }

    /// Escape hatch for tests that still poke the map directly.
    pub fn rooms_ref(&self) -> Arc<DashMap<String, Room>> {
        Arc::new(self.rooms.clone())
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RoomStore for InMemoryStore {
    async fn get(&self, code: &str) -> Option<Room> {
        self.rooms.get(code).map(|r| r.value().clone())
    }

    async fn codes(&self) -> Vec<String> {
        self.rooms.iter().map(|e| e.key().clone()).collect()
    }

    async fn create(&self, code: &str, room: &Room) -> Result<(), String> {
        if self.rooms.insert(code.to_string(), room.clone()).is_some() {
            return Err("Room already exists".to_string());
        }
        Ok(())
    }

    async fn save(&self, code: &str, room: &Room) {
        self.rooms.insert(code.to_string(), room.clone());
    }

    async fn delete(&self, code: &str) {
        self.rooms.remove(code);
        self.locks.remove(code);
    }

    async fn try_lock(&self, code: &str) -> Option<RoomLock> {
        let sem = self
            .locks
            .entry(code.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone();
        sem.try_acquire_owned().ok().map(RoomLock::Memory)
    }

    async fn unlock(&self, _code: &str, lock: RoomLock) {
        // The permit releases itself on drop.
        drop(lock);
    }

    async fn publish(&self, _code: &str, _pod: &str, _msg: &ServerMsg) {
        // No-op: in-memory mode is a single process, the mutation already
        // delivered to the local sessions via RoomManager::broadcast_local.
    }
}

// ---------------------------------------------------------------------------
// Redis backend (multi-pod)
// ---------------------------------------------------------------------------

/// The Redis-backed store. Room state is a JSON document per key:
///
///   room:{code}            -> serialized Room (JSON)
///   room:codes             -> SET of all codes (for the driver's scan)
///   room:lock:{code}       -> owning pod id, SET NX PX <ttl> (advisory lock)
///   room:events:{code}     -> pub/sub channel for cross-pod fan-out
pub struct RedisStore {
    conn: redis::aio::MultiplexedConnection,
    /// TTL (ms) for per-room advisory locks. Long enough that a live pod
    /// always holds it through a mutation, short enough that a dead pod's
    /// lock clears in one or two driver ticks (default 15s).
    lock_ttl_ms: u64,
    /// Grace TTL for the room state key itself (ms). Refreshed on every
    /// save, so an abandoned room's state eventually expires even if the
    /// orphan reaper is down.
    room_ttl_ms: u64,
    pod_id: String,
}

impl RedisStore {
    pub fn new(
        conn: redis::aio::MultiplexedConnection,
        lock_ttl_ms: u64,
        room_ttl_ms: u64,
        pod_id: String,
    ) -> Self {
        RedisStore {
            conn,
            lock_ttl_ms,
            room_ttl_ms,
            pod_id,
        }
    }

    fn room_key(code: &str) -> String {
        format!("room:{}", code)
    }
    const CODES_KEY: &'static str = "room:codes";

    fn codes_key() -> &'static str {
        Self::CODES_KEY
    }
    fn lock_key(code: &str) -> String {
        format!("room:lock:{}", code)
    }
    /// Pub/sub channel for one room's cross-pod events.
    pub fn events_key(code: &str) -> String {
        format!("room:events:{}", code)
    }
}

/// Envelope published over the per-room pub/sub channel. `pod` is the sender
/// so each receiver can drop its OWN echo (it already delivered locally) and
/// avoid double-delivery to its local clients. `pub` so the pod's pub/sub
/// subscriber (`pubsub::run`) can decode it.
#[derive(Serialize, Deserialize)]
pub struct PubEnvelope {
    pub pod: String,
    pub msg: ServerMsg,
}

#[async_trait]
impl RoomStore for RedisStore {
    async fn get(&self, code: &str) -> Option<Room> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let json: Option<String> = conn.get(Self::room_key(code)).await.ok()?;
        let json = json?;
        serde_json::from_str(&json).ok()
    }

    async fn codes(&self) -> Vec<String> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        match conn.smembers::<&str, Vec<String>>(Self::codes_key()).await {
            Ok(m) => m,
            Err(_) => Vec::new(),
        }
    }

    async fn create(&self, code: &str, room: &Room) -> Result<(), String> {
        let json = serde_json::to_string(room).map_err(|e| e.to_string())?;
        // SET NX: only create if absent.
        let created: Option<String> = redis::cmd("SET")
            .arg(Self::room_key(code))
            .arg(&json)
            .arg("NX")
            .arg("EX")
            .arg((self.room_ttl_ms / 1000).max(1))
            .query_async(&mut self.conn.clone())
            .await
            .map_err(|e| e.to_string())?;
        if created.is_none() {
            return Err("Room already exists".to_string());
        }
        let mut conn = self.conn.clone();
        let _: i64 = redis::cmd("SADD")
            .arg(Self::codes_key())
            .arg(code)
            .query_async(&mut conn)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn save(&self, code: &str, room: &Room) {
        let Ok(json) = serde_json::to_string(room) else {
            return;
        };
        // Raw cmd + explicit types: the AsyncCommands trait macros hit a
        // type-inference quirk in redis 0.27.6 (`sadd` infers its return to `!`).
        // EX keeps a grace TTL on the state key, refreshed on every save:
        // an abandoned room eventually expires even if the reaper is down.
        let mut conn = self.conn.clone();
        let r: Result<(), redis::RedisError> = redis::cmd("SET")
            .arg(Self::room_key(code))
            .arg(&json)
            .arg("EX")
            .arg((self.room_ttl_ms / 1000).max(1))
            .query_async(&mut conn)
            .await;
        if let Err(e) = r {
            eprintln!("[store] redis SET room:{code} failed: {e}");
            return;
        }
        let r2: Result<i64, redis::RedisError> =
            redis::cmd("SADD").arg(Self::codes_key()).arg(code).query_async(&mut conn).await;
        if let Err(e) = r2 {
            eprintln!("[store] redis SADD room:codes {code} failed: {e}");
        }
    }

    async fn delete(&self, code: &str) {
        let mut conn = self.conn.clone();
        let r: Result<i64, redis::RedisError> =
            redis::cmd("DEL").arg(Self::room_key(code)).query_async(&mut conn).await;
        if let Err(e) = r {
            eprintln!("[store] redis DEL room:{code} failed: {e}");
        }
        let r2: Result<i64, redis::RedisError> =
            redis::cmd("SREM").arg(Self::codes_key()).arg(code).query_async(&mut conn).await;
        if let Err(e) = r2 {
            eprintln!("[store] redis SREM room:codes {code} failed: {e}");
        }
    }

    async fn try_lock(&self, code: &str) -> Option<RoomLock> {
        // SET key pod NX PX ttl -> "OK" iff acquired.
        let acquired: Option<String> = redis::cmd("SET")
            .arg(Self::lock_key(code))
            .arg(&self.pod_id)
            .arg("NX")
            .arg("PX")
            .arg(self.lock_ttl_ms)
            .query_async(&mut self.conn.clone())
            .await
            .ok()
            .flatten();
        acquired.is_some().then(|| RoomLock::Redis)
    }

    async fn unlock(&self, code: &str, _lock: RoomLock) {
        self.release_lock(code).await;
    }

    async fn publish(&self, code: &str, pod: &str, msg: &ServerMsg) {
        use redis::AsyncCommands;
        let env = PubEnvelope {
            pod: pod.to_string(),
            msg: msg.clone(),
        };
        let payload = match serde_json::to_string(&env) {
            Ok(p) => p,
            Err(_) => return,
        };
        let mut conn = self.conn.clone();
        let r: Result<i64, redis::RedisError> = conn.publish(Self::events_key(code), payload).await;
        if let Err(e) = r {
            eprintln!("[store] redis PUBLISH room:events:{code} failed: {e}");
        }
    }
}

impl RedisStore {
    /// Compare-and-delete the room lock: only delete it if it still holds
    /// THIS pod's id (we may have been superseded after a TTL expiry).
    async fn release_lock(&self, code: &str) {
        let script = redis::Script::new(
            r#"
            if redis.call('get', KEYS[1]) == ARGV[1] then
                return redis.call('del', KEYS[1])
            else
                return 0
            end
            "#,
        );
        let _: i32 = script
            .key(Self::lock_key(code))
            .arg(&self.pod_id)
            .invoke_async(&mut self.conn.clone())
            .await
            .unwrap_or(0);
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Build a store from config. `kind` is `"memory"` (default, dev-safe — no
/// Redis required) or `"redis"`. `room_ttl_ms` is the grace TTL for room
/// state keys (Redis mode only; the reaper's `ROOM_ORPHAN_TIMEOUT_SECS` is a
/// separate, semantic orphan timeout).
pub async fn build_store(
    kind: &str,
    redis_url: Option<&str>,
    pod_id: String,
    lock_ttl_ms: u64,
    room_ttl_ms: u64,
) -> Result<Arc<dyn RoomStore>, String> {
    match kind.trim().to_lowercase().as_str() {
        "redis" => {
            let url = redis_url
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "STORAGE=redis but REDIS_URL is not set".to_string())?;
            let client = redis::Client::open(url)
                .map_err(|e| format!("failed to open redis client: {e}"))?;
            let conn = client
                .get_multiplexed_tokio_connection()
                .await
                .map_err(|e| format!("failed to connect to redis: {e}"))?;
            Ok(Arc::new(RedisStore::new(conn, lock_ttl_ms, room_ttl_ms, pod_id)))
        }
        "memory" | "" | _ => {
            if !kind.trim().is_empty() && kind.trim().to_lowercase() != "memory" {
                eprintln!(
                    "[store] WARNING: unknown STORAGE='{}' — falling back to in-memory (single-instance).",
                    kind
                );
            }
            Ok(Arc::new(InMemoryStore::new()))
        }
    }
}
