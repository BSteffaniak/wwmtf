//! Server-authoritative command validation and scoring.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    AnalyzedWord, BoardTile, Coordinate, Dictionary, GameCommand, GameError, GameEvent, GameState,
    GameStatus, MoveResult, Placement, PlacementGuidance, PlayAnalysis, PlayerId, PremiumSquare,
    RuleProfile, Tile, TileFace, TileId,
};

/// Validates a gameplay command and produces canonical events without mutating state.
///
/// # Errors
///
/// * Returns [`GameError`] when membership, turn, rack, geometry, dictionary, exchange, or game
///   lifecycle rules reject the command.
pub fn decide_command(
    state: &GameState,
    actor: PlayerId,
    command: &GameCommand,
    profile: &RuleProfile,
    dictionary: &impl Dictionary,
) -> Result<MoveResult, GameError> {
    ensure_actor(state, actor)?;
    let events = match command {
        GameCommand::Play { placements } => {
            decide_play(state, actor, placements, profile, dictionary)?
        }
        GameCommand::Exchange { tile_ids } => {
            vec![decide_exchange(state, actor, tile_ids, profile)?]
        }
        GameCommand::Pass => decide_pass(state, actor, profile),
        GameCommand::Resign => vec![GameEvent::GameResigned {
            player_id: actor,
            winner: opponent(state, actor),
        }],
    };
    Ok(MoveResult {
        resulting_revision: state.revision + u64::try_from(events.len()).unwrap_or(u64::MAX),
        events,
    })
}

fn ensure_actor(state: &GameState, actor: PlayerId) -> Result<(), GameError> {
    if state.status == GameStatus::Completed {
        return Err(GameError::GameComplete);
    }
    if !state.players.contains(&actor) {
        return Err(GameError::NotAPlayer);
    }
    if state.active_player != actor {
        return Err(GameError::OutOfTurn);
    }
    Ok(())
}

fn decide_play(
    state: &GameState,
    actor: PlayerId,
    placements: &[Placement],
    profile: &RuleProfile,
    dictionary: &impl Dictionary,
) -> Result<Vec<GameEvent>, GameError> {
    let prepared = prepare_play(state, actor, placements, profile, dictionary)?;
    let drawn: Vec<Tile> = state
        .bag
        .iter()
        .rev()
        .take(placements.len())
        .copied()
        .collect();
    let drawn_count = drawn.len();
    let play_event = GameEvent::TilesPlayed {
        player_id: actor,
        placements: prepared.placed,
        score: prepared.analysis.score,
        drawn,
    };
    let mut events = vec![play_event];
    let remaining_rack = state.racks[&actor].len() - placements.len() + drawn_count;
    if state.bag.len() <= placements.len() && remaining_rack == 0 {
        events.push(completion_event_after_rack_out(
            state,
            actor,
            prepared.analysis.score,
        ));
    }
    Ok(events)
}

struct PreparedPlay {
    placed: BTreeMap<Coordinate, BoardTile>,
    analysis: PlayAnalysis,
}

/// Analyzes a candidate play with the same deterministic rules used to accept it.
///
/// This function never mutates canonical state, draws tiles, or produces events.
///
/// # Errors
///
/// * Returns [`GameError`] for the same membership, turn, rack, geometry, dictionary, and lifecycle
///   failures as a submitted play command.
pub fn analyze_play(
    state: &GameState,
    actor: PlayerId,
    placements: &[Placement],
    profile: &RuleProfile,
    dictionary: &impl Dictionary,
) -> Result<PlayAnalysis, GameError> {
    ensure_actor(state, actor)?;
    Ok(prepare_play(state, actor, placements, profile, dictionary)?.analysis)
}

fn prepare_play(
    state: &GameState,
    actor: PlayerId,
    placements: &[Placement],
    profile: &RuleProfile,
    dictionary: &impl Dictionary,
) -> Result<PreparedPlay, GameError> {
    let placed = candidate_tiles(state, actor, placements, profile)?;

    let axis = placement_axis(&placed)?;
    let board = combined_board(&state.board, &placed);
    validate_connection(state, &placed, &board, profile, axis)?;
    let words = formed_words(&board, &placed, axis);
    for word in &words {
        if !dictionary.contains(&word.text) {
            return Err(GameError::InvalidWord(word.text.clone()));
        }
    }
    let full_rack_bonus = if placements.len() == usize::from(profile.rack_size) {
        profile.full_rack_bonus
    } else {
        0
    };
    let words = words
        .into_iter()
        .map(|word| {
            let score = score_word(&word, &board, &placed, profile);
            AnalyzedWord {
                text: word.text,
                coordinates: word.coordinates,
                score,
            }
        })
        .collect::<Vec<_>>();
    let score = words.iter().map(|word| word.score).sum::<u32>() + u32::from(full_rack_bonus);
    Ok(PreparedPlay {
        placed,
        analysis: PlayAnalysis {
            words,
            score,
            full_rack_bonus,
        },
    })
}

