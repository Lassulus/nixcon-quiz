//! The authoritative game. One clock for everyone: the server decides when a
//! question opens and closes, accepts answers while it is open, and only then
//! reveals which choice was right.
//!
//! Time is passed in explicitly (milliseconds since the Unix epoch) so the
//! whole state machine runs in tests without sleeping. Clients get absolute
//! deadlines plus the server's `now` and correct for their own clock skew.

use crate::{names, quiz::Question};
use rand::seq::SliceRandom;
use std::collections::{HashMap, HashSet};

pub type PlayerId = u64;

/// Points for a correct answer given at the last moment.
const BASE_POINTS: u64 = 500;
/// Extra points for a correct answer, scaled by the time left on the clock.
const SPEED_POINTS: u64 = 500;
const LEADERBOARD_SIZE: usize = 10;
/// Upper bound on remembered players; anyone past it is turned away.
const MAX_PLAYERS: usize = 20_000;
/// Offline players are forgotten at the next round reset after this long.
const FORGET_AFTER_MS: u64 = 60 * 60 * 1000;

/// Timings and title; server configuration rather than quiz content.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Shown in the page header and the browser tab.
    pub title: String,
    /// Time to answer, the same for every question.
    pub question_seconds: u64,
    /// How long the correct answer stays on screen before the next question.
    pub reveal_seconds: u64,
    /// Questions per round; after the last one the leaderboard is shown and
    /// points reset.
    pub round_questions: u32,
    /// How long the leaderboard is shown before the next round starts.
    pub leaderboard_seconds: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            title: "NixCon Quiz".into(),
            question_seconds: 20,
            reveal_seconds: 8,
            round_questions: 10,
            leaderboard_seconds: 30,
        }
    }
}

struct Player {
    token: String,
    name: String,
    score: u64,
    connections: u32,
    last_seen: u64,
}

struct Answer {
    choice: usize,
    at: u64,
}

enum Phase {
    Question { started: u64, ends: u64 },
    Reveal { ends: u64, counts: Vec<u32> },
    Leaderboard { ends: u64 },
}

#[derive(Debug, PartialEq, Eq)]
pub enum AnswerError {
    /// Wrong question, or the question already closed.
    Closed,
    InvalidChoice,
}

pub struct Standing {
    pub id: PlayerId,
    pub rank: usize,
    pub name: String,
    pub score: u64,
}

/// One player's picture of the game, ready to be rendered.
pub struct View<'a> {
    /// Server time the view was taken at, for clients to correct their clock.
    pub now: u64,
    pub title: &'a str,
    pub online: usize,
    /// `None` for spectators.
    pub me: Option<Me<'a>>,
    pub phase: PhaseView<'a>,
}

pub struct Me<'a> {
    pub id: PlayerId,
    pub name: &'a str,
    pub score: u64,
    pub rank: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    pub answered: usize,
    pub online: usize,
}

/// Only the reveal carries the correct choice, the explanation and the vote
/// counts, so an open question cannot leak them by construction.
pub enum PhaseView<'a> {
    Question {
        seq: u64,
        number: u32,
        of: u32,
        text: &'a str,
        choices: &'a [String],
        started: u64,
        ends: u64,
        answered: Option<usize>,
    },
    Reveal {
        number: u32,
        of: u32,
        text: &'a str,
        choices: &'a [String],
        ends: u64,
        correct: usize,
        counts: &'a [u32],
        explanation: &'a str,
        answered: Option<usize>,
        gained: Option<u64>,
    },
    Leaderboard {
        ends: u64,
        top: &'a [Standing],
        /// Players who scored this round.
        players: usize,
    },
}

pub struct Game {
    settings: Settings,
    questions: Vec<Question>,
    /// Question indices still to be asked before the questions repeat.
    deck: Vec<usize>,
    phase: Phase,
    /// Identifies the current question across rounds; answers must quote it.
    seq: u64,
    question: usize,
    /// Position of the current question within the round, from 1.
    number: u32,
    answers: HashMap<PlayerId, Answer>,
    /// Points each player earned on the last revealed question.
    gained: HashMap<PlayerId, u64>,
    players: HashMap<PlayerId, Player>,
    tokens: HashMap<String, PlayerId>,
    names: HashSet<String>,
    next_id: PlayerId,
    online: usize,
    ranks: HashMap<PlayerId, usize>,
    top: Vec<Standing>,
}

