//! Immutable, data-driven gameplay rule profiles.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Coordinate, RuleProfileRef, TileFace};

/// Board premium applied only when a tile is newly placed on the square.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PremiumSquare {
    /// Multiply the newly placed tile's letter value.
    Letter(u8),
    /// Multiply the complete word value.
    Word(u8),
}

/// One tile face and its immutable quantity/value in a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileDefinition {
    /// Printed tile face.
    pub face: TileFace,
    /// Number of these tiles in the bag.
    pub quantity: u8,
    /// Intrinsic point value.
    pub points: u8,
}

/// Complete immutable rules data consumed by gameplay algorithms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleProfile {
    /// Stable identity and version persisted with each game.
    pub reference: RuleProfileRef,
    /// Width and height of the square board.
    pub board_size: u8,
    /// Square the first move must cover.
    pub start: Coordinate,
    /// Premium squares; absent coordinates have no premium.
    pub premiums: BTreeMap<Coordinate, PremiumSquare>,
    /// Physical tile distribution in deterministic fixture order.
    pub tiles: Vec<TileDefinition>,
    /// Number of tiles each player holds when possible.
    pub rack_size: u8,
    /// Bonus for playing a full rack in one move.
    pub full_rack_bonus: u16,
    /// Minimum bag size required before an exchange.
    pub minimum_tiles_for_exchange: u8,
    /// Consecutive scoreless turns that end the game.
    pub scoreless_turn_limit: u8,
    /// Dictionary identity expected by this rules profile.
    pub dictionary_id: String,
}

impl RuleProfile {
    /// Validates all internal profile relationships.
    ///
    /// # Errors
    ///
    /// * Returns [`RuleProfileError`] when dimensions, coordinates, multipliers, tile data,
    ///   turn limits, or dictionary identity are invalid.
    pub fn validate(&self) -> Result<(), RuleProfileError> {
        if self.board_size == 0 {
            return Err(RuleProfileError::EmptyBoard);
        }
        if !self.contains(self.start) {
            return Err(RuleProfileError::StartOutOfBounds);
        }
        for (&coordinate, &premium) in &self.premiums {
            if !self.contains(coordinate) {
                return Err(RuleProfileError::PremiumOutOfBounds(coordinate));
            }
            let multiplier = match premium {
                PremiumSquare::Letter(value) | PremiumSquare::Word(value) => value,
            };
            if multiplier < 2 {
                return Err(RuleProfileError::InvalidPremiumMultiplier(multiplier));
            }
        }
        if self.tiles.is_empty() {
            return Err(RuleProfileError::EmptyTileDistribution);
        }
        let mut faces = BTreeSet::new();
        for definition in &self.tiles {
            if definition.quantity == 0 {
                return Err(RuleProfileError::ZeroTileQuantity(definition.face));
            }
            if !faces.insert(definition.face) {
                return Err(RuleProfileError::DuplicateTileFace(definition.face));
            }
            if let TileFace::Letter(letter) = definition.face
                && !letter.is_ascii_uppercase()
            {
                return Err(RuleProfileError::InvalidTileLetter(letter));
            }
            if definition.face == TileFace::Blank && definition.points != 0 {
                return Err(RuleProfileError::ScoredBlank);
            }
        }
        if self.rack_size == 0 {
            return Err(RuleProfileError::EmptyRack);
        }
        if self.minimum_tiles_for_exchange < self.rack_size {
            return Err(RuleProfileError::ExchangeBelowRackSize);
        }
        if self.scoreless_turn_limit == 0 {
            return Err(RuleProfileError::ZeroScorelessTurnLimit);
        }
        if self.dictionary_id.trim().is_empty() {
            return Err(RuleProfileError::EmptyDictionaryId);
        }
        Ok(())
    }

    /// Returns whether a coordinate lies on this profile's board.
    #[must_use]
    pub const fn contains(&self, coordinate: Coordinate) -> bool {
        coordinate.x < self.board_size && coordinate.y < self.board_size
    }

    /// Returns the total number of physical tiles in this profile.
    #[must_use]
    pub fn tile_count(&self) -> u16 {
        self.tiles
            .iter()
            .map(|definition| u16::from(definition.quantity))
            .sum()
    }
}

/// Invalid immutable rules data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RuleProfileError {
    /// Board dimensions must be non-zero.
    #[error("board size must be greater than zero")]
    EmptyBoard,
    /// Start square must lie on the board.
    #[error("start square is outside the board")]
    StartOutOfBounds,
    /// Premium square must lie on the board.
    #[error("premium square {0:?} is outside the board")]
    PremiumOutOfBounds(Coordinate),
    /// Premiums must multiply by at least two.
    #[error("premium multiplier {0} must be at least two")]
    InvalidPremiumMultiplier(u8),
    /// At least one tile must exist.
    #[error("tile distribution cannot be empty")]
    EmptyTileDistribution,
    /// Every tile definition needs a positive quantity.
    #[error("tile face {0:?} has zero quantity")]
    ZeroTileQuantity(TileFace),
    /// Faces may only be declared once.
    #[error("tile face {0:?} is declared more than once")]
    DuplicateTileFace(TileFace),
    /// Letter tiles use normalized uppercase ASCII.
    #[error("tile letter {0:?} is not uppercase ASCII")]
    InvalidTileLetter(char),
    /// Blank tiles never have intrinsic points.
    #[error("blank tile points must be zero")]
    ScoredBlank,
    /// Rack size must be positive.
    #[error("rack size must be greater than zero")]
    EmptyRack,
    /// Exchanges cannot be permitted with fewer tiles than a full rack.
    #[error("minimum exchange bag size cannot be smaller than rack size")]
    ExchangeBelowRackSize,
    /// A positive scoreless-turn threshold is required.
    #[error("scoreless turn limit must be greater than zero")]
    ZeroScorelessTurnLimit,
    /// Profiles must identify their dictionary.
    #[error("dictionary identifier cannot be empty")]
    EmptyDictionaryId,
}

