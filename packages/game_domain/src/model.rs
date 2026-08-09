//! Strongly typed gameplay identities, coordinates, tiles, commands, events, and state.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::GameMetadata;

/// Stable identity of one player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlayerId(Uuid);

impl PlayerId {
    /// Creates a new random player identity.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Returns the underlying UUID.
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl std::str::FromStr for PlayerId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

impl Default for PlayerId {
    fn default() -> Self {
        Self::new()
    }
}

/// Stable identity of one physical tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TileId(u16);

impl TileId {
    /// Creates a tile identity from its deterministic profile index.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the profile index.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Zero-based coordinate on a square board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Coordinate {
    /// Zero-based horizontal position.
    pub x: u8,
    /// Zero-based vertical position.
    pub y: u8,
}

impl Coordinate {
    /// Creates a board coordinate.
    #[must_use]
    pub const fn new(x: u8, y: u8) -> Self {
        Self { x, y }
    }
}

/// Printed face of a tile. `Blank` has no intrinsic letter or score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TileFace {
    /// An uppercase Latin letter.
    Letter(char),
    /// A blank tile whose letter is selected when played.
    Blank,
}

/// One uniquely identifiable physical tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tile {
    /// Stable physical identity.
    pub id: TileId,
    /// Printed face.
    pub face: TileFace,
    /// Intrinsic score before board multipliers.
    pub points: u8,
}

/// Tile placed on the board, including a blank's selected letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardTile {
    /// Physical tile.
    pub tile: Tile,
    /// Uppercase letter represented on the board.
    pub letter: char,
}

/// One tile placement requested by a player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    /// Tile taken from the player's rack.
    pub tile_id: TileId,
    /// Destination square.
    pub coordinate: Coordinate,
    /// Required only for blank tiles.
    pub blank_letter: Option<char>,
}

/// Server-authoritative gameplay command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameCommand {
    /// Place one or more rack tiles as a move.
    Play { placements: Vec<Placement> },
    /// Return rack tiles to the bag and draw replacements.
    Exchange { tile_ids: BTreeSet<TileId> },
    /// End the turn without changing tiles.
    Pass,
    /// End the game by resignation.
    Resign,
}

/// Canonical event from which aggregate state is rebuilt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameEvent {
    /// Initial deterministic state, including server-only bag and racks.
    GameStarted {
        metadata: GameMetadata,
        players: [PlayerId; 2],
        first_player: PlayerId,
        racks: BTreeMap<PlayerId, Vec<Tile>>,
        bag: Vec<Tile>,
    },
    /// A move placed tiles and scored words.
    TilesPlayed {
        player_id: PlayerId,
        placements: BTreeMap<Coordinate, BoardTile>,
        score: u32,
        drawn: Vec<Tile>,
    },
    /// Rack tiles were exchanged.
    TilesExchanged {
        player_id: PlayerId,
        returned: Vec<Tile>,
        drawn: Vec<Tile>,
    },
    /// A player passed.
    TurnPassed { player_id: PlayerId },
    /// A player resigned and the opponent won.
    GameResigned {
        player_id: PlayerId,
        winner: PlayerId,
    },
    /// The game ended and final scores were established.
    GameCompleted {
        scores: BTreeMap<PlayerId, u32>,
        winner: Option<PlayerId>,
    },
}

/// Current lifecycle of a game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameStatus {
    /// Commands may still be accepted.
    Active,
    /// The game has ended.
    Completed,
}

/// Canonical aggregate rebuilt from persisted events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameState {
    /// Compatibility-critical game metadata.
    pub metadata: GameMetadata,
    /// Players in deterministic turn order.
    pub players: [PlayerId; 2],
    /// Player whose command is currently accepted.
    pub active_player: PlayerId,
    /// Committed board tiles.
    pub board: BTreeMap<Coordinate, BoardTile>,
    /// Hidden racks, retained only in canonical server state.
    pub racks: BTreeMap<PlayerId, Vec<Tile>>,
    /// Tiles remaining in deterministic draw order.
    pub bag: Vec<Tile>,
    /// Accumulated scores.
    pub scores: BTreeMap<PlayerId, u32>,
    /// Number of consecutive scoreless turns.
    pub scoreless_turns: u8,
    /// Winner after completion, or `None` for an active game or completed tie.
    pub winner: Option<PlayerId>,
    /// Current lifecycle.
    pub status: GameStatus,
    /// Number of canonical events applied.
    pub revision: u64,
}

