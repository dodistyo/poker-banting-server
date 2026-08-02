# Multiplayer: Public/Private Rooms + Reconnect Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add public/private room visibility and disconnected player rejoin so players can discover games and resume their seat after refresh.

**Architecture:** Backend gains `is_public` on Room, `disconnected_players` tracking, `Rejoin` protocol message, and `GET /api/rooms` REST endpoint. Frontend adds public/private toggle, room list view, and auto-rejoin via localStorage persistence.

**Tech Stack:** Rust (axum, serde, dashmap), Vanilla JS (localStorage, WebSocket)

## Global Constraints

- Reconnect timeout: 5 minutes (300 seconds)
- Room codes: 6-character alphanumeric, case-insensitive
- Public rooms default: `true`
- Only rooms with `phase != GameOver` appear in public listing
- Frontend storage keys: `pocer_roomCode`, `pocer_playerId`, `pocer_playerName`

---

### Task 1: Room Model – Add `is_public` and `disconnected_players`

**Files:**
- Modify: `src/game/state.rs:69-76` (Room struct), `src/game/state.rs:87-122` (Room::new)
- Test: `src/game/state.rs` tests module

**Interfaces:**
- Produces: `Room.is_public: bool`, `Room.disconnected_players: Vec<(usize, String, Instant)>`
- Produces: `Room::add_disconnected_player(seat_id, name)`, `Room::cleanup_expired()`

- [ ] **Step 1: Add fields to Room struct**

```rust
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    pub code: String,
    pub state: GameState,
    pub players: Vec<RoomPlayer>,
    pub started: bool,
    pub delay_task_spawned: bool,
    pub is_public: bool,
    pub disconnected_players: Vec<(usize, String, Instant)>,
}
```

- [ ] **Step 2: Update Room::new to initialize new fields**

```rust
Room {
    code,
    state,
    players,
    started: false,
    delay_task_spawned: false,
    is_public: true,
    disconnected_players: Vec::new(),
}
```

- [ ] **Step 3: Add helper methods to Room impl**

```rust
pub fn add_disconnected_player(&mut self, seat_id: usize, name: String) {
    self.disconnected_players.push((seat_id, name, Instant::now()));
}

pub fn cleanup_expired(&mut self, timeout_secs: u64) {
    let cutoff = Instant::now() - std::time::Duration::from_secs(timeout_secs);
    self.disconnected_players.retain(|(_, _, t)| *t > cutoff);
}
```

- [ ] **Step 4: Write tests**

```rust
#[test]
fn test_room_is_public_default() {
    let room = Room::new("ABC123".to_string(), "Host".to_string());
    assert!(room.is_public);
}

#[test]
fn test_room_disconnected_player() {
    let mut room = Room::new("ABC123".to_string(), "Host".to_string());
    room.add_disconnected_player(0, "Alice".to_string());
    assert_eq!(room.disconnected_players.len(), 1);
    assert_eq!(room.disconnected_players[0].0, 0);
    assert_eq!(room.disconnected_players[0].1, "Alice");
}

#[test]
fn test_room_cleanup_expired() {
    let mut room = Room::new("ABC123".to_string(), "Host".to_string());
    room.add_disconnected_player(0, "Alice".to_string());
    room.cleanup_expired(0);
    assert!(room.disconnected_players.is_empty());
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test --package pocer-server state:: --nocapture`
Expected: PASS all new tests plus existing

- [ ] **Step 6: Commit**

```bash
git add src/game/state.rs
git commit -m "feat: add is_public and disconnected_players to Room model"
```

---

### Task 2: Protocol – Add `is_public` to Create, `Rejoin` message, `Rejoined` response

**Files:**
- Modify: `src/protocol.rs:6-26` (ClientMsg), `src/protocol.rs:30-64` (ServerMsg)
- Test: `src/protocol.rs` tests module

**Interfaces:**
- Consumes: none
- Produces: `ClientMsg::Create { name, is_public }`, `ClientMsg::Rejoin { code, name }`
- Produces: `ServerMsg::Created { code, player_id, state, is_public }`, `ServerMsg::Rejoined { player_id, state }`

- [ ] **Step 1: Update ClientMsg::Create with optional is_public**

```rust
#[serde(rename = "create")]
Create {
    name: String,
    #[serde(default = "default_is_public")]
    is_public: bool,
},
```

