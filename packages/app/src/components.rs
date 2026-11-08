//! Renderer-neutral reusable gameplay view components.

use hyperchad::{
    actions::ActionType,
    router::Container,
    template::{LayoutOverflow, container},
};
use serde::{Deserialize, Serialize};

use crate::GameVisibilitySettings;
use wwmtf_game_domain::{
    CompletionReason, Coordinate, GameEvent, GameState, GameStatus, PlayerId, RuleProfile,
    analyze_committed_play, apply_event,
};

/// Premium kind rendered on one board square.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PremiumView {
    DoubleLetter,
    TripleLetter,
    DoubleWord,
    TripleWord,
}

/// Local, non-authoritative pending placement presentation state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PendingMoveView {
    pub placements: Vec<(u16, Coordinate, Option<char>)>,
    pub selected_tile: Option<u16>,
}

impl PendingMoveView {
    /// Selects one rack tile as the next local placement operand.
    pub const fn select_tile(&mut self, tile_id: u16) {
        self.selected_tile = Some(tile_id);
    }

    /// Places the selected tile on an unoccupied local square.
    ///
    /// # Errors
    ///
    /// Returns an error when no tile is selected or the local coordinate is already occupied.
    pub fn place_selected(
        &mut self,
        coordinate: Coordinate,
        blank_letter: Option<char>,
    ) -> Result<(), PendingMoveError> {
        let tile_id = self.selected_tile.ok_or(PendingMoveError::NoTileSelected)?;
        if self
            .placements
            .iter()
            .any(|(_, placed, _)| *placed == coordinate)
        {
            return Err(PendingMoveError::OccupiedCoordinate);
        }
        if let Some(placement) = self
            .placements
            .iter_mut()
            .find(|(placed_tile, _, _)| *placed_tile == tile_id)
        {
            placement.1 = coordinate;
            placement.2 = blank_letter;
        } else {
            self.placements.push((tile_id, coordinate, blank_letter));
        }
        self.selected_tile = None;
        Ok(())
    }

    /// Removes one local placement and selects its tile again.
    pub fn unplace(&mut self, tile_id: u16) -> bool {
        let original_len = self.placements.len();
        self.placements.retain(|(placed, _, _)| *placed != tile_id);
        let removed = self.placements.len() != original_len;
        if removed {
            self.selected_tile = Some(tile_id);
        }
        removed
    }

    /// Assigns an uppercase letter to one locally placed blank.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-letter assignment or a tile that is not locally placed.
    pub fn select_blank_letter(
        &mut self,
        tile_id: u16,
        letter: char,
    ) -> Result<(), PendingMoveError> {
        if !letter.is_ascii_alphabetic() {
            return Err(PendingMoveError::InvalidBlankLetter);
        }
        let placement = self
            .placements
            .iter_mut()
            .find(|(placed, _, _)| *placed == tile_id)
            .ok_or(PendingMoveError::TileNotPlaced)?;
        placement.2 = Some(letter.to_ascii_uppercase());
        Ok(())
    }

    /// Returns a deterministic local rack order with the requested tile moved to the front.
    #[must_use]
    pub fn reorder_rack(rack: &[(u16, char, u8)], tile_id: u16) -> Vec<(u16, char, u8)> {
        let mut reordered = rack.to_vec();
        if let Some(index) = reordered.iter().position(|(id, _, _)| *id == tile_id) {
            reordered.rotate_left(index);
        }
        reordered
    }
}

/// Invalid local pending-move interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PendingMoveError {
    #[error("select a rack tile first")]
    NoTileSelected,
    #[error("the local board square is already occupied")]
    OccupiedCoordinate,
    #[error("the tile is not locally placed")]
    TileNotPlaced,
    #[error("blank letters must be ASCII letters")]
    InvalidBlankLetter,
}

/// Public state for one participant, in durable seat order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerView {
    pub player_id: PlayerId,
    pub score: u32,
    pub active: bool,
    pub current_turn: bool,
    pub leader: bool,
}

/// One unordered remaining tile face and its quantity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemainingTileFaceView {
    pub letter: Option<char>,
    pub count: usize,
}

