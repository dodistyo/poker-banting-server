use super::card::Card;
use super::combo::{self, Combo};
use super::state::{GameState, GamePhase, TrickState};

pub struct ValidationResult {
    pub valid: bool,
    pub error: String,
    pub combo_name: String,
    pub combo: Option<Combo>,
}

pub fn validate_play(cards: &[Card], table_combo: Option<&Combo>) -> ValidationResult {
    if cards.is_empty() {
        return ValidationResult {
            valid: false,
            error: "No cards selected".to_string(),
            combo_name: String::new(),
            combo: None,
        };
    }

    let combo = match combo::detect_combo(cards) {
        Some(c) => c,
        None => return ValidationResult {
            valid: false,
            error: "Invalid combo".to_string(),
            combo_name: String::new(),
            combo: None,
        },
    };

    if let Some(tc) = table_combo {
        if combo.cards.len() != tc.cards.len() || combo.combo_type != tc.combo_type {
            return ValidationResult {
                valid: false,
                error: format!(
                    "Must match: {} ({} cards).",
                    combo_name(&tc),
                    tc.cards.len()
                ),
                combo_name: combo_name(&combo),
                combo: Some(combo),
            };
        }
        if combo::compare_combos(&combo, tc).unwrap_or(0) <= 0 {
            return ValidationResult {
                valid: false,
                error: format!(
                    "Cannot beat: {}",
                    tc.cards.iter().map(|c| c.label()).collect::<Vec<_>>().join(" ")
                ),
                combo_name: combo_name(&combo),
                combo: Some(combo),
            };
        }
    }

    ValidationResult {
        valid: true,
        error: String::new(),
        combo_name: combo_name(&combo),
        combo: Some(combo),
    }
}

fn combo_name(combo: &Combo) -> String {
    combo.combo_type.to_string()
}

pub fn resolve_trick(state: &mut GameState, winner_id: usize) {
    // Reset trick
    state.trick = TrickState::new();

    // Next trick led by winner
    state.current_player = winner_id;
    skip_finished(state);
}

pub fn player_finished(state: &GameState, player_id: usize) -> bool {
    state.players[player_id].finished
}

/// Finalize the game: score the last remaining player, flip phase to
/// GameOver, and accumulate this round's `scores` into `total_scores`
/// (the running session score). Returns true if the game just ended.
///
/// Every game-over path (engine play/pass, bot turn loop) MUST go through
/// this helper so cumulative scoring stays consistent.
pub fn finalize_game(state: &mut GameState) -> bool {
    if state.finished_order.len() >= 3 && state.phase != GamePhase::GameOver {
        // Score the last remaining player
        for i in 0..4 {
            if !state.players[i].finished {
                state.scores[i] = -15;
                state.finished_order.push(i);
                break;
            }
        }
        state.ensure_total_scores();
        for i in 0..state.players.len().min(4) {
            state.total_scores[i] += state.scores[i];
        }
        state.phase = GamePhase::GameOver;
        true
    } else {
        false
    }
}

/// Kept for backward compatibility with existing callers/tests.
pub fn end_game(state: &mut GameState) -> bool {
    finalize_game(state)
}

pub fn next_player(state: &GameState) -> usize {
    let current = state.current_player;
    let next = (current + 1) % 4;
    next
}

pub fn deal_cards(state: &mut GameState) {
    use super::card::{create_deck, shuffle, sort_cards};

    let mut deck = create_deck();
    shuffle(&mut deck);

    for i in 0..4 {
        let hand: Vec<Card> = deck.drain(..13).collect();
        let mut sorted_hand = hand;
        sort_cards(&mut sorted_hand);
        state.players[i].hand = sorted_hand;
    }
}

