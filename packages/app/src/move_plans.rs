//! Private, non-authoritative move-planning persistence.

use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;
use wwmtf_game_domain::GameId;

/// Loads an authorized player's opaque private planning payload and its source revision.
///
/// # Errors
///
/// Returns membership or database failures.
pub async fn load_move_plan(
    db: &dyn Database,
    game_id: GameId,
    user_id: &str,
) -> Result<Option<(String, u64)>, MovePlanError> {
    crate::player_for_user(db, game_id, user_id).await?;
    let row = db
        .select("move_plans")
        .where_eq("move_plan_id", plan_id(game_id, user_id))
        .execute(db)
        .await?
        .into_iter()
        .next();
    let Some(row) = row else {
        return Ok(None);
    };
    let payload = row
        .get("payload")
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(MovePlanError::Malformed)?;
    let revision = row
        .get("board_revision")
        .as_ref()
        .and_then(switchy_database::DatabaseValue::as_u64)
        .ok_or(MovePlanError::Malformed)?;
    Ok(Some((payload, revision)))
}

/// Saves an authorized player's opaque private planning payload.
///
/// # Errors
///
/// Returns membership or database failures.
pub async fn save_move_plan(
    db: &dyn Database,
    game_id: GameId,
    user_id: &str,
    payload: &str,
    board_revision: u64,
    updated_at_ms: i64,
) -> Result<(), MovePlanError> {
    crate::player_for_user(db, game_id, user_id).await?;
    db.upsert("move_plans")
        .where_eq("move_plan_id", plan_id(game_id, user_id))
        .value("move_plan_id", plan_id(game_id, user_id))
        .value("game_id", game_id.to_string())
        .value("user_id", user_id)
        .value("payload", payload)
        .value("board_revision", board_revision)
        .value("updated_at_ms", updated_at_ms)
        .execute(db)
        .await?;
    Ok(())
}

/// Clears an authorized player's private plan.
///
/// # Errors
///
/// Returns membership or database failures.
pub async fn clear_move_plan(
    db: &dyn Database,
    game_id: GameId,
    user_id: &str,
) -> Result<(), MovePlanError> {
    crate::player_for_user(db, game_id, user_id).await?;
    db.delete("move_plans")
        .where_eq("move_plan_id", plan_id(game_id, user_id))
        .execute(db)
        .await?;
    Ok(())
}

fn plan_id(game_id: GameId, user_id: &str) -> String {
    format!("{game_id}:{user_id}")
}

/// Private move-plan persistence failure.
#[derive(Debug, Error)]
pub enum MovePlanError {
    #[error(transparent)]
    Game(#[from] crate::GameServiceError),
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
    #[error("stored move plan is malformed")]
    Malformed,
}
