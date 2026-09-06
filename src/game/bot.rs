use super::card::Card;
use super::combo::{self, Combo, ComboType};
use super::rules::validate_play;
use super::state::GameState;

/// Bot decides whether to play or pass.
/// Returns Some(card_indices) to play, or None to pass.
pub fn bot_play(state: &GameState, bot_id: usize) -> Option<Vec<usize>> {
    let hand = &state.players[bot_id].hand;

    // During three-discard phase, play all 3s
    if state.phase == super::state::GamePhase::ThreeDiscard {
        let threes: Vec<usize> = hand.iter().enumerate()
            .filter(|(_, c)| c.rank_index() == 0)
            .map(|(i, _)| i)
            .collect();
        if !threes.is_empty() {
            return Some(threes);
        }
        return None;
    }

    let table_combo = state.trick.combo_type.as_ref().map(|_| {
        // Reconstruct table combo from trick cards
        combo::detect_combo(&state.trick.cards)
    }).flatten();

    // If no table combo, it's a free play — play lowest valid combo
    if state.trick.combo_player.is_none() {
        return bot_free_play(hand);
    }

    // Try to beat the table combo
    if let Some(ref tc) = table_combo {
        if let Some(indices) = bot_beat_play(hand, tc) {
            return Some(indices);
        }
    }

    // Can't beat, pass
    None
}

/// Free play: play lowest valid combo to save good cards.
fn bot_free_play(hand: &[Card]) -> Option<Vec<usize>> {
    let combos = find_valid_combos(hand);

    if combos.is_empty() {
        return None;
    }

    // Play the weakest combo (lowest strength)
    let mut best = &combos[0];
    let mut best_strength = combo_strength(hand, &combos[0]);

    for combo in &combos[1..] {
        let strength = combo_strength(hand, combo);
        if strength < best_strength {
            best = combo;
            best_strength = strength;
        }
    }

    Some(best.indices.clone())
}

/// Try to beat the table combo with the lowest possible winning combo.
fn bot_beat_play(hand: &[Card], table: &Combo) -> Option<Vec<usize>> {
    let combos = find_valid_combos(hand);

    let mut best: Option<&ValidCombo> = None;
    let mut best_strength = i32::MAX;

    for combo in &combos {
        let cards: Vec<Card> = combo.indices.iter().map(|&i| hand[i].clone()).collect();
        let result = validate_play(&cards, Some(table));

        if result.valid {
            let strength = combo_strength(hand, combo);
            if strength < best_strength {
                best = Some(combo);
                best_strength = strength;
            }
        }
    }

    best.map(|c| c.indices.clone())
}

struct ValidCombo {
    indices: Vec<usize>,
    combo: Combo,
}

