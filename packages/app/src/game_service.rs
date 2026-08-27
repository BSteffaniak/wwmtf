//! Server-authoritative application orchestration for gameplay commands.

use std::str::FromStr as _;

use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;
use wwmtf_game_domain::{
    GameCommand, GameId, GameState, PlayerId, apply_event, decide_command, dictionary,
};

/// Resolves one authenticated user to their stable player identity in a game.
///
/// # Errors
///
/// * Returns [`GameServiceError::Unauthorized`] when the user is not a game member.
/// * Returns malformed identity or database errors for invalid persisted membership.
pub async fn player_for_user(
    db: &dyn Database,
    game_id: GameId,
    user_id: &str,
) -> Result<PlayerId, GameServiceError> {
    let rows = db
        .select("game_players")
        .where_eq("game_id", game_id.to_string())
        .where_eq("user_id", user_id)
        .execute(db)
        .await?;
    let player_id = rows
        .first()
        .and_then(|row| row.get("game_player_id"))
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(GameServiceError::Unauthorized)?;
    PlayerId::from_str(&player_id).map_err(|_| GameServiceError::MalformedIdentity)
}

/// Authenticates membership, decides, atomically persists, snapshots, and projects one command.
///
/// All gameplay outcomes come from the deterministic domain aggregate. The caller supplies an
/// authenticated user identity and retry/revision metadata, never a trusted player identity.
///
/// # Errors
///
/// Returns authorization, compatibility, domain validation, revision, persistence, snapshot, or
/// projection failures without partially committing the command.
#[allow(clippy::too_many_arguments)]
pub async fn submit_game_command(
    db: &dyn Database,
    game_id: GameId,
    user_id: &str,
    command_id: &str,
    idempotency_key: &str,
    expected_revision: u64,
    command: &GameCommand,
    updated_at_ms: i64,
) -> Result<GameState, GameServiceError> {
    let tx = db.begin_transaction().await?;
    let result = submit_in_transaction(
        &*tx,
        game_id,
        user_id,
        command_id,
        idempotency_key,
        expected_revision,
        command,
        updated_at_ms,
    )
    .await;
    match result {
        Ok(state) => {
            tx.commit().await?;
            Ok(state)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn submit_in_transaction(
    tx: &dyn Database,
    game_id: GameId,
    user_id: &str,
    command_id: &str,
    idempotency_key: &str,
    expected_revision: u64,
    command: &GameCommand,
    updated_at_ms: i64,
) -> Result<GameState, GameServiceError> {
    let actor = player_for_user(tx, game_id, user_id).await?;
    let current = crate::recover_game(tx, game_id).await?;
    if current.revision != expected_revision {
        #[cfg(feature = "metrics")]
        crate::observability::record_command_conflict(expected_revision, current.revision);
        return Err(GameServiceError::Conflict {
            expected: expected_revision,
            actual: current.revision,
        });
    }
    let profile = &current.rules;
    let dictionary = dictionary(current.metadata.dictionary())
        .ok_or(GameServiceError::UnsupportedCompatibility)?;
    if profile.dictionary_id != current.metadata.dictionary().id() {
        return Err(GameServiceError::UnsupportedCompatibility);
    }

    let result = decide_command(&current, actor, command, profile, dictionary)?;
    crate::append_events(
        tx,
        game_id,
        command_id,
        idempotency_key,
        expected_revision,
        &result.events,
    )
    .await?;
    let state = result
        .events
        .iter()
        .try_fold(current, |state, event| apply_event(Some(state), event))?;
    crate::store_snapshot(tx, game_id, &state, updated_at_ms).await?;
    let canonical_events = crate::load_events(tx, game_id, 0)
        .await?
        .into_iter()
        .map(|event| event.event)
        .collect::<Vec<_>>();
    crate::rebuild_game_projections(tx, &state, &canonical_events, updated_at_ms).await?;
    Ok(state)
}

/// Gameplay application-service failure.
#[derive(Debug, Error)]
pub enum GameServiceError {
    #[error("the authenticated user is not a member of this game")]
    Unauthorized,
    #[error("persisted player identity is malformed")]
    MalformedIdentity,
    #[error("unsupported persisted rules or dictionary version")]
    UnsupportedCompatibility,
    #[error("stale command revision: expected {expected}, actual {actual}")]
    Conflict { expected: u64, actual: u64 },
    #[error(transparent)]
    Domain(#[from] wwmtf_game_domain::GameError),
    #[error(transparent)]
    Replay(#[from] wwmtf_game_domain::ReplayError),
    #[error(transparent)]
    Journal(#[from] crate::JournalError),
    #[error(transparent)]
    Projection(#[from] crate::ProjectionError),
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use time::OffsetDateTime;

    use super::*;
    use crate::{accept_challenge, create_challenge, migrate_app, register};

    #[test]
    fn authenticated_member_commands_are_authoritative_and_atomic() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens");
            migrate_app(&*db).await.expect("migrations run");
            let now = OffsetDateTime::UNIX_EPOCH;
            let alice = register(&*db, "alice", "correct horse battery staple", now)
                .await
                .expect("Alice registers");
            let bob = register(&*db, "bob", "another correct horse battery", now)
                .await
                .expect("Bob registers");
            let mallory = register(&*db, "mallory", "a third correct password", now)
                .await
                .expect("Mallory registers");
            let challenge = create_challenge(&*db, &alice, &bob, now)
                .await
                .expect("challenge creates");
            let game_id = accept_challenge(&*db, &challenge, &bob, now, 9)
                .await
                .expect("game starts");
            let state = crate::recover_game(&*db, game_id)
                .await
                .expect("game loads");

            assert!(matches!(
                submit_game_command(
                    &*db,
                    game_id,
                    &mallory,
                    "forged",
                    "forged-idem",
                    state.revision,
                    &GameCommand::Pass,
                    0,
                )
                .await,
                Err(GameServiceError::Unauthorized)
            ));
            let updated = submit_game_command(
                &*db,
                game_id,
                &alice,
                "pass-1",
                "pass-idem-1",
                state.revision,
                &GameCommand::Pass,
                0,
            )
            .await
            .expect("active member passes");
            assert_eq!(updated.revision, state.revision + 1);
            assert!(matches!(
                submit_game_command(
                    &*db,
                    game_id,
                    &bob,
                    "stale",
                    "stale-idem",
                    state.revision,
                    &GameCommand::Pass,
                    0,
                )
                .await,
                Err(GameServiceError::Conflict { .. })
            ));
            assert_eq!(
                crate::recover_game(&*db, game_id)
                    .await
                    .expect("only accepted command persists"),
                updated
            );
        });
    }
}
