//! Durable, idempotent game-journal persistence using `switchy` query builders.

use serde::{Deserialize, Serialize};
use switchy_database::{
    Database,
    query::{FilterableQuery as _, SortDirection},
};
use thiserror::Error;
use words_with_spouses_game_domain::{GameEvent, GameId, GameState, apply_event, replay};

const GAME_EVENT_PAYLOAD_VERSION: u32 = 1;
const GAME_SNAPSHOT_PAYLOAD_VERSION: u32 = 1;

/// Compatibility policy for canonical persisted payloads.
///
/// Existing payload versions are immutable. A schema change must introduce a new decoder and
/// explicit migration/upgrade test before writers advance either current version. Readers reject
/// unknown versions instead of attempting best-effort deserialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistedPayloadCompatibility {
    pub event_version: u32,
    pub snapshot_version: u32,
}

/// Returns the exact payload versions written by this application release.
#[must_use]
pub const fn persisted_payload_compatibility() -> PersistedPayloadCompatibility {
    PersistedPayloadCompatibility {
        event_version: GAME_EVENT_PAYLOAD_VERSION,
        snapshot_version: GAME_SNAPSHOT_PAYLOAD_VERSION,
    }
}

/// Persisted canonical event envelope with explicit compatibility version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedGameEvent {
    /// Aggregate identifier.
    pub game_id: GameId,
    /// Monotonic aggregate revision.
    pub revision: u64,
    /// Stable unique command identifier.
    pub command_id: String,
    /// Stable retry identity scoped to the game.
    pub idempotency_key: String,
    /// Persisted event payload schema version.
    pub payload_version: u32,
    /// Canonical domain event.
    pub event: GameEvent,
}

/// Appends canonical events only when the expected aggregate revision still matches.
///
/// This API deliberately requires one database transaction so checking the game revision,
/// appending every event, and advancing the revision are atomic.
///
/// # Errors
///
/// * Returns [`JournalError::Conflict`] when `expected_revision` is stale.
/// * Returns [`JournalError::DuplicateCommand`] when command or idempotency identity was used.
/// * Returns [`JournalError::Serialization`] when an event cannot be encoded.
/// * Returns [`JournalError::Database`] when a `switchy` builder operation fails.
pub async fn append_events(
    tx: &dyn Database,
    game_id: GameId,
    command_id: &str,
    idempotency_key: &str,
    expected_revision: u64,
    events: &[GameEvent],
) -> Result<u64, JournalError> {
    let game_id = game_id.to_string();
    let rows = tx
        .select("games")
        .where_eq("game_id", game_id.clone())
        .execute(tx)
        .await?;
    let actual_revision = rows
        .first()
        .and_then(|row| row.get("canonical_revision"))
        .and_then(|value| value.as_i64())
        .ok_or(JournalError::GameNotFound)?;
    let actual_revision =
        u64::try_from(actual_revision).map_err(|_| JournalError::InvalidRevision)?;
    if actual_revision != expected_revision {
        return Err(JournalError::Conflict {
            expected: expected_revision,
            actual: actual_revision,
        });
    }

    let duplicate = tx
        .select("game_commands")
        .where_eq("game_id", game_id.clone())
        .where_eq("command_id", command_id)
        .execute(tx)
        .await?;
    if !duplicate.is_empty() {
        return Err(JournalError::DuplicateCommand);
    }
    let duplicate = tx
        .select("game_commands")
        .where_eq("game_id", game_id.clone())
        .where_eq("idempotency_key", idempotency_key)
        .execute(tx)
        .await?;
    if !duplicate.is_empty() {
        return Err(JournalError::DuplicateCommand);
    }

    for (index, event) in events.iter().enumerate() {
        let revision = expected_revision
            .checked_add(u64::try_from(index).map_err(|_| JournalError::InvalidRevision)? + 1)
            .ok_or(JournalError::InvalidRevision)?;
        tx.insert("game_journal")
            .value("event_id", format!("{game_id}:{revision}"))
            .value("game_id", game_id.clone())
            .value(
                "revision",
                i64::try_from(revision).map_err(|_| JournalError::InvalidRevision)?,
            )
            .value("command_id", command_id)
            .value("idempotency_key", idempotency_key)
            .value("payload_version", i64::from(GAME_EVENT_PAYLOAD_VERSION))
            .value("payload", serde_json::to_string(event)?)
            .execute(tx)
            .await?;
    }

    let resulting_revision = expected_revision
        .checked_add(u64::try_from(events.len()).map_err(|_| JournalError::InvalidRevision)?)
        .ok_or(JournalError::InvalidRevision)?;
    tx.insert("game_commands")
        .value("game_command_id", format!("{game_id}:{command_id}"))
        .value("game_id", game_id.clone())
        .value("command_id", command_id)
        .value("idempotency_key", idempotency_key)
        .value(
            "expected_revision",
            i64::try_from(expected_revision).map_err(|_| JournalError::InvalidRevision)?,
        )
        .value(
            "resulting_revision",
            i64::try_from(resulting_revision).map_err(|_| JournalError::InvalidRevision)?,
        )
        .execute(tx)
        .await?;
    let updated = tx
        .update("games")
        .value(
            "canonical_revision",
            i64::try_from(resulting_revision).map_err(|_| JournalError::InvalidRevision)?,
        )
        .where_eq("game_id", game_id)
        .where_eq(
            "canonical_revision",
            i64::try_from(expected_revision).map_err(|_| JournalError::InvalidRevision)?,
        )
        .execute(tx)
        .await?;
    if updated.len() != 1 {
        return Err(JournalError::ConcurrentConflict);
    }
    Ok(resulting_revision)
}