impl Game {
    /// `questions` must not be empty.
    pub fn new(settings: Settings, questions: Vec<Question>, now: u64) -> Self {
        assert!(!questions.is_empty(), "a quiz needs questions");
        let mut game = Self {
            settings,
            questions,
            deck: Vec::new(),
            phase: Phase::Leaderboard { ends: now },
            seq: 0,
            question: 0,
            number: 0,
            answers: HashMap::new(),
            gained: HashMap::new(),
            players: HashMap::new(),
            tokens: HashMap::new(),
            names: HashSet::new(),
            next_id: 1,
            online: 0,
            ranks: HashMap::new(),
            top: Vec::new(),
        };
        game.start_question(now);
        game
    }

    /// When the current phase ends.
    pub fn deadline(&self) -> u64 {
        match self.phase {
            Phase::Question { ends, .. }
            | Phase::Reveal { ends, .. }
            | Phase::Leaderboard { ends } => ends,
        }
    }

    /// Advance past the current phase if its time is up. Returns whether
    /// anything changed, i.e. whether clients need a fresh view.
    pub fn tick(&mut self, now: u64) -> bool {
        if now < self.deadline() {
            return false;
        }
        match self.phase {
            Phase::Question { .. } => self.reveal(now),
            Phase::Reveal { .. } => {
                if self.number >= self.settings.round_questions {
                    self.phase = Phase::Leaderboard {
                        ends: now + self.settings.leaderboard_seconds * 1000,
                    };
                } else {
                    self.start_question(now);
                }
            }
            Phase::Leaderboard { .. } => self.new_round(now),
        }
        true
    }

    fn draw(&mut self) -> usize {
        if self.deck.is_empty() {
            self.deck = (0..self.questions.len()).collect();
            self.deck.shuffle(&mut rand::rng());
            // The deck is drawn from the back; don't ask the same thing twice
            // in a row across a reshuffle.
            if self.deck.len() > 1 && self.seq > 0 && self.deck.last() == Some(&self.question) {
                let last = self.deck.len() - 1;
                self.deck.swap(0, last);
            }
        }
        self.deck.pop().unwrap()
    }

    fn start_question(&mut self, now: u64) {
        self.question = self.draw();
        self.seq += 1;
        self.number += 1;
        self.answers.clear();
        self.gained.clear();
        self.phase = Phase::Question {
            started: now,
            ends: now + self.settings.question_seconds * 1000,
        };
    }

    fn reveal(&mut self, now: u64) {
        let Phase::Question { started, ends } = self.phase else {
            unreachable!("reveal outside a question")
        };
        let question = &self.questions[self.question];
        let mut counts = vec![0; question.choices.len()];
        let window = (ends - started).max(1);
        for (id, answer) in &self.answers {
            counts[answer.choice] += 1;
            if answer.choice != question.correct {
                continue;
            }
            let left = ends.saturating_sub(answer.at).min(window);
            let points = BASE_POINTS + SPEED_POINTS * left / window;
            if let Some(player) = self.players.get_mut(id) {
                player.score += points;
                self.gained.insert(*id, points);
            }
        }
        self.rerank();
        self.phase = Phase::Reveal {
            ends: now + self.settings.reveal_seconds * 1000,
            counts,
        };
    }

    fn new_round(&mut self, now: u64) {
        self.players.retain(|_, p| {
            let keep = p.connections > 0 || now.saturating_sub(p.last_seen) < FORGET_AFTER_MS;
            if !keep {
                self.tokens.remove(&p.token);
                self.names.remove(&p.name);
            }
            keep
        });
        for player in self.players.values_mut() {
            player.score = 0;
        }
        self.rerank();
        self.number = 0;
        self.start_question(now);
    }