/// Public/private game view projection supplied to rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameView {
    pub board: Vec<(Coordinate, char, u8)>,
    pub premiums: Vec<(Coordinate, PremiumView)>,
    pub board_size: u8,
    pub start: Coordinate,
    pub rack: Vec<(u16, char, u8)>,
    // Transport uses a positional binary codec: absent values must still encode their None tag.
    #[serde(default)]
    pub remaining_tile_count: Option<usize>,
    #[serde(default)]
    pub remaining_tile_faces: Option<Vec<RemainingTileFaceView>>,
    pub players: Vec<PlayerView>,
    pub active_player: PlayerId,
    pub status: GameStatus,
    pub winner: Option<PlayerId>,
    pub leaders: Vec<PlayerId>,
    pub completion_reason: Option<CompletionReason>,
    pub revision: u64,
}

/// Projects canonical state for one seated viewer without exposing another rack or draw order.
#[must_use]
pub fn game_view(
    state: &GameState,
    viewer: PlayerId,
    visibility: GameVisibilitySettings,
) -> Option<GameView> {
    if !state.players.contains(&viewer) {
        return None;
    }
    let profile = &state.rules;
    Some(GameView {
        board: state
            .board
            .iter()
            .map(|(&coordinate, tile)| (coordinate, tile.letter, tile.tile.points))
            .collect(),
        premiums: profile
            .premiums
            .iter()
            .map(|(&coordinate, premium)| {
                let premium = match premium {
                    wwmtf_game_domain::PremiumSquare::Letter(2) => PremiumView::DoubleLetter,
                    wwmtf_game_domain::PremiumSquare::Letter(_) => PremiumView::TripleLetter,
                    wwmtf_game_domain::PremiumSquare::Word(2) => PremiumView::DoubleWord,
                    wwmtf_game_domain::PremiumSquare::Word(_) => PremiumView::TripleWord,
                };
                (coordinate, premium)
            })
            .collect(),
        board_size: profile.board_size,
        start: profile.start,
        rack: state.racks[&viewer]
            .iter()
            .map(|tile| {
                let letter = match tile.face {
                    wwmtf_game_domain::TileFace::Letter(letter) => letter,
                    wwmtf_game_domain::TileFace::Blank => ' ',
                };
                (tile.id.get(), letter, tile.points)
            })
            .collect(),
        remaining_tile_count: visibility
            .show_remaining_tile_count
            .then_some(state.bag.len()),
        remaining_tile_faces: visibility.show_remaining_tile_faces.then(|| {
            let mut counts = std::collections::BTreeMap::new();
            for tile in &state.bag {
                *counts.entry(tile.face).or_insert(0) += 1;
            }
            counts
                .into_iter()
                .map(|(face, count)| RemainingTileFaceView {
                    letter: match face {
                        wwmtf_game_domain::TileFace::Letter(letter) => Some(letter),
                        wwmtf_game_domain::TileFace::Blank => None,
                    },
                    count,
                })
                .collect()
        }),
        players: state
            .players
            .iter()
            .map(|&player_id| PlayerView {
                player_id,
                score: state.scores[&player_id],
                active: state.active_players.contains(&player_id),
                current_turn: state.status == GameStatus::Active
                    && state.active_player == player_id,
                leader: state.leaders.contains(&player_id),
            })
            .collect(),
        active_player: state.active_player,
        status: state.status,
        winner: state.winner,
        leaders: state.leaders.iter().copied().collect(),
        completion_reason: state.completion_reason,
        revision: state.revision,
    })
}

/// Failure while deriving public presentation from canonical history.
#[derive(Debug, thiserror::Error)]
pub enum MoveHistoryError {
    #[error(transparent)]
    Replay(#[from] wwmtf_game_domain::ReplayError),
    #[error(transparent)]
    Analysis(#[from] wwmtf_game_domain::GameError),
}

/// One structured word from an accepted canonical play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayedWordView {
    pub text: String,
    pub score: u32,
}

/// Renderer-neutral move-history row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveHistoryView {
    pub revision: u64,
    pub kind: String,
    pub description: String,
    pub score_summary: String,
    pub played_words: Vec<PlayedWordView>,
    /// Participant who took the action; `None` for game start/completion.
    pub actor: Option<PlayerId>,
    /// Points scored by an accepted play.
    pub points: Option<u32>,
    /// Short action phrase shown after the actor's name ("played", "passed", "swapped 3 tiles").
    pub headline: String,
}

