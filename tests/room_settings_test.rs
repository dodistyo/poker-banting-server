use std::sync::Arc;

use poker_banting_server::game::card::{Card, Rank, Suit};
use poker_banting_server::game::combo::{self, ComboType};
use poker_banting_server::game::rules::*;
use poker_banting_server::game::state::*;
use poker_banting_server::protocol::{ClientMsg, ServerMsg};
use poker_banting_server::rooms::RoomManager;

fn card(rank: Rank, suit: Suit) -> Card {
    Card::new(rank, suit)
}

fn make_player(id: usize, name: &str, hand: Vec<Card>) -> Player {
    Player {
        id,
        name: name.to_string(),
        hand,
        finished: false,
        is_bot: false,
        connected: true,
        is_creator: false,
    }
}

fn empty_state() -> GameState {
    GameState {
        phase: GamePhase::Playing,
        players: vec![
            make_player(0, "P0", Vec::new()),
            make_player(1, "P1", Vec::new()),
            make_player(2, "P2", Vec::new()),
            make_player(3, "P3", Vec::new()),
        ],
        ready: vec![true; 4],
        current_player: 0,
        trick: TrickState::new(),
        finished_order: Vec::new(),
        scores: vec![0, 0, 0, 0],
        round: 1,
        total_scores: vec![0, 0, 0, 0],
        three_discard: None,
        log: Vec::new(),
        play_limit_secs: 10,
        winning_point: 50,
        game_winner: None,
        turn_seq: 0,
    }
}

// ─── s1: new GameState fields ───

#[test]
fn test_game_state_deserializes_without_new_fields() {
    // Backward compat: old server payloads (no playLimitSecs/winningPoint/
    // gameWinner/turnSeq) must still parse and pick up the defaults.
    let json = r#"{
      "phase":"lobby",
      "players":[],
      "ready":[],
      "currentPlayer":0,
      "trick":{"cards":[],"comboType":null,"comboPlayer":null,"passed":[],"played":[]},
      "finishedOrder":[],
      "scores":[],
      "round":1,
      "totalScores":[],
      "log":[]
    }"#;
    let s: GameState = serde_json::from_str(json).unwrap();
    assert_eq!(s.play_limit_secs, 10, "default play limit is 10s");
    assert_eq!(s.winning_point, 50, "default winning point is 50");
    assert_eq!(s.game_winner, None);
    assert_eq!(s.turn_seq, 0);
}

#[test]
fn test_game_state_serializes_new_fields_camel_case() {
    let mut s = empty_state();
    s.play_limit_secs = 25;
    s.winning_point = 100;
    s.game_winner = Some(2);
    let v: serde_json::Value = serde_json::to_value(&s).unwrap();
    assert_eq!(v["playLimitSecs"], 25);
    assert_eq!(v["winningPoint"], 100);
    assert_eq!(v["gameWinner"], 2);
}

// ─── s2: room settings (host-only) ───

#[test]
fn test_set_room_settings_host_only() {
    let manager = RoomManager::new(6, 2500, 30, 15);
    let (code, _, _, _) = manager.create_room("Alice".to_string(), true);
    manager.join_room(&code, "Bob".to_string()).unwrap();

    // Non-creator is rejected.
    assert!(manager.set_room_settings(&code, 1, Some(5), None).is_err());

    // Creator can set both.
    manager.set_room_settings(&code, 0, Some(5), Some(100)).unwrap();
    let st = manager.get_state(&code).unwrap();
    assert_eq!(st.play_limit_secs, 5);
    assert_eq!(st.winning_point, 100);

    // Partial update keeps the other value.
    manager.set_room_settings(&code, 0, Some(30), None).unwrap();
    let st = manager.get_state(&code).unwrap();
    assert_eq!(st.play_limit_secs, 30);
    assert_eq!(st.winning_point, 100);
}