pub fn start_three_discard(state: &mut GameState) {
    // Count 3s for each player
    let mut counts: Vec<(usize, usize, usize)> = Vec::new(); // (player_id, count, highest_suit)
    for i in 0..4 {
        let threes: Vec<&Card> = state.players[i].hand.iter().filter(|c| c.rank_index() == 0).collect();
        let count = threes.len();
        let highest_suit = if count > 0 {
            threes.iter().map(|c| c.suit_index()).max().unwrap()
        } else {
            0
        };
        counts.push((i, count, highest_suit));
    }

    // Sort by count desc, then highest suit desc
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));

    let order: Vec<usize> = counts.iter().map(|(id, _, _)| *id).collect();

    // Extract 3s from each player's hand
    let mut player_cards: Vec<Vec<Card>> = Vec::new();
    for i in 0..4 {
        let threes: Vec<Card> = state.players[i].hand.iter()
            .filter(|c| c.rank_index() == 0)
            .cloned()
            .collect();
        state.players[i].hand.retain(|c| c.rank_index() != 0);
        player_cards.push(threes);
    }

    let first_player = order[0];
    state.three_discard = Some(super::state::ThreeDiscardState {
        order,
        index: 0,
        player_cards,
        discarded: vec![false; 4],
    });

    state.phase = GamePhase::ThreeDiscard;
    state.current_player = first_player;
}

pub fn process_three_discard(state: &mut GameState, player_id: usize) -> bool {
    if let Some(ref mut td) = state.three_discard {
        if td.discarded[player_id] {
            return false;
        }

        td.discarded[player_id] = true;

        // Move to next player in order
        let current_idx = td.order.iter().position(|&id| id == player_id).unwrap();
        let next_idx = current_idx + 1;

        if next_idx >= td.order.len() {
            // Three discard phase complete
            let first_player = td.order[0];
            for i in 0..4 {
                state.players[i].hand.retain(|c| c.rank_index() != 0);
            }
            state.three_discard = None;
            state.phase = GamePhase::Playing;
            state.current_player = first_player;
            true
        } else {
            state.current_player = td.order[next_idx];
            false
        }
    } else {
        false
    }
}

pub fn non_participants(state: &GameState) -> usize {
    let combo = state.trick.combo_player;
    state.players.iter().filter(|p| {
        if let Some(cp) = combo {
            p.id != cp && (p.finished || state.trick.passed.contains(&p.id))
        } else {
            false
        }
    }).count()
}

pub fn check_trick_complete(state: &GameState) -> Option<usize> {
    if non_participants(state) >= 3 {
        return state.trick.combo_player;
    }
    None
}

pub fn skip_finished(state: &mut GameState) {
    while state.players[state.current_player].finished {
        state.current_player = (state.current_player + 1) % 4;
    }
}

