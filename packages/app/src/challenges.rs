//! Private direct challenges and exactly-once game creation.

use switchy_database::{
    Database,
    query::{FilterableQuery as _, SortDirection},
};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;
use wwmtf_game_domain::{
    DictionaryRef, GameId, GameMetadata, PlayerId, initial_rule_profile, initialize_game,
};

/// Durable direct challenge status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeStatus {
    Pending,
    Accepted,
    Declined,
    Cancelled,
}

impl ChallengeStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Accepted => "ACCEPTED",
            Self::Declined => "DECLINED",
            Self::Cancelled => "CANCELLED",
        }
    }
}

/// Creates one pending challenge between distinct users.
///
/// # Errors
///
/// * Returns [`ChallengeError::Invalid`] for self-challenges.
/// * Returns [`ChallengeError::Duplicate`] for an existing pending pair.
/// * Returns [`ChallengeError::Database`] when storage fails.
#[allow(clippy::similar_names)]
pub async fn create_challenge(
    db: &dyn Database,
    challenger_user_id: &str,
    challenged_user_id: &str,
    now: OffsetDateTime,
) -> Result<String, ChallengeError> {
    if challenger_user_id == challenged_user_id {
        return Err(ChallengeError::Invalid);
    }
    let existing = db
        .select("challenges")
        .where_eq("challenger_user_id", challenger_user_id)
        .where_eq("challenged_user_id", challenged_user_id)
        .where_eq("status", ChallengeStatus::Pending.as_str())
        .execute(db)
        .await?;
    if !existing.is_empty() {
        return Err(ChallengeError::Duplicate);
    }
    let id = Uuid::new_v4().to_string();
    let now = timestamp_ms(now)?;
    db.insert("challenges")
        .value("challenge_id", id.clone())
        .value("challenger_user_id", challenger_user_id)
        .value("challenged_user_id", challenged_user_id)
        .value("status", ChallengeStatus::Pending.as_str())
        .value("created_at_ms", now)
        .value("updated_at_ms", now)
        .execute(db)
        .await?;
    Ok(id)
}

/// Declines a pending challenge as its recipient.
///
/// # Errors
///
/// * Returns [`ChallengeError::Unauthorized`] unless one owned pending challenge changes.
pub async fn decline_challenge(
    db: &dyn Database,
    challenge_id: &str,
    challenged_user_id: &str,
    now: OffsetDateTime,
) -> Result<(), ChallengeError> {
    update_status(
        db,
        challenge_id,
        "challenged_user_id",
        challenged_user_id,
        ChallengeStatus::Declined,
        now,
    )
    .await
}

/// Cancels a pending challenge as its creator.
///
/// # Errors
///
/// * Returns [`ChallengeError::Unauthorized`] unless one owned pending challenge changes.
pub async fn cancel_challenge(
    db: &dyn Database,
    challenge_id: &str,
    challenger_user_id: &str,
    now: OffsetDateTime,
) -> Result<(), ChallengeError> {
    update_status(
        db,
        challenge_id,
        "challenger_user_id",
        challenger_user_id,
        ChallengeStatus::Cancelled,
        now,
    )
    .await
}

async fn update_status(
    db: &dyn Database,
    challenge_id: &str,
    owner_column: &str,
    owner_id: &str,
    status: ChallengeStatus,
    now: OffsetDateTime,
) -> Result<(), ChallengeError> {
    let updated = db
        .update("challenges")
        .value("status", status.as_str())
        .value("updated_at_ms", timestamp_ms(now)?)
        .where_eq("challenge_id", challenge_id)
        .where_eq(owner_column, owner_id)
        .where_eq("status", ChallengeStatus::Pending.as_str())
        .execute(db)
        .await?;
    if updated.len() != 1 {
        return Err(ChallengeError::Unauthorized);
    }
    Ok(())
}

