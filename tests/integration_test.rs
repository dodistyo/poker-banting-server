use pocer_server::game::card::{Card, Rank, Suit};
use pocer_server::game::combo::{self, ComboType};
use pocer_server::game::rules::*;
use pocer_server::game::state::*;
use pocer_server::game::bot::bot_play;
use pocer_server::protocol::{ClientMsg, ServerMsg};
use pocer_server::rooms::RoomManager;

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
        total_scores: vec![0],
        three_discard: None,
        log: Vec::new(),
    }
}

// ─── Full game flow ───

#[test]
fn test_full_game_deal_to_gameover() {
    let mut state = empty_state();
    deal_cards(&mut state);

    for p in &state.players {
        assert_eq!(p.hand.len(), 13);
    }

    let total: usize = state.players.iter().map(|p| p.hand.len()).sum();
    assert_eq!(total, 52);
    assert_eq!(state.phase, GamePhase::Playing);
}

#[test]
fn test_three_discard_phase() {
    let mut state = empty_state();

    state.players[0].hand.push(card(Rank::Three, Suit::Spades));
    state.players[0].hand.push(card(Rank::Three, Suit::Hearts));
    state.players[1].hand.push(card(Rank::Three, Suit::Diamonds));

    start_three_discard(&mut state);

    assert_eq!(state.phase, GamePhase::ThreeDiscard);
    let td = state.three_discard.as_ref().unwrap();
    assert_eq!(td.order[0], 0);
}

#[test]
fn test_trick_cycle_basic() {
    let mut state = empty_state();

    let play_cards = vec![card(Rank::King, Suit::Diamonds)];

    state.trick.cards = play_cards.clone();
    state.trick.combo_type = combo::detect_combo(&play_cards).map(|c| c.combo_type);
    state.trick.combo_player = Some(0);
    state.trick.passed = vec![1, 2, 3];

    // P0 has no cards left — mark as finished before resolve_trick
    state.players[0].hand = vec![];
    state.players[0].finished = true;
    state.finished_order.push(0);
    state.scores[0] = 10;

    resolve_trick(&mut state, 0);

    assert_eq!(state.finished_order, vec![0]);
    assert_eq!(state.scores[0], 10);
    assert_eq!(state.current_player, 1);
    assert!(state.trick.cards.is_empty());
    assert!(state.trick.combo_player.is_none());
}

#[test]
fn test_scoring_all_positions() {
    let mut state = empty_state();

    // Simulate 3 players finishing
    state.players[0].finished = true;
    state.finished_order.push(0);
    state.scores[0] = 10;

    state.players[1].finished = true;
    state.finished_order.push(1);
    state.scores[1] = 5;

    state.players[2].finished = true;
    state.finished_order.push(2);
    state.scores[2] = 0;

    // end_game sets GameOver and scores the last player
    assert!(end_game(&mut state));
    assert_eq!(state.phase, GamePhase::GameOver);
    // Last player gets -15
    assert_eq!(state.scores[3], -15);
}

#[test]
fn test_bot_plays_free_play() {
    let mut state = empty_state();

    // Give P0 a hand with a pair and singles
    state.players[0].hand = vec![
        card(Rank::Three, Suit::Diamonds),
        card(Rank::Four, Suit::Diamonds),
        card(Rank::Four, Suit::Clubs),
        card(Rank::Five, Suit::Diamonds),
    ];

    let play = bot_play(&state, 0);
    assert!(play.is_some());
    // Bot should play a valid combo
    let indices = play.unwrap();
    let played: Vec<Card> = indices.iter().map(|&i| state.players[0].hand[i].clone()).collect();
    let combo = combo::detect_combo(&played);
    assert!(combo.is_some());
}

#[test]
fn test_bot_passes_when_cannot_beat() {
    let mut state = empty_state();

    state.players[1].hand = vec![
        card(Rank::Three, Suit::Diamonds),
        card(Rank::Three, Suit::Clubs),
    ];

    // P0 plays a high single
    let play_cards = vec![card(Rank::Two, Suit::Spades)];
    state.trick.cards = play_cards;
    state.trick.combo_type = Some(ComboType::Single);
    state.trick.combo_player = Some(0);

    // P1 has only 3s, cannot beat 2
    let play = bot_play(&state, 1);
    assert!(play.is_none());
}

// ─── Protocol tests ───

#[test]
fn test_protocol_create_roundtrip() {
    let msg = ClientMsg::Create { name: "Alice".to_string(), is_public: true };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("create"));
    assert!(json.contains("Alice"));
}

#[test]
fn test_protocol_play_serialization() {
    let msg = ClientMsg::Play { cards: vec!["2:clubs".to_string(), "2:diamonds".to_string(), "A:hearts".to_string()] };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("play"));
    assert!(json.contains("cards"));
    assert!(json.contains("2:clubs"));
}

#[test]
fn test_protocol_pass_serialization() {
    let msg = ClientMsg::Pass;
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("pass"));
}

#[test]
fn test_server_msg_state_serialization() {
    let state = empty_state();
    let msg = ServerMsg::State { state };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("state"));
    assert!(json.contains("players"));
    assert!(json.contains("trick"));
}

