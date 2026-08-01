//! Rebuildable game summary and move-history projections.

use serde::{Deserialize, Serialize};
use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;
use words_with_spouses_game_domain::{GameEvent, GameId, GameState, GameStatus};

/// Rebuilds projection rows from canonical aggregate state and journal events.
///
/// The operation is idempotent: prior rows are removed and regenerated inside the caller's
/// transaction, then the checkpoint advances to the canonical revision.
///
/// # Errors
///
/// * Returns [`ProjectionError::Database`] when a `switchy` builder operation fails.
/// * Returns [`ProjectionError::Revision`] for unsupported revisions.
pub async fn rebuild_game_projections(
    tx: &dyn Database,
    state: &GameState,
    events: &[GameEvent],
    updated_at_ms: i64,
) -> Result<(), ProjectionError> {
    let game_id = state.metadata.id();
    let game_id_string = game_id.to_string();
    tx.delete("move_history")
        .where_eq("game_id", game_id_string.clone())
        .execute(tx)
        .await?;
    let first_revision = state
        .revision
        .checked_sub(u64::try_from(events.len()).map_err(|_| ProjectionError::Revision)?)
        .ok_or(ProjectionError::Revision)?;
    for (index, event) in events.iter().enumerate() {
        let revision = first_revision
            .checked_add(u64::try_from(index).map_err(|_| ProjectionError::Revision)? + 1)
            .ok_or(ProjectionError::Revision)?;
        let (player, kind, score_delta) = history_fields(event);
        let player_user_id = match player {
            Some(player) => user_for_player(tx, &game_id_string, player).await?,
            None => None,
        };
        tx.insert("move_history")
            .value("move_id", format!("{game_id_string}:{revision}"))
            .value("game_id", game_id_string.clone())
            .value(
                "revision",
                i64::try_from(revision).map_err(|_| ProjectionError::Revision)?,
            )
            .value("player_user_id", player_user_id)
            .value("event_kind", kind)
            .value("score_delta", i64::from(score_delta))
            .value("created_at_ms", updated_at_ms)
            .execute(tx)
            .await?;
    }

    let status = match state.status {
        GameStatus::Active => "ACTIVE",
        GameStatus::Completed => "COMPLETED",
    };
    tx.upsert("game_summaries")
        .where_eq("game_id", game_id_string.clone())
        .value("game_id", game_id_string.clone())
        .value("status", status)
        .value(
            "active_player_user_id",
            user_for_player(tx, &game_id_string, state.active_player).await?,
        )
        .value(
            "canonical_revision",
            i64::try_from(state.revision).map_err(|_| ProjectionError::Revision)?,
        )
        .value("last_score", last_score(events).map(i64::from))
        .value(
            "winner_user_id",
            match state.winner {
                Some(winner) => user_for_player(tx, &game_id_string, winner).await?,
                None => None,
            },
        )
        .value("updated_at_ms", updated_at_ms)
        .execute(tx)
        .await?;
    rebuild_score_projections(tx, state, updated_at_ms).await?;
    crate::observability::record_projection_rebuild(state.revision);
    tx.upsert("projection_checkpoints")
        .where_eq("projection_id", format!("game-summary:{game_id_string}"))
        .value("projection_id", format!("game-summary:{game_id_string}"))
        .value("game_id", game_id_string)
        .value(
            "revision",
            i64::try_from(state.revision).map_err(|_| ProjectionError::Revision)?,
        )
        .value("updated_at_ms", updated_at_ms)
        .execute(tx)
        .await?;
    Ok(())
}

async fn rebuild_score_projections(
    tx: &dyn Database,
    state: &GameState,
    updated_at_ms: i64,
) -> Result<(), ProjectionError> {
    let game_id = state.metadata.id().to_string();
    tx.delete("game_scores")
        .where_eq("game_id", game_id.clone())
        .execute(tx)
        .await?;

    if state.status == GameStatus::Completed {
        for player in state.players {
            let user_id = user_for_player(tx, &game_id, player)
                .await?
                .ok_or(ProjectionError::Malformed)?;
            let outcome = match state.winner {
                Some(winner) if winner == player => "WIN",
                Some(_) => "LOSS",
                None => "TIE",
            };
            tx.insert("game_scores")
                .value("game_player_score_id", format!("{game_id}:{user_id}"))
                .value("game_id", game_id.clone())
                .value("user_id", user_id)
                .value("score", i64::from(state.scores[&player]))
                .value("outcome", outcome)
                .value("updated_at_ms", updated_at_ms)
                .execute(tx)
                .await?;
        }
    }

    rebuild_all_user_score_totals(tx, updated_at_ms).await
}