/// Accepts a pending challenge and creates its game exactly once in one transaction.
///
/// Seat order follows challenger then challenged, with the challenger taking the first turn for
/// the pinned profile version.
///
/// # Errors
///
/// * Returns [`ChallengeError::Unauthorized`] unless the caller owns one pending challenge.
/// * Returns initialization, serialization, timestamp, or database failures.
#[allow(clippy::similar_names)]
pub async fn accept_challenge(
    db: &dyn Database,
    challenge_id: &str,
    challenged_user_id: &str,
    now: OffsetDateTime,
    shuffle_seed: u64,
) -> Result<GameId, ChallengeError> {
    let tx = db.begin_transaction().await?;
    let rows = tx
        .select("challenges")
        .where_eq("challenge_id", challenge_id)
        .where_eq("challenged_user_id", challenged_user_id)
        .where_eq("status", ChallengeStatus::Pending.as_str())
        .execute(&*tx)
        .await?;
    let row = rows.first().ok_or(ChallengeError::Unauthorized)?;
    let challenger_user_id = string_column(row, "challenger_user_id")?;
    let accepted = tx
        .update("challenges")
        .value("status", ChallengeStatus::Accepted.as_str())
        .value("updated_at_ms", timestamp_ms(now)?)
        .where_eq("challenge_id", challenge_id)
        .where_eq("status", ChallengeStatus::Pending.as_str())
        .execute(&*tx)
        .await?;
    if accepted.len() != 1 {
        tx.rollback().await?;
        return Err(ChallengeError::Unauthorized);
    }

    let game_id = create_game_in_transaction(
        &*tx,
        &challenger_user_id,
        challenged_user_id,
        now,
        shuffle_seed,
        &format!("accept:{challenge_id}"),
    )
    .await?;
    tx.commit().await?;
    Ok(game_id)
}

/// Creates one initialized game inside an existing transaction.
///
/// # Errors
///
/// * Returns compatibility, initialization, serialization, timestamp, or database failures.
pub async fn create_game_in_transaction(
    tx: &dyn Database,
    first_user_id: &str,
    second_user_id: &str,
    now: OffsetDateTime,
    shuffle_seed: u64,
    idempotency_key: &str,
) -> Result<GameId, ChallengeError> {
    create_game_for_users_in_transaction(
        tx,
        &[first_user_id.to_string(), second_user_id.to_string()],
        0,
        now,
        shuffle_seed,
        idempotency_key,
    )
    .await
}

/// Creates one initialized classic game for an ordered variable-size user list.
///
/// The selected first player is an index into `user_ids`; this application orchestration keeps
/// account identities out of the deterministic game domain.
///
/// # Errors
///
/// Returns invalid membership, compatibility, initialization, persistence, or projection errors.
#[allow(clippy::too_many_lines)]
pub async fn create_game_for_users_in_transaction(
    tx: &dyn Database,
    user_ids: &[String],
    first_player_index: usize,
    now: OffsetDateTime,
    shuffle_seed: u64,
    idempotency_key: &str,
) -> Result<GameId, ChallengeError> {
    create_game_for_users_with_rules_in_transaction(
        tx,
        user_ids,
        first_player_index,
        &initial_rule_profile(),
        now,
        shuffle_seed,
        idempotency_key,
        crate::GameVisibilitySettings::default(),
    )
    .await
}

