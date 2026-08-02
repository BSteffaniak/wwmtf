//! Renderer-neutral reusable gameplay view components.

use hyperchad::{
    router::Container,
    template::{LayoutOverflow, container},
};
use serde::{Deserialize, Serialize};
use words_with_spouses_game_domain::{
    Coordinate, GameEvent, GameState, GameStatus, PlayerId, RuleProfile, analyze_committed_play,
    apply_event,
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

/// Public/private game view projection supplied to rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameView {
    pub board: Vec<(Coordinate, char, u8)>,
    pub premiums: Vec<(Coordinate, PremiumView)>,
    pub board_size: u8,
    pub start: Coordinate,
    pub rack: Vec<(u16, char, u8)>,
    pub scores: Vec<(PlayerId, u32)>,
    pub active_player: PlayerId,
    pub status: GameStatus,
    pub winner: Option<PlayerId>,
    pub revision: u64,
}

/// Projects canonical state for one seated viewer without exposing bag or another rack.
#[must_use]
pub fn game_view(state: &GameState, viewer: PlayerId) -> Option<GameView> {
    if !state.players.contains(&viewer) {
        return None;
    }
    let profile = words_with_spouses_game_domain::rule_profile(state.metadata.rules())?;
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
                    words_with_spouses_game_domain::PremiumSquare::Letter(2) => {
                        PremiumView::DoubleLetter
                    }
                    words_with_spouses_game_domain::PremiumSquare::Letter(_) => {
                        PremiumView::TripleLetter
                    }
                    words_with_spouses_game_domain::PremiumSquare::Word(2) => {
                        PremiumView::DoubleWord
                    }
                    words_with_spouses_game_domain::PremiumSquare::Word(_) => {
                        PremiumView::TripleWord
                    }
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
                    words_with_spouses_game_domain::TileFace::Letter(letter) => letter,
                    words_with_spouses_game_domain::TileFace::Blank => ' ',
                };
                (tile.id.get(), letter, tile.points)
            })
            .collect(),
        scores: state
            .scores
            .iter()
            .map(|(&player, &score)| (player, score))
            .collect(),
        active_player: state.active_player,
        status: state.status,
        winner: state.winner,
        revision: state.revision,
    })
}