/// Rebuilds every user's aggregate score totals exclusively from per-game completed scores.
///
/// # Errors
///
/// * Returns [`ProjectionError::Database`] when a `switchy` builder operation fails.
/// * Returns [`ProjectionError::Malformed`] when a score row cannot be represented safely.
pub async fn rebuild_all_user_score_totals(
    tx: &dyn Database,
    updated_at_ms: i64,
) -> Result<(), ProjectionError> {
    let rows = tx.select("game_scores").execute(tx).await?;
    let mut totals = std::collections::BTreeMap::<String, UserScoreTotals>::new();
    for row in rows {
        let user_id = string_column(&row, "user_id")?;
        let score = unsigned_column(&row, "score")?;
        let outcome = string_column(&row, "outcome")?;
        let total = totals.entry(user_id.clone()).or_insert(UserScoreTotals {
            user_id,
            completed_games: 0,
            wins: 0,
            ties: 0,
            total_score: 0,
        });
        total.completed_games = total
            .completed_games
            .checked_add(1)
            .ok_or(ProjectionError::Malformed)?;
        total.total_score = total
            .total_score
            .checked_add(score)
            .ok_or(ProjectionError::Malformed)?;
        match outcome.as_str() {
            "WIN" => {
                total.wins = total
                    .wins
                    .checked_add(1)
                    .ok_or(ProjectionError::Malformed)?;
            }
            "TIE" => {
                total.ties = total
                    .ties
                    .checked_add(1)
                    .ok_or(ProjectionError::Malformed)?;
            }
            "LOSS" => {}
            _ => return Err(ProjectionError::Malformed),
        }
    }

    tx.delete("user_score_totals").execute(tx).await?;
    for total in totals.into_values() {
        tx.insert("user_score_totals")
            .value("user_id", total.user_id)
            .value(
                "completed_games",
                i64::try_from(total.completed_games).map_err(|_| ProjectionError::Malformed)?,
            )
            .value(
                "wins",
                i64::try_from(total.wins).map_err(|_| ProjectionError::Malformed)?,
            )
            .value(
                "ties",
                i64::try_from(total.ties).map_err(|_| ProjectionError::Malformed)?,
            )
            .value(
                "total_score",
                i64::try_from(total.total_score).map_err(|_| ProjectionError::Malformed)?,
            )
            .value("updated_at_ms", updated_at_ms)
            .execute(tx)
            .await?;
    }
    Ok(())
}

const fn history_fields(
    event: &GameEvent,
) -> (
    Option<words_with_spouses_game_domain::PlayerId>,
    &'static str,
    u32,
) {
    match event {
        GameEvent::GameStarted { .. } => (None, "GAME_STARTED", 0),
        GameEvent::TilesPlayed {
            player_id, score, ..
        } => (Some(*player_id), "TILES_PLAYED", *score),
        GameEvent::TilesExchanged { player_id, .. } => (Some(*player_id), "TILES_EXCHANGED", 0),
        GameEvent::TurnPassed { player_id } => (Some(*player_id), "TURN_PASSED", 0),
        GameEvent::GameResigned { player_id, .. } => (Some(*player_id), "GAME_RESIGNED", 0),
        GameEvent::GameCompleted { .. } => (None, "GAME_COMPLETED", 0),
    }
}

fn last_score(events: &[GameEvent]) -> Option<u32> {
    events.iter().rev().find_map(|event| match event {
        GameEvent::TilesPlayed { score, .. } => Some(*score),
        _ => None,
    })
}

async fn user_for_player(
    db: &dyn Database,
    game_id: &str,
    player: words_with_spouses_game_domain::PlayerId,
) -> Result<Option<String>, ProjectionError> {
    let rows = db
        .select("game_players")
        .where_eq("game_id", game_id)
        .where_eq("game_player_id", player.as_uuid().to_string())
        .execute(db)
        .await?;
    Ok(rows.first().and_then(|row| {
        row.get("user_id")
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
    }))
}

/// Pending challenge or invitation displayed on a dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingItem {
    pub id: String,
    pub kind: String,
    pub direction: String,
    pub counterparty_user_id: Option<String>,
    pub counterparty_username: Option<String>,
    pub created_at_ms: i64,
}

/// Complete renderer-neutral dashboard projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardProjection {
    pub pending: Vec<PendingItem>,
    pub games: Vec<GameSummary>,
}