    fn rerank(&mut self) {
        let mut scored: Vec<(PlayerId, &Player)> = self
            .players
            .iter()
            .filter(|(_, p)| p.score > 0)
            .map(|(id, p)| (*id, p))
            .collect();
        scored.sort_by(|a, b| {
            b.1.score
                .cmp(&a.1.score)
                .then_with(|| a.1.name.cmp(&b.1.name))
        });
        self.ranks.clear();
        self.top.clear();
        let mut rank = 0;
        for (i, (id, player)) in scored.iter().enumerate() {
            // Competition ranking: ties share a place, the next one skips.
            if i == 0 || scored[i - 1].1.score != player.score {
                rank = i + 1;
            }
            self.ranks.insert(*id, rank);
            if i < LEADERBOARD_SIZE {
                self.top.push(Standing {
                    id: *id,
                    rank,
                    name: player.name.clone(),
                    score: player.score,
                });
            }
        }
    }

    fn current(&self) -> &Question {
        &self.questions[self.question]
    }

    /// The player behind `token`, or a newly named one. The second value is
    /// the token the client has to store when it did not already have one.
    /// `None` when the server is full.
    pub fn join(&mut self, token: Option<&str>, now: u64) -> Option<(PlayerId, Option<String>)> {
        if let Some(id) = token.and_then(|t| self.tokens.get(t)) {
            return Some((*id, None));
        }
        if self.players.len() >= MAX_PLAYERS {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        let token = format!("{:032x}", rand::random::<u128>());
        let name = names::generate(&self.names);
        self.names.insert(name.clone());
        self.tokens.insert(token.clone(), id);
        self.players.insert(
            id,
            Player {
                token: token.clone(),
                name,
                score: 0,
                connections: 0,
                last_seen: now,
            },
        );
        Some((id, Some(token)))
    }

    pub fn player_for(&self, token: &str) -> Option<PlayerId> {
        self.tokens.get(token).copied()
    }

    pub fn connect(&mut self, id: PlayerId, now: u64) {
        if let Some(player) = self.players.get_mut(&id) {
            if player.connections == 0 {
                self.online += 1;
            }
            player.connections += 1;
            player.last_seen = now;
        }
    }

    pub fn disconnect(&mut self, id: PlayerId, now: u64) {
        if let Some(player) = self.players.get_mut(&id) {
            player.connections -= 1;
            if player.connections == 0 {
                self.online -= 1;
            }
            player.last_seen = now;
        }
    }

    /// Record or change a player's answer. The last answer before the
    /// deadline counts, and its speed bonus is measured from when it was
    /// given: switching late costs the bonus of the early pick. Sending the
    /// same choice again changes nothing.
    pub fn answer(
        &mut self,
        id: PlayerId,
        seq: u64,
        choice: usize,
        now: u64,
    ) -> Result<(), AnswerError> {
        let Phase::Question { ends, .. } = self.phase else {
            return Err(AnswerError::Closed);
        };
        if seq != self.seq || now >= ends {
            return Err(AnswerError::Closed);
        }
        if choice >= self.current().choices.len() {
            return Err(AnswerError::InvalidChoice);
        }
        let Some(player) = self.players.get_mut(&id) else {
            return Err(AnswerError::Closed);
        };
        player.last_seen = now;
        if self.answers.get(&id).is_none_or(|a| a.choice != choice) {
            self.answers.insert(id, Answer { choice, at: now });
        }
        Ok(())
    }

    /// Everything one player's screen needs.
    pub fn view(&self, id: PlayerId, now: u64) -> View<'_> {
        let player = self.players.get(&id);
        View {
            now,
            title: &self.settings.title,
            online: self.online,
            me: Some(Me {
                id,
                name: player.map_or("", |p| &p.name),
                score: player.map_or(0, |p| p.score),
                rank: self.ranks.get(&id).copied(),
            }),
            phase: self.phase_view(Some(id)),
        }
    }