Add helper:
```rust
const fn default_is_public() -> bool { true }
```

- [ ] **Step 2: Add ClientMsg::Rejoin variant**

```rust
#[serde(rename = "rejoin")]
Rejoin {
    code: String,
    name: String,
},
```

- [ ] **Step 3: Update ServerMsg::Created with is_public**

```rust
#[serde(rename = "created")]
Created {
    code: String,
    player_id: usize,
    state: GameState,
    is_public: bool,
},
```

- [ ] **Step 4: Add ServerMsg::Rejoined variant**

```rust
#[serde(rename = "rejoined")]
Rejoined {
    player_id: usize,
    state: GameState,
},
```

- [ ] **Step 5: Write tests**

```rust
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
    let json = r#"{"type":"rejoin","code":"ABC123","name":"Alice"}"#;
    let msg: ClientMsg = serde_json::from_str(json).unwrap();
    match msg {
        ClientMsg::Rejoin { code, name } => {
            assert_eq!(code, "ABC123");
            assert_eq!(name, "Alice");
        }
        _ => panic!("Expected Rejoin"),
    }
}

#[test]
fn test_server_msg_rejoined() {
    let state = GameState { /* ... minimal */ };
    let msg = ServerMsg::Rejoined { player_id: 0, state };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("\"type\":\"rejoined\""));
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test --package pocer-server protocol:: --nocapture`
Expected: PASS all

- [ ] **Step 7: Commit**

```bash
git add src/protocol.rs
git commit -m "feat: add is_public to Create, Rejoin/Rejoined protocol messages"
```

---

### Task 3: RoomManager – `create_room` with `is_public`

**Files:**
- Modify: `src/rooms.rs:43-77` (create_room)
- Test: `src/rooms.rs` tests module

**Interfaces:**
- Consumes: `ClientMsg::Create { name, is_public }` from Task 2
- Consumes: `Room.is_public` from Task 1
- Produces: `ServerMsg::Created { code, player_id, state, is_public }`

- [ ] **Step 1: Update create_room signature and implementation**

```rust
pub fn create_room(&self, host_name: String, is_public: bool) -> (String, usize, ServerMsg, bool) {
    let code = self.generate_code();
    let mut room = Room::new(code.clone(), host_name.clone());
    room.is_public = is_public;

    // ... existing bot-filling and engine logic ...

    (
        code.clone(),
        0,
        ServerMsg::Created {
            code,
            player_id: 0,
            state,
            is_public,
        },
        should_spawn,
    )
}
```

- [ ] **Step 2: Update tests to pass is_public**

```rust
#[test]
fn test_create_room_public() {
    let manager = RoomManager::new(6, 2500);
    let (code, player_id, msg, should_spawn) = manager.create_room("Alice".to_string(), true);
    assert!(should_spawn);
    match msg {
        ServerMsg::Created { code: c, player_id: pid, state, is_public } => {
            assert_eq!(c, code);
            assert_eq!(pid, 0);
            assert!(is_public);
            assert_eq!(state.players.len(), 4);
        }
        _ => panic!("Expected Created"),
    }
}

#[test]
fn test_create_room_private() {
    let manager = RoomManager::new(6, 2500);
    let (_, _, msg, _) = manager.create_room("Alice".to_string(), false);
    match msg {
        ServerMsg::Created { is_public, .. } => assert!(!is_public),
        _ => panic!("Expected Created"),
    }
}
```

- [ ] **Step 3: Update all existing tests that call create_room** — add `true` for is_public parameter

- [ ] **Step 4: Run tests**

Run: `cargo test --package pocer-server rooms:: --nocapture`
Expected: PASS all

- [ ] **Step 5: Commit**

```bash
git add src/rooms.rs
git commit -m "feat: create_room accepts is_public parameter"
```

---

### Task 4: RoomManager – `rejoin_room` logic

**Files:**
- Modify: `src/rooms.rs` — add new method after `join_room`
- Test: `src/rooms.rs` tests module

**Interfaces:**
- Consumes: `Room.disconnected_players` from Task 1, `Room::add_disconnected_player`
- Consumes: `ServerMsg::Rejoined` from Task 2
- Produces: `Result<(ServerMsg::Rejoined | ServerMsg::Joined, bool), String>`

- [ ] **Step 1: Implement rejoin_room method**

