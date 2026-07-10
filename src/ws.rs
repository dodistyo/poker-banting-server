use axum::{
    extract::{State, WebSocketUpgrade, ws::{Message, WebSocket}},
    response::Response,
};
use futures_util::{StreamExt, SinkExt};
use std::sync::Arc;
use crate::protocol::{ClientMsg, ServerMsg};
use crate::rooms::RoomManager;
use crate::game::rules::{validate_play, resolve_trick, end_game, check_trick_complete, non_participants, process_one_bot_turn};
use crate::game::state::GamePhase;
use crate::game::combo;

pub async fn ws_index(
    State(rooms): State<Arc<RoomManager>>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket: WebSocket| handle_ws(socket, rooms))
}

async fn handle_ws(socket: WebSocket, rooms: Arc<RoomManager>) {
    eprintln!("[WS] New connection established");
    let (write, mut read) = socket.split();

    let (ws_tx, ws_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
    let mut room_code: Option<String> = None;
    let mut player_id: Option<usize> = None;
    let mut session_tx: Option<Arc<tokio::sync::mpsc::UnboundedSender<Message>>> = None;

    tokio::spawn(write_forward(write, ws_rx));
    eprintln!("[WS] write_forward spawned, starting read loop");

    while let Some(msg) = read.next().await {
        let text = match msg {
            Ok(Message::Text(t)) => {
                eprintln!("[WS] Received text: {}", t);
                t
            }
            Ok(Message::Binary(_)) => {
                eprintln!("[WS] Binary messages not supported");
                continue;
            }
            Ok(Message::Close(_)) => {
                eprintln!("[WS] Close received");
                if let (Some(code), Some(tx)) = (&room_code, &session_tx) {
                    rooms.remove_session(code, tx);
                }
                break;
            }
            Ok(_) => continue,
            Err(e) => {
                eprintln!("[WS] Error reading message: {}", e);
                if let (Some(code), Some(tx)) = (&room_code, &session_tx) {
                    rooms.remove_session(code, tx);
                }
                break;
            }
        };

        let client_msg: ClientMsg = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[WS] Failed to parse message: {}", e);
                let _ = ws_tx.send(Message::Text(
                    serde_json::to_string(&ServerMsg::Error {
                        message: "Invalid message format".to_string(),
                    }).unwrap().into(),
                ));
                continue;
            }
        };

        match client_msg {
            ClientMsg::Create { name } => {
                eprintln!("[WS] Received Create: {}", name);
                let (code, pid, server_msg, should_spawn) = rooms.create_room(name);
                room_code = Some(code.clone());
                player_id = Some(pid);

                let _ = ws_tx.send(Message::Text(
                    serde_json::to_string(&server_msg).unwrap().into(),
                ));

                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let session = Arc::new(tx);
                session_tx = Some(session.clone());
                rooms.add_session(code.clone(), session);

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
                    let code_clone = code.clone();
                    tokio::spawn(async move {
                        rooms_clone.process_three_discard_delayed(&code_clone).await;
                    });
                }
            }
            ClientMsg::Join { code, name } => {
                eprintln!("[WS] Received Join: {} -> {}", name, code);
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
            ClientMsg::Play { cards } => {
                if room_code.is_none() || player_id.is_none() {
                    eprintln!("[WS] Play received but not in a room");
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
                    eprintln!("[WS] Pass received but not in a room");
                    continue;
                }
                let code = room_code.clone().unwrap();
                let pid = player_id.unwrap();
                let rooms_clone = rooms.clone();
                tokio::spawn(async move {
                    handle_pass(&rooms_clone, &code, pid).await;
                });
            }
            ClientMsg::Ping => {
                let _ = ws_tx.send(Message::Text(
                    serde_json::to_string(&ServerMsg::Pong).unwrap().into(),
                ));
            }
        }
    }

    eprintln!("[WS] Connection closed");
    if let (Some(code), Some(tx)) = (&room_code, &session_tx) {
        rooms.remove_session(code, tx);
    }
}