/// Aggregate completed-game statistics derived from canonical game results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserScoreTotals {
    pub user_id: String,
    pub completed_games: u64,
    pub wins: u64,
    pub ties: u64,
    pub total_score: u64,
}

/// One chronological history row derived from a canonical game event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameHistoryEntry {
    pub revision: u64,
    pub player_user_id: Option<String>,
    pub event_kind: String,
    pub score_delta: u32,
    pub created_at_ms: i64,
}

/// Loads aggregate score history for one user.
///
/// # Errors
///
/// * Returns database or malformed projection errors.
pub async fn user_score_totals(
    db: &dyn Database,
    user_id: &str,
) -> Result<Option<UserScoreTotals>, ProjectionError> {
    let rows = db
        .select("user_score_totals")
        .where_eq("user_id", user_id)
        .execute(db)
        .await?;
    rows.first()
        .map(|row| {
            Ok(UserScoreTotals {
                user_id: string_column(row, "user_id")?,
                completed_games: unsigned_column(row, "completed_games")?,
                wins: unsigned_column(row, "wins")?,
                ties: unsigned_column(row, "ties")?,
                total_score: unsigned_column(row, "total_score")?,
            })
        })
        .transpose()
}

/// Loads chronological move and score history for one game.
///
/// # Errors
///
/// * Returns database or malformed projection errors.
pub async fn game_history(
    db: &dyn Database,
    game_id: GameId,
) -> Result<Vec<GameHistoryEntry>, ProjectionError> {
    let rows = db
        .select("move_history")
        .where_eq("game_id", game_id.to_string())
        .sort("revision", switchy_database::query::SortDirection::Asc)
        .execute(db)
        .await?;
    rows.iter()
        .map(|row| {
            Ok(GameHistoryEntry {
                revision: unsigned_column(row, "revision")?,
                player_user_id: optional_string(row, "player_user_id"),
                event_kind: string_column(row, "event_kind")?,
                score_delta: u32::try_from(unsigned_column(row, "score_delta")?)
                    .map_err(|_| ProjectionError::Malformed)?,
                created_at_ms: signed_column(row, "created_at_ms")?,
            })
        })
        .collect()
}

/// Loads pending challenge/invitation state and active/completed games for one user.
///
/// # Errors
///
/// * Returns database or malformed projection errors.
pub async fn dashboard_projection(
    db: &dyn Database,
    user_id: &str,
) -> Result<DashboardProjection, ProjectionError> {
    let mut pending = Vec::new();
    for row in db
        .select("challenges")
        .where_eq("challenger_user_id", user_id)
        .where_eq("status", "PENDING")
        .execute(db)
        .await?
    {
        pending.push(PendingItem {
            id: string_column(&row, "challenge_id")?,
            kind: "CHALLENGE".to_string(),
            direction: "OUTGOING".to_string(),
            counterparty_user_id: Some(string_column(&row, "challenged_user_id")?),
            counterparty_username: username_for_user(
                db,
                &string_column(&row, "challenged_user_id")?,
            )
            .await?,
            created_at_ms: signed_column(&row, "created_at_ms")?,
        });
    }
    for row in db
        .select("challenges")
        .where_eq("challenged_user_id", user_id)
        .where_eq("status", "PENDING")
        .execute(db)
        .await?
    {
        pending.push(PendingItem {
            id: string_column(&row, "challenge_id")?,
            kind: "CHALLENGE".to_string(),
            direction: "INCOMING".to_string(),
            counterparty_user_id: Some(string_column(&row, "challenger_user_id")?),
            counterparty_username: username_for_user(
                db,
                &string_column(&row, "challenger_user_id")?,
            )
            .await?,
            created_at_ms: signed_column(&row, "created_at_ms")?,
        });
    }
    for row in db
        .select("invitations")
        .where_eq("creator_user_id", user_id)
        .where_eq("status", "ACTIVE")
        .execute(db)
        .await?
    {
        pending.push(PendingItem {
            id: string_column(&row, "invitation_id")?,
            kind: "INVITATION".to_string(),
            direction: "OUTGOING".to_string(),
            counterparty_user_id: None,
            counterparty_username: None,
            created_at_ms: signed_column(&row, "created_at_ms")?,
        });
    }
    pending.sort_by(|left, right| {
        right
            .created_at_ms
            .cmp(&left.created_at_ms)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(DashboardProjection {
        pending,
        games: user_game_summaries(db, user_id).await?,
    })
}

/// Projected game lifecycle for a user's dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameSummary {
    pub game_id: String,
    pub status: String,
    pub active_player_user_id: Option<String>,
    pub canonical_revision: u64,
    pub last_score: Option<u32>,
    pub winner_user_id: Option<String>,
    pub updated_at_ms: i64,
}

/// Loads one user's active/completed games ordered by most recent activity.
///
/// # Errors
///
/// * Returns database or malformed projection errors.
pub async fn user_game_summaries(
    db: &dyn Database,
    user_id: &str,
) -> Result<Vec<GameSummary>, ProjectionError> {
    let memberships = db
        .select("game_players")
        .where_eq("user_id", user_id)
        .execute(db)
        .await?;
    let mut summaries = Vec::new();
    for membership in memberships {
        let game_id = string_column(&membership, "game_id")?;
        let rows = db
            .select("game_summaries")
            .where_eq("game_id", game_id)
            .execute(db)
            .await?;
        if let Some(row) = rows.first() {
            summaries.push(GameSummary {
                game_id: string_column(row, "game_id")?,
                status: string_column(row, "status")?,
                active_player_user_id: optional_string(row, "active_player_user_id"),
                canonical_revision: unsigned_column(row, "canonical_revision")?,
                last_score: optional_unsigned(row, "last_score")?,
                winner_user_id: optional_string(row, "winner_user_id"),
                updated_at_ms: signed_column(row, "updated_at_ms")?,
            });
        }
    }
    summaries.sort_by(|left, right| {
        let left_actionable =
            left.status == "ACTIVE" && left.active_player_user_id.as_deref() == Some(user_id);
        let right_actionable =
            right.status == "ACTIVE" && right.active_player_user_id.as_deref() == Some(user_id);
        right_actionable
            .cmp(&left_actionable)
            .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms))
            .then_with(|| left.game_id.cmp(&right.game_id))
    });
    Ok(summaries)
}