/// Creates one game using complete resolved immutable rules.
///
/// # Errors
///
/// Returns invalid membership, rules, initialization, persistence, or projection errors.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub async fn create_game_for_users_with_rules_in_transaction(
    tx: &dyn Database,
    user_ids: &[String],
    first_player_index: usize,
    rules: &wwmtf_game_domain::RuleProfile,
    now: OffsetDateTime,
    shuffle_seed: u64,
    idempotency_key: &str,
    visibility: crate::GameVisibilitySettings,
) -> Result<GameId, ChallengeError> {
    if user_ids.len() < 2
        || user_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != user_ids.len()
        || first_player_index >= user_ids.len()
    {
        return Err(ChallengeError::Invalid);
    }
    let game_id = GameId::new();
    let players = (0..user_ids.len())
        .map(|_| PlayerId::new())
        .collect::<Vec<_>>();
    let metadata = GameMetadata::new(
        game_id,
        rules.reference.clone(),
        DictionaryRef::new(
            "enable1-en",
            1,
            "sha256:3f16130220645692ed49c7134e24a18504c2ca55b3c012f7290e3e77c63b1a89",
        )
        .map_err(|_| ChallengeError::Compatibility)?,
        now,
    );
    let started = initialize_game(
        metadata,
        players.clone(),
        players[first_player_index],
        rules,
        shuffle_seed,
    )?;
    let now_ms = timestamp_ms(now)?;
    tx.insert("games")
        .value("game_id", game_id.to_string())
        .value("rules_id", rules.reference.id())
        .value("rules_version", i64::from(rules.reference.version()))
        .value("dictionary_id", "enable1-en")
        .value("dictionary_version", 1_i64)
        .value(
            "dictionary_checksum",
            "sha256:3f16130220645692ed49c7134e24a18504c2ca55b3c012f7290e3e77c63b1a89",
        )
        .value("canonical_revision", 1_i64)
        .value("status", "ACTIVE")
        .value(
            "show_remaining_tile_count",
            i64::from(visibility.show_remaining_tile_count),
        )
        .value(
            "show_remaining_tile_faces",
            i64::from(visibility.show_remaining_tile_faces),
        )
        .value("created_at_ms", now_ms)
        .value("updated_at_ms", now_ms)
        .execute(tx)
        .await?;
    for (seat, (user_id, player_id)) in user_ids.iter().zip(players.iter().copied()).enumerate() {
        tx.insert("game_players")
            .value("game_player_id", player_id.as_uuid().to_string())
            .value("game_id", game_id.to_string())
            .value("user_id", user_id)
            .value(
                "seat",
                i64::try_from(seat).map_err(|_| ChallengeError::Invalid)?,
            )
            .execute(tx)
            .await?;
    }
    tx.insert("game_journal")
        .value("event_id", format!("{game_id}:1"))
        .value("game_id", game_id.to_string())
        .value("revision", 1_i64)
        .value("command_id", idempotency_key)
        .value("idempotency_key", idempotency_key)
        .value(
            "payload_version",
            i64::from(crate::persisted_payload_compatibility().event_version),
        )
        .value("payload", crate::journal::encode_game_event(&started)?)
        .execute(tx)
        .await?;
    tx.insert("game_commands")
        .value("game_command_id", format!("{game_id}:{idempotency_key}"))
        .value("game_id", game_id.to_string())
        .value("command_id", idempotency_key)
        .value("idempotency_key", idempotency_key)
        .value("expected_revision", 0_i64)
        .value("resulting_revision", 1_i64)
        .execute(tx)
        .await?;
    let state = wwmtf_game_domain::replay([&started]).map_err(ChallengeError::Replay)?;
    let player_count = tx
        .select("game_players")
        .where_eq("game_id", game_id.to_string())
        .execute(tx)
        .await?
        .len();
    if player_count != user_ids.len() {
        return Err(ChallengeError::Invalid);
    }
    let canonical_events = crate::load_events(tx, game_id, 0)
        .await
        .map_err(ChallengeError::Journal)?
        .into_iter()
        .map(|event| event.event)
        .collect::<Vec<_>>();
    crate::rebuild_game_projections(tx, &state, &canonical_events, now_ms)
        .await
        .map_err(ChallengeError::Projection)?;
    Ok(game_id)
}

/// Searches users by normalized exact username without public discovery behavior.
///
/// # Errors
///
/// * Returns [`ChallengeError::Database`] when querying fails.
pub async fn find_user_by_username(
    db: &dyn Database,
    normalized_username: &str,
) -> Result<Option<(String, String)>, ChallengeError> {
    let rows = db
        .select("users")
        .where_eq("username_normalized", normalized_username)
        .sort("username_normalized", SortDirection::Asc)
        .limit(1)
        .execute(db)
        .await?;
    rows.first()
        .map(|row| {
            Ok((
                string_column(row, "user_id")?,
                string_column(row, "username_display")?,
            ))
        })
        .transpose()
}

fn string_column(row: &switchy_database::Row, name: &str) -> Result<String, ChallengeError> {
    row.get(name)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(ChallengeError::Invalid)
}

fn timestamp_ms(timestamp: OffsetDateTime) -> Result<i64, ChallengeError> {
    i64::try_from(timestamp.unix_timestamp_nanos() / 1_000_000).map_err(|_| ChallengeError::Invalid)
}

