//! Durable, idempotent game-journal persistence using `switchy` query builders.

use serde::{Deserialize, Serialize};
use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;
use words_with_spouses_game_domain::{GameEvent, GameId};

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
        .select("game_journal")
        .where_eq("game_id", game_id.clone())
        .where_eq("command_id", command_id)
        .execute(tx)
        .await?;
    if !duplicate.is_empty() {
        return Err(JournalError::DuplicateCommand);
    }
    let duplicate = tx
        .select("game_journal")
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
            .value("payload_version", 1_i64)
            .value("payload", serde_json::to_string(event)?)
            .execute(tx)
            .await?;
    }

    let resulting_revision = expected_revision
        .checked_add(u64::try_from(events.len()).map_err(|_| JournalError::InvalidRevision)?)
        .ok_or(JournalError::InvalidRevision)?;
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
    /// Domain event serialization failed.
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
    /// Portable database operation failed.
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
}