#[test]
fn test_set_room_settings_clamps_and_requires_value() {
    let manager = RoomManager::new(6, 2500, 30, 15);
    let (code, _, _, _) = manager.create_room("Alice".to_string(), true);

    // Out-of-range rejected: 0s, >120s, 0 points, >9999 points.
    assert!(manager.set_room_settings(&code, 0, Some(0), None).is_err());
    assert!(manager.set_room_settings(&code, 0, Some(121), None).is_err());
    assert!(manager.set_room_settings(&code, 0, None, Some(0)).is_err());
    assert!(manager.set_room_settings(&code, 0, None, Some(10000)).is_err());
    // Nothing at all is also an error (nothing to set).
    assert!(manager.set_room_settings(&code, 0, None, None).is_err());

    // Boundaries accepted.
    manager.set_room_settings(&code, 0, Some(1), Some(9999)).unwrap();
    let st = manager.get_state(&code).unwrap();
    assert_eq!(st.play_limit_secs, 1);
    assert_eq!(st.winning_point, 9999);
}

#[test]
fn test_set_room_settings_only_in_lobby_or_gameover() {
    let manager = RoomManager::new(6, 2500, 30, 15);
    let (code, _, _, _) = manager.create_room("Alice".to_string(), true);

    // Works in Lobby.
    manager.set_room_settings(&code, 0, Some(7), None).unwrap();

    // Not mid-round.
    let mut st = manager.get_state(&code).unwrap();
    st.phase = GamePhase::Playing;
    manager.update_state(&code, st);
    assert!(manager.set_room_settings(&code, 0, Some(7), None).is_err());
}

// ─── s3: watchdog auto-move ───

#[test]
fn test_idle_auto_move_leads_with_lowest_single() {
    let mut s = empty_state();
    s.players[0].hand = vec![
        card(Rank::Ace, Suit::Spades),
        card(Rank::Five, Suit::Hearts),
        card(Rank::King, Suit::Diamonds),
    ];
    let move_ = idle_auto_move(&s, 0).unwrap();
    assert_eq!(move_, vec!["5:hearts".to_string()]);
}

#[test]
fn test_idle_auto_move_passes_when_cannot_beat() {
    let mut s = empty_state();
    s.players[0].hand = vec![card(Rank::Three, Suit::Spades), card(Rank::Four, Suit::Hearts)];
    // P1 leads a King on the table.
    s.trick.cards = vec![card(Rank::King, Suit::Hearts)];
    s.trick.combo_type = Some(combo::detect_combo(&s.trick.cards).unwrap().combo_type.clone());
    s.trick.combo_player = Some(1);
    assert_eq!(idle_auto_move(&s, 0), None);
}

#[test]
fn test_idle_auto_move_beats_with_lowest_valid_single() {
    let mut s = empty_state();
    s.players[0].hand = vec![
        card(Rank::Five, Suit::Spades),
        card(Rank::Nine, Suit::Hearts),
        card(Rank::Jack, Suit::Diamonds),
    ];
    // P1 leads a 7 on the table.
    s.trick.cards = vec![card(Rank::Seven, Suit::Clubs)];
    s.trick.combo_type = Some(combo::detect_combo(&s.trick.cards).unwrap().combo_type.clone());
    s.trick.combo_player = Some(1);
    let move_ = idle_auto_move(&s, 0).unwrap();
    assert_eq!(move_, vec!["9:hearts".to_string()]);
}

#[tokio::test]
async fn test_watchdog_auto_moves_stalled_human() {
    let manager = Arc::new(RoomManager::new(6, 200, 30, 15));
    let (code, _, _, _) = manager.create_room("Alice".to_string(), true);

    // Tiny turn limit so the test is quick.
    manager.set_room_settings(&code, 0, Some(1), None).unwrap();

    let should_spawn = manager.start_game(&code, 0).unwrap().1;
    assert!(should_spawn);
    // Drive the three-discard phase synchronously (in production this is a
    // spawned task). Lands on the first trick.
    manager.process_three_discard_delayed(&code).await;

    let start_hand = manager.get_state(&code).unwrap().players[0].hand.len();
    assert_eq!(start_hand, 13);

    // No human input. Poll until the watchdog auto-moves the human or 8s
    // pass (fail).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    let mut moved = false;
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let st = manager.get_state(&code).unwrap();
        if st.players[0].hand.len() < start_hand
            || st.log.iter().any(|l| l.contains("time limit"))
        {
            moved = true;
            break;
        }
    }
    assert!(moved, "watchdog should auto-move the stalled human");

    let st = manager.get_state(&code).unwrap();
    assert!(
        st.log.iter().any(|l| l.contains("time limit")),
        "auto-move must be logged, log = {:?}",
        st.log
    );
}

// ─── s4: winning point + permanent game over ───

