use super::card::Card;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ComboType {
    Single,
    Pair,
    Triple,
    Straight,
    FullHouse,
    FourKind,
}

impl std::fmt::Display for ComboType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComboType::Single => write!(f, "Single"),
            ComboType::Pair => write!(f, "Pair"),
            ComboType::Triple => write!(f, "Triple"),
            ComboType::Straight => write!(f, "Straight"),
            ComboType::FullHouse => write!(f, "Full House"),
            ComboType::FourKind => write!(f, "Four of a Kind"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Combo {
    pub combo_type: ComboType,
    pub cards: Vec<Card>,
}

pub fn is_straight(ranks: &[usize]) -> bool {
    if ranks.len() < 3 {
        return false;
    }
    let mut unique: Vec<usize> = ranks.iter().cloned().collect();
    unique.sort();
    unique.dedup();
    if unique.len() != ranks.len() {
        return false;
    }

    // 2s cannot appear in straights
    if unique.iter().any(|&r| r >= 12) {
        return false;
    }

    // All numbers (3-9): rank indices 0-6
    let all_numbers = unique.iter().all(|&r| r <= 6);
    // All letters (10-A): rank indices 7-11
    let all_letters = unique.iter().all(|&r| r >= 7);

    if !all_numbers && !all_letters {
        return false;
    }

    for i in 1..unique.len() {
        if unique[i] != unique[i - 1] + 1 {
            return false;
        }
    }

    true
}

pub fn detect_combo(cards: &[Card]) -> Option<Combo> {
    let n = cards.len();
    let mut sorted = cards.to_vec();
    sorted.sort_by(|a, b| {
        a.rank_index().cmp(&b.rank_index())
            .then(a.suit_index().cmp(&b.suit_index()))
    });

    let ranks = sorted.iter().map(|c| c.rank_index()).collect::<Vec<_>>();
    let suits = sorted.iter().map(|c| c.suit).collect::<Vec<_>>();

    let is_same_suit = suits.iter().all(|&s| s == suits[0]);

    // Count rank frequencies
    let mut rank_counts: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for &r in &ranks {
        *rank_counts.entry(r).or_insert(0) += 1;
    }
    let mut counts: Vec<usize> = rank_counts.values().cloned().collect();
    counts.sort();

    match n {
        1 => Some(Combo {
            combo_type: ComboType::Single,
            cards: sorted,
        }),
        2 if counts[0] == 2 => Some(Combo {
            combo_type: ComboType::Pair,
            cards: sorted,
        }),
        3 => {
            if counts[0] == 3 {
                Some(Combo {
                    combo_type: ComboType::Triple,
                    cards: sorted,
                })
            } else if is_same_suit && is_straight(&ranks) {
                Some(Combo {
                    combo_type: ComboType::Straight,
                    cards: sorted,
                })
            } else {
                None
            }
        }
        4 => {
            if is_same_suit && is_straight(&ranks) {
                Some(Combo {
                    combo_type: ComboType::Straight,
                    cards: sorted,
                })
            } else {
                None
            }
        }
        5 => {
            if counts[0] == 1 && counts[1] == 4 {
                Some(Combo {
                    combo_type: ComboType::FourKind,
                    cards: sorted,
                })
            } else if counts[0] == 2 && counts[1] == 3 {
                Some(Combo {
                    combo_type: ComboType::FullHouse,
                    cards: sorted,
                })
            } else if is_same_suit && is_straight(&ranks) {
                Some(Combo {
                    combo_type: ComboType::Straight,
                    cards: sorted,
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Compare two combos of the same type.
/// Returns: positive if A > B, negative if A < B, 0 if equal, None if different types.
pub fn compare_combos(combo_a: &Combo, combo_b: &Combo) -> Option<i32> {
    if combo_a.combo_type != combo_b.combo_type {
        return None;
    }

    match combo_a.combo_type {
        ComboType::Single => {
            let a = &combo_a.cards[0];
            let b = &combo_b.cards[0];
            Some(a.rank_index() as i32 - b.rank_index() as i32)
        }
        ComboType::Pair | ComboType::Triple => {
            let a_high = combo_a.cards.last().unwrap().rank_index() as i32;
            let b_high = combo_b.cards.last().unwrap().rank_index() as i32;
            Some(a_high - b_high)
        }
        ComboType::Straight => {
            let a_high = combo_a.cards.last().unwrap().rank_index() as i32;
            let b_high = combo_b.cards.last().unwrap().rank_index() as i32;
            Some(a_high - b_high)
        }
        ComboType::FullHouse => {
            let a_triple_rank = find_triple_rank(&combo_a.cards);
            let b_triple_rank = find_triple_rank(&combo_b.cards);
            if a_triple_rank != b_triple_rank {
                return Some(a_triple_rank as i32 - b_triple_rank as i32);
            }
            let a_pair_rank = find_pair_rank(&combo_a.cards);
            let b_pair_rank = find_pair_rank(&combo_b.cards);
            Some(a_pair_rank as i32 - b_pair_rank as i32)
        }
        ComboType::FourKind => {
            let a_quad_rank = find_quad_rank(&combo_a.cards);
            let b_quad_rank = find_quad_rank(&combo_b.cards);
            if a_quad_rank != b_quad_rank {
                return Some(a_quad_rank as i32 - b_quad_rank as i32);
            }
            let a_kicker = find_kicker(&combo_a.cards, a_quad_rank);
            let b_kicker = find_kicker(&combo_b.cards, b_quad_rank);
            Some(a_kicker as i32 - b_kicker as i32)
        }
    }
}

fn find_triple_rank(cards: &[Card]) -> usize {
    let mut counts: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for c in cards {
        *counts.entry(c.rank_index()).or_insert(0) += 1;
    }
    for (rank, count) in &counts {
        if *count == 3 {
            return *rank;
        }
    }
    cards[0].rank_index()
}

fn find_pair_rank(cards: &[Card]) -> usize {
    let mut counts: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for c in cards {
        *counts.entry(c.rank_index()).or_insert(0) += 1;
    }
    for (rank, count) in &counts {
        if *count == 2 {
            return *rank;
        }
    }
    cards[0].rank_index()
}

fn find_quad_rank(cards: &[Card]) -> usize {
    let mut counts: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for c in cards {
        *counts.entry(c.rank_index()).or_insert(0) += 1;
    }
    for (rank, count) in &counts {
        if *count == 4 {
            return *rank;
        }
    }
    cards[0].rank_index()
}

fn find_kicker(cards: &[Card], quad_rank: usize) -> usize {
    for c in cards {
        if c.rank_index() != quad_rank {
            return c.rank_index();
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::card::{create_deck, Card, Rank, Suit};

    fn card(rank: Rank, suit: Suit) -> Card {
        Card::new(rank, suit)
    }

    // --- is_straight tests ---

    #[test]
    fn test_straight_min_3_cards() {
        assert!(is_straight(&[0, 1, 2])); // 3-4-5
    }

    #[test]
    fn test_straight_5_cards() {
        assert!(is_straight(&[0, 1, 2, 3, 4])); // 3-4-5-6-7
    }

    #[test]
    fn test_straight_letters() {
        assert!(is_straight(&[7, 8, 9])); // 10-J-Q
    }

    #[test]
    fn test_straight_10_j_q_k() {
        assert!(is_straight(&[7, 8, 9, 10])); // 10-J-Q-K
    }

    #[test]
    fn test_straight_10_j_q_k_a() {
        assert!(is_straight(&[7, 8, 9, 10, 11])); // 10-J-Q-K-A
    }

    #[test]
    fn test_straight_no_two() {
        assert!(!is_straight(&[9, 10, 11, 12])); // Q-K-A-2
    }

    #[test]
    fn test_straight_mixed_invalid() {
        assert!(!is_straight(&[5, 6, 7])); // 9-10-J — mixed number/letter
    }

    #[test]
    fn test_straight_not_consecutive() {
        assert!(!is_straight(&[0, 2, 3])); // 3-5-6
    }

    #[test]
    fn test_straight_too_few() {
        assert!(!is_straight(&[0, 1]));
    }

    #[test]
    fn test_straight_duplicates() {
        assert!(!is_straight(&[0, 0, 1, 2]));
    }

    #[test]
    fn test_straight_single() {
        assert!(!is_straight(&[0]));
    }

    // --- detect_combo tests ---

    #[test]
    fn test_single() {
        let c = card(Rank::Three, Suit::Diamonds);
        let combo = detect_combo(&[c]).unwrap();
        assert_eq!(combo.combo_type, ComboType::Single);
    }

    #[test]
    fn test_pair() {
        let cards = vec![card(Rank::Three, Suit::Diamonds), card(Rank::Three, Suit::Clubs)];
        let combo = detect_combo(&cards).unwrap();
        assert_eq!(combo.combo_type, ComboType::Pair);
    }

    #[test]
    fn test_triple() {
        let cards = vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Three, Suit::Clubs),
            card(Rank::Three, Suit::Hearts),
        ];
        let combo = detect_combo(&cards).unwrap();
        assert_eq!(combo.combo_type, ComboType::Triple);
    }

    #[test]
    fn test_straight_3_card() {
        let cards = vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Four, Suit::Diamonds),
            card(Rank::Five, Suit::Diamonds),
        ];
        let combo = detect_combo(&cards).unwrap();
        assert_eq!(combo.combo_type, ComboType::Straight);
    }

    #[test]
    fn test_straight_4_card() {
        let cards = vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Four, Suit::Diamonds),
            card(Rank::Five, Suit::Diamonds),
            card(Rank::Six, Suit::Diamonds),
        ];
        let combo = detect_combo(&cards).unwrap();
        assert_eq!(combo.combo_type, ComboType::Straight);
    }

    #[test]
    fn test_straight_5_card() {
        let cards = vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Four, Suit::Diamonds),
            card(Rank::Five, Suit::Diamonds),
            card(Rank::Six, Suit::Diamonds),
            card(Rank::Seven, Suit::Diamonds),
        ];
        let combo = detect_combo(&cards).unwrap();
        assert_eq!(combo.combo_type, ComboType::Straight);
    }

    #[test]
    fn test_straight_mixed_suit_invalid() {
        let cards = vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Four, Suit::Clubs),
            card(Rank::Five, Suit::Diamonds),
        ];
        assert!(detect_combo(&cards).is_none());
    }

    #[test]
    fn test_full_house() {
        let cards = vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Three, Suit::Clubs),
            card(Rank::Three, Suit::Hearts),
            card(Rank::Seven, Suit::Diamonds),
            card(Rank::Seven, Suit::Clubs),
        ];
        let combo = detect_combo(&cards).unwrap();
        assert_eq!(combo.combo_type, ComboType::FullHouse);
    }

    #[test]
    fn test_four_kind() {
        let cards = vec![
            card(Rank::King, Suit::Diamonds),
            card(Rank::King, Suit::Clubs),
            card(Rank::King, Suit::Hearts),
            card(Rank::King, Suit::Spades),
            card(Rank::Seven, Suit::Diamonds),
        ];
        let combo = detect_combo(&cards).unwrap();
        assert_eq!(combo.combo_type, ComboType::FourKind);
    }

    #[test]
    fn test_invalid_combo_2_cards_different() {
        let cards = vec![card(Rank::Three, Suit::Diamonds), card(Rank::Four, Suit::Diamonds)];
        assert!(detect_combo(&cards).is_none());
    }

    #[test]
    fn test_invalid_combo_4_cards_no_straight() {
        let cards = vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Three, Suit::Clubs),
            card(Rank::Four, Suit::Diamonds),
            card(Rank::Five, Suit::Diamonds),
        ];
        assert!(detect_combo(&cards).is_none());
    }

    #[test]
    fn test_invalid_combo_6_cards() {
        let cards = vec![
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Three, Suit::Clubs),
            card(Rank::Three, Suit::Hearts),
            card(Rank::Three, Suit::Spades),
            card(Rank::Seven, Suit::Diamonds),
            card(Rank::Seven, Suit::Clubs),
        ];
        assert!(detect_combo(&cards).is_none());
    }

    // --- compare_combos tests ---

    #[test]
    fn test_compare_different_types() {
        let single = detect_combo(&[card(Rank::Three, Suit::Diamonds)]).unwrap();
        let pair = detect_combo(&[card(Rank::Three, Suit::Diamonds), card(Rank::Three, Suit::Clubs)]).unwrap();
        assert_eq!(compare_combos(&single, &pair), None);
    }

    #[test]
    fn test_compare_single_rank() {
        let a = detect_combo(&[card(Rank::King, Suit::Diamonds)]).unwrap();
        let b = detect_combo(&[card(Rank::Three, Suit::Diamonds)]).unwrap();
        assert!(compare_combos(&a, &b).unwrap() > 0);
    }

    #[test]
    fn test_compare_single_same_rank_equal() {
        let a = detect_combo(&[card(Rank::Three, Suit::Spades)]).unwrap();
        let b = detect_combo(&[card(Rank::Three, Suit::Diamonds)]).unwrap();
        assert_eq!(compare_combos(&a, &b).unwrap(), 0);
    }

    #[test]
    fn test_compare_pair_rank() {
        let a = detect_combo(&[card(Rank::King, Suit::Diamonds), card(Rank::King, Suit::Clubs)]).unwrap();
        let b = detect_combo(&[card(Rank::Three, Suit::Diamonds), card(Rank::Three, Suit::Clubs)]).unwrap();
        assert!(compare_combos(&a, &b).unwrap() > 0);
    }

    #[test]
    fn test_compare_pair_same_rank() {
        let a = detect_combo(&[card(Rank::King, Suit::Diamonds), card(Rank::King, Suit::Clubs)]).unwrap();
        let b = detect_combo(&[card(Rank::King, Suit::Hearts), card(Rank::King, Suit::Spades)]).unwrap();
        assert_eq!(compare_combos(&a, &b).unwrap(), 0);
    }

    #[test]
    fn test_compare_straight_higher_wins() {
        let a = detect_combo(&[
            card(Rank::Five, Suit::Diamonds),
            card(Rank::Six, Suit::Diamonds),
            card(Rank::Seven, Suit::Diamonds),
        ]).unwrap();
        let b = detect_combo(&[
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Four, Suit::Diamonds),
            card(Rank::Five, Suit::Diamonds),
        ]).unwrap();
        assert!(compare_combos(&a, &b).unwrap() > 0);
    }

    #[test]
    fn test_compare_full_house_triple_rank() {
        let a = detect_combo(&[
            card(Rank::Ace, Suit::Diamonds),
            card(Rank::Ace, Suit::Clubs),
            card(Rank::Ace, Suit::Hearts),
            card(Rank::Two, Suit::Diamonds),
            card(Rank::Two, Suit::Clubs),
        ]).unwrap();
        let b = detect_combo(&[
            card(Rank::King, Suit::Diamonds),
            card(Rank::King, Suit::Clubs),
            card(Rank::King, Suit::Hearts),
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Three, Suit::Clubs),
        ]).unwrap();
        assert!(compare_combos(&a, &b).unwrap() > 0);
    }

    #[test]
    fn test_compare_four_kind_quad_rank() {
        let a = detect_combo(&[
            card(Rank::Ace, Suit::Diamonds),
            card(Rank::Ace, Suit::Clubs),
            card(Rank::Ace, Suit::Hearts),
            card(Rank::Ace, Suit::Spades),
            card(Rank::Two, Suit::Diamonds),
        ]).unwrap();
        let b = detect_combo(&[
            card(Rank::King, Suit::Diamonds),
            card(Rank::King, Suit::Clubs),
            card(Rank::King, Suit::Hearts),
            card(Rank::King, Suit::Spades),
            card(Rank::Three, Suit::Diamonds),
        ]).unwrap();
        assert!(compare_combos(&a, &b).unwrap() > 0);
    }

    #[test]
    fn test_compare_four_kind_kicker() {
        let a = detect_combo(&[
            card(Rank::King, Suit::Diamonds),
            card(Rank::King, Suit::Clubs),
            card(Rank::King, Suit::Hearts),
            card(Rank::King, Suit::Spades),
            card(Rank::Ace, Suit::Diamonds),
        ]).unwrap();
        let b = detect_combo(&[
            card(Rank::King, Suit::Diamonds),
            card(Rank::King, Suit::Clubs),
            card(Rank::King, Suit::Hearts),
            card(Rank::King, Suit::Spades),
            card(Rank::Three, Suit::Diamonds),
        ]).unwrap();
        assert!(compare_combos(&a, &b).unwrap() > 0);
    }
}