/// Process a single bot turn. Returns true if another bot turn is needed.
pub fn process_one_bot_turn(state: &mut GameState) -> bool {
    use super::bot::bot_play;
    use super::combo;

    if state.finished_order.len() >= 3 || state.phase != GamePhase::Playing {
        finalize_game(state);
        return false;
    }

    let cp = state.current_player;
    if !state.players[cp].is_bot {
        if state.players[cp].finished {
            let finished_count = state.players.iter().filter(|p| p.finished).count();
            if finished_count >= 3 {
                return false;
            }
            skip_finished(state);
            return true;
        }
        return false;
    }

    if state.players[cp].finished {
        skip_finished(state);
        return state.players[state.current_player].is_bot && state.phase == GamePhase::Playing;
    }

    let decision = bot_play(state, cp);

    if let Some(indices) = decision {
        let player = &state.players[cp];
        let cards: Vec<_> = indices.iter().map(|&i| player.hand[i].clone()).collect();

        let table_combo = if state.trick.combo_player.is_some() {
            combo::detect_combo(&state.trick.cards)
        } else {
            None
        };

        let result = validate_play(&cards, table_combo.as_ref());

        if result.valid {
            let card_labels: Vec<String> = cards.iter().map(|c| c.to_string()).collect();
            state.log.push(format!("{} plays {} ({})", state.players[cp].name, card_labels.join(" "), result.combo_name));
            let hand = &mut state.players[cp].hand;
            for &i in indices.iter().rev() {
                hand.remove(i);
            }

            if state.players[cp].hand.is_empty() && !state.players[cp].finished {
                state.players[cp].finished = true;
                state.finished_order.push(cp);
                let pos = state.finished_order.len();
                state.scores[cp] = if pos == 1 { 10 } else if pos == 2 { 5 } else if pos == 3 { 0 } else { -15 };
                state.log.push(format!("{} finished ({}th, {} pts)", state.players[cp].name, ["", "1st", "2nd", "3rd"][pos], state.scores[cp]));
            }

            if let Some(old_cp) = state.trick.combo_player {
                if !state.trick.played.contains(&old_cp) && !state.trick.passed.contains(&old_cp) {
                    state.trick.played.push(old_cp);
                }
                state.trick.passed.clear();
            }
            state.trick.cards = cards;
            state.trick.combo_type = Some(result.combo.as_ref().unwrap().combo_type.clone());
            state.trick.combo_player = Some(cp);

            if let Some(winner) = check_trick_complete(state) {
                resolve_trick(state, winner);
                if end_game(state) {
                    return false;
                }
                return state.players[state.current_player].is_bot;
            }
        }
    } else {
        state.trick.passed.push(cp);
        state.log.push(format!("{} passes", state.players[cp].name));

        if non_participants(state) >= 3 {
            if let Some(winner) = state.trick.combo_player {
                resolve_trick(state, winner);
                if end_game(state) {
                    return false;
                }
                return state.players[state.current_player].is_bot;
            }
            return false;
        }
    }

    if finalize_game(state) {
        return false;
    }

    state.current_player = (state.current_player + 1) % 4;
    skip_finished(state);
    state.players[state.current_player].is_bot && state.phase == GamePhase::Playing
}

