use axum::{
    extract::{State, WebSocketUpgrade, ws::{Message, WebSocket}},
    response::Response,
};
use futures_util::{StreamExt, SinkExt};
use std::sync::Arc;
use crate::protocol::{ClientMsg, ServerMsg};
use crate::rooms::RoomManager;

fn spawn_broadcast_forwarder(ws_tx: tokio::sync::mpsc::UnboundedSender<Message>, mut rx: tokio::sync::mpsc::UnboundedReceiver<Message>) {
    let fwd_tx = ws_tx.clone();
    tokio::spawn(async move {
        while let Some(broadcast_msg) = rx.recv().await {
            if fwd_tx.send(broadcast_msg).is_err() {
                break;
            }
        }
    });
}

pub async fn ws_index(
    State(rooms): State<Arc<RoomManager>>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket: WebSocket| handle_ws(socket, rooms))
}

async fn handle_ws(socket: WebSocket, rooms: Arc<RoomManager>) {
    let (write, mut read) = socket.split();

    let (ws_tx, ws_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
    let mut room_code: Option<String> = None;
    let mut player_id: Option<usize> = None;
    let mut session_tx: Option<Arc<tokio::sync::mpsc::UnboundedSender<Message>>> = None;
    let mut cleaned_up = false;

    tokio::spawn(write_forward(write, ws_rx));

    while let Some(msg) = read.next().await {
        let text = match msg {
            Ok(Message::Text(t)) => t,
            Ok(Message::Binary(_)) => continue,
            Ok(Message::Close(_)) => {
                if let (Some(code), Some(pid)) = (&room_code, player_id) {
                    rooms.leave_room(code, pid).await;
                }
                cleaned_up = true;
                break;
            }
            Ok(_) => continue,
            Err(_) => {
                if let (Some(code), Some(pid)) = (&room_code, player_id) {
                    rooms.leave_room(code, pid).await;
                }
                cleaned_up = true;
                break;
            }
        };

        let client_msg: ClientMsg = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(_) => {
                let _ = ws_tx.send(Message::Text(
                    serde_json::to_string(&ServerMsg::Error {
                        message: "Invalid message format".to_string(),
                    }).unwrap().into(),
                ));
                continue;
            }
        };

        match client_msg {
            ClientMsg::Create { name, is_public } => {
                let (code, pid, server_msg) = rooms.create_room(name, is_public).await;
                room_code = Some(code.clone());
                player_id = Some(pid);

                let _ = ws_tx.send(Message::Text(
                    crate::protocol::personalise_for_viewer(&server_msg, pid).into(),
                ));

                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                let session = Arc::new(tx);
                session_tx = Some(session.clone());
                rooms.add_session(code.clone(), pid, session);

                spawn_broadcast_forwarder(ws_tx.clone(), rx);
            }
            ClientMsg::Join { code, name } => {
                match rooms.join_room(&code, name).await {
                    Ok(server_msg) => {
                        let join_code = code.to_uppercase();
                        room_code = Some(join_code.clone());
                        if let ServerMsg::Joined { player_id: pid, .. } = &server_msg {
                            player_id = Some(*pid);
                        }
                        let _ = ws_tx.send(Message::Text(
                            crate::protocol::personalise_for_viewer(&server_msg, player_id.unwrap_or(0)).into(),
                        ));

                        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                        let session = Arc::new(tx);
                        session_tx = Some(session.clone());
                        rooms.add_session(join_code.clone(), player_id.unwrap_or(0), session);

                        spawn_broadcast_forwarder(ws_tx.clone(), rx);
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
            ClientMsg::Play { cards } => {
                if room_code.is_none() || player_id.is_none() {
                    continue;
                }
                let code = room_code.clone().unwrap();
                let pid = player_id.unwrap();
                let rooms_clone = rooms.clone();
                tokio::spawn(async move {
                    let _ = rooms_clone.apply_human_move(&code, pid, Some(cards)).await;
                });
            }
            ClientMsg::Pass => {
                if room_code.is_none() || player_id.is_none() {
                    continue;
                }
                let code = room_code.clone().unwrap();
                let pid = player_id.unwrap();
                let rooms_clone = rooms.clone();
                tokio::spawn(async move {
                    let _ = rooms_clone.apply_human_move(&code, pid, None).await;
                });
            }
            ClientMsg::Rejoin { code, name, token } => {
                match rooms.rejoin_room(&code, &name, &token).await {
                    Ok(server_msg) => {
                        let rejoin_code = code.to_uppercase();
                        room_code = Some(rejoin_code.clone());
                        if let ServerMsg::Rejoined { player_id: pid, .. } = &server_msg {
                            player_id = Some(*pid);
                        }
                        let _ = ws_tx.send(Message::Text(
                            crate::protocol::personalise_for_viewer(&server_msg, player_id.unwrap_or(0)).into(),
                        ));

                        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                        let session = Arc::new(tx);
                        session_tx = Some(session.clone());
                        rooms.add_session(rejoin_code.clone(), player_id.unwrap_or(0), session);

                        spawn_broadcast_forwarder(ws_tx.clone(), rx);
                    }
                    Err(err) => {
                        // Forward the specific reason: "Seat already in use by
                        // another window" (second tab holds the seat) needs a
                        // different client reaction than a genuinely gone
                        // seat. The client matches the in-use string; any
                        // other message keeps the old retry-then-clear path.
                        let _ = ws_tx.send(Message::Text(
                            serde_json::to_string(&ServerMsg::Error {
                                message: err,
                            }).unwrap().into(),
                        ));
                    }
                }
            }
            ClientMsg::Ready { ready } => {
                if room_code.is_none() || player_id.is_none() {
                    continue;
                }
                let code = room_code.clone().unwrap();
                let pid = player_id.unwrap();
                match rooms.ready_player(&code, pid, ready).await {
                    Ok(server_msg) => {
                        // The manager already broadcast PlayerReady to the
                        // whole room (local + other pods); reply only to the
                        // toggler.
                        let _ = ws_tx.send(Message::Text(
                            serde_json::to_string(&server_msg).unwrap().into(),
                        ));
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
            ClientMsg::SetRoomSettings { play_limit_secs, winning_point } => {
                if room_code.is_none() || player_id.is_none() {
                    continue;
                }
                let code = room_code.clone().unwrap();
                let pid = player_id.unwrap();
                match rooms.set_room_settings(&code, pid, play_limit_secs, winning_point).await {
                    Ok(server_msg) => {
                        // The manager already broadcast RoomSettings to the
                        // whole room so every client stays in sync; reply only
                        // to the host.
                        let _ = ws_tx.send(Message::Text(
                            serde_json::to_string(&server_msg).unwrap().into(),
                        ));
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
            ClientMsg::StartGame => {
                if room_code.is_none() || player_id.is_none() {
                    continue;
                }
                let code = room_code.clone().unwrap();
                let pid = player_id.unwrap();
                match rooms.start_game(&code, pid).await {
                    Ok(server_msg) => {
                        // GameStarted + State were already broadcast by the
                        // manager; the host's own reply carries the (masked)
                        // state for the start screen.
                        let _ = ws_tx.send(Message::Text(
                            crate::protocol::personalise_for_viewer(&server_msg, pid).into(),
                        ));
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
            ClientMsg::LeaveRoom => {
                if let (Some(code), Some(pid)) = (&room_code, player_id) {
                    if let Some(msg) = rooms.remove_player(&code, pid).await {
                        let _ = ws_tx.send(Message::Text(
                            serde_json::to_string(&msg).unwrap().into(),
                        ));
                    }
                }
                cleaned_up = true;
                break;
            }
            ClientMsg::CheckRoom { code, token } => {
                // Read-only: no seat is touched, so the user can still click
                // the (still visible) Rejoin item right afterwards.
                let (found, rejoinable) = rooms.check_room(&code, &token).await;
                let _ = ws_tx.send(Message::Text(
                    serde_json::to_string(&ServerMsg::RoomStatus {
                        code: code.to_uppercase(),
                        found,
                        rejoinable,
                    }).unwrap().into(),
                ));
            }
            ClientMsg::Ping => {
                let _ = ws_tx.send(Message::Text(
                    serde_json::to_string(&ServerMsg::Pong).unwrap().into(),
                ));
            }
        }
    }

    if !cleaned_up {
        if let (Some(code), Some(pid)) = (&room_code, player_id) {
            rooms.leave_room(code, pid).await;
        }
    }
    if let (Some(code), Some(tx)) = (&room_code, &session_tx) {
        rooms.remove_session(code, tx);
    }
}

async fn write_forward(mut write: impl futures_util::Sink<Message> + Unpin, mut rx: tokio::sync::mpsc::UnboundedReceiver<Message>) {
    while let Some(msg) = rx.recv().await {
        if write.send(msg).await.is_err() {
            break;
        }
    }
}
