//! Renderer-neutral reusable gameplay view components.

use hyperchad::{router::Container, template::container};
use serde::{Deserialize, Serialize};
use words_with_spouses_game_domain::{
    Coordinate, GameEvent, GameState, GameStatus, PlayerId, apply_event,
};

/// Premium kind rendered on one board square.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub board: Vec<(Coordinate, char)>,
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
    Some(GameView {
        board: state
            .board
            .iter()
            .map(|(&coordinate, tile)| (coordinate, tile.letter))
            .collect(),
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

/// Renderer-neutral move-history row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveHistoryView {
    pub revision: u64,
    pub kind: String,
    pub score_delta: u32,
}

/// Derives chronological renderer-neutral history directly from canonical events.
///
/// # Errors
///
/// * Returns replay errors when the event sequence is not a valid canonical journal.
pub fn move_history_view(
    events: &[GameEvent],
) -> Result<Vec<MoveHistoryView>, words_with_spouses_game_domain::ReplayError> {
    let mut state = None;
    let mut history = Vec::with_capacity(events.len());
    for event in events {
        let next_state = apply_event(state, event)?;
        let revision = next_state.revision;
        state = Some(next_state);
        let (kind, score_delta) = match event {
            GameEvent::GameStarted { .. } => ("GAME_STARTED", 0),
            GameEvent::TilesPlayed { score, .. } => ("TILES_PLAYED", *score),
            GameEvent::TilesExchanged { .. } => ("TILES_EXCHANGED", 0),
            GameEvent::TurnPassed { .. } => ("TURN_PASSED", 0),
            GameEvent::GameResigned { .. } => ("GAME_RESIGNED", 0),
            GameEvent::GameCompleted { .. } => ("GAME_COMPLETED", 0),
        };
        history.push(MoveHistoryView {
            revision,
            kind: kind.to_string(),
            score_delta,
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
    let rows = history
        .iter()
        .map(|entry| format!("{}:{}:+{}", entry.revision, entry.kind, entry.score_delta))
        .collect::<Vec<_>>()
        .join(" | ");
    container! {
        section id="move-history" gap=8 {
            h2 { "Move history" }
            span { (rows) }
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
    let tiles = view
        .board
        .iter()
        .map(|(coordinate, letter)| format!("{}:{}={letter}", coordinate.x, coordinate.y))
        .collect::<Vec<_>>()
        .join(" ");
    container! {
        section id="game-board" data-revision=(view.revision) gap=8 {
            h2 { "Board" }
            span { (tiles) }
        }
    }
    .into()
}

/// Renders the private rack component.
#[must_use]
pub fn rack_component(view: &GameView) -> Container {
    let rack = view
        .rack
        .iter()
        .map(|(id, letter, points)| format!("{id}:{letter}:{points}"))
        .collect::<Vec<_>>()
        .join(" ");
    container! {
        section id="player-rack" gap=8 {
            h2 { "Your rack" }
            span { (rack) }
        }
    }
    .into()
}

/// Renders score and turn status.
#[must_use]
pub fn status_component(view: &GameView) -> Container {
    let scores = view
        .scores
        .iter()
        .map(|(player, score)| format!("{player:?}:{score}"))
        .collect::<Vec<_>>()
        .join(" ");
    let status = match view.status {
        GameStatus::Active => format!("Current turn: {:?}", view.active_player),
        GameStatus::Completed => view.winner.map_or_else(
            || "Completed: tie".to_string(),
            |winner| format!("Completed: winner {winner:?}"),
        ),
    };
    container! {
        section id="game-status" gap=8 {
            h2 { "Scores" }
            span { (scores) }
            span { (status) }
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
        span id="viewer-turn-status" { (status) }
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