fn decide_pass(state: &GameState, actor: PlayerId, profile: &RuleProfile) -> Vec<GameEvent> {
    let mut events = vec![GameEvent::TurnPassed { player_id: actor }];
    if state.scoreless_turns.saturating_add(1) >= profile.scoreless_turn_limit {
        events.push(completion_event_after_scoreless_end(state));
    }
    events
}

fn completion_event_after_rack_out(
    state: &GameState,
    finisher: PlayerId,
    move_score: u32,
) -> GameEvent {
    let mut scores = state.scores.clone();
    let opponent = opponent(state, finisher);
    let remaining = rack_points(&state.racks[&opponent]);
    *scores.get_mut(&finisher).expect("finisher is seated") += move_score + remaining;
    let opponent_score = scores[&opponent];
    *scores.get_mut(&opponent).expect("opponent is seated") =
        opponent_score.saturating_sub(remaining);
    let winner = winner(&scores);
    GameEvent::GameCompleted { scores, winner }
}

fn completion_event_after_scoreless_end(state: &GameState) -> GameEvent {
    let scores = state
        .players
        .into_iter()
        .map(|player| {
            let score = state.scores[&player];
            (
                player,
                score.saturating_sub(rack_points(&state.racks[&player])),
            )
        })
        .collect();
    let winner = winner(&scores);
    GameEvent::GameCompleted { scores, winner }
}

fn winner(scores: &BTreeMap<PlayerId, u32>) -> Option<PlayerId> {
    let mut entries = scores.iter();
    let (&first_player, &first_score) = entries.next()?;
    let (&second_player, &second_score) = entries.next()?;
    match first_score.cmp(&second_score) {
        std::cmp::Ordering::Greater => Some(first_player),
        std::cmp::Ordering::Less => Some(second_player),
        std::cmp::Ordering::Equal => None,
    }
}

fn rack_points(rack: &[Tile]) -> u32 {
    rack.iter().map(|tile| u32::from(tile.points)).sum()
}

fn opponent(state: &GameState, actor: PlayerId) -> PlayerId {
    if state.players[0] == actor {
        state.players[1]
    } else {
        state.players[0]
    }
}