/// Begins, appends, and commits one canonical command transaction.
///
/// # Errors
///
/// * Returns [`JournalError`] for revision, idempotency, serialization, or database failures.
pub async fn append_events_transactionally(
    db: &dyn Database,
    game_id: GameId,
    command_id: &str,
    idempotency_key: &str,
    expected_revision: u64,
    events: &[GameEvent],
) -> Result<u64, JournalError> {
    let tx = db.begin_transaction().await?;
    match append_events(
        &*tx,
        game_id,
        command_id,
        idempotency_key,
        expected_revision,
        events,
    )
    .await
    {
        Ok(revision) => {
            tx.commit().await?;
            Ok(revision)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}

/// Loads the canonical aggregate from its latest snapshot plus journal tail.
///
/// # Errors
///
/// * Returns [`JournalError::EmptyJournal`] when neither snapshot nor start event exists.
/// * Returns [`JournalError::Replay`] for malformed canonical event order.
/// * Returns persistence and compatibility errors from snapshot/journal loading.
pub async fn recover_game(db: &dyn Database, game_id: GameId) -> Result<GameState, JournalError> {
    let snapshot = load_latest_snapshot(db, game_id).await?;
    let after_revision = snapshot.as_ref().map_or(0, |state| state.revision);
    let tail = load_events(db, game_id, after_revision).await?;
    match snapshot {
        Some(state) => tail
            .iter()
            .try_fold(state, |state, persisted| {
                apply_event(Some(state), &persisted.event)
            })
            .map_err(JournalError::Replay),
        None if tail.is_empty() => Err(JournalError::EmptyJournal),
        None => replay(tail.iter().map(|persisted| &persisted.event)).map_err(JournalError::Replay),
    }
}

/// Loads a canonical journal in revision order and decodes its versioned events.
///
/// # Errors
///
/// * Returns [`JournalError::UnsupportedPayloadVersion`] for an unknown event format.
/// * Returns [`JournalError::Serialization`] for malformed event data.
/// * Returns [`JournalError::Database`] when a `switchy` query fails.
pub async fn load_events(
    db: &dyn Database,
    game_id: GameId,
    after_revision: u64,
) -> Result<Vec<PersistedGameEvent>, JournalError> {
    let rows = db
        .select("game_journal")
        .where_eq("game_id", game_id.to_string())
        .where_gt(
            "revision",
            i64::try_from(after_revision).map_err(|_| JournalError::InvalidRevision)?,
        )
        .sort("revision", SortDirection::Asc)
        .execute(db)
        .await?;
    rows.into_iter()
        .map(|row| {
            let revision = integer_column(&row, "revision")?;
            let payload_version = integer_column(&row, "payload_version")?;
            if payload_version != u64::from(GAME_EVENT_PAYLOAD_VERSION) {
                return Err(JournalError::UnsupportedPayloadVersion(payload_version));
            }
            let payload = string_column(&row, "payload")?;
            Ok(PersistedGameEvent {
                game_id,
                revision,
                command_id: string_column(&row, "command_id")?,
                idempotency_key: string_column(&row, "idempotency_key")?,
                payload_version: GAME_EVENT_PAYLOAD_VERSION,
                event: serde_json::from_str(&payload)?,
            })
        })
        .collect()
}

/// Stores an idempotently replaceable canonical snapshot.
///
/// # Errors
///
/// * Returns [`JournalError::Serialization`] when snapshot encoding fails.
/// * Returns [`JournalError::Database`] when storage fails.
pub async fn store_snapshot(
    db: &dyn Database,
    game_id: GameId,
    state: &words_with_spouses_game_domain::GameState,
    created_at_ms: i64,
) -> Result<(), JournalError> {
    let game_id = game_id.to_string();
    db.delete("game_snapshots")
        .where_eq("game_id", game_id.clone())
        .where_eq(
            "revision",
            i64::try_from(state.revision).map_err(|_| JournalError::InvalidRevision)?,
        )
        .execute(db)
        .await?;
    db.insert("game_snapshots")
        .value("snapshot_id", format!("{game_id}:{}", state.revision))
        .value("game_id", game_id)
        .value(
            "revision",
            i64::try_from(state.revision).map_err(|_| JournalError::InvalidRevision)?,
        )
        .value("payload_version", i64::from(GAME_SNAPSHOT_PAYLOAD_VERSION))
        .value("payload", serde_json::to_string(state)?)
        .value("created_at_ms", created_at_ms)
        .execute(db)
        .await?;
    Ok(())
}

/// Loads the latest compatible canonical snapshot.
///
/// # Errors
///
/// * Returns compatibility, serialization, or database errors for invalid persisted data.
pub async fn load_latest_snapshot(
    db: &dyn Database,
    game_id: GameId,
) -> Result<Option<words_with_spouses_game_domain::GameState>, JournalError> {
    let rows = db
        .select("game_snapshots")
        .where_eq("game_id", game_id.to_string())
        .sort("revision", SortDirection::Desc)
        .execute(db)
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let version = integer_column(row, "payload_version")?;
    if version != u64::from(GAME_SNAPSHOT_PAYLOAD_VERSION) {
        return Err(JournalError::UnsupportedPayloadVersion(version));
    }
    let payload = string_column(row, "payload")?;
    Ok(Some(serde_json::from_str(&payload)?))
}

fn string_column(row: &switchy_database::Row, name: &str) -> Result<String, JournalError> {
    row.get(name)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(JournalError::MalformedRow)
}

fn integer_column(row: &switchy_database::Row, name: &str) -> Result<u64, JournalError> {
    row.get(name)
        .and_then(|value| value.as_i64())
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(JournalError::MalformedRow)
}

/// Journal persistence failure.
#[derive(Debug, Error)]
pub enum JournalError {
    /// Aggregate does not exist.
    #[error("game does not exist")]
    GameNotFound,
    /// Persisted revision cannot be represented safely.
    #[error("game revision is invalid")]
    InvalidRevision,
    /// Optimistic concurrency rejected a stale command.
    #[error("expected revision {expected}, actual revision {actual}")]
    Conflict { expected: u64, actual: u64 },
    /// Revision changed after it was read in the current transaction.
    #[error("game revision changed concurrently")]
    ConcurrentConflict,
    /// Command retry identity has already been persisted.
    #[error("command or idempotency key has already been used")]
    DuplicateCommand,
    /// Persisted row is missing a required field or has an invalid type.
    #[error("persisted journal row is malformed")]
    MalformedRow,
    /// Persisted payload version is not understood by this application.
    #[error("unsupported payload version {0}")]
    UnsupportedPayloadVersion(u64),
    /// No canonical start event or snapshot exists.
    #[error("canonical journal is empty")]
    EmptyJournal,
    /// Persisted canonical event sequence is invalid.
    #[error(transparent)]
    Replay(#[from] words_with_spouses_game_domain::ReplayError),
    /// Domain event serialization failed.
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
    /// Portable database operation failed.
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use time::OffsetDateTime;
    use words_with_spouses_game_domain::{
        GameMetadata, PlayerId, RuleProfileRef, bundled_dictionary_ref, initial_rule_profile,
        initialize_game, replay,
    };

    use super::*;
    use crate::migrate_app;

    async fn initialized_database() -> (
        Box<dyn Database>,
        GameId,
        words_with_spouses_game_domain::GameState,
    ) {
        let db = switchy_database_connection::builder()
            .turso()
            .with_in_memory()
            .build()
            .await
            .expect("Turso opens");
        migrate_app(&*db).await.expect("migrations run");
        let game_id = GameId::new();
        let players = [PlayerId::new(), PlayerId::new()];
        let metadata = GameMetadata::new(
            game_id,
            RuleProfileRef::new("classic-en", 1).expect("rules reference"),
            bundled_dictionary_ref().expect("dictionary reference"),
            OffsetDateTime::UNIX_EPOCH,
        );
        let started = initialize_game(metadata, players, players[0], &initial_rule_profile(), 1)
            .expect("game starts");
        let state = replay([&started]).expect("start replays");
        db.insert("games")
            .value("game_id", game_id.to_string())
            .value("rules_id", "classic-en")
            .value("rules_version", 1_i64)
            .value("dictionary_id", "enable1-en")
            .value("dictionary_version", 1_i64)
            .value("dictionary_checksum", "sha256:test")
            .value("canonical_revision", 0_i64)
            .value("status", "ACTIVE")
            .value("created_at_ms", 0_i64)
            .value("updated_at_ms", 0_i64)
            .execute(&*db)
            .await
            .expect("game inserts");
        (db, game_id, state)
    }

    fn started_event(state: &GameState) -> GameEvent {
        GameEvent::GameStarted {
            metadata: state.metadata.clone(),
            players: state.players,
            first_player: state.players[0],
            racks: state.racks.clone(),
            bag: state.bag.clone(),
        }
    }

    #[test]
    fn one_command_can_append_multiple_events_with_one_idempotency_record() {
        block_on(async {
            let (db, game_id, state) = initialized_database().await;
            let started = started_event(&state);
            let passed = GameEvent::TurnPassed {
                player_id: state.players[0],
            };
            assert_eq!(
                append_events_transactionally(
                    &*db,
                    game_id,
                    "multi-event-command",
                    "multi-event-idempotency",
                    0,
                    &[started, passed],
                )
                .await
                .expect("multi-event command appends atomically"),
                2
            );
            let events = load_events(&*db, game_id, 0)
                .await
                .expect("both events load");
            assert_eq!(events.len(), 2);
            assert!(
                events
                    .iter()
                    .all(|event| event.command_id == "multi-event-command")
            );
            assert!(matches!(
                append_events_transactionally(
                    &*db,
                    game_id,
                    "multi-event-command",
                    "different-idempotency",
                    2,
                    &[],
                )
                .await,
                Err(JournalError::DuplicateCommand)
            ));
        });
    }

    #[test]
    fn stale_revision_and_rolled_back_appends_leave_no_partial_events() {
        block_on(async {
            let (db, game_id, state) = initialized_database().await;
            let started = started_event(&state);
            assert!(matches!(
                append_events_transactionally(
                    &*db,
                    game_id,
                    "stale-command",
                    "stale-idempotency",
                    1,
                    std::slice::from_ref(&started),
                )
                .await,
                Err(JournalError::Conflict {
                    expected: 1,
                    actual: 0
                })
            ));
            assert!(
                load_events(&*db, game_id, 0)
                    .await
                    .expect("journal remains queryable")
                    .is_empty()
            );
            let rows = db
                .select("games")
                .where_eq("game_id", game_id.to_string())
                .execute(&*db)
                .await
                .expect("game revision loads");
            assert_eq!(
                rows[0]
                    .get("canonical_revision")
                    .and_then(|value| value.as_i64()),
                Some(0)
            );
        });
    }

    #[test]
    fn transactional_append_is_idempotent_and_recoverable() {
        block_on(async {
            let (db, game_id, state) = initialized_database().await;
            let started = started_event(&state);
            assert_eq!(
                append_events_transactionally(
                    &*db,
                    game_id,
                    "command-1",
                    "idem-1",
                    0,
                    std::slice::from_ref(&started),
                )
                .await
                .expect("event appends"),
                1
            );
            assert!(matches!(
                append_events_transactionally(
                    &*db,
                    game_id,
                    "command-1",
                    "idem-1",
                    1,
                    std::slice::from_ref(&started),
                )
                .await,
                Err(JournalError::DuplicateCommand)
            ));
            assert_eq!(
                recover_game(&*db, game_id).await.expect("game recovers"),
                state
            );
        });
    }

    #[test]
    fn persisted_rule_dictionary_event_and_snapshot_fixtures_remain_replayable() {
        block_on(async {
            let (db, game_id, state) = initialized_database().await;
            assert_eq!(state.metadata.rules(), &initial_rule_profile().reference);
            assert_eq!(
                state.metadata.dictionary().id(),
                bundled_dictionary_ref()
                    .expect("bundled dictionary reference")
                    .id()
            );
            let started = started_event(&state);
            let fixture = serde_json::to_string(&started).expect("event fixture serializes");
            let decoded: GameEvent = serde_json::from_str(&fixture).expect("event fixture decodes");
            assert_eq!(replay([&decoded]).expect("event fixture replays"), state);

            append_events_transactionally(
                &*db,
                game_id,
                "fixture-command",
                "fixture-idempotency",
                0,
                std::slice::from_ref(&started),
            )
            .await
            .expect("fixture event persists");
            store_snapshot(&*db, game_id, &state, 0)
                .await
                .expect("fixture snapshot persists");
            assert_eq!(
                load_events(&*db, game_id, 0)
                    .await
                    .expect("fixture events load")[0]
                    .event,
                started
            );
            assert_eq!(
                load_latest_snapshot(&*db, game_id)
                    .await
                    .expect("fixture snapshot loads"),
                Some(state)
            );
        });
    }

    #[test]
    fn payload_versions_are_explicit_and_unknown_versions_fail_closed() {
        block_on(async {
            let (db, game_id, state) = initialized_database().await;
            let started = started_event(&state);
            append_events_transactionally(
                &*db,
                game_id,
                "command-1",
                "idem-1",
                0,
                std::slice::from_ref(&started),
            )
            .await
            .expect("event appends");
            store_snapshot(&*db, game_id, &state, 0)
                .await
                .expect("snapshot stores");

            let compatibility = persisted_payload_compatibility();
            assert_eq!(compatibility.event_version, 1);
            assert_eq!(compatibility.snapshot_version, 1);

            db.update("game_journal")
                .value("payload_version", 2_i64)
                .where_eq("game_id", game_id.to_string())
                .execute(&*db)
                .await
                .expect("event version changes");
            assert!(matches!(
                load_events(&*db, game_id, 0).await,
                Err(JournalError::UnsupportedPayloadVersion(2))
            ));

            db.update("game_snapshots")
                .value("payload_version", 2_i64)
                .where_eq("game_id", game_id.to_string())
                .execute(&*db)
                .await
                .expect("snapshot version changes");
            assert!(matches!(
                load_latest_snapshot(&*db, game_id).await,
                Err(JournalError::UnsupportedPayloadVersion(2))
            ));
        });
    }

    #[test]
    fn snapshot_plus_tail_recovery_matches_full_replay() {
        block_on(async {
            let (db, game_id, state) = initialized_database().await;
            let started = started_event(&state);
            append_events_transactionally(
                &*db,
                game_id,
                "command-1",
                "idem-1",
                0,
                std::slice::from_ref(&started),
            )
            .await
            .expect("start appends");
            store_snapshot(&*db, game_id, &state, 0)
                .await
                .expect("snapshot stores");
            let passed = GameEvent::TurnPassed {
                player_id: state.players[0],
            };
            append_events_transactionally(
                &*db,
                game_id,
                "command-2",
                "idem-2",
                1,
                std::slice::from_ref(&passed),
            )
            .await
            .expect("tail appends");
            let expected = apply_event(Some(state), &passed).expect("event applies");
            assert_eq!(
                recover_game(&*db, game_id).await.expect("game recovers"),
                expected
            );
        });
    }

    #[test]
    fn journal_load_and_snapshot_round_trip_on_turso() {
        block_on(async {
            let (db, game_id, state) = initialized_database().await;
            assert!(
                load_events(&*db, game_id, 0)
                    .await
                    .expect("events load")
                    .is_empty()
            );
            store_snapshot(&*db, game_id, &state, 0)
                .await
                .expect("snapshot stores");
            assert_eq!(
                load_latest_snapshot(&*db, game_id)
                    .await
                    .expect("snapshot loads"),
                Some(state)
            );
        });
    }
}