async fn write_forward(mut write: impl futures_util::Sink<Message> + Unpin, mut rx: tokio::sync::mpsc::UnboundedReceiver<Message>) {
    eprintln!("[WS] write_forward: starting");
    while let Some(msg) = rx.recv().await {
        eprintln!("[WS] write_forward: sending message");
        if write.send(msg).await.is_err() {
            eprintln!("[WS] write_forward: send failed, exiting");
            break;
        }
    }
    eprintln!("[WS] write_forward: exited cleanly");
}

async fn handle_play(
    rooms: &Arc<RoomManager>,
    code: &str,
    player_id: usize,
    card_ids: Vec<String>,
) {
    tokio::task::yield_now().await;
    let mut state = match rooms.get_state(code) {
        Some(s) => s,
        None => return,
    };

    if state.current_player != player_id {
        eprintln!("[PLAY] Not player {}'s turn, current={}", player_id, state.current_player);
        return;
    }

    if state.finished_order.len() >= 3 {
        eprintln!("[PLAY] 3 players already finished, game over");
        for i in 0..4 {
            if !state.players[i].finished {
                state.scores[i] = -15;
                state.finished_order.push(i);
                break;
            }
        }
        state.phase = GamePhase::GameOver;
        rooms.update_state(code, state.clone());
        broadcast_state(rooms, code);
        return;
    }

    let player = &state.players[player_id];

    let mut cards = Vec::new();
    let mut indices_to_remove = Vec::new();
    for card_id in &card_ids {
        let parts: Vec<&str> = card_id.splitn(2, ':').collect();
        if parts.len() != 2 {
            eprintln!("[PLAY] Invalid card id: {}", card_id);
            return;
        }
        let (rank_str, suit_str) = (parts[0], parts[1]);

        let rank = match rank_str {
            "3" => crate::game::card::Rank::Three,
            "4" => crate::game::card::Rank::Four,
            "5" => crate::game::card::Rank::Five,
            "6" => crate::game::card::Rank::Six,
            "7" => crate::game::card::Rank::Seven,
            "8" => crate::game::card::Rank::Eight,
            "9" => crate::game::card::Rank::Nine,
            "10" => crate::game::card::Rank::Ten,
            "J" => crate::game::card::Rank::Jack,
            "Q" => crate::game::card::Rank::Queen,
            "K" => crate::game::card::Rank::King,
            "A" => crate::game::card::Rank::Ace,
            "2" => crate::game::card::Rank::Two,
            _ => { eprintln!("[PLAY] Unknown rank: {}", rank_str); return; }
        };

        let suit = match suit_str {
            "diamonds" => crate::game::card::Suit::Diamonds,
            "clubs" => crate::game::card::Suit::Clubs,
            "hearts" => crate::game::card::Suit::Hearts,
            "spades" => crate::game::card::Suit::Spades,
            _ => { eprintln!("[PLAY] Unknown suit: {}", suit_str); return; }
        };

        let found = player.hand.iter().position(|c| c.rank == rank && c.suit == suit);
        match found {
            Some(idx) => {
                cards.push(player.hand[idx].clone());
                indices_to_remove.push(idx);
            }
            None => {
                eprintln!("[PLAY] Card {} not in player {}'s hand", card_id, player_id);
                return;
            }
        }
    }

    let table_combo = if state.trick.combo_player.is_some() {
        combo::detect_combo(&state.trick.cards)
    } else {
        None
    };

    let result = validate_play(&cards, table_combo.as_ref());

    if !result.valid {
        eprintln!("[PLAY] Invalid play by player {}: {}", player_id, result.error);
        state.log.push(format!("{}'s play is invalid: {}", state.players[player_id].name, result.error));
        rooms.update_state(code, state.clone());
        broadcast_state(rooms, code);
        return;
    }

    let hand = &mut state.players[player_id].hand;
    let mut sorted_indices = indices_to_remove;
    sorted_indices.sort_unstable();
    for &i in sorted_indices.iter().rev() {
        hand.remove(i);
    }

    if state.players[player_id].hand.is_empty() && !state.players[player_id].finished {
        state.players[player_id].finished = true;
        state.finished_order.push(player_id);
        let pos = state.finished_order.len();
        state.scores[player_id] = if pos == 1 { 10 } else if pos == 2 { 5 } else if pos == 3 { 0 } else { -15 };
        eprintln!("[PLAY] Player {} finished (empty hand), position: {}", player_id, pos);
    }

    if let Some(old_cp) = state.trick.combo_player {
        if !state.trick.played.contains(&old_cp) && !state.trick.passed.contains(&old_cp) {
            state.trick.played.push(old_cp);
        }
        state.trick.passed.clear();
    }
    let card_labels: Vec<String> = cards.iter().map(|c| c.to_string()).collect();
    state.trick.cards = cards;
    state.trick.combo_type = Some(result.combo.as_ref().unwrap().combo_type.clone());
    state.trick.combo_player = Some(player_id);

    state.log.push(format!("{} plays {} ({})", state.players[player_id].name, card_labels.join(" "), result.combo_name));

    if let Some(winner) = check_trick_complete(&state) {
        resolve_trick(&mut state, winner);
        if end_game(&mut state) {
            for i in 0..4 {
                if !state.players[i].finished {
                    state.finished_order.push(i);
                    break;
                }
            }
        }
    } else if state.finished_order.len() >= 3 {
        for i in 0..4 {
            if !state.players[i].finished {
                state.scores[i] = -15;
                state.finished_order.push(i);
                break;
            }
        }
        state.phase = GamePhase::GameOver;
    } else {
        state.current_player = (state.current_player + 1) % 4;
    }

    rooms.update_state(code, state.clone());
    broadcast_state(rooms, code);
    process_bot_turns_delayed(rooms, code).await;
}