/// Reviewed initial neutral profile fixture.
///
/// This data is deliberately separate from move algorithms so persisted games pin behavior to a
/// stable profile version rather than to application branches.
///
/// # Panics
///
/// Panics only if the compile-time reviewed fixture is internally invalid.
#[must_use]
pub fn initial_rule_profile() -> RuleProfile {
    let profile = RuleProfile {
        reference: RuleProfileRef::new("classic-en", 1).expect("fixture reference is valid"),
        board_size: 15,
        start: Coordinate::new(7, 7),
        premiums: initial_premiums(),
        tiles: initial_tiles(),
        rack_size: 7,
        full_rack_bonus: 50,
        minimum_tiles_for_exchange: 7,
        scoreless_turn_limit: 6,
        dictionary_id: "enable1-en".to_string(),
    };
    profile.validate().expect("reviewed rules fixture is valid");
    profile
}

fn initial_tiles() -> Vec<TileDefinition> {
    const LETTERS: &[(char, u8, u8)] = &[
        ('A', 9, 1),
        ('B', 2, 3),
        ('C', 2, 3),
        ('D', 4, 2),
        ('E', 12, 1),
        ('F', 2, 4),
        ('G', 3, 2),
        ('H', 2, 4),
        ('I', 9, 1),
        ('J', 1, 8),
        ('K', 1, 5),
        ('L', 4, 1),
        ('M', 2, 3),
        ('N', 6, 1),
        ('O', 8, 1),
        ('P', 2, 3),
        ('Q', 1, 10),
        ('R', 6, 1),
        ('S', 4, 1),
        ('T', 6, 1),
        ('U', 4, 1),
        ('V', 2, 4),
        ('W', 2, 4),
        ('X', 1, 8),
        ('Y', 2, 4),
        ('Z', 1, 10),
    ];
    LETTERS
        .iter()
        .map(|&(letter, quantity, points)| TileDefinition {
            face: TileFace::Letter(letter),
            quantity,
            points,
        })
        .chain(std::iter::once(TileDefinition {
            face: TileFace::Blank,
            quantity: 2,
            points: 0,
        }))
        .collect()
}

fn initial_premiums() -> BTreeMap<Coordinate, PremiumSquare> {
    let mut premiums = BTreeMap::new();
    for &(x, y) in &[
        (0, 0),
        (0, 7),
        (0, 14),
        (7, 0),
        (7, 14),
        (14, 0),
        (14, 7),
        (14, 14),
    ] {
        premiums.insert(Coordinate::new(x, y), PremiumSquare::Word(3));
    }
    for &(x, y) in &[
        (1, 1),
        (2, 2),
        (3, 3),
        (4, 4),
        (7, 7),
        (10, 10),
        (11, 11),
        (12, 12),
        (13, 13),
        (1, 13),
        (2, 12),
        (3, 11),
        (4, 10),
        (10, 4),
        (11, 3),
        (12, 2),
        (13, 1),
    ] {
        premiums.insert(Coordinate::new(x, y), PremiumSquare::Word(2));
    }
    for &(x, y) in &[
        (1, 5),
        (1, 9),
        (5, 1),
        (5, 5),
        (5, 9),
        (5, 13),
        (9, 1),
        (9, 5),
        (9, 9),
        (9, 13),
        (13, 5),
        (13, 9),
    ] {
        premiums.insert(Coordinate::new(x, y), PremiumSquare::Letter(3));
    }
    for &(x, y) in &[
        (0, 3),
        (0, 11),
        (2, 6),
        (2, 8),
        (3, 0),
        (3, 7),
        (3, 14),
        (6, 2),
        (6, 6),
        (6, 8),
        (6, 12),
        (7, 3),
        (7, 11),
        (8, 2),
        (8, 6),
        (8, 8),
        (8, 12),
        (11, 0),
        (11, 7),
        (11, 14),
        (12, 6),
        (12, 8),
        (14, 3),
        (14, 11),
    ] {
        premiums.insert(Coordinate::new(x, y), PremiumSquare::Letter(2));
    }
    premiums
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_profile_fixture_is_stable_and_valid() {
        let profile = initial_rule_profile();

        assert_eq!(profile.reference.id(), "classic-en");
        assert_eq!(profile.reference.version(), 1);
        assert_eq!(profile.board_size, 15);
        assert_eq!(profile.start, Coordinate::new(7, 7));
        assert_eq!(profile.rack_size, 7);
        assert_eq!(profile.full_rack_bonus, 50);
        assert_eq!(profile.dictionary_id, "enable1-en");
        assert_eq!(profile.tile_count(), 100);
        assert_eq!(profile.tiles.len(), 27);
        assert_eq!(profile.premiums.len(), 61);
        assert_eq!(
            format!("{profile:#?}"),
            include_str!("../data/initial-rule-profile-v1.txt").trim()
        );
        assert_eq!(
            profile.premiums.get(&Coordinate::new(7, 7)),
            Some(&PremiumSquare::Word(2))
        );
        assert_eq!(profile.validate(), Ok(()));
    }

    #[test]
    fn profile_validation_rejects_branch_worthy_bad_data() {
        let mut profile = initial_rule_profile();
        profile.start = Coordinate::new(15, 15);
        assert_eq!(profile.validate(), Err(RuleProfileError::StartOutOfBounds));

        let mut profile = initial_rule_profile();
        profile.tiles[0].face = TileFace::Letter('a');
        assert_eq!(
            profile.validate(),
            Err(RuleProfileError::InvalidTileLetter('a'))
        );
    }
}
