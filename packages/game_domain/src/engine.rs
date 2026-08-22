//! Deterministic bag construction, aggregate initialization, reduction, and replay.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::{GameEvent, GameMetadata, GameState, GameStatus, PlayerId, RuleProfile, Tile, TileId};

/// Builds all uniquely identified physical tiles in stable profile order.
///
/// # Panics
///
/// Panics if an unvalidated profile contains more than [`u16::MAX`] physical tiles.
#[must_use]
pub fn build_bag(profile: &RuleProfile) -> Vec<Tile> {
    let mut bag = Vec::with_capacity(usize::from(profile.tile_count()));
    for definition in &profile.tiles {
        for _ in 0..definition.quantity {
            let id =
                u16::try_from(bag.len()).expect("validated profile contains at most u16 tiles");
            bag.push(Tile {
                id: TileId::new(id),
                face: definition.face,
                points: definition.points,
            });
        }
    }
    bag
}

/// Deterministically shuffles tiles using a persisted seed.
///
/// This uses fixed `SplitMix64` output and Fisher-Yates ordering rather than a dependency whose
/// algorithm may change between releases. A game's seed is canonical input and must be retained
/// with its start event or equivalent persisted creation record.
pub fn shuffle_bag(tiles: &mut [Tile], seed: u64) {
    let mut generator = SplitMix64(seed);
    for upper in (1..tiles.len()).rev() {
        let index = generator.index(upper + 1);
        tiles.swap(upper, index);
    }
}

/// Creates the canonical start event with deterministic bag order and rack fill.
///
/// # Errors
///
/// * Returns [`InitializationError::TooFewPlayers`] when fewer than two players are supplied.
/// * Returns [`InitializationError::DuplicatePlayer`] when a player occupies multiple seats.
/// * Returns [`InitializationError::UnknownFirstPlayer`] when `first_player` is not seated.
/// * Returns [`InitializationError::InsufficientTiles`] when the profile cannot fill every rack.
pub fn initialize_game(
    metadata: GameMetadata,
    players: Vec<PlayerId>,
    first_player: PlayerId,
    profile: &RuleProfile,
    shuffle_seed: u64,
) -> Result<GameEvent, InitializationError> {
    if players.len() < 2 {
        return Err(InitializationError::TooFewPlayers);
    }
    if players.iter().copied().collect::<BTreeSet<_>>().len() != players.len() {
        return Err(InitializationError::DuplicatePlayer);
    }
    if !players.contains(&first_player) {
        return Err(InitializationError::UnknownFirstPlayer);
    }

    let mut bag = build_bag(profile);
    let required = usize::from(profile.rack_size) * players.len();
    if bag.len() < required {
        return Err(InitializationError::InsufficientTiles);
    }
    shuffle_bag(&mut bag, shuffle_seed);

    let mut racks = BTreeMap::new();
    for &player in &players {
        racks.insert(player, draw_tiles(&mut bag, usize::from(profile.rack_size)));
    }

    Ok(GameEvent::GameStarted {
        metadata,
        players,
        first_player,
        rules: profile.clone(),
        racks,
        bag,
    })
}

/// Applies one canonical event to aggregate state.
///
/// # Errors
///
/// * Returns [`ReplayError`] when the journal is malformed or violates tile/state invariants.
pub fn apply_event(state: Option<GameState>, event: &GameEvent) -> Result<GameState, ReplayError> {
    match (state, event) {
        (
            None,
            GameEvent::GameStarted {
                metadata,
                players,
                first_player,
                rules,
                racks,
                bag,
            },
        ) => {
            validate_start(players, *first_player, racks, bag)?;
            Ok(GameState {
                metadata: metadata.clone(),
                rules: rules.clone(),
                players: players.clone(),
                active_players: players.iter().copied().collect(),
                active_player: *first_player,
                board: BTreeMap::new(),
                racks: racks.clone(),
                bag: bag.clone(),
                scores: players.iter().copied().map(|player| (player, 0)).collect(),
                scoreless_turns: 0,
                consecutive_passes: 0,
                winner: None,
                leaders: BTreeSet::new(),
                completion_reason: None,
                status: GameStatus::Active,
                revision: 1,
            })
        }
        (None, _) => Err(ReplayError::MissingStart),
        (Some(_), GameEvent::GameStarted { .. }) => Err(ReplayError::DuplicateStart),
        (Some(state), event) => apply_active_event(state, event),
    }
}

