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
        if self.state.finished_order.len() >= 3 && self.state.phase != GamePhase::GameOver {
            for i in 0..4 {
                if !self.state.players[i].finished {
                    self.state.scores[i] = -15;
                    self.state.finished_order.push(i);
                    break;
                }
            }
            self.state.phase = GamePhase::GameOver;
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