/// Find all valid combos from a hand.
fn find_valid_combos(hand: &[Card]) -> Vec<ValidCombo> {
    let mut combos = Vec::new();

    let n = hand.len();

    // Singles
    for i in 0..n {
        if let Some(combo) = combo::detect_combo(&[hand[i].clone()]) {
            combos.push(ValidCombo {
                indices: vec![i],
                combo,
            });
        }
    }

    // Pairs and Triples
    for i in 0..n {
        for j in (i + 1)..n {
            if hand[i].rank_index() == hand[j].rank_index() {
                if let Some(combo) = combo::detect_combo(&[hand[i].clone(), hand[j].clone()]) {
                    combos.push(ValidCombo {
                        indices: vec![i, j],
                        combo,
                    });
                }

                // Triples
                for k in (j + 1)..n {
                    if hand[k].rank_index() == hand[i].rank_index() {
                        if let Some(combo) = combo::detect_combo(&[
                            hand[i].clone(),
                            hand[j].clone(),
                            hand[k].clone(),
                        ]) {
                            combos.push(ValidCombo {
                                indices: vec![i, j, k],
                                combo,
                            });
                        }
                    }
                }
            }
        }
    }

    // Straights (3-5 cards, same suit)
    for suit in [0, 1, 2, 3] {
        let suit_cards: Vec<(usize, &Card)> = hand.iter().enumerate()
            .filter(|(_, c)| c.suit_index() == suit)
            .collect();

        if suit_cards.len() >= 3 {
            // Try all consecutive runs of 3-5
            for len in 3..=std::cmp::min(5, suit_cards.len()) {
                for start in 0..=suit_cards.len() - len {
                    let subset: Vec<Card> = suit_cards[start..start + len]
                        .iter().map(|(_, c)| (*c).clone()).collect();
                    if let Some(combo) = combo::detect_combo(&subset) {
                        let indices: Vec<usize> = suit_cards[start..start + len]
                            .iter().map(|&(i, _)| i).collect();
                        combos.push(ValidCombo { indices, combo });
                    }
                }
            }
        }
    }

    // Full House and Four of a Kind (5 cards)
    for i in 0..n {
        for j in (i + 1)..n {
            for k in (j + 1)..n {
                for l in (k + 1)..n {
                    for m in (l + 1)..n {
                        let cards = vec![
                            hand[i].clone(),
                            hand[j].clone(),
                            hand[k].clone(),
                            hand[l].clone(),
                            hand[m].clone(),
                        ];
                        if let Some(combo) = combo::detect_combo(&cards) {
                            combos.push(ValidCombo {
                                indices: vec![i, j, k, l, m],
                                combo,
                            });
                        }
                    }
                }
            }
        }
    }

    // Bombs (4 cards of the same rank, any suit). A hand can hold at most
    // one bomb, so a single pass over rank groups is enough.
    let mut by_rank: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for (i, c) in hand.iter().enumerate() {
        by_rank.entry(c.rank_index()).or_insert_with(Vec::new).push(i);
    }
    for indices in by_rank.values() {
        if indices.len() == 4 {
            let cards: Vec<Card> = indices.iter().map(|&i| hand[i].clone()).collect();
            if let Some(combo) = combo::detect_combo(&cards) {
                combos.push(ValidCombo {
                    indices: indices.clone(),
                    combo,
                });
            }
        }
    }

    combos
}