    /// What the livestream shows: the same phases, seen by nobody in
    /// particular.
    pub fn spectate(&self, now: u64) -> View<'_> {
        View {
            now,
            title: &self.settings.title,
            online: self.online,
            me: None,
            phase: self.phase_view(None),
        }
    }

    /// How many players answered the current question, and how many are
    /// online. Unlike the per-choice counts this is safe to show while the
    /// question is open.
    pub fn tally(&self) -> Tally {
        Tally {
            answered: self.answers.len(),
            online: self.online,
        }
    }

    fn phase_view(&self, id: Option<PlayerId>) -> PhaseView<'_> {
        let question = self.current();
        let answered = id.and_then(|id| self.answers.get(&id)).map(|a| a.choice);
        match &self.phase {
            Phase::Question { started, ends } => PhaseView::Question {
                seq: self.seq,
                number: self.number,
                of: self.settings.round_questions,
                text: &question.text,
                choices: &question.choices,
                started: *started,
                ends: *ends,
                answered,
            },
            Phase::Reveal { ends, counts } => PhaseView::Reveal {
                number: self.number,
                of: self.settings.round_questions,
                text: &question.text,
                choices: &question.choices,
                ends: *ends,
                correct: question.correct,
                counts,
                explanation: &question.explanation,
                answered,
                gained: id.and_then(|id| self.gained.get(&id)).copied(),
            },
            Phase::Leaderboard { ends } => PhaseView::Leaderboard {
                ends: *ends,
                top: &self.top,
                players: self.ranks.len(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game() -> Game {
        let settings = Settings {
            question_seconds: 10,
            reveal_seconds: 5,
            round_questions: 2,
            leaderboard_seconds: 20,
            ..Settings::default()
        };
        let question = crate::quiz::parse(
            r#"
            question = "Which one?"
            choices = ["wrong", "right", "also wrong"]
            answer = "right"
            explanation = "because"
            "#,
        )
        .unwrap();
        Game::new(settings, vec![question], 1_000_000)
    }

    fn player(game: &mut Game) -> PlayerId {
        game.join(None, 1_000_000).unwrap().0
    }

    /// (answered, gained) as the reveal shows them to `id`.
    fn result(g: &Game, id: PlayerId) -> (Option<usize>, Option<u64>) {
        match g.view(id, 0).phase {
            PhaseView::Reveal {
                answered, gained, ..
            } => (answered, gained),
            _ => panic!("not revealing"),
        }
    }

    #[test]
    fn open_question_shows_own_pick_but_no_points_yet() {
        let mut g = game();
        let a = player(&mut g);
        g.answer(a, g.seq, 1, 1_000_100).unwrap();
        let v = g.view(a, 0);
        assert!(matches!(
            v.phase,
            PhaseView::Question {
                answered: Some(1),
                ..
            }
        ));
        assert_eq!(
            v.me.as_ref().unwrap().score,
            0,
            "score must not move before the reveal"
        );
    }

    #[test]
    fn faster_correct_answers_score_more_and_wrong_ones_nothing() {
        let mut g = game();
        let (fast, slow, wrong, silent) = (
            player(&mut g),
            player(&mut g),
            player(&mut g),
            player(&mut g),
        );
        let seq = g.seq;
        g.answer(fast, seq, 1, 1_000_000).unwrap();
        g.answer(slow, seq, 1, 1_009_000).unwrap();
        g.answer(wrong, seq, 0, 1_000_000).unwrap();
        assert!(!g.tick(1_009_999));
        assert!(g.tick(1_010_000));

        let PhaseView::Reveal {
            correct, counts, ..
        } = g.view(fast, 0).phase
        else {
            panic!("not revealing")
        };
        assert_eq!(correct, 1);
        assert_eq!(counts, [1, 2, 0]);
        assert_eq!(result(&g, fast), (Some(1), Some(1000)));
        assert_eq!(result(&g, slow), (Some(1), Some(550)));
        assert_eq!(result(&g, wrong), (Some(0), None));
        assert_eq!(result(&g, silent), (None, None));
        assert_eq!(g.view(fast, 0).me.as_ref().unwrap().rank, Some(1));
        assert_eq!(g.view(slow, 0).me.as_ref().unwrap().rank, Some(2));
        assert_eq!(g.view(wrong, 0).me.as_ref().unwrap().score, 0);
        assert_eq!(g.view(wrong, 0).me.as_ref().unwrap().rank, None);
    }

    #[test]
    fn answers_can_change_until_the_question_closes() {
        let mut g = game();
        let (switcher, repeater) = (player(&mut g), player(&mut g));
        let seq = g.seq;
        assert_eq!(
            g.answer(switcher, seq + 1, 1, 1_000_001),
            Err(AnswerError::Closed)
        );
        assert_eq!(
            g.answer(switcher, seq, 3, 1_000_001),
            Err(AnswerError::InvalidChoice)
        );
        assert_eq!(
            g.answer(switcher, seq, 1, 1_010_000),
            Err(AnswerError::Closed),
            "at the deadline"
        );
        // Wrong first, right halfway through: scored as a halfway answer.
        g.answer(switcher, seq, 0, 1_000_000).unwrap();
        g.answer(switcher, seq, 1, 1_005_000).unwrap();
        // Re-sending the same choice keeps the original, faster time.
        g.answer(repeater, seq, 1, 1_000_000).unwrap();
        g.answer(repeater, seq, 1, 1_009_000).unwrap();

        g.tick(1_010_000);
        assert_eq!(result(&g, switcher), (Some(1), Some(750)));
        assert_eq!(result(&g, repeater), (Some(1), Some(1000)));
        assert_eq!(
            g.answer(switcher, seq, 0, 1_010_001),
            Err(AnswerError::Closed),
            "during the reveal"
        );
    }

    #[test]
    fn round_ends_with_leaderboard_then_scores_reset() {
        let mut g = game();
        let a = player(&mut g);
        let b = player(&mut g);
        let mut now = 1_000_000;
        // Two questions per round: the leaderboard follows the second reveal.
        for _ in 0..2 {
            assert!(matches!(g.view(a, 0).phase, PhaseView::Question { .. }));
            g.answer(a, g.seq, 1, now).unwrap();
            now = g.deadline();
            g.tick(now);
            now = g.deadline();
            g.tick(now);
        }
        let v = g.view(b, 0);
        let PhaseView::Leaderboard { top, players, .. } = v.phase else {
            panic!("no leaderboard")
        };
        assert_eq!(players, 1);
        assert_eq!((top[0].id, top[0].score), (a, 2000));
        assert_eq!(v.me.as_ref().unwrap().rank, None);

        g.tick(g.deadline());
        let v = g.view(a, 0);
        assert!(matches!(v.phase, PhaseView::Question { number: 1, .. }));
        assert_eq!(
            (v.me.as_ref().unwrap().score, v.me.as_ref().unwrap().rank),
            (0, None)
        );
    }

    #[test]
    fn known_token_keeps_player_unknown_token_gets_new_one() {
        let mut g = game();
        let (id, token) = g.join(None, 1_000_000).unwrap();
        let token = token.unwrap();
        assert_eq!(g.join(Some(&token), 1_000_000), Some((id, None)));
        let (other, fresh) = g
            .join(Some("stale-from-before-restart"), 1_000_000)
            .unwrap();
        assert_ne!(other, id);
        assert!(fresh.is_some());
        assert_ne!(
            g.view(id, 0).me.as_ref().unwrap().name,
            g.view(other, 0).me.as_ref().unwrap().name
        );
    }

    #[test]
    fn spectators_see_how_many_answered_but_not_what() {
        let mut g = game();
        let (a, b) = (player(&mut g), player(&mut g));
        g.connect(a, 1_000_000);
        g.connect(b, 1_000_000);
        assert_eq!(
            g.tally(),
            Tally {
                answered: 0,
                online: 2
            }
        );
        g.answer(a, g.seq, 1, 1_000_001).unwrap();
        g.answer(a, g.seq, 0, 1_000_002).unwrap();
        assert_eq!(
            g.tally(),
            Tally {
                answered: 1,
                online: 2
            },
            "a change is not a second answer"
        );
        let v = g.spectate(0);
        assert!(v.me.is_none());
        assert!(matches!(
            v.phase,
            PhaseView::Question { answered: None, .. }
        ));

        g.tick(g.deadline());
        let PhaseView::Reveal {
            counts,
            answered,
            gained,
            ..
        } = g.spectate(0).phase
        else {
            panic!("not revealing")
        };
        assert_eq!((counts, answered, gained), (&[1, 0, 0][..], None, None));
    }
}