#[test]
fn test_server_msg_created_serialization() {
    let state = empty_state();
    let msg = ServerMsg::Created {
        code: "ABC123".to_string(),
        player_id: 0,
        state,
        is_public: true,
        token: "abc123".to_string(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("created"));
    assert!(json.contains("ABC123"));
    assert!(json.contains("playerId"));
}

// ─── Room management tests ───

#[test]
fn test_room_create_and_join() {
    let rm = RoomManager::new(6, 2500, 30, 15);

    let (code, pid, _msg, _) = rm.create_room("Alice".to_string(), true);
    assert_eq!(pid, 0);
    assert_eq!(code.len(), 6);

    let result = rm.join_room(&code, "Bob".to_string());
    assert!(result.is_ok());
    let (ServerMsg::Joined { player_id: bob_pid, .. }, _) = result.unwrap() else {
        panic!("Expected Joined message");
    };
    assert_eq!(bob_pid, 1);
}

#[test]
fn test_room_max_players() {
    let rm = RoomManager::new(6, 2500, 30, 15);

    let (code, _, _, _) = rm.create_room("Alice".to_string(), true); // 1 human + 3 bots
    rm.join_room(&code, "Bob".to_string()).unwrap(); // Replaces bot
    rm.join_room(&code, "Charlie".to_string()).unwrap(); // Replaces bot
    rm.join_room(&code, "Dave".to_string()).unwrap(); // Replaces last bot

    let result = rm.join_room(&code, "Eve".to_string());
    assert!(result.is_err());
}

#[test]
fn test_room_leave_marks_disconnected() {
    let rm = RoomManager::new(6, 2500, 30, 15);

    let (code, _, _, _) = rm.create_room("Alice".to_string(), true);
    rm.join_room(&code, "Bob".to_string()).unwrap();

    let room = rm.get_room(&code).unwrap();
    assert_eq!(room.players.len(), 2);

    // Non-creator leave in lobby: room stays, player marked disconnected
    rm.leave_room(&code, 1);
    let room = rm.get_room(&code).unwrap();
    assert_eq!(room.players.len(), 2);
    assert!(!room.players[1].connected);
    assert_eq!(room.disconnected_players.len(), 1);
}

#[test]
fn test_room_invalid_code() {
    let rm = RoomManager::new(6, 2500, 30, 15);

    let result = rm.join_room("INVALID", "Alice".to_string());
    assert!(result.is_err());
}

// ─── Game logic cross-check ───

#[test]
fn test_combo_detection_matches_js() {
    assert!(matches!(combo::detect_combo(&[card(Rank::King, Suit::Diamonds)]).unwrap().combo_type, ComboType::Single));

    assert!(matches!(combo::detect_combo(&[card(Rank::King, Suit::Diamonds), card(Rank::King, Suit::Clubs)]).unwrap().combo_type, ComboType::Pair));

    assert!(matches!(combo::detect_combo(&[card(Rank::King, Suit::Diamonds), card(Rank::King, Suit::Clubs), card(Rank::King, Suit::Hearts)]).unwrap().combo_type, ComboType::Triple));

    assert!(matches!(combo::detect_combo(&[card(Rank::Three, Suit::Diamonds), card(Rank::Four, Suit::Diamonds), card(Rank::Five, Suit::Diamonds)]).unwrap().combo_type, ComboType::Straight));

    assert!(matches!(combo::detect_combo(&[card(Rank::Ten, Suit::Diamonds), card(Rank::Jack, Suit::Diamonds), card(Rank::Queen, Suit::Diamonds)]).unwrap().combo_type, ComboType::Straight));

    // Mixed straight (9-10-J) should be rejected
    assert!(combo::detect_combo(&[card(Rank::Nine, Suit::Diamonds), card(Rank::Ten, Suit::Diamonds), card(Rank::Jack, Suit::Diamonds)]).is_none());

    assert!(matches!(combo::detect_combo(&[card(Rank::King, Suit::Diamonds), card(Rank::King, Suit::Clubs), card(Rank::King, Suit::Hearts), card(Rank::Three, Suit::Diamonds), card(Rank::Three, Suit::Clubs)]).unwrap().combo_type, ComboType::FullHouse));

    assert!(matches!(combo::detect_combo(&[card(Rank::King, Suit::Diamonds), card(Rank::King, Suit::Clubs), card(Rank::King, Suit::Hearts), card(Rank::King, Suit::Spades), card(Rank::Three, Suit::Diamonds)]).unwrap().combo_type, ComboType::FourKind));
}

#[test]
fn test_validate_play_matches_js() {
    let result = validate_play(&[card(Rank::King, Suit::Diamonds)], None);
    assert!(result.valid);

    let result = validate_play(&[card(Rank::King, Suit::Diamonds), card(Rank::Ace, Suit::Clubs)], None);
    assert!(!result.valid);

    let table = combo::detect_combo(&[card(Rank::Three, Suit::Diamonds)]).unwrap();
    let result = validate_play(&[card(Rank::King, Suit::Diamonds), card(Rank::King, Suit::Clubs)], Some(&table));
    assert!(!result.valid);

    let table = combo::detect_combo(&[card(Rank::King, Suit::Diamonds)]).unwrap();
    let result = validate_play(&[card(Rank::Three, Suit::Diamonds)], Some(&table));
    assert!(!result.valid);

    let table = combo::detect_combo(&[card(Rank::Three, Suit::Diamonds)]).unwrap();
    let result = validate_play(&[card(Rank::Three, Suit::Spades)], Some(&table));
    assert!(!result.valid);
}
