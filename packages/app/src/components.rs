//! Renderer-neutral reusable gameplay view components.

use hyperchad::{router::Container, template::container};
use serde::{Deserialize, Serialize};
use words_with_spouses_game_domain::{Coordinate, GameState, PlayerId};

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

/// Public/private game view projection supplied to rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameView {
    pub board: Vec<(Coordinate, char)>,
    pub rack: Vec<(u16, char, u8)>,
    pub scores: Vec<(PlayerId, u32)>,
    pub active_player: PlayerId,
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

/// Renders local pending-move presentation state and declarative actions.
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
            button fx-click="unplace-selected" { "Unplace selected" }
            button fx-click="reorder-rack" { "Reorder rack" }
            button fx-click="select-blank-letter" { "Choose blank letter" }
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
    let turn = format!("{:?}", view.active_player);
    container! {
        section id="game-status" gap=8 {
            h2 { "Scores" }
            span { (scores) }
            span { "Current turn: " (turn) }
        }
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