fn apply_active_event(mut state: GameState, event: &GameEvent) -> Result<GameState, ReplayError> {
    if state.status == GameStatus::Completed {
        return Err(ReplayError::EventAfterCompletion);
    }
    match event {
        GameEvent::TilesPlayed {
            player_id,
            placements,
            score,
            drawn,
        } => {
            ensure_turn(&state, *player_id)?;
            let rack = state
                .racks
                .get_mut(player_id)
                .ok_or(ReplayError::UnknownPlayer)?;
            for board_tile in placements.values() {
                take_tile(rack, board_tile.tile.id)?;
            }
            for (&coordinate, &board_tile) in placements {
                if state.board.insert(coordinate, board_tile).is_some() {
                    return Err(ReplayError::OccupiedCoordinate);
                }
            }
            take_drawn_tiles(&mut state.bag, drawn)?;
            rack.extend_from_slice(drawn);
            *state
                .scores
                .get_mut(player_id)
                .ok_or(ReplayError::UnknownPlayer)? += score;
            state.scoreless_turns = 0;
            state.consecutive_passes = 0;
            advance_turn(&mut state);
        }
        GameEvent::TilesExchanged {
            player_id,
            returned,
            drawn,
        } => {
            ensure_turn(&state, *player_id)?;
            let rack = state
                .racks
                .get_mut(player_id)
                .ok_or(ReplayError::UnknownPlayer)?;
            for tile in returned {
                let actual = take_tile(rack, tile.id)?;
                if actual != *tile {
                    return Err(ReplayError::TileMismatch(tile.id));
                }
            }
            take_drawn_tiles(&mut state.bag, drawn)?;
            rack.extend_from_slice(drawn);
            state.bag.extend_from_slice(returned);
            state.scoreless_turns = 0;
            state.consecutive_passes = 0;
            advance_turn(&mut state);
        }
        GameEvent::TurnPassed { player_id } => {
            ensure_turn(&state, *player_id)?;
            state.scoreless_turns = state.scoreless_turns.saturating_add(1);
            state.consecutive_passes = state.consecutive_passes.saturating_add(1);
            advance_turn(&mut state);
        }
        GameEvent::GameResigned { player_id, .. } => {
            ensure_turn(&state, *player_id)?;
            if !state.active_players.remove(player_id) {
                return Err(ReplayError::InactivePlayer);
            }
            state.consecutive_passes = 0;
            if state.active_players.len() >= 2 {
                advance_turn(&mut state);
            }
        }
        GameEvent::GameCompleted {
            scores,
            winner,
            leaders,
            reason,
        } => {
            let calculated = calculated_leaders(scores);
            let outright =
                (calculated.len() == 1).then(|| *calculated.first().expect("one leader"));
            if scores.keys().copied().collect::<BTreeSet<_>>()
                != state.players.iter().copied().collect()
                || calculated != *leaders
                || outright != *winner
            {
                return Err(ReplayError::InvalidFinalScores);
            }
            state.scores.clone_from(scores);
            state.winner = *winner;
            state.leaders.clone_from(leaders);
            state.completion_reason = Some(*reason);
            state.status = GameStatus::Completed;
        }
        GameEvent::GameStarted { .. } => unreachable!("handled before active events"),
    }
    state.revision += 1;
    validate_tile_uniqueness(&state)?;
    Ok(state)
}

