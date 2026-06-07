use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum Rank {
    #[serde(rename = "3")]
    Three,
    #[serde(rename = "4")]
    Four,
    #[serde(rename = "5")]
    Five,
    #[serde(rename = "6")]
    Six,
    #[serde(rename = "7")]
    Seven,
    #[serde(rename = "8")]
    Eight,
    #[serde(rename = "9")]
    Nine,
    #[serde(rename = "10")]
    Ten,
    #[serde(rename = "J")]
    Jack,
    #[serde(rename = "Q")]
    Queen,
    #[serde(rename = "K")]
    King,
    #[serde(rename = "A")]
    Ace,
    #[serde(rename = "2")]
    Two,
}

impl fmt::Display for Rank {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rank::Three => write!(f, "3"),
            Rank::Four => write!(f, "4"),
            Rank::Five => write!(f, "5"),
            Rank::Six => write!(f, "6"),
            Rank::Seven => write!(f, "7"),
            Rank::Eight => write!(f, "8"),
            Rank::Nine => write!(f, "9"),
            Rank::Ten => write!(f, "10"),
            Rank::Jack => write!(f, "J"),
            Rank::Queen => write!(f, "Q"),
            Rank::King => write!(f, "K"),
            Rank::Ace => write!(f, "A"),
            Rank::Two => write!(f, "2"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum Suit {
    #[serde(rename = "diamonds")]
    Diamonds,
    #[serde(rename = "clubs")]
    Clubs,
    #[serde(rename = "hearts")]
    Hearts,
    #[serde(rename = "spades")]
    Spades,
}

impl fmt::Display for Suit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Suit::Diamonds => write!(f, "♦"),
            Suit::Clubs => write!(f, "♣"),
            Suit::Hearts => write!(f, "♥"),
            Suit::Spades => write!(f, "♠"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Card {
    pub rank: Rank,
    pub suit: Suit,
}

impl Card {
    pub fn new(rank: Rank, suit: Suit) -> Self {
        Card { rank, suit }
    }

    pub fn rank_index(&self) -> usize {
        match self.rank {
            Rank::Three => 0,
            Rank::Four => 1,
            Rank::Five => 2,
            Rank::Six => 3,
            Rank::Seven => 4,
            Rank::Eight => 5,
            Rank::Nine => 6,
            Rank::Ten => 7,
            Rank::Jack => 8,
            Rank::Queen => 9,
            Rank::King => 10,
            Rank::Ace => 11,
            Rank::Two => 12,
        }
    }

    pub fn suit_index(&self) -> usize {
        match self.suit {
            Suit::Diamonds => 0,
            Suit::Clubs => 1,
            Suit::Hearts => 2,
            Suit::Spades => 3,
        }
    }

    pub fn label(&self) -> String {
        format!("{}{}", self.rank, self.suit)
    }
}

impl fmt::Display for Card {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.rank, self.suit)
    }
}

pub fn create_deck() -> Vec<Card> {
    let ranks = [
        Rank::Three, Rank::Four, Rank::Five, Rank::Six, Rank::Seven,
        Rank::Eight, Rank::Nine, Rank::Ten, Rank::Jack, Rank::Queen,
        Rank::King, Rank::Ace, Rank::Two,
    ];
    let suits = [
        Suit::Diamonds, Suit::Clubs, Suit::Hearts, Suit::Spades,
    ];

    let mut deck = Vec::with_capacity(52);
    for &suit in &suits {
        for &rank in &ranks {
            deck.push(Card::new(rank, suit));
        }
    }
    deck
}

pub fn shuffle(deck: &mut Vec<Card>) {
    use rand::seq::SliceRandom;
    deck.shuffle(&mut rand::thread_rng());
}

pub fn sort_cards(cards: &mut Vec<Card>) {
    cards.sort_by(|a, b| {
        a.rank_index().cmp(&b.rank_index())
            .then(a.suit_index().cmp(&b.suit_index()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rank_ordering() {
        let ranks = [
            Rank::Three, Rank::Four, Rank::Five, Rank::Six, Rank::Seven,
            Rank::Eight, Rank::Nine, Rank::Ten, Rank::Jack, Rank::Queen,
            Rank::King, Rank::Ace, Rank::Two,
        ];
        for i in 0..ranks.len() {
            assert_eq!(Card::new(ranks[i], Suit::Diamonds).rank_index(), i);
        }
    }

    #[test]
    fn test_suit_ordering() {
        let suits = [Suit::Diamonds, Suit::Clubs, Suit::Hearts, Suit::Spades];
        for i in 0..suits.len() {
            assert_eq!(Card::new(Rank::Three, suits[i]).suit_index(), i);
        }
    }

    #[test]
    fn test_create_deck_count() {
        let deck = create_deck();
        assert_eq!(deck.len(), 52);
    }

    #[test]
    fn test_create_deck_unique_cards() {
        let deck = create_deck();
        let unique: std::collections::HashSet<_> = deck.iter().cloned().collect();
        assert_eq!(unique.len(), 52);
    }

    #[test]
    fn test_shuffle_preserves_count() {
        let mut deck = create_deck();
        shuffle(&mut deck);
        assert_eq!(deck.len(), 52);
    }

    #[test]
    fn test_sort_cards_basic() {
        let mut cards = vec![
            Card::new(Rank::King, Suit::Diamonds),
            Card::new(Rank::Three, Suit::Diamonds),
            Card::new(Rank::Ace, Suit::Diamonds),
        ];
        sort_cards(&mut cards);
        assert_eq!(cards[0].rank, Rank::Three);
        assert_eq!(cards[1].rank, Rank::King);
        assert_eq!(cards[2].rank, Rank::Ace);
    }

    #[test]
    fn test_card_label() {
        let card = Card::new(Rank::Ten, Suit::Diamonds);
        assert_eq!(card.label(), "10♦");
    }

    #[test]
    fn test_card_display() {
        let card = Card::new(Rank::Jack, Suit::Hearts);
        assert_eq!(format!("{}", card), "J♥");
    }

    #[test]
    fn test_rank_display() {
        assert_eq!(format!("{}", Rank::Three), "3");
        assert_eq!(format!("{}", Rank::Ten), "10");
        assert_eq!(format!("{}", Rank::Jack), "J");
        assert_eq!(format!("{}", Rank::Queen), "Q");
        assert_eq!(format!("{}", Rank::King), "K");
        assert_eq!(format!("{}", Rank::Ace), "A");
        assert_eq!(format!("{}", Rank::Two), "2");
    }

    #[test]
    fn test_suit_display() {
        assert_eq!(format!("{}", Suit::Diamonds), "♦");
        assert_eq!(format!("{}", Suit::Clubs), "♣");
        assert_eq!(format!("{}", Suit::Hearts), "♥");
        assert_eq!(format!("{}", Suit::Spades), "♠");
    }

    #[test]
    fn test_deck_has_all_ranks() {
        let deck = create_deck();
        for rank in [
            Rank::Three, Rank::Four, Rank::Five, Rank::Six, Rank::Seven,
            Rank::Eight, Rank::Nine, Rank::Ten, Rank::Jack, Rank::Queen,
            Rank::King, Rank::Ace, Rank::Two,
        ] {
            assert_eq!(
                deck.iter().filter(|c| c.rank == rank).count(),
                4,
                "Rank {} should appear 4 times",
                rank
            );
        }
    }

    #[test]
    fn test_deck_has_all_suits() {
        let deck = create_deck();
        for suit in [Suit::Diamonds, Suit::Clubs, Suit::Hearts, Suit::Spades] {
            assert_eq!(
                deck.iter().filter(|c| c.suit == suit).count(),
                13,
                "Suit {:?} should appear 13 times",
                suit
            );
        }
    }

    #[test]
    fn test_sort_stable() {
        let mut cards = vec![
            Card::new(Rank::Three, Suit::Diamonds),
            Card::new(Rank::Three, Suit::Clubs),
            Card::new(Rank::Three, Suit::Hearts),
            Card::new(Rank::Three, Suit::Spades),
        ];
        sort_cards(&mut cards);
        assert_eq!(cards[0].suit, Suit::Diamonds);
        assert_eq!(cards[1].suit, Suit::Clubs);
        assert_eq!(cards[2].suit, Suit::Hearts);
        assert_eq!(cards[3].suit, Suit::Spades);
    }

    #[test]
    fn test_shuffle_randomizes() {
        let mut deck1 = create_deck();
        let mut deck2 = create_deck();
        shuffle(&mut deck1);
        shuffle(&mut deck2);
        // Highly unlikely they'd be identical
        assert_ne!(deck1, deck2);
    }
}