/// Derives chronological renderer-neutral public history directly from canonical events.
///
/// `player_name` must return only an authorized public display label.
///
/// # Errors
///
/// Returns replay or canonical-play analysis errors when the event sequence is invalid.
#[allow(clippy::too_many_lines)]
pub fn move_history_view(
    events: &[GameEvent],
    profile: &RuleProfile,
    player_name: impl Fn(PlayerId) -> String,
) -> Result<Vec<MoveHistoryView>, MoveHistoryError> {
    let mut state = None;
    let mut history = Vec::with_capacity(events.len());
    for event in events {
        let mut played_words = Vec::new();
        let (kind, description, actor, points, headline) = match (state.as_ref(), event) {
            (None, GameEvent::GameStarted { .. }) => (
                "GAME_STARTED",
                "Game started.".to_string(),
                None,
                None,
                "Game started".to_string(),
            ),
            (
                Some(previous),
                GameEvent::TilesPlayed {
                    player_id,
                    placements,
                    score,
                    ..
                },
            ) => {
                let analysis = analyze_committed_play(previous, placements, profile)?;
                debug_assert_eq!(analysis.score, *score);
                let words = analysis
                    .words
                    .iter()
                    .map(|word| word.text.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                played_words = analysis
                    .words
                    .into_iter()
                    .map(|word| PlayedWordView {
                        text: word.text,
                        score: word.score,
                    })
                    .collect();
                (
                    "TILES_PLAYED",
                    format!(
                        "{} played {words} for {score} points.",
                        player_name(*player_id)
                    ),
                    Some(*player_id),
                    Some(*score),
                    "played".to_string(),
                )
            }
            (
                Some(_),
                GameEvent::TilesExchanged {
                    player_id,
                    returned,
                    ..
                },
            ) => {
                let plural = if returned.len() == 1 { "" } else { "s" };
                (
                    "TILES_EXCHANGED",
                    format!(
                        "{} exchanged {} tile{plural}.",
                        player_name(*player_id),
                        returned.len(),
                    ),
                    Some(*player_id),
                    None,
                    format!("swapped {} tile{plural}", returned.len()),
                )
            }
            (Some(_), GameEvent::TurnPassed { player_id }) => (
                "TURN_PASSED",
                format!("{} passed.", player_name(*player_id)),
                Some(*player_id),
                None,
                "passed".to_string(),
            ),
            (Some(_), GameEvent::GameResigned { player_id, .. }) => (
                "GAME_RESIGNED",
                format!("{} resigned.", player_name(*player_id)),
                Some(*player_id),
                None,
                "resigned".to_string(),
            ),
            (Some(_), GameEvent::GameCompleted { winner, .. }) => (
                "GAME_COMPLETED",
                winner.as_ref().map_or_else(
                    || "Game completed in a tie.".to_string(),
                    |winner| format!("Game completed; {} won.", player_name(*winner)),
                ),
                None,
                None,
                winner.as_ref().map_or_else(
                    || "Game over · tie".to_string(),
                    |winner| format!("Game over · {} won", player_name(*winner)),
                ),
            ),
            _ => return Err(wwmtf_game_domain::ReplayError::MissingStart.into()),
        };
        let next_state = apply_event(state, event)?;
        let revision = next_state.revision;
        let score_summary = next_state
            .players
            .iter()
            .map(|player| format!("{} {}", player_name(*player), next_state.scores[player]))
            .collect::<Vec<_>>()
            .join(" – ");
        state = Some(next_state);
        history.push(MoveHistoryView {
            revision,
            kind: kind.to_string(),
            description,
            score_summary,
            played_words,
            actor,
            points,
            headline,
        });
    }
    Ok(history)
}

/// Returns per-player final score adjustments from the last pre-completion state.
///
/// Positive values are bonuses and negative values are deductions. An empty map means the game
/// has no final-score event (for example, resignation).
///
/// # Errors
///
/// * Returns replay errors when the canonical event sequence is invalid.
pub fn final_score_adjustments(
    events: &[GameEvent],
) -> Result<std::collections::BTreeMap<PlayerId, i64>, wwmtf_game_domain::ReplayError> {
    let mut state: Option<GameState> = None;
    for event in events {
        if let GameEvent::GameCompleted { scores, .. } = event {
            let before = state
                .as_ref()
                .ok_or(wwmtf_game_domain::ReplayError::EmptyJournal)?;
            return Ok(scores
                .iter()
                .map(|(&player, &score)| {
                    let prior = before.scores[&player];
                    (player, i64::from(score) - i64::from(prior))
                })
                .collect());
        }
        state = Some(apply_event(state, event)?);
    }
    Ok(std::collections::BTreeMap::new())
}

/// Public identity used to label move-history rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPlayer {
    pub player_id: PlayerId,
    pub display_name: String,
    /// Row label: the display name, or "You" for the viewer.
    pub label: String,
    pub avatar_url: Option<String>,
    pub seat: crate::theme::SeatColor,
}

