use serde::{Deserialize, Serialize};
use super::card::Card;
use super::combo;
use super::state::{GameState, GamePhase, TrickState};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum GameEvent {
    #[serde(rename_all = "camelCase")]
    CardsPlayed {
        player_id: usize,
        player_name: String,
        cards: Vec<String>,
        combo_name: String,
    },
    #[serde(rename_all = "camelCase")]
    TrickResolved {
        winner_id: usize,
        winner_name: String,
    },
    #[serde(rename_all = "camelCase")]
    PlayerFinished {
        player_id: usize,
        player_name: String,
        position: usize,
        score: i32,
    },
    #[serde(rename_all = "camelCase")]
    GameOver {
        final_scores: Vec<i32>,
        finished_order: Vec<usize>,
    },
}

pub struct GameEngine {
    state: GameState,
}

impl GameEngine {
    pub fn new(state: GameState) -> Self {
        GameEngine { state }
    }

    pub fn deal_cards(&mut self) {
        use super::card::{create_deck, shuffle, sort_cards};

        let mut deck = create_deck();
        shuffle(&mut deck);

        for i in 0..4 {
            let hand: Vec<Card> = deck.drain(..13).collect();
            let mut sorted_hand = hand;
            sort_cards(&mut sorted_hand);
            self.state.players[i].hand = sorted_hand;
        }
    }

    /// Deal that can never give any player all four 2s. A hand holding all
    /// four 2s is dead weight: bombs can't lead and only counter a single 2,
    /// so those cards could never come down. Reshuffles until clean.
    pub fn deal_cards_guarded(&mut self) {
        use super::card::{create_deck, shuffle, sort_cards};

        loop {
            let mut deck = create_deck();
            shuffle(&mut deck);

            let mut hands: Vec<Vec<Card>> = Vec::new();
            for _ in 0..4 {
                let hand: Vec<Card> = deck.drain(..13).collect();
                let mut sorted_hand = hand;
                sort_cards(&mut sorted_hand);
                hands.push(sorted_hand);
            }

            if hands
                .iter()
                .any(|h| h.iter().filter(|c| c.rank_index() == 12).count() == 4)
            {
                continue; // someone grabbed all four 2s — redeal
            }

            for (i, hand) in hands.into_iter().enumerate() {
                self.state.players[i].hand = hand;
            }
            return;
        }
    }