/// Rebuilds canonical state exclusively from an ordered event journal.
///
/// # Errors
///
/// * Returns [`ReplayError`] when an event cannot validly follow the preceding state.
pub fn replay<'a>(
    events: impl IntoIterator<Item = &'a GameEvent>,
) -> Result<GameState, ReplayError> {
    let mut state = None;
    for event in events {
        state = Some(apply_event(state, event)?);
    }
    state.ok_or(ReplayError::EmptyJournal)
}

fn calculated_leaders(scores: &BTreeMap<PlayerId, u32>) -> BTreeSet<PlayerId> {
    let Some(highest) = scores.values().copied().max() else {
        return BTreeSet::new();
    };
    scores
        .iter()
        .filter_map(|(&player, &score)| (score == highest).then_some(player))
        .collect()
}

fn draw_tiles(bag: &mut Vec<Tile>, count: usize) -> Vec<Tile> {
    let draw_count = count.min(bag.len());
    bag.drain(bag.len() - draw_count..).collect()
}

fn take_drawn_tiles(bag: &mut Vec<Tile>, drawn: &[Tile]) -> Result<(), ReplayError> {
    for tile in drawn {
        let actual = bag.pop().ok_or(ReplayError::BagUnderflow)?;
        if actual != *tile {
            return Err(ReplayError::UnexpectedDraw {
                expected: actual.id,
                actual: tile.id,
            });
        }
    }
    Ok(())
}

fn take_tile(rack: &mut Vec<Tile>, tile_id: TileId) -> Result<Tile, ReplayError> {
    let position = rack
        .iter()
        .position(|tile| tile.id == tile_id)
        .ok_or(ReplayError::TileNotInRack(tile_id))?;
    Ok(rack.remove(position))
}

fn ensure_player(state: &GameState, player: PlayerId) -> Result<(), ReplayError> {
    if state.players.contains(&player) {
        Ok(())
    } else {
        Err(ReplayError::UnknownPlayer)
    }
}

fn ensure_turn(state: &GameState, player: PlayerId) -> Result<(), ReplayError> {
    ensure_player(state, player)?;
    if !state.active_players.contains(&player) {
        return Err(ReplayError::InactivePlayer);
    }
    if state.active_player == player {
        Ok(())
    } else {
        Err(ReplayError::OutOfTurn)
    }
}

fn advance_turn(state: &mut GameState) {
    let current = state
        .players
        .iter()
        .position(|player| *player == state.active_player)
        .expect("active player is seated");
    state.active_player = (1..=state.players.len())
        .map(|offset| state.players[(current + offset) % state.players.len()])
        .find(|player| state.active_players.contains(player))
        .expect("an active game has an active player");
}

fn validate_start(
    players: &[PlayerId],
    first_player: PlayerId,
    racks: &BTreeMap<PlayerId, Vec<Tile>>,
    bag: &[Tile],
) -> Result<(), ReplayError> {
    let unique = players.iter().copied().collect::<BTreeSet<_>>();
    if players.len() < 2 || unique.len() != players.len() || !players.contains(&first_player) {
        return Err(ReplayError::InvalidStart);
    }
    if racks.keys().copied().collect::<BTreeSet<_>>() != unique {
        return Err(ReplayError::InvalidStart);
    }
    let state_tiles = bag
        .iter()
        .chain(racks.values().flatten())
        .map(|tile| tile.id)
        .collect::<BTreeSet<_>>();
    let tile_count = bag.len() + racks.values().map(Vec::len).sum::<usize>();
    if state_tiles.len() != tile_count {
        return Err(ReplayError::DuplicateTile);
    }
    Ok(())
}