/// Calculate a simple strength score for a combo.
/// Lower score = weaker combo (better to play first).
fn combo_strength(hand: &[Card], valid_combo: &ValidCombo) -> i32 {
    match valid_combo.combo.combo_type {
        ComboType::Single => {
            hand[valid_combo.indices[0]].rank_index() as i32 * 10 + 900
        }
        ComboType::Pair => {
            hand[valid_combo.indices[0]].rank_index() as i32 * 10 + 2
        }
        ComboType::Triple => {
            hand[valid_combo.indices[0]].rank_index() as i32 * 10 + 3
        }
        ComboType::Straight => {
            let high = valid_combo.combo.cards.last().unwrap().rank_index() as i32;
            let len = valid_combo.combo.cards.len() as i32;
            high * 10 + len
        }
        ComboType::FullHouse => {
            let triple_rank = valid_combo.combo.cards[0].rank_index() as i32;
            triple_rank * 10 + 50
        }
        ComboType::FourKind => {
            let quad_rank = valid_combo.combo.cards[0].rank_index() as i32;
            quad_rank * 10 + 100
        }
        ComboType::Bomb => {
            // Bombs are reaction-only (validate_play gates them), so free
            // play should almost never pick one. High base strength keeps
            // them below singles in preference; the lowest legal bomb rank
            // is still preferred when a bomb IS the right play.
            let bomb_rank = valid_combo.combo.cards[0].rank_index() as i32;
            9000 + bomb_rank * 10
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::card::{Card, Rank, Suit};
    use crate::game::state::{GamePhase, Player, TrickState};

    fn card(rank: Rank, suit: Suit) -> Card {
        Card::new(rank, suit)
    }

    fn make_state(hand: Vec<Card>) -> GameState {
        GameState {
            phase: GamePhase::Playing,
            players: vec![
                Player { id: 0, name: "P0".to_string(), hand, finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 1, name: "Bot1".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 2, name: "Bot2".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
                Player { id: 3, name: "Bot3".to_string(), hand: Vec::new(), finished: false, is_bot: false, connected: true, is_creator: false },
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
            play_limit_secs: 10,
            winning_point: 50,
            game_winner: None,
            turn_seq: 0,
        }
    }

    #[test]
    fn test_bot_free_play_picks_single() {
        let hand = vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::King, Suit::Diamonds),
            card(Rank::Two, Suit::Diamonds),
        ];
        let state = make_state(hand);
        let result = bot_play(&state, 0);
        assert!(result.is_some());
        // Should play lowest card
        assert_eq!(result.unwrap(), vec![0]);
    }

    #[test]
    fn test_bot_free_play_picks_pair() {
        let hand = vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Three, Suit::Clubs),
            card(Rank::Two, Suit::Diamonds),
        ];
        let state = make_state(hand);
        let result = bot_play(&state, 0);
        assert!(result.is_some());
        // Should play the pair of 3s (lowest combo)
        assert_eq!(result.unwrap(), vec![0, 1]);
    }

    #[test]
    fn test_bot_beat_play() {
        let mut state = make_state(vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::King, Suit::Diamonds),
            card(Rank::Two, Suit::Diamonds),
        ]);

        // Table has a 3 of clubs
        state.trick.cards = vec![card(Rank::Three, Suit::Clubs)];
        state.trick.combo_type = Some(ComboType::Single);
        state.trick.combo_player = Some(1);

        let result = bot_play(&state, 0);
        assert!(result.is_some());
        // Should play King (lowest card that beats 3)
        assert_eq!(result.unwrap(), vec![1]);
    }

    #[test]
    fn test_bot_pass_when_cannot_beat() {
        let mut state = make_state(vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Four, Suit::Diamonds),
            card(Rank::Five, Suit::Diamonds),
        ]);

        // Table has a 2 (highest single)
        state.trick.cards = vec![card(Rank::Two, Suit::Spades)];
        state.trick.combo_type = Some(ComboType::Single);
        state.trick.combo_player = Some(1);

        let result = bot_play(&state, 0);
        assert!(result.is_none());
    }

    #[test]
    fn test_bot_three_discard() {
        let mut state = make_state(vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Three, Suit::Clubs),
            card(Rank::King, Suit::Diamonds),
        ]);
        state.phase = GamePhase::ThreeDiscard;

        let result = bot_play(&state, 0);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), vec![0, 1]);
    }

    #[test]
    fn test_bot_three_discard_no_threes() {
        let mut state = make_state(vec![
            card(Rank::King, Suit::Diamonds),
            card(Rank::Ace, Suit::Diamonds),
            card(Rank::Two, Suit::Diamonds),
        ]);
        state.phase = GamePhase::ThreeDiscard;

        let result = bot_play(&state, 0);
        assert!(result.is_none());
    }

    #[test]
    fn test_find_valid_combos_singles() {
        let hand = vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::King, Suit::Diamonds),
        ];
        let combos = find_valid_combos(&hand);
        assert_eq!(combos.len(), 2); // 2 singles
    }

    #[test]
    fn test_find_valid_combos_pair() {
        let hand = vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Three, Suit::Clubs),
            card(Rank::King, Suit::Diamonds),
        ];
        let combos = find_valid_combos(&hand);
        // 3 singles + 1 pair = 4
        assert_eq!(combos.len(), 4);
    }

    #[test]
    fn test_combo_strength_lower_is_weaker() {
        let hand = vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::King, Suit::Diamonds),
        ];
        let combos = find_valid_combos(&hand);

        let three_strength = combo_strength(&hand, &combos[0]);
        let king_strength = combo_strength(&hand, &combos[1]);

        assert!(three_strength < king_strength);
    }
}
