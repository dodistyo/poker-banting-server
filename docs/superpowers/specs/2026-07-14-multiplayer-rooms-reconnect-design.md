# Multiplayer: Public/Private Rooms + Reconnect

**Date:** 2026-07-14
**Status:** Approved

## Problem

1. All rooms are effectively private (join by code only). No way to discover active games.
2. When a player disconnects and refreshes, they must re-enter the room code and join as a new player instead of resuming their original seat.

## Goals

- Room creators can choose public (listed) or private (code-only) visibility
- Disconnected players can rejoin their original seat within a timeout window
- Page refresh mid-game attempts automatic rejoin via localStorage

## Non-Goals

- Room password protection
- Kick / transfer host
- Spectator mode
- Chat
- Turn timer

## Design

### Room Model Changes

`Room` struct adds:
- `is_public: bool` — default `true`
- `disconnected_players: Vec<(usize, String, std::time::Instant)>` — tracks (seat_index, name, disconnect_time)

### Protocol Changes

**Client → Server:**
- `ClientMsg::Create { name, is_public }` — new `is_public` field (default `true` if omitted)
- `ClientMsg::Rejoin { code, name }` — rejoin with original name

**Server → Client:**
- `ServerMsg::Created { code, player_id, state, is_public }` — confirms visibility
- `ServerMsg::Rejoined { player_id, state }` — restored seat response

### Rejoin Logic

On `Rejoin { code, name }`:
1. Find room by code (case-insensitive)
2. Scan `disconnected_players` for matching `name` within 5-minute timeout
3. If found: restore original `player_id`, mark `connected = true`, remove from `disconnected_players`, broadcast `PlayerJoined`, return `Rejoined`
4. If not found or seat already taken: fall back to normal `Join` logic (replace bot or add new player)

### Frontend Changes

**Create Room:**
- Toggle/checkbox for public vs private room
- Default: public

**Room List View:**
- New page showing active public rooms: code, player count, phase
- "Join" button auto-fills code and navigates to join flow
- Fetches via `GET /api/rooms` (REST)

**Auto-Rejoin:**
- On page load, check `localStorage` for `pocer_roomCode` and `pocer_playerId`
- If present, send `Rejoin { code, name }` instead of `Join`
- On successful rejoin, restore game state
- On failure (room gone, timeout expired), fall back to lobby

**Persistence:**
- On `Created` or `Joined`: save `roomCode`, `playerId`, `name` to localStorage
- On `PlayerLeft` or game over: clear localStorage

### Backend REST Endpoint

`GET /api/rooms` — returns list of public rooms:
```json
[
  { "code": "ABC123", "players": 3, "max_players": 4, "phase": "playing", "host": "Alice" }
]
```

Only returns rooms where `is_public = true` and `phase != GameOver`.

### Reconnect Timeout

- Default: 5 minutes
- After timeout, `disconnected_players` entry is cleaned up on next room state access
- If a bot fills the seat during timeout, rejoin still restores human seat (human takes priority)

## Testing

- Unit: `test_rejoin_restores_seat`, `test_rejoin_timeout_expired`, `test_public_room_listing`
- Integration: disconnect → rejoin within timeout → verify same `player_id` and hand
- Integration: refresh mid-game → auto-rejoin → state matches

## Files Affected

**Backend:**
- `src/game/state.rs` — `Room` struct additions
- `src/protocol.rs` — new message variants
- `src/rooms.rs` — rejoin logic, REST handler
- `src/ws.rs` — handle `Rejoin`, cleanup disconnected on ping/pong failure
- `src/main.rs` — REST route for `/api/rooms`

**Frontend:**
- `src/network.js` — `rejoin()`, `listRooms()`
- `src/app.js` — auto-rejoin on load, room list view, public/private toggle
- `index.html` — room list UI, toggle in create room form