/// One server-derived word formed by a candidate play.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalyzedWord {
    /// Normalized uppercase word text.
    pub text: String,
    /// Ordered board coordinates occupied by the word.
    pub coordinates: Vec<Coordinate>,
    /// Score contributed by this word before any full-rack bonus.
    pub score: u32,
}

/// Deterministic, non-mutating analysis of a legal candidate play.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayAnalysis {
    /// Main word and any cross-words formed by the candidate play.
    pub words: Vec<AnalyzedWord>,
    /// Total score including the full-rack bonus when earned.
    pub score: u32,
    /// Full-rack bonus included in `score`.
    pub full_rack_bonus: u16,
}

/// Deterministic, non-mutating analysis of a structurally complete candidate play.
///
/// Unlike [`PlayAnalysis`], this result may describe a play rejected by the pinned dictionary.
/// Scores remain authoritative because they are derived by the same rules used for acceptance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidatePlayAnalysis {
    /// Formed words and score calculated from the candidate placement.
    pub play: PlayAnalysis,
    /// Every formed word rejected by the pinned dictionary, in formation order.
    pub invalid_words: Vec<String>,
}

impl CandidatePlayAnalysis {
    /// Returns whether every formed word is accepted by the pinned dictionary.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.invalid_words.is_empty()
    }
}

/// Structural board guidance for extending a candidate placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementGuidance {
    /// Empty squares which must be filled to close gaps or cover the opening square.
    pub required: BTreeSet<Coordinate>,
    /// Empty squares which would make or preserve a structurally legal placement when added next.
    pub eligible: BTreeSet<Coordinate>,
}

/// Result of accepting a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveResult {
    /// Events produced by the command.
    pub events: Vec<GameEvent>,
    /// Aggregate revision after applying the events.
    pub resulting_revision: u64,
}

/// Deterministic command rejection.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GameError {
    /// The actor is not seated in this game.
    #[error("player is not a member of this game")]
    NotAPlayer,
    /// The actor attempted a command outside their turn.
    #[error("it is not this player's turn")]
    OutOfTurn,
    /// The game has already ended.
    #[error("the game is complete")]
    GameComplete,
    /// The command referenced a tile not in the actor's rack.
    #[error("tile {0:?} is not in the player's rack")]
    TileNotInRack(TileId),
    /// A coordinate falls outside the pinned board.
    #[error(
        "coordinate ({coordinate_x}, {coordinate_y}) is outside a {board_size} by {board_size} board"
    )]
    CoordinateOutOfBounds {
        coordinate_x: u8,
        coordinate_y: u8,
        board_size: u8,
    },
    /// Two placements target the same coordinate.
    #[error("more than one tile targets the same coordinate")]
    DuplicateCoordinate,
    /// A placement targets an occupied square.
    #[error("placement targets an occupied square")]
    OccupiedCoordinate,
    /// A blank assignment was missing or invalid.
    #[error("blank tiles require an uppercase ASCII letter")]
    InvalidBlankLetter,
    /// A non-blank tile was assigned a blank letter.
    #[error("only blank tiles may specify a selected letter")]
    UnexpectedBlankLetter,
    /// The command had no tile operands.
    #[error("the command must include at least one tile")]
    EmptyTileSelection,
    /// New placements must occupy one row or one column.
    #[error("new tiles must be placed in one row or one column")]
    NotLinear,
    /// The resulting main word has an empty square between its endpoints.
    #[error("the played word contains a gap")]
    Gap,
    /// The first move must cover the profile start square.
    #[error("the first move must cover the start square")]
    FirstMoveMustCoverStart,
    /// A later move must touch the committed board.
    #[error("the move must connect to an existing tile")]
    Disconnected,
    /// One or more server-derived words are not accepted by the pinned dictionary.
    #[error("dictionary rejected words: {}", .0.join(", "))]
    InvalidWords(Vec<String>),
    /// Exchange requires enough tiles in the bag.
    #[error("the bag does not contain enough tiles for an exchange")]
    ExchangeUnavailable,
    /// A tile appeared more than once in the command.
    #[error("the same tile was selected more than once")]
    DuplicateTile,
}
