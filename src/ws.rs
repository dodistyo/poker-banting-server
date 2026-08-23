use axum::{
    extract::{State, WebSocketUpgrade, ws::{Message, WebSocket}},
    response::Response,
};
use futures_util::{StreamExt, SinkExt};
use std::sync::Arc;
use crate::protocol::{ClientMsg, ServerMsg};
use crate::rooms::RoomManager;
use crate::game::rules::process_one_bot_turn;
use crate::game::state::GamePhase;

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
                    rooms.leave_room(code, pid);
                }
                cleaned_up = true;
                break;
            }
            Ok(_) => continue,
            Err(_) => {
                if let (Some(code), Some(pid)) = (&room_code, player_id) {
                    rooms.leave_room(code, pid);
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

                let (code, pid, server_msg, should_spawn) = rooms.create_room(name, is_public);
                room_code = Some(code.clone());
                player_id = Some(pid);

                let _ = ws_tx.send(Message::Text(
                    serde_json::to_string(&server_msg).unwrap().into(),
                ));

                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                let session = Arc::new(tx);
                session_tx = Some(session.clone());
                rooms.add_session(code.clone(), pid, session);

                spawn_broadcast_forwarder(ws_tx.clone(), rx);

                if should_spawn {
                    let rooms_clone = rooms.clone();
                    let code_clone = code.clone();
                    tokio::spawn(async move {
                        rooms_clone.process_three_discard_delayed(&code_clone).await;
                    });
                }
            }
            ClientMsg::Join { code, name } => {

                match rooms.join_room(&code, name) {
                    Ok((server_msg, should_spawn)) => {
                        let join_code = code.clone();
                        room_code = Some(join_code.clone());
                        if let ServerMsg::Joined { player_id: pid, .. } = &server_msg {
                            player_id = Some(*pid);
                        }
                        let _ = ws_tx.send(Message::Text(
                            serde_json::to_string(&server_msg).unwrap().into(),
                        ));

                        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                        let session = Arc::new(tx);
                        session_tx = Some(session.clone());
                        rooms.add_session(join_code.clone(), player_id.unwrap_or(0), session);

                        spawn_broadcast_forwarder(ws_tx.clone(), rx);

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
            ClientMsg::Play { cards } => {
                if room_code.is_none() || player_id.is_none() {
                    continue;
                }
                let code = room_code.clone().unwrap();
                let pid = player_id.unwrap();
                let rooms_clone = rooms.clone();
                tokio::spawn(async move {
                    handle_play(&rooms_clone, &code, pid, cards).await;
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
                    handle_pass(&rooms_clone, &code, pid).await;
                });
            }
            ClientMsg::Rejoin { code, name, token } => {

                match rooms.rejoin_room(&code, &name, &token) {
                    Ok((server_msg, _)) => {
                        let rejoin_code = code.clone();
                        room_code = Some(rejoin_code.clone());
                        if let ServerMsg::Rejoined { player_id: pid, .. } = &server_msg {
                            player_id = Some(*pid);
                        }
                        let _ = ws_tx.send(Message::Text(
                            serde_json::to_string(&server_msg).unwrap().into(),
                        ));

                        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                        let session = Arc::new(tx);
                        session_tx = Some(session.clone());
                        rooms.add_session(rejoin_code.clone(), player_id.unwrap_or(0), session);

                        spawn_broadcast_forwarder(ws_tx.clone(), rx);
                    }
            Err(_) => {

                        let _ = ws_tx.send(Message::Text(
                            serde_json::to_string(&ServerMsg::Error {
                                message: "Rejoin failed. Please create or join a new room.".to_string(),
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
                match rooms.ready_player(&code, pid, ready) {
                    Ok(server_msg) => {
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
                match rooms.start_game(&code, pid) {
                    Ok((server_msg, should_spawn)) => {
                        let _ = ws_tx.send(Message::Text(
                            serde_json::to_string(&server_msg).unwrap().into(),
                        ));
                        if should_spawn {
                            let rooms_clone = rooms.clone();
                            let code_clone = code.clone();
                            tokio::spawn(async move {
                                rooms_clone.process_three_discard_delayed(&code_clone).await;
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
            ClientMsg::LeaveRoom => {
                if let (Some(code), Some(pid)) = (&room_code, player_id) {
                    if let Some(msg) = rooms.remove_player(&code, pid) {
                        let _ = ws_tx.send(Message::Text(
                            serde_json::to_string(&msg).unwrap().into(),
                        ));
                    }
                }
                cleaned_up = true;
                break;
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
            rooms.leave_room(code, pid);
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

async fn handle_play(
    rooms: &Arc<RoomManager>,
    code: &str,
    player_id: usize,
    card_ids: Vec<String>,
) {
    tokio::task::yield_now().await;
    let state = match rooms.get_state(code) {
        Some(s) => s,
        None => return,
    };

    let mut engine = crate::game::engine::GameEngine::new(state);
    engine.apply_play(player_id, &card_ids);
    rooms.update_state(code, engine.state().clone());
    broadcast_state(rooms, code);
    process_bot_turns_delayed(rooms, code).await;
}

async fn handle_pass(
    rooms: &Arc<RoomManager>,
    code: &str,
    player_id: usize,
) {
    tokio::task::yield_now().await;
    let state = match rooms.get_state(code) {
        Some(s) => s,
        None => return,
    };

    let mut engine = crate::game::engine::GameEngine::new(state);
    engine.apply_pass(player_id);
    rooms.update_state(code, engine.state().clone());
    broadcast_state(rooms, code);
    process_bot_turns_delayed(rooms, code).await;
}

async fn process_bot_turns_delayed(rooms: &Arc<RoomManager>, code: &str) {
    loop {
        let mut state = match rooms.get_state(code) {
            Some(s) if s.phase == GamePhase::Playing => s,
            _ => return,
        };
        let needs_more = process_one_bot_turn(&mut state);
        rooms.update_state(code, state.clone());
        broadcast_state(rooms, code);
        if !needs_more {
            break;
        }
        let delay_ms = rooms.get_bot_turn_delay_ms();
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    }
    let mut state = match rooms.get_state(code) {
        Some(s) if s.phase == GamePhase::Playing => s,
        _ => return,
    };
    crate::game::rules::skip_finished(&mut state);
    rooms.update_state(code, state.clone());
    broadcast_state(rooms, code);
}

fn broadcast_state(rooms: &Arc<RoomManager>, code: &str) {
    if let Some(state) = rooms.get_state(code) {
        rooms.broadcast(code, ServerMsg::State { state });
    }
}