async fn username_for_user(
    db: &dyn Database,
    user_id: &str,
) -> Result<Option<String>, ProjectionError> {
    Ok(db
        .select("users")
        .where_eq("user_id", user_id)
        .execute(db)
        .await?
        .first()
        .and_then(|row| optional_string(row, "username_display")))
}

fn string_column(row: &switchy_database::Row, name: &str) -> Result<String, ProjectionError> {
    row.get(name)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(ProjectionError::Malformed)
}

fn optional_string(row: &switchy_database::Row, name: &str) -> Option<String> {
    row.get(name)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
}

fn signed_column(row: &switchy_database::Row, name: &str) -> Result<i64, ProjectionError> {
    row.get(name)
        .and_then(|value| value.as_i64())
        .ok_or(ProjectionError::Malformed)
}

fn unsigned_column(row: &switchy_database::Row, name: &str) -> Result<u64, ProjectionError> {
    signed_column(row, name)
        .and_then(|value| u64::try_from(value).map_err(|_| ProjectionError::Malformed))
}

fn optional_unsigned(
    row: &switchy_database::Row,
    name: &str,
) -> Result<Option<u32>, ProjectionError> {
    row.get(name)
        .and_then(|value| value.as_i64())
        .map(|value| u32::try_from(value).map_err(|_| ProjectionError::Malformed))
        .transpose()
}

/// Projection rebuild failure.
#[derive(Debug, Error)]
pub enum ProjectionError {
    #[error("projection revision is invalid")]
    Revision,
    #[error("projection row is malformed")]
    Malformed,
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
}

