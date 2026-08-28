# Pocer Server

Real-time card game server for Pocer (Poker Banting). WebSocket-based multiplayer with room management, bot support, and combo detection.

## Tech Stack

- **Runtime:** Rust (edition 2024)
- **Web Framework:** axum 0.8
- **WebSocket:** axum `ws` feature
- **Async:** tokio
- **State:** dashmap (concurrent room storage)

## Prerequisites

- Rust 1.75+ (edition 2024)
- [just](https://just.systems/man/en/) (command runner)
- [cargo-watch](https://github.com/watchexec/cargo-watch) (dev hot reload)

```bash
cargo install cargo-watch
```

## Quick Start

```bash
# Enter server directory
cd server

# Dev mode with hot reload
just dev

# Or directly
cargo watch -x run
```

The server starts on `0.0.0.0:8080` by default.

## Configuration

Environment variables (or `.env` file):

| Variable | Default | Description |
|---|---|---|
| `SERVER_HOST` | `0.0.0.0` | Bind address |
| `SERVER_PORT` | `8080` | Bind port |
| `ROOM_CODE_LENGTH` | `6` | Length of generated room codes |
| `ROOM_TTL_SECONDS` | `1800` | Room expiration time |
| `MAX_PLAYERS` | `4` | Max players per room |
| `DISCONNECT_TIMEOUT_SEC` | `15` | Seconds before a disconnected player is removed |

## Commands

```bash
just dev      # Hot reload dev server
just build    # Release build
just test     # Run tests
just check    # Clippy (deny warnings)
just fmt      # Format code
just clean    # Clean target dir
```

## Endpoints

- `GET /health` — Health check with room count
- `GET /ws` — WebSocket endpoint for game clients

## Project Structure

```
src/
├── main.rs        # Entry point, HTTP server setup
├── lib.rs         # Library crate root
├── config.rs      # Environment configuration
├── protocol.rs    # WebSocket message types
├── rooms.rs       # Room manager (create, join, cleanup)
├── ws.rs          # WebSocket handler
└── game/
    ├── mod.rs     # Game module
    ├── state.rs   # Game state machine
    ├── engine.rs  # Game engine (trick flow, turns)
    ├── rules.rs   # Game rules engine
    ├── card.rs    # Card and deck logic
    ├── combo.rs   # Hand evaluation / combos
    └── bot.rs     # AI bot player
```

## License

MIT