/// Challenge lifecycle failure.
#[derive(Debug, Error)]
pub enum ChallengeError {
    #[error("challenge input is invalid")]
    Invalid,
    #[error("a pending challenge already exists")]
    Duplicate,
    #[error("challenge is unknown, resolved, or unauthorized")]
    Unauthorized,
    #[error("pinned compatibility data is invalid")]
    Compatibility,
    #[error(transparent)]
    Rules(#[from] wwmtf_game_domain::RuleProfileError),
    #[error(transparent)]
    Initialization(#[from] wwmtf_game_domain::InitializationError),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
    #[error(transparent)]
    Replay(wwmtf_game_domain::ReplayError),
    #[error(transparent)]
    Journal(crate::JournalError),
    #[error(transparent)]
    Projection(crate::ProjectionError),
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;

    use super::*;
    use crate::{migrate_app, register};

    #[test]
    fn challenge_lifecycle_creates_one_authorized_game() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens");
            migrate_app(&*db).await.expect("migrations run");
            let alice = register(
                &*db,
                "alice",
                "correct horse battery staple",
                OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .expect("Alice registers");
            let bob = register(
                &*db,
                "bob",
                "another correct horse battery",
                OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .expect("Bob registers");
            assert_eq!(
                find_user_by_username(&*db, "bob")
                    .await
                    .expect("lookup succeeds"),
                Some((bob.clone(), "bob".to_string()))
            );
            let challenge = create_challenge(&*db, &alice, &bob, OffsetDateTime::UNIX_EPOCH)
                .await
                .expect("challenge creates");
            assert!(matches!(
                create_challenge(&*db, &alice, &bob, OffsetDateTime::UNIX_EPOCH).await,
                Err(ChallengeError::Duplicate)
            ));
            let game = accept_challenge(&*db, &challenge, &bob, OffsetDateTime::UNIX_EPOCH, 4)
                .await
                .expect("challenge accepts");
            assert!(matches!(
                accept_challenge(&*db, &challenge, &bob, OffsetDateTime::UNIX_EPOCH, 4).await,
                Err(ChallengeError::Unauthorized)
            ));
            assert_eq!(
                db.select("game_players")
                    .where_eq("game_id", game.to_string())
                    .execute(&*db)
                    .await
                    .expect("players load")
                    .len(),
                2
            );
        });
    }

    #[test]
    fn concurrent_games_and_independent_tab_revisions_remain_isolated() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens");
            migrate_app(&*db).await.expect("migrations run");
            let alice = register(
                &*db,
                "alice",
                "correct horse battery staple",
                OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .expect("Alice registers");
            let bob = register(
                &*db,
                "bob",
                "another correct horse battery",
                OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .expect("Bob registers");
            let mut games = Vec::new();
            for seed in [10_u64, 20, 30] {
                let challenge = create_challenge(&*db, &alice, &bob, OffsetDateTime::UNIX_EPOCH)
                    .await
                    .expect("challenge creates");
                let game =
                    accept_challenge(&*db, &challenge, &bob, OffsetDateTime::UNIX_EPOCH, seed)
                        .await
                        .expect("game creates");
                games.push(game);
            }
            let tab_one = crate::recover_game(&*db, games[0])
                .await
                .expect("first game loads");
            let tab_two = crate::recover_game(&*db, games[1])
                .await
                .expect("second game loads");
            let pass = wwmtf_game_domain::GameEvent::TurnPassed {
                player_id: tab_one.active_player,
            };
            crate::append_events_transactionally(
                &*db,
                games[0],
                "tab-one-command",
                "tab-one-idempotency",
                tab_one.revision,
                std::slice::from_ref(&pass),
            )
            .await
            .expect("first game advances");
            let tab_one_updated = crate::recover_game(&*db, games[0])
                .await
                .expect("first game reloads");
            let tab_two_unchanged = crate::recover_game(&*db, games[1])
                .await
                .expect("second game reloads");

            assert_eq!(tab_one_updated.revision, tab_one.revision + 1);
            assert_eq!(tab_two_unchanged, tab_two);
            assert_ne!(
                tab_one_updated.metadata.id(),
                tab_two_unchanged.metadata.id()
            );
            assert_eq!(
                db.select("games")
                    .execute(&*db)
                    .await
                    .expect("games load")
                    .len(),
                3
            );
        });
    }

    #[test]
    fn challenge_decline_cancel_and_authorization_are_enforced() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens");
            migrate_app(&*db).await.expect("migrations run");
            let alice = register(
                &*db,
                "alice",
                "correct horse battery staple",
                OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .expect("Alice registers");
            let bob = register(
                &*db,
                "bob",
                "another correct horse battery",
                OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .expect("Bob registers");
            let challenge = create_challenge(&*db, &alice, &bob, OffsetDateTime::UNIX_EPOCH)
                .await
                .expect("challenge creates");
            assert!(matches!(
                cancel_challenge(&*db, &challenge, &bob, OffsetDateTime::UNIX_EPOCH).await,
                Err(ChallengeError::Unauthorized)
            ));
            decline_challenge(&*db, &challenge, &bob, OffsetDateTime::UNIX_EPOCH)
                .await
                .expect("recipient declines");
            let challenge = create_challenge(&*db, &bob, &alice, OffsetDateTime::UNIX_EPOCH)
                .await
                .expect("reverse challenge creates");
            cancel_challenge(&*db, &challenge, &bob, OffsetDateTime::UNIX_EPOCH)
                .await
                .expect("creator cancels");
        });
    }
}