async fn handle_pass(
    rooms: &Arc<RoomManager>,
    code: &str,
    player_id: usize,
) {
    tokio::task::yield_now().await;
    let mut state = match rooms.get_state(code) {
        Some(s) => s,
        None => return,
    };

    if state.current_player != player_id {
        eprintln!("[PASS] Not this player's turn: {} != {}", state.current_player, player_id);
        return;
    }

    if state.finished_order.len() >= 3 {
        eprintln!("[PASS] 3 players already finished, game over");
        for i in 0..4 {
            if !state.players[i].finished {
                state.scores[i] = -15;
                state.finished_order.push(i);
                break;
            }
        }
        state.phase = GamePhase::GameOver;
        rooms.update_state(code, state.clone());
        broadcast_state(rooms, code);
        return;
    }

    if state.trick.combo_player.is_none() {
        eprintln!("[PASS] No combo player yet");
        return;
    }

    if state.trick.combo_player == Some(player_id) {
        eprintln!("[PASS] Combo player cannot pass");
        return;
    }

    eprintln!("[PASS] Player {} passes. Current combo: {:?}, passed: {:?}", player_id, state.trick.combo_player, state.trick.passed);

    state.trick.passed.push(player_id);
    state.log.push(format!("{} passes", state.players[player_id].name));

    if non_participants(&state) >= 3 {
        if let Some(winner) = state.trick.combo_player {
            resolve_trick(&mut state, winner);
            if end_game(&mut state) {
                for i in 0..4 {
                    if !state.players[i].finished {
                        state.finished_order.push(i);
                        break;
                    }
                }
            }
        }
    } else if state.finished_order.len() >= 3 {
        for i in 0..4 {
            if !state.players[i].finished {
                state.scores[i] = -15;
                state.finished_order.push(i);
                break;
            }
        }
        state.phase = GamePhase::GameOver;
    } else {
        state.current_player = (state.current_player + 1) % 4;
    }

    rooms.update_state(code, state.clone());
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