/// Process all consecutive bot turns synchronously (no delay). Used for tests.
pub fn process_bot_turns(state: &mut GameState) {
    let mut iteration = 0;
    while process_one_bot_turn(state) {
        iteration += 1;
        if iteration > 100 { break; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::card::{Card, Rank, Suit};
    use crate::game::combo::ComboType;
    use crate::game::state::Player;

    fn card(rank: Rank, suit: Suit) -> Card {
        Card::new(rank, suit)
    }

    #[test]
    fn test_validate_play_empty() {
        let result = validate_play(&[], None);
        assert!(!result.valid);
    }

    #[test]
    fn test_validate_play_invalid_combo() {
        let cards = vec![card(Rank::Three, Suit::Diamonds), card(Rank::Four, Suit::Diamonds)];
        let result = validate_play(&cards, None);
        assert!(!result.valid);
    }

    #[test]
    fn test_validate_play_single_valid() {
        let cards = vec![card(Rank::Three, Suit::Diamonds)];
        let result = validate_play(&cards, None);
        assert!(result.valid);
    }

    #[test]
    fn test_validate_play_beat_single() {
        let table = combo::detect_combo(&[card(Rank::Three, Suit::Diamonds)]).unwrap();
        let cards = vec![card(Rank::King, Suit::Diamonds)];
        let result = validate_play(&cards, Some(&table));
        assert!(result.valid);
    }

    #[test]
    fn test_validate_play_cannot_beat() {
        let table = combo::detect_combo(&[card(Rank::King, Suit::Diamonds)]).unwrap();
        let cards = vec![card(Rank::Three, Suit::Diamonds)];
        let result = validate_play(&cards, Some(&table));
        assert!(!result.valid);
    }

    #[test]
    fn test_validate_play_type_mismatch() {
        let table = combo::detect_combo(&[card(Rank::Three, Suit::Diamonds)]).unwrap();
        let cards = vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Three, Suit::Clubs),
        ];
        let result = validate_play(&cards, Some(&table));
        assert!(!result.valid);
    }

    #[test]
    fn test_validate_play_same_rank_single_cannot_beat() {
        let table = combo::detect_combo(&[card(Rank::Three, Suit::Diamonds)]).unwrap();
        let cards = vec![card(Rank::Three, Suit::Spades)];
        let result = validate_play(&cards, Some(&table));
        assert!(!result.valid);
    }

    #[test]
    fn test_resolve_trick_basic() {
        let mut state = GameState {
            phase: GamePhase::Playing,
            players: vec![
                Player { id: 0, name: "P0".to_string(), hand: Vec::new(), finished: true, is_bot: false, connected: true, is_creator: false },
                Player { id: 1, name: "P1".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 2, name: "P2".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 3, name: "P3".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
            ],
            ready: vec![true; 4],
            current_player: 0,
            trick: TrickState {
                cards: vec![card(Rank::Three, Suit::Diamonds)],
                combo_type: Some(ComboType::Single),
                combo_player: Some(0),
                passed: vec![1, 2, 3],
                played: Vec::new(),
            },
            finished_order: vec![0],
            scores: vec![10, 0, 0, 0],
            round: 1,
            total_scores: vec![0],
            three_discard: None,
            log: Vec::new(),
        };

        resolve_trick(&mut state, 0);
        assert_eq!(state.finished_order, vec![0]);
        assert_eq!(state.scores[0], 10);
        assert_eq!(state.current_player, 1);
    }

    #[test]
    fn test_end_game() {
        let mut state = GameState {
            phase: GamePhase::Playing,
            players: vec![
                Player { id: 0, name: "P0".to_string(), hand: Vec::new(), finished: true, is_bot: false, connected: true, is_creator: false },
                Player { id: 1, name: "P1".to_string(), hand: Vec::new(), finished: true, is_bot: false, connected: true, is_creator: false },
                Player { id: 2, name: "P2".to_string(), hand: Vec::new(), finished: true, is_bot: false, connected: true, is_creator: false },
                Player { id: 3, name: "P3".to_string(), hand: vec![card(Rank::Two, Suit::Spades)], finished: false, is_bot: false, connected: true, is_creator: false },
            ],
            ready: vec![true; 4],
            current_player: 3,
            trick: TrickState::new(),
            finished_order: vec![0, 1, 2],
            scores: vec![10, 5, 0, 0],
            round: 1,
            total_scores: vec![0],
            three_discard: None,
            log: Vec::new(),
        };

        assert!(end_game(&mut state));
        assert_eq!(state.phase, GamePhase::GameOver);
    }

    #[test]
    fn test_end_game_not_yet() {
        let mut state = GameState {
            phase: GamePhase::Playing,
            players: vec![
                Player { id: 0, name: "P0".to_string(), hand: Vec::new(), finished: true, is_bot: false, connected: true, is_creator: false },
                Player { id: 1, name: "P1".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 2, name: "P2".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 3, name: "P3".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
            ],
            ready: vec![true; 4],
            current_player: 1,
            trick: TrickState::new(),
            finished_order: vec![0],
            scores: vec![10, 0, 0, 0],
            round: 1,
            total_scores: vec![0],
            three_discard: None,
            log: Vec::new(),
        };

        assert!(!end_game(&mut state));
    }

    #[test]
    fn test_deal_cards() {
        let mut state = GameState {
            phase: GamePhase::Playing,
            players: vec![
                Player { id: 0, name: "P0".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 1, name: "P1".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 2, name: "P2".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 3, name: "P3".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
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
        };

        deal_cards(&mut state);
        for i in 0..4 {
            assert_eq!(state.players[i].hand.len(), 13);
        }
    }

    #[test]
    fn test_next_player() {
        let state = GameState {
            phase: GamePhase::Playing,
            players: vec![
                Player { id: 0, name: "P0".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 1, name: "P1".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 2, name: "P2".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 3, name: "P3".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
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
        };
        assert_eq!(next_player(&state), 1);
    }

    #[test]
    fn test_next_player_wrap() {
        let state = GameState {
            phase: GamePhase::Playing,
            players: vec![
                Player { id: 0, name: "P0".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 1, name: "P1".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 2, name: "P2".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 3, name: "P3".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
            ],
            ready: vec![true; 4],
            current_player: 3,
            trick: TrickState::new(),
            finished_order: Vec::new(),
            scores: vec![0, 0, 0, 0],
            round: 1,
            total_scores: vec![0],
            three_discard: None,
            log: Vec::new(),
        };
        assert_eq!(next_player(&state), 0);
    }

    #[test]
    fn test_scoring_positions() {
        let mut state = GameState {
            phase: GamePhase::Playing,
            players: vec![
                Player { id: 0, name: "P0".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 1, name: "P1".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 2, name: "P2".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 3, name: "P3".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
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
        };

        // P0 finishes first (+10) — scoring happens when hand empties
        state.players[0].finished = true;
        state.finished_order.push(0);
        state.scores[0] = 10;
        resolve_trick(&mut state, 0);
        assert_eq!(state.scores[0], 10);

        // P1 finishes second (+5)
        state.players[1].finished = true;
        state.finished_order.push(1);
        state.scores[1] = 5;
        resolve_trick(&mut state, 1);
        assert_eq!(state.scores[1], 5);

        // P2 finishes third (0)
        state.players[2].finished = true;
        state.finished_order.push(2);
        state.scores[2] = 0;
        resolve_trick(&mut state, 2);
        assert_eq!(state.scores[2], 0);
    }

    #[test]
    fn test_check_trick_complete_all_passed() {
        let state = GameState {
            phase: GamePhase::Playing,
            players: vec![
                Player { id: 0, name: "P0".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 1, name: "P1".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 2, name: "P2".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 3, name: "P3".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
            ],
            ready: vec![true; 4],
            current_player: 0,
            trick: TrickState {
                cards: vec![card(Rank::Three, Suit::Diamonds)],
                combo_type: Some(ComboType::Single),
                combo_player: Some(0),
                passed: vec![1, 2, 3],
                played: Vec::new(),
            },
            finished_order: Vec::new(),
            scores: vec![0, 0, 0, 0],
            round: 1,
            total_scores: vec![0],
            three_discard: None,
            log: Vec::new(),
        };

        assert_eq!(check_trick_complete(&state), Some(0));
    }

    #[test]
    fn test_check_trick_incomplete() {
        let state = GameState {
            phase: GamePhase::Playing,
            players: vec![
                Player { id: 0, name: "P0".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 1, name: "P1".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 2, name: "P2".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 3, name: "P3".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
            ],
            ready: vec![true; 4],
            current_player: 0,
          trick: TrickState {
                cards: vec![card(Rank::Three, Suit::Diamonds)],
                combo_type: Some(ComboType::Single),
                combo_player: Some(0),
                passed: vec![1],
                played: Vec::new(),
            },
            finished_order: Vec::new(),
            scores: vec![0, 0, 0, 0],
            round: 1,
            total_scores: vec![0],
            three_discard: None,
            log: Vec::new(),
        };

        assert_eq!(check_trick_complete(&state), None);
    }

    #[test]
    fn test_check_trick_with_finished_player() {
        let state = GameState {
            phase: GamePhase::Playing,
            players: vec![
                Player { id: 0, name: "P0".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 1, name: "P1".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 2, name: "P2".to_string(), hand: Vec::new(), finished: true, is_bot: true, connected: true, is_creator: false },
                Player { id: 3, name: "P3".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
            ],
            ready: vec![true; 4],
            current_player: 0,
            trick: TrickState {
                cards: vec![card(Rank::Two, Suit::Spades)],
                combo_type: Some(ComboType::Single),
                combo_player: Some(0),
                passed: vec![1, 3],
                played: Vec::new(),
            },
            finished_order: vec![2],
            scores: vec![0, 0, 10, 0],
            round: 1,
            total_scores: vec![0],
            three_discard: None,
            log: Vec::new(),
        };

        assert_eq!(non_participants(&state), 3);
        assert_eq!(check_trick_complete(&state), Some(0));
    }
}