#[test]
fn test_finalize_game_sets_match_winner_at_target() {
    let mut s = empty_state();
    s.total_scores = vec![40, 10, 5, 0];
    // P0 finishes 1st this round (+10) -> 50 >= 50 -> match winner.
    s.players[0].finished = true;
    s.finished_order.push(0);
    s.scores[0] = 10;
    s.players[1].finished = true;
    s.finished_order.push(1);
    s.scores[1] = 5;
    s.players[2].finished = true;
    s.finished_order.push(2);
    s.scores[2] = 0;

    let ended = finalize_game(&mut s);
    assert!(ended);
    assert_eq!(s.phase, GamePhase::GameOver);
    assert_eq!(s.total_scores[0], 50);
    assert_eq!(s.game_winner, Some(0));
}

#[test]
fn test_no_match_winner_below_target() {
    let mut s = empty_state();
    s.total_scores = vec![39, 10, 5, 0];
    s.players[0].finished = true;
    s.finished_order.push(0);
    s.scores[0] = 10;
    s.players[1].finished = true;
    s.finished_order.push(1);
    s.scores[1] = 5;
    s.players[2].finished = true;
    s.finished_order.push(2);
    s.scores[2] = 0;

    finalize_game(&mut s);
    assert_eq!(s.total_scores[0], 49);
    assert_eq!(s.game_winner, None);
}

#[test]
fn test_bomb_endgame_sets_match_winner() {
    let mut s = empty_state();
    s.total_scores = vec![0, 40, 5, 0];
    // P1 (bomber, +10) reaches 50 via a bomb endgame.
    s.players[0].hand = vec![card(Rank::Three, Suit::Spades)];
    s.players[1].hand = vec![card(Rank::Four, Suit::Spades)];
    s.players[2].hand = vec![card(Rank::Five, Suit::Spades)];
    s.players[3].hand = vec![card(Rank::Six, Suit::Spades)];
    s.trick.cards = vec![
        card(Rank::King, Suit::Spades),
        card(Rank::King, Suit::Hearts),
        card(Rank::King, Suit::Diamonds),
        card(Rank::King, Suit::Clubs),
    ];
    s.trick.combo_type = Some(ComboType::Bomb);
    s.trick.combo_player = Some(1);
    s.trick.played = vec![0]; // P0 held the single 2, pushed out by the bomb
    s.trick.passed = vec![2, 3, 0]; // P2, P3 and P0 all passed after the bomb

    let ended = maybe_end_game_by_bomb(&mut s);
    assert!(ended);
    assert_eq!(s.scores[1], 10);
    assert_eq!(s.total_scores[1], 50);
    assert_eq!(s.game_winner, Some(1));
}

#[test]
fn test_start_game_blocked_after_match_winner() {
    let manager = RoomManager::new(6, 2500, 30, 15);
    let (code, _, _, _) = manager.create_room("Alice".to_string(), true);

    let mut st = manager.get_state(&code).unwrap();
    st.phase = GamePhase::GameOver;
    st.game_winner = Some(1);
    manager.update_state(&code, st);

    let err = manager.start_game(&code, 0).unwrap_err();
    assert!(
        err.contains("Match already won"),
        "expected match-won rejection, got: {err}"
    );
}

#[test]
fn test_set_room_settings_message_wire_format() {
    // ClientMsg must round-trip as {"type":"setRoomSettings",...}.
    let msg = ClientMsg::SetRoomSettings {
        play_limit_secs: Some(15),
        winning_point: Some(60),
    };
    let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "setRoomSettings");
    assert_eq!(v["playLimitSecs"], 15);
    assert_eq!(v["winningPoint"], 60);

    let back: ClientMsg = serde_json::from_str(
        r#"{"type":"setRoomSettings","playLimitSecs":15,"winningPoint":60}"#,
    )
    .unwrap();
    assert_eq!(
        back,
        ClientMsg::SetRoomSettings {
            play_limit_secs: Some(15),
            winning_point: Some(60),
        }
    );

    // ServerMsg::RoomSettings carries both values for clients.
    let s = ServerMsg::RoomSettings {
        play_limit_secs: 15,
        winning_point: 60,
    };
    let v: serde_json::Value = serde_json::to_value(&s).unwrap();
    assert_eq!(v["type"], "roomSettings");
    assert_eq!(v["playLimitSecs"], 15);
    assert_eq!(v["winningPoint"], 60);
}