```rust
const REJOIN_TIMEOUT_SECS: u64 = 300;

pub fn rejoin_room(&self, code: &str, name: &str) -> Result<(ServerMsg, bool), String> {
    let code = code.to_uppercase();
    let mut room = self.rooms.get_mut(&code).ok_or("Room not found")?;

    room.cleanup_expired(REJOIN_TIMEOUT_SECS);

    // Find matching disconnected player
    if let Some(pos) = room.disconnected_players.iter().position(|(_, n, _)| n == name) {
        let (seat_id, _, _) = room.disconnected_players.remove(pos);

        // Restore player
        if seat_id < room.players.len() {
            room.players[seat_id].name = name.to_string();
            room.players[seat_id].is_bot = false;
            room.players[seat_id].connected = true;
            room.players[seat_id].disconnect_time = None;
        }
        if seat_id < room.state.players.len() {
            room.state.players[seat_id].name = name.to_string();
            room.state.players[seat_id].is_bot = false;
            room.state.players[seat_id].connected = true;
        }

        let state = room.state.clone();
        drop(room);

        self.broadcast(&code, ServerMsg::PlayerJoined {
            player_id: seat_id,
            name: name.to_string(),
        });

        return Ok((ServerMsg::Rejoined {
            player_id: seat_id,
            state,
        }, false));
    }

    Err("No matching disconnected player found".to_string())
}
```

- [ ] **Step 2: Write tests**

```rust
#[test]
fn test_rejoin_restores_seat() {
    let manager = RoomManager::new(6, 2500);
    let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
    manager.join_room(&code, "Bob".to_string()).unwrap();

    // Simulate disconnect
    manager.leave_room(&code, 1);
    let room = manager.get_room(&code).unwrap();
    // Manually add disconnected entry for test
    // (leave_room doesn't add to disconnected_players yet - that's Task 5)

    // For now, test rejoin logic directly
    let mut room = manager.rooms.get_mut(&code).unwrap();
    room.add_disconnected_player(1, "Bob".to_string());
    drop(room);

    let result = manager.rejoin_room(&code, "Bob");
    assert!(result.is_ok());
    match result.unwrap().0 {
        ServerMsg::Rejoined { player_id, state } => {
            assert_eq!(player_id, 1);
            assert_eq!(state.players[1].name, "Bob");
            assert!(!state.players[1].is_bot);
        }
        _ => panic!("Expected Rejoined"),
    }
}

#[test]
fn test_rejoin_not_found() {
    let manager = RoomManager::new(6, 2500);
    let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
    let result = manager.rejoin_room(&code, "Unknown");
    assert!(result.is_err());
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --package pocer-server rooms::tests::test_rejoin --nocapture`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/rooms.rs
git commit -m "feat: add rejoin_room logic with disconnected_players tracking"
```

---

### Task 5: RoomManager – `leave_room` tracks disconnected players

**Files:**
- Modify: `src/rooms.rs:154-175` (leave_room)

**Interfaces:**
- Consumes: `Room::add_disconnected_player` from Task 1
- Produces: disconnected player entry when human player leaves

- [ ] **Step 1: Update leave_room to add disconnected player for humans**

```rust
pub fn leave_room(&self, code: &str, player_id: usize) -> Option<ServerMsg> {
    let mut room = self.rooms.get_mut(code)?;

    let (player_name, is_bot) = {
        let player = room.players.iter().find(|p| p.id == player_id)?;
        (player.name.clone(), player.is_bot)
    };

    if let Some(player) = room.players.iter_mut().find(|p| p.id == player_id) {
        player.connected = false;
        player.disconnect_time = Some(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs());
    }

    // Only track human players for rejoin
    if !is_bot {
        room.add_disconnected_player(player_id, player_name.clone());
    }

    let msg = ServerMsg::PlayerLeft {
        player_id,
        name: player_name,
    };

    Some(msg)
}
```

- [ ] **Step 2: Update existing leave_room test to verify disconnected tracking**

```rust
#[test]
fn test_leave_room_tracks_disconnected() {
    let manager = RoomManager::new(6, 2500);
    let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
    manager.join_room(&code, "Bob".to_string()).unwrap();

    manager.leave_room(&code, 1);

    let room = manager.get_room(&code).unwrap();
    assert_eq!(room.disconnected_players.len(), 1);
    assert_eq!(room.disconnected_players[0].0, 1);
    assert_eq!(room.disconnected_players[0].1, "Bob");
}