/// Returns the projected revision for one game.
///
/// # Errors
///
/// * Returns [`ProjectionError::Database`] when querying fails.
/// * Returns [`ProjectionError::Revision`] for malformed persisted revisions.
pub async fn projected_revision(
    db: &dyn Database,
    game_id: GameId,
) -> Result<Option<u64>, ProjectionError> {
    let rows = db
        .select("projection_checkpoints")
        .where_eq("projection_id", format!("game-summary:{game_id}"))
        .execute(db)
        .await?;
    rows.first()
        .map(|row| {
            row.get("revision")
                .and_then(|value| value.as_i64())
                .and_then(|value| u64::try_from(value).ok())
                .ok_or(ProjectionError::Revision)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use time::OffsetDateTime;
    use words_with_spouses_game_domain::{
        DictionaryRef, GameMetadata, GameStatus, PlayerId, RuleProfileRef, initial_rule_profile,
        initialize_game, replay,
    };

    use super::*;
    use crate::{create_challenge, create_invitation, migrate_app, register};

    #[test]
    fn dashboard_combines_pending_items_with_ordered_games() {
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
            create_challenge(&*db, &bob, &alice, OffsetDateTime::UNIX_EPOCH)
                .await
                .expect("challenge creates");
            create_invitation(
                &*db,
                &alice,
                OffsetDateTime::UNIX_EPOCH,
                time::Duration::days(1),
            )
            .await
            .expect("invitation creates");

            let dashboard = dashboard_projection(&*db, &alice)
                .await
                .expect("dashboard loads");
            assert_eq!(dashboard.pending.len(), 2);
            assert!(
                dashboard
                    .pending
                    .iter()
                    .any(|item| { item.kind == "CHALLENGE" && item.direction == "INCOMING" })
            );
            assert!(
                dashboard
                    .pending
                    .iter()
                    .any(|item| item.kind == "INVITATION")
            );
        });
    }

    #[test]
    fn completed_game_score_totals_rebuild_idempotently() {
        block_on(async {
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
                DictionaryRef::new("enable1-en", 1, "sha256:test").expect("dictionary reference"),
                OffsetDateTime::UNIX_EPOCH,
            );
            let started =
                initialize_game(metadata, players, players[0], &initial_rule_profile(), 2)
                    .expect("game starts");
            let mut state = replay([&started]).expect("start replays");
            state.status = GameStatus::Completed;
            state.winner = Some(players[0]);
            state.scores.insert(players[0], 120);
            state.scores.insert(players[1], 85);
            for (seat, player) in players.into_iter().enumerate() {
                db.insert("game_players")
                    .value("game_player_id", player.as_uuid().to_string())
                    .value("game_id", game_id.to_string())
                    .value("user_id", format!("user-{seat}"))
                    .value("seat", i64::try_from(seat).expect("seat fits"))
                    .execute(&*db)
                    .await
                    .expect("membership inserts");
            }

            for _ in 0..2 {
                let tx = db.begin_transaction().await.expect("transaction begins");
                rebuild_game_projections(&*tx, &state, std::slice::from_ref(&started), 0)
                    .await
                    .expect("projection rebuilds");
                tx.commit().await.expect("transaction commits");
            }

            assert_eq!(
                user_score_totals(&*db, "user-0")
                    .await
                    .expect("totals load"),
                Some(UserScoreTotals {
                    user_id: "user-0".to_string(),
                    completed_games: 1,
                    wins: 1,
                    ties: 0,
                    total_score: 120,
                })
            );
            assert_eq!(
                user_score_totals(&*db, "user-1")
                    .await
                    .expect("totals load")
                    .expect("loser totals exist")
                    .total_score,
                85
            );
            assert_eq!(
                db.select("game_scores")
                    .where_eq("game_id", game_id.to_string())
                    .execute(&*db)
                    .await
                    .expect("scores load")
                    .len(),
                2
            );
        });
    }

    #[test]
    fn projection_rebuild_is_idempotent_on_turso() {
        block_on(async {
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
                DictionaryRef::new("enable1-en", 1, "sha256:test").expect("dictionary reference"),
                OffsetDateTime::UNIX_EPOCH,
            );
            let started =
                initialize_game(metadata, players, players[0], &initial_rule_profile(), 2)
                    .expect("game starts");
            let events = vec![
                started,
                GameEvent::TurnPassed {
                    player_id: players[0],
                },
            ];
            let state = replay(&events).expect("events replay");
            for (seat, player) in players.into_iter().enumerate() {
                db.insert("game_players")
                    .value("game_player_id", player.as_uuid().to_string())
                    .value("game_id", game_id.to_string())
                    .value("user_id", format!("user-{seat}"))
                    .value("seat", i64::try_from(seat).expect("seat fits"))
                    .execute(&*db)
                    .await
                    .expect("membership inserts");
            }

            for _ in 0..2 {
                let tx = db.begin_transaction().await.expect("transaction begins");
                rebuild_game_projections(&*tx, &state, &events, 0)
                    .await
                    .expect("projection rebuilds");
                tx.commit().await.expect("transaction commits");
            }
            assert_eq!(
                projected_revision(&*db, game_id)
                    .await
                    .expect("revision loads"),
                Some(2)
            );
            assert_eq!(
                db.select("move_history")
                    .where_eq("game_id", game_id.to_string())
                    .execute(&*db)
                    .await
                    .expect("history loads")
                    .len(),
                2
            );
            let summaries = user_game_summaries(&*db, "user-1")
                .await
                .expect("summaries load");
            assert_eq!(summaries.len(), 1);
            assert_eq!(summaries[0].game_id, game_id.to_string());
            assert_eq!(summaries[0].canonical_revision, 2);
            assert_eq!(
                summaries[0].active_player_user_id.as_deref(),
                Some("user-1")
            );
        });
    }
}