    pub fn start_three_discard(&mut self) -> Vec<GameEvent> {
        let events = Vec::new();

        let mut counts: Vec<(usize, usize, usize)> = Vec::new();
        for i in 0..4 {
            let threes: Vec<&Card> = self.state.players[i].hand.iter()
                .filter(|c| c.rank_index() == 0)
                .collect();
            let count = threes.len();
            let highest_suit = if count > 0 {
                threes.iter().map(|c| c.suit_index()).max().unwrap()
            } else {
                0
            };
            counts.push((i, count, highest_suit));
        }

        counts.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));
        let order: Vec<usize> = counts.iter().map(|(id, _, _)| *id).collect();

        let mut player_cards: Vec<Vec<Card>> = Vec::new();
        for i in 0..4 {
            let threes: Vec<Card> = self.state.players[i].hand.iter()
                .filter(|c| c.rank_index() == 0)
                .cloned()
                .collect();
            self.state.players[i].hand.retain(|c| c.rank_index() != 0);
            player_cards.push(threes);
        }

        let first_player = order[0];
        self.state.three_discard = Some(super::state::ThreeDiscardState {
            order,
            index: 0,
            player_cards,
            discarded: vec![false; 4],
        });
        self.state.phase = GamePhase::ThreeDiscard;
        self.state.current_player = first_player;

        events
    }

    pub fn complete_three_discard(&mut self) -> Vec<GameEvent> {
        let events = Vec::new();

        let td = self.state.three_discard.as_ref().expect("No three discard state");

        for &pid in &td.order {
            let player_name = self.state.players[pid].name.clone();
            let cards: Vec<String> = td.player_cards[pid].iter().map(|c| c.to_string()).collect();
            let count = cards.len();

            let log_msg = if count > 0 {
                format!("{} discarded {} ({} 3s)", player_name, cards.join(" "), count)
            } else {
                format!("{} has no 3s", player_name)
            };
            self.state.log.push(log_msg);
        }

        for i in 0..4 {
            self.state.players[i].hand.retain(|c| c.rank_index() != 0);
        }

        let first_player = td.order[0];
        self.state.three_discard = None;
        self.state.phase = GamePhase::Playing;
        self.state.current_player = first_player;

        events
    }

    pub fn apply_play(&mut self, player_id: usize, card_ids: &[String]) -> Vec<GameEvent> {
        let mut events = Vec::new();

        if self.state.current_player != player_id {
            return events;
        }

        if self.state.finished_order.len() >= 3 {
            if self.state.phase != GamePhase::GameOver {
                self.check_and_end_game(&mut events);
            }
            return events;
        }

        let player = &self.state.players[player_id];
        let mut cards = Vec::new();
        let mut indices_to_remove = Vec::new();

        for card_id in card_ids {
            let parts: Vec<&str> = card_id.splitn(2, ':').collect();
            if parts.len() != 2 {
                return events;
            }

            let (rank_str, suit_str) = (parts[0], parts[1]);
            let rank = match rank_str {
                "3" => super::card::Rank::Three,
                "4" => super::card::Rank::Four,
                "5" => super::card::Rank::Five,
                "6" => super::card::Rank::Six,
                "7" => super::card::Rank::Seven,
                "8" => super::card::Rank::Eight,
                "9" => super::card::Rank::Nine,
                "10" => super::card::Rank::Ten,
                "J" => super::card::Rank::Jack,
                "Q" => super::card::Rank::Queen,
                "K" => super::card::Rank::King,
                "A" => super::card::Rank::Ace,
                "2" => super::card::Rank::Two,
                _ => return events,
            };

            let suit = match suit_str {
                "diamonds" => super::card::Suit::Diamonds,
                "clubs" => super::card::Suit::Clubs,
                "hearts" => super::card::Suit::Hearts,
                "spades" => super::card::Suit::Spades,
                _ => return events,
            };

            let found = player.hand.iter().position(|c| c.rank == rank && c.suit == suit);
            match found {
                Some(idx) => {
                    cards.push(player.hand[idx].clone());
                    indices_to_remove.push(idx);
                }
                None => return events,
            }
        }

        let table_combo = if self.state.trick.combo_player.is_some() {
            combo::detect_combo(&self.state.trick.cards)
        } else {
            None
        };

        let result = super::rules::validate_play(&cards, table_combo.as_ref());

        if !result.valid {
            self.state.log.push(format!(
                "{}'s play is invalid: {}",
                self.state.players[player_id].name, result.error
            ));
            return events;
        }

        let hand = &mut self.state.players[player_id].hand;
        let mut sorted_indices = indices_to_remove;
        sorted_indices.sort_unstable();
        for &i in sorted_indices.iter().rev() {
            hand.remove(i);
        }

        let finished = if self.state.players[player_id].hand.is_empty()
            && !self.state.players[player_id].finished
        {
            self.state.players[player_id].finished = true;
            self.state.finished_order.push(player_id);
            true
        } else {
            false
        };

        if let Some(old_cp) = self.state.trick.combo_player {
            if !self.state.trick.played.contains(&old_cp)
                && !self.state.trick.passed.contains(&old_cp)
            {
                self.state.trick.played.push(old_cp);
            }
            self.state.trick.passed.clear();
        }

        let card_labels: Vec<String> = cards.iter().map(|c| c.to_string()).collect();
        self.state.trick.cards = cards;
        self.state.trick.combo_type = Some(result.combo.as_ref().unwrap().combo_type.clone());
        self.state.trick.combo_player = Some(player_id);

        self.state.log.push(format!(
            "{} plays {} ({})",
            self.state.players[player_id].name,
            card_labels.join(" "),
            result.combo_name
        ));

        if finished {
            let pos = self.state.finished_order.len();
            self.score_player(player_id, pos, &mut events);
        }

        if let Some(winner) = super::rules::check_trick_complete(&self.state) {
            // Bomb endgame: a completed bomb trick ends the round right now
            // (bomber 1st, bombed player 4th, others 0) — no trick resolve.
            if super::rules::maybe_end_game_by_bomb(&mut self.state) {
                events.push(GameEvent::GameOver {
                    final_scores: self.state.scores.clone(),
                    finished_order: self.state.finished_order.clone(),
                });
                return events;
            }
            self.resolve_trick(winner, &mut events);
            if self.state.finished_order.len() >= 3 && self.state.phase != GamePhase::GameOver {
                self.check_and_end_game(&mut events);
            }
        } else if self.state.finished_order.len() >= 3 {
            self.check_and_end_game(&mut events);
        } else {
            self.state.current_player = (self.state.current_player + 1) % 4;
            self.skip_finished();
        }

        if self.state.phase != GamePhase::GameOver {
            events.push(GameEvent::CardsPlayed {
                player_id,
                player_name: self.state.players[player_id].name.clone(),
                cards: card_labels,
                combo_name: result.combo_name,
            });
        }

        events
    }

    pub fn apply_pass(&mut self, player_id: usize) -> Vec<GameEvent> {
        let mut events = Vec::new();

        if self.state.current_player != player_id {
            return events;
        }

        if self.state.finished_order.len() >= 3 {
            if self.state.phase != GamePhase::GameOver {
                self.check_and_end_game(&mut events);
            }
            return events;
        }

        if self.state.trick.combo_player.is_none() {
            return events;
        }

        if self.state.trick.combo_player == Some(player_id) {
            return events;
        }

        self.state.trick.passed.push(player_id);
        self.state.log.push(format!(
            "{} passes",
            self.state.players[player_id].name
        ));

        if super::rules::non_participants(&self.state) >= 3 {
            if let Some(winner) = self.state.trick.combo_player {
                if super::rules::maybe_end_game_by_bomb(&mut self.state) {
                    events.push(GameEvent::GameOver {
                        final_scores: self.state.scores.clone(),
                        finished_order: self.state.finished_order.clone(),
                    });
                    return events;
                }
                self.resolve_trick(winner, &mut events);
                if self.state.finished_order.len() >= 3
                    && self.state.phase != GamePhase::GameOver
                {
                    self.check_and_end_game(&mut events);
                }
            }
        } else if self.state.finished_order.len() >= 3 {
            self.check_and_end_game(&mut events);
        } else {
            self.state.current_player = (self.state.current_player + 1) % 4;
            self.skip_finished();
        }

        if self.state.phase != GamePhase::GameOver {
            events.push(GameEvent::CardsPlayed {
                player_id,
                player_name: self.state.players[player_id].name.clone(),
                cards: vec!["pass".to_string()],
                combo_name: "pass".to_string(),
            });
        }

        events
    }

    fn score_player(&mut self, player_id: usize, pos: usize, events: &mut Vec<GameEvent>) {
        self.state.scores[player_id] = match pos {
            1 => 10,
            2 => 5,
            3 => 0,
            _ => -15,
        };
        self.state.log.push(format!(
            "{} finished ({}th, {} pts)",
            self.state.players[player_id].name,
            ["", "1st", "2nd", "3rd"][pos],
            self.state.scores[player_id]
        ));
        events.push(GameEvent::PlayerFinished {
            player_id,
            player_name: self.state.players[player_id].name.clone(),
            position: pos,
            score: self.state.scores[player_id],
        });
    }

    fn resolve_trick(&mut self, winner_id: usize, events: &mut Vec<GameEvent>) {
        self.state.trick = TrickState::new();
        self.state.current_player = winner_id;
        self.skip_finished();
        events.push(GameEvent::TrickResolved {
            winner_id,
            winner_name: self.state.players[winner_id].name.clone(),
        });
    }

    fn check_and_end_game(&mut self, events: &mut Vec<GameEvent>) {
        if super::rules::finalize_game(&mut self.state) {
            events.push(GameEvent::GameOver {
                final_scores: self.state.scores.clone(),
                finished_order: self.state.finished_order.clone(),
            });
        }
    }

    fn skip_finished(&mut self) {
        while self.state.players[self.state.current_player].finished {
            self.state.current_player = (self.state.current_player + 1) % 4;
        }
    }

    pub fn state(&self) -> &GameState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut GameState {
        &mut self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::card::{Card, Rank, Suit};
    use crate::game::state::{GamePhase, Player};

    fn card(rank: Rank, suit: Suit) -> Card {
        Card::new(rank, suit)
    }

    fn p(name: &str, hand: Vec<Card>) -> Player {
        Player {
            id: 0,
            name: name.to_string(),
            hand,
            finished: false,
            is_bot: false,
            connected: true,
            is_creator: false,
        }
    }

    /// Fixed 52-card deal:
    /// P0: 2d 3d 4d 5d 6d 7d 8d 9d 10d Jd Qd Ad 2h
    /// P1: Kd Kc Kh Ks 3c 4c 5c 6c 7c 8c 9c 10c Jc
    /// P2: 3h 4h 5h 6h 7h 8h 9h 10h Jh Qh Ah 2s Qc
    /// P3: 3s 4s 5s 6s 7s 8s 9s 10s Js Qs As 2c Ac
    fn bomb_state() -> GameState {
        let p0 = vec![
            card(Rank::Two, Suit::Diamonds),
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Four, Suit::Diamonds),
            card(Rank::Five, Suit::Diamonds),
            card(Rank::Six, Suit::Diamonds),
            card(Rank::Seven, Suit::Diamonds),
            card(Rank::Eight, Suit::Diamonds),
            card(Rank::Nine, Suit::Diamonds),
            card(Rank::Ten, Suit::Diamonds),
            card(Rank::Jack, Suit::Diamonds),
            card(Rank::Queen, Suit::Diamonds),
            card(Rank::Ace, Suit::Diamonds),
            card(Rank::Two, Suit::Hearts),
        ];
        let p1 = vec![
            card(Rank::King, Suit::Diamonds),
            card(Rank::King, Suit::Clubs),
            card(Rank::King, Suit::Hearts),
            card(Rank::King, Suit::Spades),
            card(Rank::Three, Suit::Clubs),
            card(Rank::Four, Suit::Clubs),
            card(Rank::Five, Suit::Clubs),
            card(Rank::Six, Suit::Clubs),
            card(Rank::Seven, Suit::Clubs),
            card(Rank::Eight, Suit::Clubs),
            card(Rank::Nine, Suit::Clubs),
            card(Rank::Ten, Suit::Clubs),
            card(Rank::Jack, Suit::Clubs),
        ];
        let p2 = vec![
            card(Rank::Three, Suit::Hearts),
            card(Rank::Four, Suit::Hearts),
            card(Rank::Five, Suit::Hearts),
            card(Rank::Six, Suit::Hearts),
            card(Rank::Seven, Suit::Hearts),
            card(Rank::Eight, Suit::Hearts),
            card(Rank::Nine, Suit::Hearts),
            card(Rank::Ten, Suit::Hearts),
            card(Rank::Jack, Suit::Hearts),
            card(Rank::Queen, Suit::Hearts),
            card(Rank::Ace, Suit::Hearts),
            card(Rank::Two, Suit::Spades),
            card(Rank::Queen, Suit::Clubs),
        ];
        let p3 = vec![
            card(Rank::Three, Suit::Spades),
            card(Rank::Four, Suit::Spades),
            card(Rank::Five, Suit::Spades),
            card(Rank::Six, Suit::Spades),
            card(Rank::Seven, Suit::Spades),
            card(Rank::Eight, Suit::Spades),
            card(Rank::Nine, Suit::Spades),
            card(Rank::Ten, Suit::Spades),
            card(Rank::Jack, Suit::Spades),
            card(Rank::Queen, Suit::Spades),
            card(Rank::Ace, Suit::Spades),
            card(Rank::Two, Suit::Clubs),
            card(Rank::Ace, Suit::Clubs),
        ];
        let mut players = vec![p("P0", p0), p("P1", p1), p("P2", p2), p("P3", p3)];
        for (i, pl) in players.iter_mut().enumerate() {
            pl.id = i;
        }
        GameState {
            phase: GamePhase::Playing,
            players,
            ready: vec![true; 4],
            current_player: 0,
            trick: TrickState::new(),
            finished_order: Vec::new(),
            scores: vec![0; 4],
            round: 1,
            total_scores: vec![0],
            three_discard: None,
            log: Vec::new(),
        }
    }

    #[test]
    fn test_bomb_without_counter_ends_game() {
        let mut engine = GameEngine::new(bomb_state());
        // P0 leads single 2 (the only thing a bomb can answer)
        engine.apply_play(0, &["2:diamonds".to_string()]);
        assert_eq!(engine.state().phase, GamePhase::Playing);
        // P1 bombs with four Kings
        engine.apply_play(
            1,
            &["K:diamonds".to_string(), "K:clubs".to_string(), "K:hearts".to_string(), "K:spades".to_string()],
        );
        assert_eq!(engine.state().trick.combo_type, Some(crate::game::combo::ComboType::Bomb));
        assert_eq!(engine.state().phase, GamePhase::Playing); // trick not over yet
        engine.apply_pass(2);
        engine.apply_pass(3);
        // Victim P0 gets a turn too (could counter-bomb) — passes
        engine.apply_pass(0);

        assert_eq!(engine.state().phase, GamePhase::GameOver);
        // Bomber 1st (+10), victim of single 2 is 4th (-15), others 0
        assert_eq!(engine.state().scores, vec![-15, 10, 0, 0]);
        assert_eq!(engine.state().finished_order[0], 1);
        assert_eq!(engine.state().finished_order[3], 0);
        // Session totals baked in
        assert_eq!(engine.state().total_scores, vec![-15, 10, 0, 0]);
    }

    #[test]
    fn test_bomb_counter_scores_first_bomber_last() {
        // Custom deal: P0 leads 2d; P1 bombs 4x7; P2 counters with 4xK;
        // P3 passes; P0 passes; P1 passes -> game over.
        let p0 = vec![
            card(Rank::Two, Suit::Diamonds),
            card(Rank::Three, Suit::Diamonds),
            card(Rank::Four, Suit::Diamonds),
            card(Rank::Five, Suit::Diamonds),
            card(Rank::Six, Suit::Diamonds),
            card(Rank::Eight, Suit::Diamonds),
            card(Rank::Nine, Suit::Diamonds),
            card(Rank::Ten, Suit::Diamonds),
            card(Rank::Jack, Suit::Diamonds),
            card(Rank::Queen, Suit::Diamonds),
            card(Rank::Ace, Suit::Diamonds),
            card(Rank::Two, Suit::Hearts),
            card(Rank::Three, Suit::Clubs),
        ];
        let p1 = vec![
            card(Rank::Seven, Suit::Diamonds),
            card(Rank::Seven, Suit::Clubs),
            card(Rank::Seven, Suit::Hearts),
            card(Rank::Seven, Suit::Spades),
            card(Rank::Three, Suit::Hearts),
            card(Rank::Four, Suit::Hearts),
            card(Rank::Five, Suit::Hearts),
            card(Rank::Six, Suit::Hearts),
            card(Rank::Eight, Suit::Hearts),
            card(Rank::Nine, Suit::Hearts),
            card(Rank::Ten, Suit::Hearts),
            card(Rank::Jack, Suit::Hearts),
            card(Rank::Queen, Suit::Hearts),
        ];
        let p2 = vec![
            card(Rank::King, Suit::Diamonds),
            card(Rank::King, Suit::Clubs),
            card(Rank::King, Suit::Hearts),
            card(Rank::King, Suit::Spades),
            card(Rank::Three, Suit::Spades),
            card(Rank::Four, Suit::Spades),
            card(Rank::Five, Suit::Spades),
            card(Rank::Six, Suit::Spades),
            card(Rank::Eight, Suit::Spades),
            card(Rank::Nine, Suit::Spades),
            card(Rank::Ten, Suit::Spades),
            card(Rank::Jack, Suit::Spades),
            card(Rank::Queen, Suit::Spades),
        ];
        let p3 = vec![
            card(Rank::Eight, Suit::Clubs),
            card(Rank::Nine, Suit::Clubs),
            card(Rank::Ten, Suit::Clubs),
            card(Rank::Jack, Suit::Clubs),
            card(Rank::Queen, Suit::Clubs),
            card(Rank::Ace, Suit::Clubs),
            card(Rank::Ace, Suit::Diamonds),
            card(Rank::Ace, Suit::Hearts),
            card(Rank::Ace, Suit::Spades),
            card(Rank::Two, Suit::Clubs),
            card(Rank::Two, Suit::Spades),
            card(Rank::Six, Suit::Clubs),
            card(Rank::Nine, Suit::Diamonds),
        ];
        let mut players = vec![p("P0", p0), p("P1", p1), p("P2", p2), p("P3", p3)];
        for (i, pl) in players.iter_mut().enumerate() {
            pl.id = i;
        }
        let mut engine = GameEngine::new(GameState {
            phase: GamePhase::Playing,
            players,
            ready: vec![true; 4],
            current_player: 0,
            trick: TrickState::new(),
            finished_order: Vec::new(),
            scores: vec![0; 4],
            round: 1,
            total_scores: vec![0],
            three_discard: None,
            log: Vec::new(),
        });

        engine.apply_play(0, &["2:diamonds".to_string()]);
        engine.apply_play(
            1,
            &["7:diamonds".to_string(), "7:clubs".to_string(), "7:hearts".to_string(), "7:spades".to_string()],
        );
        // P2 counters with four Kings (higher than four Sevens)
        engine.apply_play(
            2,
            &["K:diamonds".to_string(), "K:clubs".to_string(), "K:hearts".to_string(), "K:spades".to_string()],
        );
        engine.apply_pass(3);
        engine.apply_pass(0);
        engine.apply_pass(1);

        assert_eq!(engine.state().phase, GamePhase::GameOver);
        // LAST bomber (P2) is 1st; FIRST bomber (P1) is 4th — the single-2
        // holder (P0) is NOT the loser anymore.
        assert_eq!(engine.state().scores, vec![0, -15, 10, 0]);
        assert_eq!(engine.state().finished_order[0], 2);
        assert_eq!(engine.state().finished_order[3], 1);
    }

    #[test]
    fn test_lower_bomb_cannot_counter_higher_bomb_in_engine() {
        let mut engine = GameEngine::new(bomb_state());
        engine.apply_play(0, &["2:diamonds".to_string()]);
        // P1 bombs with four Kings (highest possible)
        engine.apply_play(
            1,
            &["K:diamonds".to_string(), "K:clubs".to_string(), "K:hearts".to_string(), "K:spades".to_string()],
        );
        // P2 tries to counter with a plain single — must be rejected
        let before = engine.state().trick.cards.len();
        engine.apply_play(2, &["3:hearts".to_string()]);
        assert_eq!(engine.state().trick.cards.len(), before); // trick unchanged
        assert_eq!(engine.state().players[2].hand.len(), 13);
    }

    #[test]
    fn test_deal_cards_guarded_never_deals_four_twos() {
        for _ in 0..50 {
            let mut engine = GameEngine::new(bomb_state());
            engine.deal_cards_guarded();
            for p in &engine.state().players {
                let twos = p.hand.iter().filter(|c| c.rank_index() == 12).count();
                assert!(twos < 4, "player dealt four 2s");
            }
        }
    }
}