fn decide_exchange(
    state: &GameState,
    actor: PlayerId,
    tile_ids: &BTreeSet<TileId>,
    profile: &RuleProfile,
) -> Result<GameEvent, GameError> {
    if tile_ids.is_empty() {
        return Err(GameError::EmptyTileSelection);
    }
    if state.bag.len() < usize::from(profile.minimum_tiles_for_exchange) {
        return Err(GameError::ExchangeUnavailable);
    }
    let rack = &state.racks[&actor];
    let returned = tile_ids
        .iter()
        .map(|tile_id| {
            rack.iter()
                .find(|tile| tile.id == *tile_id)
                .copied()
                .ok_or(GameError::TileNotInRack(*tile_id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let drawn = state
        .bag
        .iter()
        .rev()
        .take(returned.len())
        .copied()
        .collect();
    Ok(GameEvent::TilesExchanged {
        player_id: actor,
        returned,
        drawn,
    })
}

fn candidate_tiles(
    state: &GameState,
    actor: PlayerId,
    placements: &[Placement],
    profile: &RuleProfile,
) -> Result<BTreeMap<Coordinate, BoardTile>, GameError> {
    if placements.is_empty() {
        return Err(GameError::EmptyTileSelection);
    }
    let rack = &state.racks[&actor];
    let mut selected = BTreeSet::new();
    let mut placed = BTreeMap::new();
    for placement in placements {
        if !selected.insert(placement.tile_id) {
            return Err(GameError::DuplicateTile);
        }
        if !profile.contains(placement.coordinate) {
            return Err(GameError::CoordinateOutOfBounds {
                coordinate_x: placement.coordinate.x,
                coordinate_y: placement.coordinate.y,
                board_size: profile.board_size,
            });
        }
        if state.board.contains_key(&placement.coordinate) {
            return Err(GameError::OccupiedCoordinate);
        }
        let tile = rack
            .iter()
            .find(|tile| tile.id == placement.tile_id)
            .copied()
            .ok_or(GameError::TileNotInRack(placement.tile_id))?;
        let letter = placement_letter(tile, placement.blank_letter)?;
        if placed
            .insert(placement.coordinate, BoardTile { tile, letter })
            .is_some()
        {
            return Err(GameError::DuplicateCoordinate);
        }
    }
    Ok(placed)
}

/// Derives structural guidance for a partial candidate placement without consulting the dictionary.
///
/// The result contains no word suggestions and never mutates canonical state. Invalid tile or
/// coordinate operands still return their normal deterministic errors.
///
/// # Errors
///
/// * Returns [`GameError`] when the actor, rack operands, blank assignment, or coordinates are
///   invalid for the current canonical state.
pub fn placement_guidance(
    state: &GameState,
    actor: PlayerId,
    placements: &[Placement],
    profile: &RuleProfile,
) -> Result<PlacementGuidance, GameError> {
    ensure_actor(state, actor)?;
    if placements.is_empty() {
        return Ok(PlacementGuidance {
            required: BTreeSet::new(),
            eligible: if state.board.is_empty() {
                std::iter::once(profile.start).collect()
            } else {
                adjacent_open_squares(&state.board, profile)
            },
        });
    }
    let placed = candidate_tiles(state, actor, placements, profile)?;
    let guide_tile = *placed
        .values()
        .next()
        .ok_or(GameError::EmptyTileSelection)?;
    let required = required_gap_squares(state, &placed, profile);
    let mut eligible = BTreeSet::new();
    for y in 0..profile.board_size {
        for x in 0..profile.board_size {
            let coordinate = Coordinate::new(x, y);
            if state.board.contains_key(&coordinate) || placed.contains_key(&coordinate) {
                continue;
            }
            let mut candidate = placed.clone();
            candidate.insert(coordinate, guide_tile);
            if let Ok(axis) = placement_axis(&candidate) {
                let board = combined_board(&state.board, &candidate);
                if validate_connection(state, &candidate, &board, profile, axis).is_ok() {
                    eligible.insert(coordinate);
                }
            }
        }
    }
    Ok(PlacementGuidance { required, eligible })
}

fn adjacent_open_squares(
    board: &BTreeMap<Coordinate, BoardTile>,
    profile: &RuleProfile,
) -> BTreeSet<Coordinate> {
    board
        .keys()
        .flat_map(|&coordinate| neighbors(coordinate).into_iter().flatten())
        .filter(|coordinate| profile.contains(*coordinate) && !board.contains_key(coordinate))
        .collect()
}

fn required_gap_squares(
    state: &GameState,
    placed: &BTreeMap<Coordinate, BoardTile>,
    profile: &RuleProfile,
) -> BTreeSet<Coordinate> {
    if state.board.is_empty() && !placed.contains_key(&profile.start) {
        return std::iter::once(profile.start).collect();
    }
    let mut required = BTreeSet::new();
    let same_row = placed
        .keys()
        .all(|coordinate| coordinate.y == placed.keys().next().expect("non-empty").y);
    let same_column = placed
        .keys()
        .all(|coordinate| coordinate.x == placed.keys().next().expect("non-empty").x);
    if same_row {
        let y = placed.keys().next().expect("non-empty").y;
        let min = placed
            .keys()
            .map(|coordinate| coordinate.x)
            .min()
            .expect("non-empty");
        let max = placed
            .keys()
            .map(|coordinate| coordinate.x)
            .max()
            .expect("non-empty");
        required.extend(
            (min..=max)
                .map(|x| Coordinate::new(x, y))
                .filter(|coordinate| {
                    !state.board.contains_key(coordinate) && !placed.contains_key(coordinate)
                }),
        );
    } else if same_column {
        let x = placed.keys().next().expect("non-empty").x;
        let min = placed
            .keys()
            .map(|coordinate| coordinate.y)
            .min()
            .expect("non-empty");
        let max = placed
            .keys()
            .map(|coordinate| coordinate.y)
            .max()
            .expect("non-empty");
        required.extend(
            (min..=max)
                .map(|y| Coordinate::new(x, y))
                .filter(|coordinate| {
                    !state.board.contains_key(coordinate) && !placed.contains_key(coordinate)
                }),
        );
    }
    required
}

const fn placement_letter(tile: Tile, blank_letter: Option<char>) -> Result<char, GameError> {
    match (tile.face, blank_letter) {
        (TileFace::Letter(letter), None) => Ok(letter),
        (TileFace::Letter(_), Some(_)) => Err(GameError::UnexpectedBlankLetter),
        (TileFace::Blank, Some(letter)) if letter.is_ascii_uppercase() => Ok(letter),
        (TileFace::Blank, _) => Err(GameError::InvalidBlankLetter),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

fn placement_axis(placed: &BTreeMap<Coordinate, BoardTile>) -> Result<Axis, GameError> {
    let first = *placed.keys().next().expect("placements are non-empty");
    let same_row = placed.keys().all(|coordinate| coordinate.y == first.y);
    let same_column = placed.keys().all(|coordinate| coordinate.x == first.x);
    match (same_row, same_column) {
        (true, false | true) => Ok(Axis::Horizontal),
        (false, true) => Ok(Axis::Vertical),
        (false, false) => Err(GameError::NotLinear),
    }
}

fn combined_board(
    existing: &BTreeMap<Coordinate, BoardTile>,
    placed: &BTreeMap<Coordinate, BoardTile>,
) -> BTreeMap<Coordinate, BoardTile> {
    existing
        .iter()
        .chain(placed)
        .map(|(&coordinate, &tile)| (coordinate, tile))
        .collect()
}

fn validate_connection(
    state: &GameState,
    placed: &BTreeMap<Coordinate, BoardTile>,
    board: &BTreeMap<Coordinate, BoardTile>,
    profile: &RuleProfile,
    axis: Axis,
) -> Result<(), GameError> {
    let main = collect_word(board, *placed.keys().next().expect("non-empty"), axis);
    if main.coordinates.len() < placed.len()
        || !placed
            .keys()
            .all(|coordinate| main.coordinates.contains(coordinate))
    {
        return Err(GameError::Gap);
    }
    if state.board.is_empty() {
        if !placed.contains_key(&profile.start) {
            return Err(GameError::FirstMoveMustCoverStart);
        }
    } else if !placed.keys().any(|coordinate| {
        neighbors(*coordinate)
            .into_iter()
            .flatten()
            .any(|neighbor| state.board.contains_key(&neighbor))
    }) {
        return Err(GameError::Disconnected);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    text: String,
    coordinates: Vec<Coordinate>,
}

fn formed_words(
    board: &BTreeMap<Coordinate, BoardTile>,
    placed: &BTreeMap<Coordinate, BoardTile>,
    axis: Axis,
) -> Vec<Word> {
    let main = collect_word(board, *placed.keys().next().expect("non-empty"), axis);
    let perpendicular = match axis {
        Axis::Horizontal => Axis::Vertical,
        Axis::Vertical => Axis::Horizontal,
    };
    let crosses = placed
        .keys()
        .filter_map(|&coordinate| {
            let word = collect_word(board, coordinate, perpendicular);
            (word.coordinates.len() > 1).then_some(word)
        })
        .collect::<Vec<_>>();
    if main.coordinates.len() == 1 && !crosses.is_empty() {
        crosses
    } else {
        std::iter::once(main).chain(crosses).collect()
    }
}

fn collect_word(board: &BTreeMap<Coordinate, BoardTile>, origin: Coordinate, axis: Axis) -> Word {
    let mut start = origin;
    while let Some(previous) = step(start, axis, false)
        && board.contains_key(&previous)
    {
        start = previous;
    }
    let mut coordinates = Vec::new();
    let mut cursor = Some(start);
    while let Some(coordinate) = cursor
        && board.contains_key(&coordinate)
    {
        coordinates.push(coordinate);
        cursor = step(coordinate, axis, true);
    }
    let text = coordinates
        .iter()
        .map(|coordinate| board[coordinate].letter)
        .collect();
    Word { text, coordinates }
}

fn score_word(
    word: &Word,
    board: &BTreeMap<Coordinate, BoardTile>,
    placed: &BTreeMap<Coordinate, BoardTile>,
    profile: &RuleProfile,
) -> u32 {
    let mut word_multiplier = 1_u32;
    let letters = word
        .coordinates
        .iter()
        .map(|coordinate| {
            let points = u32::from(board[coordinate].tile.points);
            if let Some(premium) = placed
                .contains_key(coordinate)
                .then(|| profile.premiums.get(coordinate))
                .flatten()
            {
                match premium {
                    PremiumSquare::Letter(multiplier) => {
                        return points * u32::from(*multiplier);
                    }
                    PremiumSquare::Word(multiplier) => {
                        word_multiplier *= u32::from(*multiplier);
                    }
                }
            }
            points
        })
        .sum::<u32>();
    letters * word_multiplier
}

fn neighbors(coordinate: Coordinate) -> [Option<Coordinate>; 4] {
    [
        coordinate
            .x
            .checked_sub(1)
            .map(|x| Coordinate::new(x, coordinate.y)),
        coordinate
            .x
            .checked_add(1)
            .map(|x| Coordinate::new(x, coordinate.y)),
        coordinate
            .y
            .checked_sub(1)
            .map(|y| Coordinate::new(coordinate.x, y)),
        coordinate
            .y
            .checked_add(1)
            .map(|y| Coordinate::new(coordinate.x, y)),
    ]
}

fn step(coordinate: Coordinate, axis: Axis, forward: bool) -> Option<Coordinate> {
    match (axis, forward) {
        (Axis::Horizontal, true) => coordinate
            .x
            .checked_add(1)
            .map(|x| Coordinate::new(x, coordinate.y)),
        (Axis::Horizontal, false) => coordinate
            .x
            .checked_sub(1)
            .map(|x| Coordinate::new(x, coordinate.y)),
        (Axis::Vertical, true) => coordinate
            .y
            .checked_add(1)
            .map(|y| Coordinate::new(coordinate.x, y)),
        (Axis::Vertical, false) => coordinate
            .y
            .checked_sub(1)
            .map(|y| Coordinate::new(coordinate.x, y)),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use time::OffsetDateTime;

    use super::*;
    use crate::{
        DictionaryRef, GameId, GameMetadata, RuleProfileRef, WordSetDictionary,
        initial_rule_profile, initialize_game, replay,
    };

    fn state() -> (GameState, RuleProfile, WordSetDictionary) {
        let profile = initial_rule_profile();
        let players = [PlayerId::new(), PlayerId::new()];
        let metadata = GameMetadata::new(
            GameId::new(),
            RuleProfileRef::new("classic-en", 1).expect("valid rules"),
            DictionaryRef::new("curated-en", 1, "sha256:test").expect("valid dictionary"),
            OffsetDateTime::UNIX_EPOCH,
        );
        let started =
            initialize_game(metadata, players, players[0], &profile, 3).expect("game initializes");
        (
            replay([&started]).expect("start replays"),
            profile,
            WordSetDictionary::new(["AT", "A"]),
        )
    }

    fn set_rack(state: &mut GameState, player: PlayerId, tiles: &[(u16, TileFace, u8)]) {
        state.racks.insert(
            player,
            tiles
                .iter()
                .map(|&(id, face, points)| Tile {
                    id: TileId::new(id),
                    face,
                    points,
                })
                .collect(),
        );
    }

    fn play(tile_id: u16, x: u8, y: u8) -> Placement {
        Placement {
            tile_id: TileId::new(tile_id),
            coordinate: Coordinate::new(x, y),
            blank_letter: None,
        }
    }

    #[test]
    fn analysis_matches_accepted_play_words_score_and_bonus() {
        let (mut state, mut profile, dictionary) = state();
        let actor = state.active_player;
        profile.premiums.clear();
        profile.full_rack_bonus = 11;
        profile.rack_size = 2;
        set_rack(
            &mut state,
            actor,
            &[
                (200, TileFace::Letter('A'), 1),
                (201, TileFace::Letter('T'), 2),
            ],
        );
        let placements = vec![play(200, 7, 7), play(201, 8, 7)];
        let before = state.clone();

        let analysis = analyze_play(&state, actor, &placements, &profile, &dictionary)
            .expect("fixture play analyzes");
        let accepted = decide_command(
            &state,
            actor,
            &GameCommand::Play { placements },
            &profile,
            &dictionary,
        )
        .expect("analyzed play is accepted");

        assert_eq!(analysis.words.len(), 1);
        assert_eq!(analysis.words[0].text, "AT");
        assert_eq!(analysis.words[0].score, 3);
        assert_eq!(analysis.full_rack_bonus, 11);
        assert_eq!(analysis.score, 14);
        assert!(matches!(
            accepted.events.first(),
            Some(GameEvent::TilesPlayed { score: 14, .. })
        ));
        assert_eq!(state, before, "analysis must not mutate canonical state");
    }

    #[test]
    fn placement_guidance_covers_opening_connection_and_gaps() {
        let (mut state, profile, _dictionary) = state();
        let actor = state.active_player;
        set_rack(
            &mut state,
            actor,
            &[
                (200, TileFace::Letter('A'), 1),
                (201, TileFace::Letter('T'), 1),
            ],
        );

        let opening = placement_guidance(&state, actor, &[], &profile)
            .expect("empty opening draft has guidance");
        assert_eq!(opening.eligible, std::iter::once(profile.start).collect());

        let missing_start = placement_guidance(&state, actor, &[play(200, 8, 7)], &profile)
            .expect("partial opening draft has guidance");
        assert_eq!(
            missing_start.required,
            std::iter::once(profile.start).collect()
        );
        assert!(missing_start.eligible.contains(&profile.start));

        let gap = placement_guidance(&state, actor, &[play(200, 7, 7), play(201, 9, 7)], &profile)
            .expect("gapped draft has guidance");
        assert_eq!(
            gap.required,
            std::iter::once(Coordinate::new(8, 7)).collect()
        );
        assert!(gap.eligible.contains(&Coordinate::new(8, 7)));

        state.board.insert(
            profile.start,
            BoardTile {
                tile: Tile {
                    id: TileId::new(300),
                    face: TileFace::Letter('A'),
                    points: 1,
                },
                letter: 'A',
            },
        );
        let connected =
            placement_guidance(&state, actor, &[], &profile).expect("active board has guidance");
        assert!(connected.eligible.contains(&Coordinate::new(8, 7)));
        assert!(!connected.eligible.contains(&Coordinate::new(0, 0)));
    }

    #[test]
    fn first_move_must_cover_start_and_uses_server_score() {
        let (mut state, profile, dictionary) = state();
        let actor = state.active_player;
        state.racks.get_mut(&actor).expect("rack")[0] = Tile {
            id: TileId::new(200),
            face: TileFace::Letter('A'),
            points: 1,
        };
        state.racks.get_mut(&actor).expect("rack")[1] = Tile {
            id: TileId::new(201),
            face: TileFace::Letter('T'),
            points: 1,
        };
        let command = GameCommand::Play {
            placements: vec![
                Placement {
                    tile_id: TileId::new(200),
                    coordinate: Coordinate::new(7, 7),
                    blank_letter: None,
                },
                Placement {
                    tile_id: TileId::new(201),
                    coordinate: Coordinate::new(8, 7),
                    blank_letter: None,
                },
            ],
        };
        let result = decide_command(&state, actor, &command, &profile, &dictionary)
            .expect("legal first move");
        assert!(matches!(
            result.events.as_slice(),
            [GameEvent::TilesPlayed { score: 4, .. }]
        ));
    }

    #[test]
    fn pass_limit_and_resignation_complete_the_game() {
        let (mut state, profile, dictionary) = state();
        let actor = state.active_player;
        state.scoreless_turns = profile.scoreless_turn_limit - 1;
        let passed = decide_command(&state, actor, &GameCommand::Pass, &profile, &dictionary)
            .expect("pass is valid");
        assert!(matches!(
            passed.events.as_slice(),
            [
                GameEvent::TurnPassed { .. },
                GameEvent::GameCompleted { .. }
            ]
        ));

        let resigned = decide_command(&state, actor, &GameCommand::Resign, &profile, &dictionary)
            .expect("resignation is valid");
        assert!(matches!(
            resigned.events.as_slice(),
            [GameEvent::GameResigned { player_id, winner }]
                if *player_id == actor && *winner != actor
        ));
    }

    #[test]
    fn tied_final_scores_have_no_winner() {
        let (mut state, profile, dictionary) = state();
        let actor = state.active_player;
        state.scoreless_turns = profile.scoreless_turn_limit - 1;
        state.racks.values_mut().for_each(Vec::clear);
        state.scores.values_mut().for_each(|score| *score = 25);
        let result = decide_command(&state, actor, &GameCommand::Pass, &profile, &dictionary)
            .expect("final pass is valid");

        assert!(matches!(
            result.events.as_slice(),
            [GameEvent::TurnPassed { .. }, GameEvent::GameCompleted { scores, winner }]
                if scores.values().all(|score| *score == 25) && winner.is_none()
        ));
    }

    #[test]
    fn rack_out_applies_final_adjustments() {
        let (mut state, mut profile, dictionary) = state();
        let actor = state.active_player;
        let other = opponent(&state, actor);
        profile.rack_size = 1;
        state.bag.clear();
        state.racks.insert(
            actor,
            vec![Tile {
                id: TileId::new(200),
                face: TileFace::Letter('A'),
                points: 1,
            }],
        );
        state.racks.insert(
            other,
            vec![Tile {
                id: TileId::new(201),
                face: TileFace::Letter('Q'),
                points: 10,
            }],
        );
        let command = GameCommand::Play {
            placements: vec![Placement {
                tile_id: TileId::new(200),
                coordinate: profile.start,
                blank_letter: None,
            }],
        };
        let result = decide_command(&state, actor, &command, &profile, &dictionary)
            .expect("rack-out move is legal");

        assert!(matches!(
            result.events.as_slice(),
            [GameEvent::TilesPlayed { score: 52, .. }, GameEvent::GameCompleted { scores, winner }]
                if scores[&actor] == 62 && scores[&other] == 0 && *winner == Some(actor)
        ));
    }

    proptest! {
        #[test]
        fn pass_commands_preserve_scores_and_advance_one_revision(score in 0_u32..1_000) {
            let (mut state, mut profile, dictionary) = state();
            let actor = state.active_player;
            profile.scoreless_turn_limit = u8::MAX;
            state.scores.values_mut().for_each(|value| *value = score);
            let before = state.scores.clone();
            let result = decide_command(&state, actor, &GameCommand::Pass, &profile, &dictionary)
                .expect("pass is legal");

            prop_assert_eq!(result.events, vec![GameEvent::TurnPassed { player_id: actor }]);
            prop_assert_eq!(result.resulting_revision, state.revision + 1);
            prop_assert_eq!(state.scores, before);
        }

        #[test]
        fn analysis_and_accepted_play_always_have_the_same_score(
            first_points in 0_u8..11,
            second_points in 0_u8..11,
        ) {
            let (mut state, profile, dictionary) = state();
            let actor = state.active_player;
            state.racks.insert(actor, vec![
                Tile { id: TileId::new(200), face: TileFace::Letter('A'), points: first_points },
                Tile { id: TileId::new(201), face: TileFace::Letter('T'), points: second_points },
            ]);
            let placements = vec![
                Placement { tile_id: TileId::new(200), coordinate: profile.start, blank_letter: None },
                Placement { tile_id: TileId::new(201), coordinate: Coordinate::new(8, 7), blank_letter: None },
            ];
            let analysis = analyze_play(&state, actor, &placements, &profile, &dictionary)
                .expect("fixture word analyzes");
            let result = decide_command(
                &state,
                actor,
                &GameCommand::Play { placements },
                &profile,
                &dictionary,
            )
            .expect("analyzed fixture word is valid");
            let accepted_score = match result.events.first() {
                Some(GameEvent::TilesPlayed { score, .. }) => *score,
                event => panic!("expected play event, got {event:?}"),
            };

            prop_assert_eq!(analysis.score, accepted_score);
            prop_assert_eq!(analysis.words.iter().map(|word| word.score).sum::<u32>() + u32::from(analysis.full_rack_bonus), accepted_score);
        }

        #[test]
        fn first_move_score_is_nonnegative_and_server_derived(
            first_points in 0_u8..11,
            second_points in 0_u8..11,
        ) {
            let (mut state, profile, dictionary) = state();
            let actor = state.active_player;
            state.racks.insert(actor, vec![
                Tile { id: TileId::new(200), face: TileFace::Letter('A'), points: first_points },
                Tile { id: TileId::new(201), face: TileFace::Letter('T'), points: second_points },
            ]);
            let command = GameCommand::Play { placements: vec![
                Placement { tile_id: TileId::new(200), coordinate: profile.start, blank_letter: None },
                Placement { tile_id: TileId::new(201), coordinate: Coordinate::new(8, 7), blank_letter: None },
            ]};
            let result = decide_command(&state, actor, &command, &profile, &dictionary)
                .expect("fixture word is valid");
            let expected = 2 * (u32::from(first_points) + u32::from(second_points));

            let actual = match result.events.as_slice() {
                [GameEvent::TilesPlayed { score, .. }] => *score,
                events => panic!("expected one play event, got {events:?}"),
            };
            prop_assert_eq!(actual, expected);
        }
    }

    #[test]
    fn geometry_rejects_missing_start_non_linear_and_disconnected_moves() {
        let (mut state, profile, dictionary) = state();
        let actor = state.active_player;
        set_rack(
            &mut state,
            actor,
            &[
                (200, TileFace::Letter('A'), 1),
                (201, TileFace::Letter('T'), 1),
            ],
        );
        let missing_start = GameCommand::Play {
            placements: vec![play(200, 0, 0), play(201, 1, 0)],
        };
        assert_eq!(
            decide_command(&state, actor, &missing_start, &profile, &dictionary),
            Err(GameError::FirstMoveMustCoverStart)
        );
        let diagonal = GameCommand::Play {
            placements: vec![play(200, 7, 7), play(201, 8, 8)],
        };
        assert_eq!(
            decide_command(&state, actor, &diagonal, &profile, &dictionary),
            Err(GameError::NotLinear)
        );

        state.board.insert(
            profile.start,
            BoardTile {
                tile: Tile {
                    id: TileId::new(300),
                    face: TileFace::Letter('A'),
                    points: 1,
                },
                letter: 'A',
            },
        );
        let disconnected = GameCommand::Play {
            placements: vec![play(200, 0, 0), play(201, 1, 0)],
        };
        assert_eq!(
            decide_command(&state, actor, &disconnected, &profile, &dictionary),
            Err(GameError::Disconnected)
        );
    }

    #[test]
    fn cross_words_score_existing_tiles_without_reusing_premiums() {
        let (mut state, mut profile, dictionary) = state();
        let actor = state.active_player;
        profile.premiums.clear();
        profile
            .premiums
            .insert(Coordinate::new(8, 7), PremiumSquare::Letter(3));
        set_rack(&mut state, actor, &[(200, TileFace::Letter('T'), 1)]);
        state.board.insert(
            Coordinate::new(7, 7),
            BoardTile {
                tile: Tile {
                    id: TileId::new(300),
                    face: TileFace::Letter('A'),
                    points: 1,
                },
                letter: 'A',
            },
        );
        state.board.insert(
            Coordinate::new(8, 6),
            BoardTile {
                tile: Tile {
                    id: TileId::new(301),
                    face: TileFace::Letter('A'),
                    points: 1,
                },
                letter: 'A',
            },
        );
        let command = GameCommand::Play {
            placements: vec![play(200, 8, 7)],
        };
        let result = decide_command(&state, actor, &command, &profile, &dictionary)
            .expect("AT is valid in both directions");

        assert!(matches!(
            result.events.as_slice(),
            [GameEvent::TilesPlayed { score: 8, .. }]
        ));
    }

    #[test]
    fn blanks_score_zero_and_require_assignments() {
        let (mut state, profile, dictionary) = state();
        let actor = state.active_player;
        set_rack(
            &mut state,
            actor,
            &[(200, TileFace::Blank, 0), (201, TileFace::Letter('T'), 1)],
        );
        let missing = GameCommand::Play {
            placements: vec![play(200, 7, 7)],
        };
        assert_eq!(
            decide_command(&state, actor, &missing, &profile, &dictionary),
            Err(GameError::InvalidBlankLetter)
        );
        let command = GameCommand::Play {
            placements: vec![
                Placement {
                    tile_id: TileId::new(200),
                    coordinate: profile.start,
                    blank_letter: Some('A'),
                },
                play(201, 8, 7),
            ],
        };
        let result = decide_command(&state, actor, &command, &profile, &dictionary)
            .expect("assigned blank is valid");
        assert!(matches!(
            result.events.as_slice(),
            [GameEvent::TilesPlayed { score: 2, .. }]
        ));
    }

    #[test]
    fn exchange_checks_bag_and_preserves_requested_count() {
        let (mut state, mut profile, dictionary) = state();
        let actor = state.active_player;
        let tile_id = state.racks[&actor][0].id;
        let command = GameCommand::Exchange {
            tile_ids: std::iter::once(tile_id).collect(),
        };
        let result = decide_command(&state, actor, &command, &profile, &dictionary)
            .expect("exchange is available");
        assert!(matches!(
            result.events.as_slice(),
            [GameEvent::TilesExchanged { returned, drawn, .. }]
                if returned.len() == 1 && drawn.len() == 1
        ));

        profile.minimum_tiles_for_exchange = u8::MAX;
        state.bag.clear();
        assert_eq!(
            decide_command(&state, actor, &command, &profile, &dictionary),
            Err(GameError::ExchangeUnavailable)
        );
    }

    #[test]
    fn forged_rack_and_rejected_words_fail() {
        let (state, profile, dictionary) = state();
        let actor = state.active_player;
        let forged = GameCommand::Play {
            placements: vec![Placement {
                tile_id: TileId::new(u16::MAX),
                coordinate: profile.start,
                blank_letter: None,
            }],
        };
        assert!(matches!(
            decide_command(&state, actor, &forged, &profile, &dictionary),
            Err(GameError::TileNotInRack(_))
        ));
    }
}
