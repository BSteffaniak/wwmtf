//! Renderer-neutral authenticated presentation orchestration.

use std::str::FromStr as _;

use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;
use time::OffsetDateTime;
use words_with_spouses_game_domain::{GameId, GameStatus};

use crate::{
    DashboardProjection, GameView, MoveHistoryView, UserScoreTotals, dashboard_projection,
    final_score_adjustments, game_view, load_events, move_history_view, player_for_user,
    recover_game, resolve_session, user_score_totals,
};

/// Cookie name used by renderer-neutral routes and the HTML transport security adapter.
pub const SESSION_COOKIE_NAME: &str = "words-with-spouses-session";
/// Cookie name used by `HyperChad`'s renderer-owned CSRF client.
pub const CSRF_COOKIE_NAME: &str = "words-with-spouses-csrf";
/// Header name used by `HyperChad`'s renderer-owned CSRF client.
pub const CSRF_HEADER_NAME: &str = "x-hyperchad-csrf-token";

/// Signed-in dashboard presentation data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedDashboard {
    pub user_id: String,
    pub username: String,
    pub projection: DashboardProjection,
    pub score_totals: Option<UserScoreTotals>,
}

/// Authorized game presentation reconstructed from canonical persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedGamePage {
    pub user_id: String,
    pub viewer_player: words_with_spouses_game_domain::PlayerId,
    pub game_id: GameId,
    pub view: GameView,
    pub rules: words_with_spouses_game_domain::RuleProfile,
    pub history: Vec<MoveHistoryView>,
    pub final_score_adjustments:
        std::collections::BTreeMap<words_with_spouses_game_domain::PlayerId, i64>,
    pub completed: bool,
}

/// Resolves the durable opaque session cookie to a user.
///
/// # Errors
///
/// * Returns [`PresentationError::Unauthenticated`] for missing, expired, or revoked sessions.
/// * Returns persistence failures without exposing credential values.
pub async fn authenticated_user(
    db: &dyn Database,
    cookies: &std::collections::BTreeMap<String, String>,
    now: OffsetDateTime,
) -> Result<String, PresentationError> {
    let session = cookies.get(SESSION_COOKIE_NAME).ok_or_else(|| {
        crate::observability::record_authentication_failure("missing_session");
        PresentationError::Unauthenticated
    })?;
    resolve_session(db, session, now)
        .await
        .map_err(|error| match error {
            crate::SessionError::Invalid | crate::SessionError::Timestamp => {
                crate::observability::record_authentication_failure("invalid_session");
                PresentationError::Unauthenticated
            }
            crate::SessionError::Busy => {
                crate::observability::record_database_failure("resolve_session_busy");
                PresentationError::Database(switchy_database::DatabaseError::UnexpectedResult)
            }
            crate::SessionError::Database(error) => {
                crate::observability::record_database_failure("resolve_session");
                PresentationError::Database(error)
            }
        })
}

/// Loads the complete dashboard model for an authenticated user.
///
/// # Errors
///
/// * Returns authentication, projection, or persistence failures.
pub async fn load_authenticated_dashboard(
    db: &dyn Database,
    cookies: &std::collections::BTreeMap<String, String>,
    now: OffsetDateTime,
) -> Result<AuthenticatedDashboard, PresentationError> {
    let user_id = authenticated_user(db, cookies, now).await?;
    let username = db
        .select("users")
        .where_eq("user_id", user_id.clone())
        .execute(db)
        .await?
        .first()
        .and_then(|row| row.get("username_display"))
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(PresentationError::Malformed)?;
    Ok(AuthenticatedDashboard {
        projection: dashboard_projection(db, &user_id).await?,
        score_totals: user_score_totals(db, &user_id).await?,
        user_id,
        username,
    })
}