/// Failure while deriving public presentation from canonical history.
#[derive(Debug, thiserror::Error)]
pub enum MoveHistoryError {
    #[error(transparent)]
    Replay(#[from] words_with_spouses_game_domain::ReplayError),
    #[error(transparent)]
    Analysis(#[from] words_with_spouses_game_domain::GameError),
}

/// Renderer-neutral move-history row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveHistoryView {
    pub revision: u64,
    pub kind: String,
    pub description: String,
    pub score_summary: String,
}

/// Derives chronological renderer-neutral public history directly from canonical events.
///
/// `player_name` must return only an authorized public display label.
///
/// # Errors
///
/// Returns replay or canonical-play analysis errors when the event sequence is invalid.
pub fn move_history_view(
    events: &[GameEvent],
    profile: &RuleProfile,
    player_name: impl Fn(PlayerId) -> String,
) -> Result<Vec<MoveHistoryView>, MoveHistoryError> {
    let mut state = None;
    let mut history = Vec::with_capacity(events.len());
    for event in events {
        let (kind, description) = match (state.as_ref(), event) {
            (None, GameEvent::GameStarted { .. }) => ("GAME_STARTED", "Game started.".to_string()),
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
                (
                    "TILES_PLAYED",
                    format!(
                        "{} played {words} for {score} points.",
                        player_name(*player_id)
                    ),
                )
            }
            (
                Some(_),
                GameEvent::TilesExchanged {
                    player_id,
                    returned,
                    ..
                },
            ) => (
                "TILES_EXCHANGED",
                format!(
                    "{} exchanged {} tile{}.",
                    player_name(*player_id),
                    returned.len(),
                    if returned.len() == 1 { "" } else { "s" }
                ),
            ),
            (Some(_), GameEvent::TurnPassed { player_id }) => (
                "TURN_PASSED",
                format!("{} passed.", player_name(*player_id)),
            ),
            (Some(_), GameEvent::GameResigned { player_id, winner }) => (
                "GAME_RESIGNED",
                format!(
                    "{} resigned; {} won.",
                    player_name(*player_id),
                    player_name(*winner)
                ),
            ),
            (Some(_), GameEvent::GameCompleted { winner, .. }) => (
                "GAME_COMPLETED",
                winner.as_ref().map_or_else(
                    || "Game completed in a tie.".to_string(),
                    |winner| format!("Game completed; {} won.", player_name(*winner)),
                ),
            ),
            _ => return Err(words_with_spouses_game_domain::ReplayError::MissingStart.into()),
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
) -> Result<std::collections::BTreeMap<PlayerId, i64>, words_with_spouses_game_domain::ReplayError>
{
    let mut state: Option<GameState> = None;
    for event in events {
        if let GameEvent::GameCompleted { scores, .. } = event {
            let before = state
                .as_ref()
                .ok_or(words_with_spouses_game_domain::ReplayError::EmptyJournal)?;
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

/// Renders chronological move/score history.
#[must_use]
pub fn move_history_component(history: &[MoveHistoryView]) -> Container {
    container! {
        section id="move-history" gap=8 {
            @if history.is_empty() {
                div background=#f3f0e8 border-radius="12px" padding="12px" {
                    span color=#777b73 { "No moves yet. The first word will start the story." }
                }
            }
            @for entry in history {
                div background=#f7f5ef border-left=(("#8eb59a", 3)) border-radius="10px"
                    padding-y="10px" padding-x="12px" gap="3px" {
                    span font-weight=bold { (entry.description.as_str()) }
                    span color=#5d6258 font-size="12px" { (entry.score_summary.as_str()) }
                }
            }
        }
    }
    .into()
}

/// Renders one premium-square label.
#[must_use]
pub fn premium_square_component(premium: PremiumView) -> Container {
    let label = match premium {
        PremiumView::DoubleLetter => "Double letter",
        PremiumView::TripleLetter => "Triple letter",
        PremiumView::DoubleWord => "Double word",
        PremiumView::TripleWord => "Triple word",
    };
    container! { span class="premium-square" { (label) } }.into()
}

/// Renders one tile.
#[must_use]
pub fn tile_component(id: u16, letter: char, points: u8) -> Container {
    let id_label = format!("Tile {id}");
    container! {
        button class="game-tile" fx-click="select-tile" {
            span { (id_label) }
            span { (letter) }
            span { (points) }
        }
    }
    .into()
}

/// Renders local pending-move presentation state.
#[must_use]
pub fn pending_move_component(pending: &PendingMoveView) -> Container {
    let placements = pending
        .placements
        .iter()
        .map(|(tile, coordinate, blank)| {
            format!(
                "{tile}@{},{}:{}",
                coordinate.x,
                coordinate.y,
                blank.unwrap_or(' ')
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    container! {
        section id="pending-move" gap=8 {
            h2 { "Pending move" }
            span { (placements) }
        }
    }
    .into()
}

/// Renders a reusable board component.
#[must_use]
pub fn board_component(view: &GameView) -> Container {
    let occupied = view
        .board
        .iter()
        .map(|(coordinate, letter, points)| (*coordinate, (*letter, *points)))
        .collect::<std::collections::BTreeMap<_, _>>();
    let premiums = view
        .premiums
        .iter()
        .copied()
        .collect::<std::collections::BTreeMap<_, _>>();
    container! {
        section id="game-board" data-revision=(view.revision) gap="10px" {
            h2 { "Board" }
            div overflow-x="auto" {
                div width="690px" background=#7c6547 border=(("#7c6547", 5)) gap="2px" {
                    @for y in 0..view.board_size {
                        div direction="row" gap="2px" {
                            @for x in 0..view.board_size {
                                @let coordinate = Coordinate::new(x, y);
                                @let tile = occupied.get(&coordinate).copied();
                                @let premium = premiums.get(&coordinate).copied();
                                @let (background, label, color) = if let Some((letter, _)) = tile {
                                    ("#f2d79b", letter.to_string(), "#2e291f")
                                } else if coordinate == view.start {
                                    ("#e79b9b", "★".to_string(), "#6b3535")
                                } else {
                                    match premium {
                                        Some(PremiumView::DoubleLetter) => ("#b9dbe8", "DL".to_string(), "#31596a"),
                                        Some(PremiumView::TripleLetter) => ("#77b6d1", "TL".to_string(), "#173f52"),
                                        Some(PremiumView::DoubleWord) => ("#e9b2b2", "DW".to_string(), "#743d3d"),
                                        Some(PremiumView::TripleWord) => ("#d87f7f", "TW".to_string(), "#ffffff"),
                                        None => ("#ede6d4", String::new(), "#756f64"),
                                    }
                                };
                                div class="board-square" data-x=(x) data-y=(y) width="44px" height="44px"
                                    background=(background) color=(color) align-items="center" justify-content="center"
                                    border=(("#aa9e85", 1)) font-weight=bold position="relative" {
                                    span font-size=(if tile.is_some() { "20px" } else { "16px" }) { (label) }
                                    @if let Some((_, points)) = tile {
                                        span class="board-tile-points" position="absolute" right="4px" bottom="2px" font-size="10px" { (points) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    .into()
}

/// Renders the private rack component.
#[must_use]
pub fn rack_component(view: &GameView) -> Container {
    container! {
        section id="player-rack" gap="10px" {
            h2 { "Your rack" }
            div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap="6px" background=#7c6547 border-radius="8px" padding="8px" {
                @for (id, letter, points) in &view.rack {
                    @let face = if *letter == ' ' { "?".to_string() } else { letter.to_string() };
                    div class="rack-tile" data-tile-id=(id) width="50px" height="56px"
                        background=#f2d79b color=#2e291f border=(("#d1b36f", 2)) border-radius="6px"
                        align-items="center" justify-content="center" position="relative" font-weight=bold {
                        span font-size="24px" { (face) }
                        span position="absolute" right="5px" bottom="3px" font-size="12px" { (points) }
                    }
                }
            }
        }
    }
    .into()
}

/// Renders public score status with the viewer first.
#[must_use]
pub fn status_component(view: &GameView, viewer: PlayerId) -> Container {
    let viewer_score = view
        .scores
        .iter()
        .find(|(player, _)| *player == viewer)
        .map_or(0, |(_, score)| *score);
    let opponent_score = view
        .scores
        .iter()
        .find(|(player, _)| *player != viewer)
        .map_or(0, |(_, score)| *score);
    container! {
        section id="game-status" direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap="12px" {
            div flex=1 background=#ffffff border=(("#ded8c9", 1)) border-radius="12px" padding-y=14 padding-x=18 gap="4px" {
                span color=#777b73 { "You" }
                span font-size="26px" font-weight=bold { (viewer_score) }
            }
            div flex=1 background=#ffffff border=(("#ded8c9", 1)) border-radius="12px" padding-y=14 padding-x=18 gap="4px" {
                span color=#777b73 { "Opponent" }
                span font-size="26px" font-weight=bold { (opponent_score) }
            }
        }
    }
    .into()
}

/// Renders whether the authorized viewer owns the current turn.
#[must_use]
pub fn viewer_turn_component(view: &GameView, viewer: PlayerId) -> Container {
    let status = if view.status == GameStatus::Completed {
        "Game complete"
    } else if view.active_player == viewer {
        "Your turn"
    } else {
        "Waiting for opponent"
    };
    container! {
        span id="viewer-turn-status" background=(if status == "Your turn" { "#2f8a57" } else { "#e5e1d7" })
            color=(if status == "Your turn" { "#ffffff" } else { "#526057" })
            border-radius="999px" padding-y=8 padding-x=13 font-weight=bold { (status) }
    }
    .into()
}

/// Renders recoverable validation/error feedback.
#[must_use]
pub fn error_component(message: &str) -> Container {
    container! {
        section id="game-error" {
            span { (message) }
        }
    }
    .into()
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;
    use words_with_spouses_game_domain::{
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
    fn history_describes_public_actions_without_private_tiles() {
        let players = [PlayerId::new(), PlayerId::new()];
        let metadata = GameMetadata::new(
            GameId::new(),
            RuleProfileRef::new("classic-en", 1).expect("rules reference"),
            DictionaryRef::new("enable1-en", 1, "sha256:test").expect("dictionary reference"),
            OffsetDateTime::UNIX_EPOCH,
        );
        let profile = initial_rule_profile();
        let started =
            initialize_game(metadata, players, players[0], &profile, 4).expect("game starts");
        let passed = GameEvent::TurnPassed {
            player_id: players[0],
        };
        let resigned = GameEvent::GameResigned {
            player_id: players[1],
            winner: players[0],
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
        assert_eq!(history[2].description, "Bob resigned; Alice won.");
    }

    #[test]
    fn viewer_projection_contains_only_their_private_rack() {
        let players = [PlayerId::new(), PlayerId::new()];
        let metadata = GameMetadata::new(
            GameId::new(),
            RuleProfileRef::new("classic-en", 1).expect("rules reference"),
            DictionaryRef::new("enable1-en", 1, "sha256:test").expect("dictionary reference"),
            OffsetDateTime::UNIX_EPOCH,
        );
        let started = initialize_game(metadata, players, players[0], &initial_rule_profile(), 4)
            .expect("game starts");
        let state = replay([&started]).expect("game replays");
        let first = game_view(&state, players[0]).expect("member projects");
        let second = game_view(&state, players[1]).expect("member projects");

        assert_eq!(first.rack.len(), 7);
        assert_eq!(second.rack.len(), 7);
        assert_ne!(first.rack, second.rack);
        assert!(game_view(&state, PlayerId::new()).is_none());
        let rendered = rack_component(&first)
            .display_to_string(false, false)
            .expect("rack renders");
        assert!(rendered.contains("player-rack"));
    }
}