fn validate_tile_uniqueness(state: &GameState) -> Result<(), ReplayError> {
    let tiles = state
        .bag
        .iter()
        .chain(state.racks.values().flatten())
        .map(|tile| tile.id)
        .chain(state.board.values().map(|tile| tile.tile.id));
    let mut ids = BTreeSet::new();
    for tile_id in tiles {
        if !ids.insert(tile_id) {
            return Err(ReplayError::DuplicateTile);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct SplitMix64(u64);

impl SplitMix64 {
    const fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn index(&mut self, upper: usize) -> usize {
        let index = (u128::from(self.next()) * upper as u128) >> 64;
        usize::try_from(index).expect("scaled random index is always below slice length")
    }
}

/// Invalid deterministic game initialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum InitializationError {
    #[error("at least two players are required")]
    TooFewPlayers,
    #[error("a player cannot occupy multiple seats")]
    DuplicatePlayer,
    #[error("first player must occupy a seat")]
    UnknownFirstPlayer,
    #[error("tile distribution cannot fill both starting racks")]
    InsufficientTiles,
}

/// Canonical journal inconsistency detected during reduction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ReplayError {
    #[error("event journal is empty")]
    EmptyJournal,
    #[error("first event must start the game")]
    MissingStart,
    #[error("game may only start once")]
    DuplicateStart,
    #[error("game start data is invalid")]
    InvalidStart,
    #[error("event appears after game completion")]
    EventAfterCompletion,
    #[error("event references a player not seated in the game")]
    UnknownPlayer,
    #[error("event actor is out of turn")]
    OutOfTurn,
    #[error("event actor is no longer active")]
    InactivePlayer,
    #[error("tile {0:?} is not in the player's rack")]
    TileNotInRack(TileId),
    #[error("tile {0:?} does not match canonical rack data")]
    TileMismatch(TileId),
    #[error("event places a tile on an occupied coordinate")]
    OccupiedCoordinate,
    #[error("event draws from an empty bag")]
    BagUnderflow,
    #[error("draw expected tile {expected:?} but event supplied {actual:?}")]
    UnexpectedDraw { expected: TileId, actual: TileId },
    #[error("the same physical tile appears in more than one state location")]
    DuplicateTile,
    #[error("final scores do not contain exactly the seated players")]
    InvalidFinalScores,
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use time::OffsetDateTime;

    use super::*;
    use crate::{DictionaryRef, GameId, RuleProfileRef, initial_rule_profile};

    fn fixture() -> (GameMetadata, Vec<PlayerId>, RuleProfile) {
        let players = vec![PlayerId::new(), PlayerId::new()];
        let metadata = GameMetadata::new(
            GameId::new(),
            RuleProfileRef::new("classic-en", 1).expect("valid profile reference"),
            DictionaryRef::new("curated-en", 1, "sha256:test").expect("valid dictionary reference"),
            OffsetDateTime::UNIX_EPOCH,
        );
        (metadata, players, initial_rule_profile())
    }

    #[test]
    fn bag_construction_and_shuffle_are_deterministic() {
        let profile = initial_rule_profile();
        let mut first = build_bag(&profile);
        let mut second = build_bag(&profile);
        shuffle_bag(&mut first, 42);
        shuffle_bag(&mut second, 42);

        assert_eq!(first, second);
        assert_eq!(first.len(), 100);
        assert_ne!(first, build_bag(&profile));
        assert_eq!(
            first
                .iter()
                .map(|tile| tile.id)
                .collect::<BTreeSet<_>>()
                .len(),
            100
        );
    }

    #[test]
    fn initialization_fills_racks_and_replay_round_trips() {
        let (metadata, players, profile) = fixture();
        let started = initialize_game(metadata, players.clone(), players[0], &profile, 7)
            .expect("game initializes");
        let state = replay([&started]).expect("start event replays");
        let encoded = serde_json::to_string(&state).expect("state serializes");
        let decoded: GameState = serde_json::from_str(&encoded).expect("state deserializes");

        assert_eq!(decoded, state);
        assert_eq!(state.racks[&players[0]].len(), 7);
        assert_eq!(state.racks[&players[1]].len(), 7);
        assert_eq!(state.bag.len(), 86);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn replay_advances_turns_and_rejects_out_of_turn_events() {
        let (metadata, players, profile) = fixture();
        let started = initialize_game(metadata, players.clone(), players[0], &profile, 9)
            .expect("game initializes");
        let passed = GameEvent::TurnPassed {
            player_id: players[0],
        };
        let state = replay([&started, &passed]).expect("ordered events replay");
        assert_eq!(state.active_player, players[1]);
        assert_eq!(state.revision, 2);

        assert_eq!(
            replay([&started, &passed, &passed]),
            Err(ReplayError::OutOfTurn)
        );
    }

    #[test]
    fn golden_move_sequence_replays_to_expected_state() {
        let (metadata, players, profile) = fixture();
        let started = initialize_game(metadata, players.clone(), players[0], &profile, 11)
            .expect("game initializes");
        let events = vec![
            started,
            GameEvent::TurnPassed {
                player_id: players[0],
            },
            GameEvent::TurnPassed {
                player_id: players[1],
            },
            GameEvent::GameResigned {
                player_id: players[0],
                winner: None,
            },
            GameEvent::GameCompleted {
                scores: BTreeMap::from([(players[0], 0), (players[1], 0)]),
                winner: None,
                leaders: BTreeSet::from([players[0], players[1]]),
                reason: crate::CompletionReason::InsufficientActivePlayers,
            },
        ];
        let state = replay(&events).expect("golden journal replays");

        assert_eq!(state.revision, 5);
        assert_eq!(state.active_player, players[0]);
        assert_eq!(state.consecutive_passes, 0);
        assert_eq!(state.status, GameStatus::Completed);
        assert_eq!(state.leaders, BTreeSet::from([players[0], players[1]]));
        assert!(state.board.is_empty());
        assert_eq!(state.racks[&players[0]].len(), 7);
        assert_eq!(state.racks[&players[1]].len(), 7);
        assert_eq!(state.bag.len(), 86);
    }

    proptest! {
        #[test]
        fn seeded_initialization_conserves_unique_tiles(seed in any::<u64>()) {
            let (metadata, players, profile) = fixture();
            let started = initialize_game(metadata, players.clone(), players[0], &profile, seed)
                .expect("valid fixture initializes");
            let state = replay([&started]).expect("start replays");
            let ids = state
                .bag
                .iter()
                .chain(state.racks.values().flatten())
                .map(|tile| tile.id)
                .collect::<BTreeSet<_>>();

            prop_assert_eq!(ids.len(), usize::from(profile.tile_count()));
            prop_assert_eq!(state.revision, 1);
        }

        #[test]
        fn replay_is_deterministic_and_revisions_are_monotonic(
            seed in any::<u64>(),
            pass_count in 0_usize..32,
        ) {
            let (metadata, players, profile) = fixture();
            let started = initialize_game(metadata, players.clone(), players[0], &profile, seed)
                .expect("valid fixture initializes");
            let mut events = vec![started];
            for index in 0..pass_count {
                events.push(GameEvent::TurnPassed {
                    player_id: players[index % players.len()],
                });
            }
            let references = events.iter().collect::<Vec<_>>();
            let replayed = replay(references.iter().copied()).expect("journal replays");
            let repeated = replay(references.iter().copied()).expect("journal replays again");

            prop_assert_eq!(&replayed, &repeated);
            prop_assert_eq!(replayed.revision, events.len() as u64);

            let split = events.len() / 2;
            let snapshot = replay(events[..split.max(1)].iter()).expect("prefix replays");
            let rebuilt = events[split.max(1)..]
                .iter()
                .try_fold(snapshot, |state, event| apply_event(Some(state), event))
                .expect("snapshot tail replays");
            prop_assert_eq!(rebuilt, replayed);
        }
    }
}