/// Loads an authorized game view and chronological canonical history.
///
/// # Errors
///
/// * Returns authentication, malformed route identity, membership, replay, or persistence errors.
pub async fn load_authorized_game_page(
    db: &dyn Database,
    cookies: &std::collections::BTreeMap<String, String>,
    game_id: &str,
    now: OffsetDateTime,
) -> Result<AuthorizedGamePage, PresentationError> {
    let user_id = authenticated_user(db, cookies, now).await?;
    let game_id = GameId::from_str(game_id).map_err(|_| PresentationError::UnknownGame)?;
    let player = player_for_user(db, game_id, &user_id)
        .await
        .map_err(|error| match error {
            crate::GameServiceError::Unauthorized | crate::GameServiceError::MalformedIdentity => {
                PresentationError::Forbidden
            }
            crate::GameServiceError::Database(error) => PresentationError::Database(error),
            _ => PresentationError::Game(error),
        })?;
    let state = recover_game(db, game_id).await?;
    let rules = words_with_spouses_game_domain::rule_profile(state.metadata.rules())
        .ok_or(PresentationError::UnsupportedRules)?;
    let view = game_view(&state, player).ok_or(PresentationError::Forbidden)?;
    let events = load_events(db, game_id, 0)
        .await?
        .into_iter()
        .map(|event| event.event)
        .collect::<Vec<_>>();
    let history = move_history_view(&events)?;
    let final_score_adjustments = final_score_adjustments(&events)?;
    Ok(AuthorizedGamePage {
        user_id,
        viewer_player: player,
        game_id,
        view,
        rules,
        history,
        final_score_adjustments,
        completed: state.status == GameStatus::Completed,
    })
}

/// Authenticated presentation failure mapped to recoverable product states by routes.
#[derive(Debug, Error)]
pub enum PresentationError {
    #[error("sign in to continue")]
    Unauthenticated,
    #[error("stored presentation data is malformed")]
    Malformed,
    #[error("this game uses an unsupported rules profile")]
    UnsupportedRules,
    #[error("this game is unavailable")]
    UnknownGame,
    #[error("you are not authorized to view this game")]
    Forbidden,
    #[error(transparent)]
    Game(crate::GameServiceError),
    #[error(transparent)]
    Journal(#[from] crate::JournalError),
    #[error(transparent)]
    Projection(#[from] crate::ProjectionError),
    #[error(transparent)]
    Replay(#[from] words_with_spouses_game_domain::ReplayError),
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use time::Duration;

    use super::*;
    use crate::{accept_challenge, create_challenge, create_session, migrate_app, register};

    #[test]
    fn routes_load_only_authenticated_member_presentations() {
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
            let game_id = accept_challenge(&*db, &challenge, &bob, now, 7)
                .await
                .expect("game starts");
            let alice_session = create_session(&*db, &alice, now, Duration::days(1))
                .await
                .expect("Alice session creates");
            let mallory_session = create_session(&*db, &mallory, now, Duration::days(1))
                .await
                .expect("Mallory session creates");
            let alice_cookies = std::collections::BTreeMap::from([(
                SESSION_COOKIE_NAME.to_string(),
                alice_session.expose().to_string(),
            )]);
            let mallory_cookies = std::collections::BTreeMap::from([(
                SESSION_COOKIE_NAME.to_string(),
                mallory_session.expose().to_string(),
            )]);

            let dashboard = load_authenticated_dashboard(&*db, &alice_cookies, now)
                .await
                .expect("Alice dashboard loads");
            assert_eq!(dashboard.user_id, alice);
            assert_eq!(dashboard.projection.games.len(), 1);

            let page = load_authorized_game_page(&*db, &alice_cookies, &game_id.to_string(), now)
                .await
                .expect("Alice game loads");
            assert_eq!(page.game_id, game_id);
            assert_eq!(page.view.rack.len(), 7);
            assert_eq!(page.history.len(), 1);

            assert!(matches!(
                load_authorized_game_page(&*db, &mallory_cookies, &game_id.to_string(), now).await,
                Err(PresentationError::Forbidden)
            ));
            assert!(matches!(
                load_authenticated_dashboard(&*db, &std::collections::BTreeMap::new(), now).await,
                Err(PresentationError::Unauthenticated)
            ));
        });
    }
}