/// Renders move history newest first as compact, scannable rows: who acted, what they did, the
/// points scored, and tappable word chips that open definitions.
#[must_use]
pub fn move_history_component(
    game_id: wwmtf_game_domain::GameId,
    history: &[MoveHistoryView],
    players: &[HistoryPlayer],
) -> Container {
    use crate::theme;

    let turns = history.iter().filter(|entry| entry.actor.is_some()).count();
    container! {
        section id="move-history" gap=0 {
            @if turns == 0 {
                div align-items="center" gap=(theme::SPACE_2) padding-y=(theme::SPACE_6) text-align="center" {
                    span color=(theme::ACCENT) font-size=(theme::TEXT_XXL) font-weight=bold { "✦" }
                    span font-weight=bold color=(theme::INK) { "No moves yet" }
                    span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) { "The first word will start the story." }
                }
            }
            @for (index, entry) in history.iter().enumerate().rev() {
                @let player = entry.actor.and_then(|actor| players.iter().find(|player| player.player_id == actor));
                @let turn_number = history[..=index].iter().filter(|previous| previous.actor.is_some()).count();
                @if let Some(player) = player {
                    div class="move-entry" data-kind=(entry.kind.as_str()) direction="row" align-items="start"
                        gap=(theme::SPACE_3) padding-y=(theme::SPACE_3) padding-x=(theme::SPACE_1)
                        border-bottom=((theme::BORDER, 1)) {
                        (crate::ui::avatar(&player.display_name, player.avatar_url.as_deref(), player.seat, 32))
                        div flex=1 min-width=0 gap=6 {
                            div direction="row" align-items="center" justify-content="space-between" gap=(theme::SPACE_2) {
                                span min-width=0 overflow-wrap="anywhere" color=(theme::INK) font-size=(theme::TEXT_SM) {
                                    span font-weight=bold { (player.label.as_str()) }
                                    span color=(theme::INK_MUTED) { " " (entry.headline.as_str()) }
                                }
                                @if let Some(points) = entry.points {
                                    span min-width=40 padding-y=2 padding-x=8 border-radius=(theme::RADIUS_PILL)
                                        background=(theme::PRIMARY_SOFT) color=(theme::PRIMARY_INK)
                                        align-items="center" justify-content="center"
                                        font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_SM) font-weight=bold {
                                        "+" (points)
                                    }
                                } @else {
                                    span color=(theme::INK_SOFT) font-size=(theme::TEXT_XS) font-weight=bold white-space="preserve" {
                                        "Turn " (turn_number)
                                    }
                                }
                            }
                            @if !entry.played_words.is_empty() {
                                div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=6 {
                                    @for word in &entry.played_words {
                                        @let panel_href = format!("/games/{game_id}/word-panels/{}", word.text.to_ascii_lowercase());
                                        button type=button class="played-word-definition"
                                            hx-get=(panel_href) hx-target="#game-definition-layer" hx-swap="this"
                                            fx-click=(ActionType::display_by_id("game-definition-layer"))
                                            direction="row" align-items="center" gap=6
                                            color=(theme::TILE_INK) background=(theme::TILE)
                                            border=((theme::TILE_EDGE, 1)) border-bottom=((theme::TILE_EDGE, 3))
                                            border-radius=(theme::RADIUS_SM) padding-y=3 padding-x=8
                                            font-family=(theme::FONT_DISPLAY) font-weight=bold cursor=pointer
                                            fx-hover=(ActionType::set_background_self(theme::TILE_HI)) {
                                            span { (word.text.as_str()) }
                                            span font-size=(theme::TEXT_XS) opacity=0.7 { (word.score) }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } @else {
                    div class="move-entry move-entry-milestone" data-kind=(entry.kind.as_str()) direction="row"
                        align-items="center" justify-content="center" gap=(theme::SPACE_2)
                        padding-y=(theme::SPACE_3) border-bottom=((theme::BORDER, 1)) {
                        span color=(theme::ACCENT) font-weight=bold { "✦" }
                        span color=(theme::INK_MUTED) font-size=(theme::TEXT_XS) font-weight=bold { (entry.headline.as_str()) }
                        span color=(theme::ACCENT) font-weight=bold { "✦" }
                    }
                }
            }
        }
    }
    .into()
}

/// Renders recoverable validation/error feedback.
#[must_use]
pub fn error_component(message: &str) -> Container {
    crate::ui::with_id(
        crate::ui::notice(message, crate::ui::Tone::Danger),
        "game-error",
    )
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;
    use wwmtf_game_domain::{
        DictionaryRef, GameId, GameMetadata, RuleProfileRef, initial_rule_profile, initialize_game,
        replay,
    };

    use super::*;

    #[test]
    fn pending_move_interactions_are_local_deterministic_and_reversible() {
        let mut pending = PendingMoveView::default();
        pending.select_tile(4);
        pending
            .place_selected(Coordinate::new(7, 7), None)
            .expect("selected tile places");
        pending
            .select_blank_letter(4, 'q')
            .expect("blank assignment normalizes");
        assert_eq!(
            pending.placements,
            vec![(4, Coordinate::new(7, 7), Some('Q'))]
        );

        pending.select_tile(5);
        assert_eq!(
            pending.place_selected(Coordinate::new(7, 7), None),
            Err(PendingMoveError::OccupiedCoordinate)
        );
        assert!(pending.unplace(4));
        assert_eq!(pending.selected_tile, Some(4));
        assert!(pending.placements.is_empty());

        let rack = vec![(3, 'A', 1), (4, 'B', 3), (5, 'C', 3)];
        assert_eq!(
            PendingMoveView::reorder_rack(&rack, 4),
            vec![(4, 'B', 3), (5, 'C', 3), (3, 'A', 1)]
        );
        assert_eq!(PendingMoveView::reorder_rack(&rack, 99), rack);
    }

    #[test]
    fn move_history_word_buttons_target_the_definition_panel() {
        let game_id = GameId::new();
        let alice = PlayerId::new();
        let history = vec![MoveHistoryView {
            revision: 1,
            kind: "TILES_PLAYED".to_string(),
            description: "Alice played WORD.".to_string(),
            score_summary: "Alice 4 – Bob 0".to_string(),
            played_words: vec![PlayedWordView {
                text: "WORD".to_string(),
                score: 4,
            }],
            actor: Some(alice),
            points: Some(4),
            headline: "played".to_string(),
        }];
        let players = vec![HistoryPlayer {
            player_id: alice,
            display_name: "Alice".to_string(),
            label: "Alice".to_string(),
            avatar_url: None,
            seat: crate::theme::seat_color(0),
        }];
        let rendered = move_history_component(game_id, &history, &players)
            .display_to_string(false, false)
            .expect("history renders");

        assert!(!rendered.contains("played-word-definition-fallback"));
        assert!(!rendered.contains("Open page"));
        assert!(rendered.contains(&format!("hx-get=\"/games/{game_id}/word-panels/word\"")));
        assert!(rendered.contains("hx-target=\"#game-definition-layer\""));
        assert!(rendered.contains("hx-swap=\"this\""));
    }

    #[test]
    fn history_describes_public_actions_without_private_tiles() {
        let players = [PlayerId::new(), PlayerId::new()];
        let metadata = GameMetadata::new(
            GameId::new(),
            RuleProfileRef::new("classic-en", 1).expect("rules reference"),
            DictionaryRef::new("enable1-en", 1, "sha256:test").expect("dictionary reference"),
            OffsetDateTime::UNIX_EPOCH,
        );
        let profile = initial_rule_profile();
        let started = initialize_game(metadata, players.to_vec(), players[0], &profile, 4)
            .expect("game starts");
        let passed = GameEvent::TurnPassed {
            player_id: players[0],
        };
        let resigned = GameEvent::GameResigned {
            player_id: players[1],
            winner: Some(players[0]),
        };
        let names = |player| {
            if player == players[0] {
                "Alice".to_string()
            } else {
                "Bob".to_string()
            }
        };
        let history = move_history_view(&[started, passed, resigned], &profile, names)
            .expect("history derives");

        assert_eq!(history[1].description, "Alice passed.");
        assert_eq!(history[1].score_summary, "Alice 0 – Bob 0");
        assert_eq!(history[2].description, "Bob resigned.");
    }

    #[test]
    fn bag_projection_obeys_visibility_without_exposing_order_or_tile_ids() {
        let players = [PlayerId::new(), PlayerId::new()];
        let metadata = GameMetadata::new(
            GameId::new(),
            RuleProfileRef::new("classic-en", 1).expect("rules reference"),
            DictionaryRef::new("enable1-en", 1, "sha256:test").expect("dictionary reference"),
            OffsetDateTime::UNIX_EPOCH,
        );
        let started = initialize_game(
            metadata,
            players.to_vec(),
            players[0],
            &initial_rule_profile(),
            4,
        )
        .expect("game starts");
        let state = replay([&started]).expect("game replays");

        let hidden = game_view(
            &state,
            players[0],
            GameVisibilitySettings {
                show_remaining_tile_count: false,
                show_remaining_tile_faces: false,
            },
        )
        .expect("member projects");
        assert_eq!(hidden.remaining_tile_count, None);
        assert_eq!(hidden.remaining_tile_faces, None);

        let count_only = game_view(
            &state,
            players[0],
            GameVisibilitySettings {
                show_remaining_tile_count: true,
                show_remaining_tile_faces: false,
            },
        )
        .expect("member projects");
        assert_eq!(count_only.remaining_tile_count, Some(state.bag.len()));
        assert_eq!(count_only.remaining_tile_faces, None);

        let faces_only = game_view(
            &state,
            players[0],
            GameVisibilitySettings {
                show_remaining_tile_count: false,
                show_remaining_tile_faces: true,
            },
        )
        .expect("member projects");
        assert_eq!(faces_only.remaining_tile_count, None);
        assert!(faces_only.remaining_tile_faces.is_some());

        let visible = game_view(&state, players[0], GameVisibilitySettings::default())
            .expect("member projects");
        for view in [&hidden, &count_only, &faces_only, &visible] {
            let payload = hyperchad::shared_state_models::PayloadBlob::from_serializable(view)
                .expect("game view encodes with transport codec");
            let decoded: GameView = payload
                .deserialize()
                .expect("game view decodes with transport codec");
            assert_eq!(&decoded, view);
        }
        assert_eq!(visible.remaining_tile_count, Some(state.bag.len()));
        let faces = visible.remaining_tile_faces.expect("faces are visible");
        assert_eq!(
            faces.iter().map(|face| face.count).sum::<usize>(),
            state.bag.len()
        );
        let payload = serde_json::to_string(&faces).expect("faces serialize");
        assert!(!payload.contains("id"));
        assert!(!payload.contains("points"));
    }

    #[test]
    fn viewer_projection_contains_only_their_private_rack_for_large_membership() {
        let players = (0..12).map(|_| PlayerId::new()).collect::<Vec<_>>();
        let metadata = GameMetadata::new(
            GameId::new(),
            RuleProfileRef::new("classic-en", 1).expect("rules reference"),
            DictionaryRef::new("enable1-en", 1, "sha256:test").expect("dictionary reference"),
            OffsetDateTime::UNIX_EPOCH,
        );
        let started = initialize_game(
            metadata,
            players.clone(),
            players[0],
            &initial_rule_profile(),
            4,
        )
        .expect("game starts");
        let state = replay([&started]).expect("game replays");
        let first = game_view(&state, players[0], GameVisibilitySettings::default())
            .expect("member projects");
        let second = game_view(&state, players[1], GameVisibilitySettings::default())
            .expect("member projects");

        assert_eq!(first.rack.len(), 7);
        assert_eq!(second.rack.len(), 7);
        assert_eq!(first.players.len(), players.len());
        assert_eq!(
            first
                .players
                .iter()
                .map(|player| player.player_id)
                .collect::<Vec<_>>(),
            players
        );
        assert!(first.players[0].active);
        assert!(first.players[0].current_turn);
        assert!(!first.players[0].leader);
        assert_ne!(first.rack, second.rack);
        assert!(game_view(&state, PlayerId::new(), GameVisibilitySettings::default()).is_none());
    }
}