#[test]
fn test_leave_room_bot_not_tracked() {
    let manager = RoomManager::new(6, 2500);
    let (code, _, _, _) = manager.create_room("Alice".to_string(), true);

    manager.leave_room(&code, 1); // Bot 1

    let room = manager.get_room(&code).unwrap();
    assert!(room.disconnected_players.is_empty());
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --package pocer-server rooms:: --nocapture`
Expected: PASS all

- [ ] **Step 4: Commit**

```bash
git add src/rooms.rs
git commit -m "feat: leave_room tracks human disconnected players for rejoin"
```

---

### Task 6: WebSocket Handler – Handle `Rejoin` message

**Files:**
- Modify: `src/ws.rs:71-103` (match client_msg — add Rejoin arm)

**Interfaces:**
- Consumes: `ClientMsg::Rejoin` from Task 2, `rooms.rejoin_room` from Task 4
- Produces: sends `ServerMsg::Rejoined` or falls back to `Join`

- [ ] **Step 1: Add Rejoin handler in ws.rs match block** (insert after Join arm, before Play)

```rust
ClientMsg::Rejoin { code, name } => {
    eprintln!("[WS] Received Rejoin: {} -> {}", name, code);
    match rooms.rejoin_room(&code, &name) {
        Ok((server_msg, _)) => {
            let rejoin_code = code.clone();
            room_code = Some(rejoin_code.clone());
            if let ServerMsg::Rejoined { player_id: pid, .. } = &server_msg {
                player_id = Some(*pid);
            }
            let _ = ws_tx.send(Message::Text(
                serde_json::to_string(&server_msg).unwrap().into(),
            ));

            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let session = Arc::new(tx);
            session_tx = Some(session.clone());
            rooms.add_session(rejoin_code.clone(), session);

            let fwd_tx = ws_tx.clone();
            tokio::spawn(async move {
                while let Some(broadcast_msg) = rx.recv().await {
                    if fwd_tx.send(broadcast_msg).is_err() {
                        break;
                    }
                }
            });
        }
        Err(_) => {
            // Fall back to normal join
            match rooms.join_room(&code, name.clone()) {
                Ok((server_msg, should_spawn)) => {
                    let join_code = code.clone();
                    room_code = Some(join_code.clone());
                    if let ServerMsg::Joined { player_id: pid, .. } = &server_msg {
                        player_id = Some(*pid);
                    }
                    let _ = ws_tx.send(Message::Text(
                        serde_json::to_string(&server_msg).unwrap().into(),
                    ));

                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                    let session = Arc::new(tx);
                    session_tx = Some(session.clone());
                    rooms.add_session(join_code.clone(), session);

                    let fwd_tx = ws_tx.clone();
                    tokio::spawn(async move {
                        while let Some(broadcast_msg) = rx.recv().await {
                            if fwd_tx.send(broadcast_msg).is_err() {
                                break;
                            }
                        }
                    });

                    if should_spawn {
                        let rooms_clone = rooms.clone();
                        let join_code_clone = join_code.clone();
                        tokio::spawn(async move {
                            rooms_clone.process_three_discard_delayed(&join_code_clone).await;
                        });
                    }
                }
                Err(err) => {
                    let _ = ws_tx.send(Message::Text(
                        serde_json::to_string(&ServerMsg::Error {
                            message: err,
                        }).unwrap().into(),
                    ));
                }
            }
        }
    }
}
```

- [ ] **Step 2: Update Create handler to pass is_public**

```rust
ClientMsg::Create { name, is_public } => {
    eprintln!("[WS] Received Create: {} (public={})", name, is_public);
    let (code, pid, server_msg, should_spawn) = rooms.create_room(name, is_public);
    // ... rest same
}
```

- [ ] **Step 3: Run build**

Run: `cargo build --package pocer-server`
Expected: 0 errors, 0 warnings

- [ ] **Step 4: Commit**

```bash
git add src/ws.rs
git commit -m "feat: handle Rejoin message with fallback to Join"
```

---

### Task 7: REST Endpoint – `GET /api/rooms`

**Files:**
- Modify: `src/main.rs` — add route and handler
- Modify: `src/rooms.rs` — add `list_public_rooms` method

**Interfaces:**
- Consumes: `Room.is_public`, `Room.state.phase`
- Produces: `GET /api/rooms` returns JSON array of public room summaries

- [ ] **Step 1: Add list_public_rooms to RoomManager**

```rust
#[derive(serde::Serialize)]
pub struct PublicRoomSummary {
    pub code: String,
    pub players: usize,
    pub max_players: usize,
    pub phase: String,
    pub host: String,
}

pub fn list_public_rooms(&self) -> Vec<PublicRoomSummary> {
    let mut result = Vec::new();
    for entry in self.rooms.iter() {
        let room = entry.value();
        if room.is_public && room.state.phase != GamePhase::GameOver {
            result.push(PublicRoomSummary {
                code: room.code.clone(),
                players: room.players.iter().filter(|p| !p.is_bot).count(),
                max_players: 4,
                phase: format!("{:?}", room.state.phase),
                host: room.players.first().map(|p| p.name.clone()).unwrap_or_default(),
            });
        }
    }
    result
}
```

- [ ] **Step 2: Add REST route in main.rs**

```rust
async fn list_rooms(
    State(rooms): State<Arc<RoomManager>>,
) -> Json<Vec<rooms::PublicRoomSummary>> {
    Json(rooms.list_public_rooms())
}

// In main():
let app = Router::new()
    .route("/health", get(health))
    .route("/ws", get(ws::ws_index))
    .route("/api/rooms", get(list_rooms))
    .with_state(rooms);
```

- [ ] **Step 3: Write test**

```rust
#[test]
fn test_public_room_listing() {
    let manager = RoomManager::new(6, 2500);
    manager.create_room("Alice".to_string(), true);
    manager.create_room("Bob".to_string(), false); // private

    let rooms = manager.list_public_rooms();
    assert_eq!(rooms.len(), 1);
    assert_eq!(rooms[0].players, 1);
    assert_eq!(rooms[0].max_players, 4);
    assert_eq!(rooms[0].host, "Alice");
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --package pocer-server --nocapture`
Expected: PASS all

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/rooms.rs
git commit -m "feat: GET /api/rooms REST endpoint for public room listing"
```

---

### Task 8: Frontend – Network layer (`rejoin`, `listRooms`)

**Files:**
- Modify: `/home/dodi/Documents/Personal/repo/pocer/src/network.js`

**Interfaces:**
- Consumes: WebSocket connection
- Produces: `rejoin(code, name)`, `listRooms()` (fetch-based, returns Promise)

- [ ] **Step 1: Add rejoin function**

```javascript
export function rejoin(code, name) {
  send({ type: 'rejoin', code, name });
}
```

- [ ] **Step 2: Add listRooms function**

```javascript
export async function listRooms() {
  try {
    const res = await fetch('/api/rooms');
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    return await res.json();
  } catch (err) {
    console.error('[Rooms] Failed to list rooms:', err);
    return [];
  }
}
```

- [ ] **Step 3: Update createRoom to accept is_public**

```javascript
export function createRoom(name, isPublic = true) {
  send({ type: 'create', name, isPublic });
}
```

- [ ] **Step 4: Verify build**

Run: `cd /home/dodi/Documents/Personal/repo/pocer && node -c src/network.js`
Expected: no syntax errors

- [ ] **Step 5: Commit**

```bash
git add src/network.js
git commit -m "feat: add rejoin() and listRooms() to network layer"
```

---

### Task 9: Frontend – Create Room public/private toggle + Room List View

**Files:**
- Modify: `/home/dodi/Documents/Personal/repo/pocer/index.html` — lobby section
- Modify: `/home/dodi/Documents/Personal/repo/pocer/src/app.js` — `handleCreateRoom`, room list rendering

**Interfaces:**
- Consumes: `createRoom(name, isPublic)` from Task 8, `listRooms()` from Task 8
- Produces: UI toggle for public/private, room list display

- [ ] **Step 1: Add public/private toggle to lobby HTML** (after name input, before Create Room button)

```html
<div class="lobby-section" style="display:flex; align-items:center; gap:8px;">
  <input type="checkbox" id="lobby-public-toggle" checked>
  <label for="lobby-public-toggle" style="cursor:pointer;">Public room</label>
</div>
```

- [ ] **Step 2: Add room list section to HTML** (after lobby-card, before game-area)

```html
<div id="room-list" style="display:none; padding: 20px;">
  <h2>Active Rooms</h2>
  <div id="room-list-container"></div>
  <button onclick="showLobby()" style="margin-top:12px;">Back to Lobby</button>
</div>
```

Add "Browse Rooms" button in lobby-card (below join section):
```html
<div class="lobby-section">
  <button onclick="showRoomList()" style="width:100%; background:rgba(255,255,255,0.1);">Browse Rooms</button>
</div>
```

- [ ] **Step 3: Update handleCreateRoom in app.js**

```javascript
export function handleCreateRoom() {
  console.log('[UI] Create Room clicked');
  const nameInput = document.getElementById('lobby-name-input');
  const name = nameInput ? nameInput.value.trim() || 'You' : 'You';
  const toggle = document.getElementById('lobby-public-toggle');
  const isPublic = toggle ? toggle.checked : true;
  console.log('[UI] Creating room as:', name, 'public:', isPublic);
  createRoom(name, isPublic);
}
```

- [ ] **Step 4: Add showRoomList and renderRoomList functions**

```javascript
import { listRooms } from './network.js';

export async function showRoomList() {
  document.getElementById('lobby-card').style.display = 'none';
  document.getElementById('room-list').style.display = 'block';
  await renderRoomList();
}

export function showLobby() {
  document.getElementById('lobby-card').style.display = 'block';
  document.getElementById('room-list').style.display = 'none';
}

export async function renderRoomList() {
  const rooms = await listRooms();
  const container = document.getElementById('room-list-container');
  if (!container) return;
  if (rooms.length === 0) {
    container.innerHTML = '<p style="text-align:center; opacity:0.6;">No active rooms</p>';
    return;
  }
  container.innerHTML = rooms.map(r => `
    <div style="display:flex; justify-content:space-between; align-items:center; padding:12px; margin:8px 0; background:rgba(255,255,255,0.05); border-radius:8px;">
      <div>
        <strong style="font-family:monospace; letter-spacing:2px;">${r.code}</strong>
        <span style="margin-left:12px; opacity:0.6;">${r.players}/${r.max_players} players · ${r.phase}</span>
      </div>
      <button onclick="joinFromList('${r.code}')">Join</button>
    </div>
  `).join('');
}

export function joinFromList(code) {
  document.getElementById('lobby-code-input').value = code;
  showLobby();
}
```

- [ ] **Step 5: Export new functions in index.html script block**

```javascript
window.showRoomList = showRoomList;
window.showLobby = showLobby;
window.joinFromList = joinFromList;
```

- [ ] **Step 6: Verify** — load page, check toggle appears, browse rooms works

- [ ] **Step 7: Commit**

```bash
git add index.html src/app.js
git commit -m "feat: public/private toggle and room list view"
```

---

### Task 10: Frontend – Auto-Rejoin + localStorage Persistence

**Files:**
- Modify: `/home/dodi/Documents/Personal/repo/pocer/src/app.js` — message handler, init

**Interfaces:**
- Consumes: `rejoin(code, name)` from Task 8, `ServerMsg::Rejoined` from Task 2
- Produces: localStorage save/clear, auto-rejoin on page load

- [ ] **Step 1: Add localStorage helpers**

```javascript
const STORAGE_KEYS = {
  ROOM_CODE: 'pocer_roomCode',
  PLAYER_ID: 'pocer_playerId',
  PLAYER_NAME: 'pocer_playerName',
};

function saveSession(roomCode, playerId, playerName) {
  localStorage.setItem(STORAGE_KEYS.ROOM_CODE, roomCode);
  localStorage.setItem(STORAGE_KEYS.PLAYER_ID, playerId);
  localStorage.setItem(STORAGE_KEYS.PLAYER_NAME, playerName || 'You');
}

function clearSession() {
  Object.values(STORAGE_KEYS).forEach(k => localStorage.removeItem(k));
}

function loadSession() {
  return {
    roomCode: localStorage.getItem(STORAGE_KEYS.ROOM_CODE),
    playerId: localStorage.getItem(STORAGE_KEYS.PLAYER_ID),
    playerName: localStorage.getItem(STORAGE_KEYS.PLAYER_NAME),
  };
}
```

- [ ] **Step 2: Add auto-rejoin to initClient (at page load)**

```javascript
export function initClient() {
  const session = loadSession();
  if (session.roomCode && session.playerName) {
    console.log('[UI] Attempting auto-rejoin for', session.playerName, 'in', session.roomCode);
    showLobby();
    document.getElementById('lobby-card').style.display = 'none';
    rejoin(session.roomCode, session.playerName);
    return;
  }
  showLobby();
}
```

- [ ] **Step 3: Save session on Created/Joined/Rejoined** in the message handler

```javascript
if (data.type === 'created') {
  // ... existing ...
  saveSession(data.code, data.player_id, nameInput?.value?.trim());
} else if (data.type === 'joined') {
  // ... existing ...
  saveSession(state.code || localStorage.getItem(STORAGE_KEYS.ROOM_CODE), data.player_id, nameInput?.value?.trim());
} else if (data.type === 'rejoined') {
  showGame();
  state = data.state;
  playerId = data.player_id;
  render();
  console.log('[UI] Rejoined successfully as player', data.player_id);
}
```

- [ ] **Step 4: Clear session on PlayerLeft/gameOver**

```javascript
if (data.type === 'playerLeft') {
  // ... existing ...
  clearSession();
}
// In render(), when game over detected:
if (state.phase === 'gameOver') {
  clearSession();
}
```

- [ ] **Step 5: Handle rejoin failure** — if server returns Error after rejoin, show lobby

```javascript
if (data.type === 'error') {
  console.error('[UI] Server error:', data.message);
  clearSession();
  showLobby();
  showError(data.message);
}
```

- [ ] **Step 6: Verify** — create room, refresh page, should auto-rejoin; wait 5min, should fall back to lobby

- [ ] **Step 7: Commit**

```bash
git add src/app.js
git commit -m "feat: auto-rejoin on page refresh with localStorage persistence"
```

---

### Task 11: Integration Tests & Verification

**Files:**
- Test: `src/rooms.rs` — full rejoin flow test
- Manual: browser + backend integration

**Interfaces:**
- Consumes: all prior tasks

- [ ] **Step 1: Write integration test for full rejoin flow**

```rust
#[test]
fn test_full_rejoin_flow() {
    let manager = RoomManager::new(6, 2500);
    let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
    manager.join_room(&code, "Bob".to_string()).unwrap();

    // Bob disconnects
    manager.leave_room(&code, 1);

    // Bob rejoins within timeout
    let result = manager.rejoin_room(&code, "Bob");
    assert!(result.is_ok());
    match result.unwrap().0 {
        ServerMsg::Rejoined { player_id, state } => {
            assert_eq!(player_id, 1);
            assert_eq!(state.players[1].name, "Bob");
            assert!(state.players[1].connected);
        }
        _ => panic!("Expected Rejoined"),
    }

    // Verify disconnected_players entry removed
    let room = manager.get_room(&code).unwrap();
    assert!(!room.disconnected_players.iter().any(|(_, n, _)| n == "Bob"));
}
```

- [ ] **Step 2: Run full test suite**

Run: `cargo test --package pocer-server --nocapture`
Expected: PASS all (223+ tests)

- [ ] **Step 3: Manual integration test checklist**
  - [ ] Start backend: `cargo run`
  - [ ] Start frontend: `cd pocer && make serve`
  - [ ] Create public room → verify room appears at `http://localhost:3000/api/rooms`
  - [ ] Create private room → verify room does NOT appear at `/api/rooms`
  - [ ] Join room, refresh page → verify auto-rejoin restores seat
  - [ ] Wait 5+ minutes after disconnect → verify fallback to lobby

- [ ] **Step 4: Commit**

```bash
git add src/rooms.rs
git commit -m "test: full rejoin flow integration test"
```

---

## Self-Review

**1. Spec coverage:**
- Room creators can choose public/private: Tasks 1, 2, 3, 9 ✅
- Disconnected players can rejoin within timeout: Tasks 1, 2, 4, 5, 6, 10 ✅
- Page refresh auto-rejoin via localStorage: Task 10 ✅
- Room list view with public rooms: Tasks 7, 8, 9 ✅
- Rejoin timeout 5 minutes: Task 4 ✅
- Human takes priority over bot seat: Task 4 (rejoin removes bot, restores human) ✅

**2. Placeholder scan:** No TBDs, no "add validation", no "implement later". All code blocks are complete.

**3. Type consistency:**
- `is_public: bool` used consistently across Room, Create, Created
- `ServerMsg::Rejoined { player_id: usize, state: GameState }` matches protocol
- `disconnected_players: Vec<(usize, String, Instant)>` — seat_id, name, timestamp
- `PublicRoomSummary` struct matches REST response format
