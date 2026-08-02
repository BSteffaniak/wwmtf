//! Renderer-neutral authenticated presentation orchestration.

use std::str::FromStr as _;

use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;
use time::OffsetDateTime;
use words_with_spouses_game_domain::{
    GameError, GameId, GameStatus, Placement, PlacementGuidance, PlayAnalysis, analyze_play,
    dictionary, placement_guidance,
};

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
    pub rack_order: Vec<u16>,
    pub exchange_available: bool,
    pub viewer_username: String,
    pub opponent_username: String,
    pub latest_action: Option<String>,
    pub latest_play_coordinates:
        std::collections::BTreeSet<words_with_spouses_game_domain::Coordinate>,
    pub viewer_play_coordinates:
        std::collections::BTreeSet<words_with_spouses_game_domain::Coordinate>,
    pub completion_reason: Option<String>,
    state: words_with_spouses_game_domain::GameState,
    dictionary: words_with_spouses_game_domain::WordSetDictionary,
    pub completed: bool,
}

impl AuthorizedGamePage {
    /// Analyzes a candidate play against this authorized viewer's canonical game state.
    ///
    /// # Errors
    ///
    /// * Returns deterministic gameplay validation failures for the candidate placement.
    pub fn analyze_play(&self, placements: &[Placement]) -> Result<PlayAnalysis, GameError> {
        analyze_play(
            &self.state,
            self.viewer_player,
            placements,
            &self.rules,
            &self.dictionary,
        )
    }

    /// Returns safe structural guidance for a partial candidate placement.
    ///
    /// # Errors
    ///
    /// * Returns deterministic actor, rack, blank, or coordinate validation failures.
    pub fn placement_guidance(
        &self,
        placements: &[Placement],
    ) -> Result<PlacementGuidance, GameError> {
        placement_guidance(&self.state, self.viewer_player, placements, &self.rules)
    }
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
    let dictionary =
        dictionary(state.metadata.dictionary()).ok_or(PresentationError::UnsupportedDictionary)?;
    let view = game_view(&state, player).ok_or(PresentationError::Forbidden)?;
    let events = load_events(db, game_id, 0)
        .await?
        .into_iter()
        .map(|event| event.event)
        .collect::<Vec<_>>();
    let viewer_username = username_for_user(db, &user_id).await?;
    let opponent_user_id = db
        .select("game_players")
        .where_eq("game_id", game_id.to_string())
        .execute(db)
        .await?
        .iter()
        .filter_map(|row| row.get("user_id"))
        .filter_map(|value| value.as_str().map(ToOwned::to_owned))
        .find(|candidate| candidate != &user_id)
        .ok_or(PresentationError::Malformed)?;
    let opponent_username = username_for_user(db, &opponent_user_id).await?;
    let name = |event_player| {
        if event_player == player {
            viewer_username.clone()
        } else {
            opponent_username.clone()
        }
    };
    let history = move_history_view(&events, &rules, name)?;
    let final_score_adjustments = final_score_adjustments(&events)?;
    let (latest_action, latest_play_coordinates) =
        latest_public_action(&events, &viewer_username, &opponent_username, player);
    let viewer_play_coordinates = player_play_coordinates(&events, player);
    let completion_reason = completion_reason(&events, rules.scoreless_turn_limit);
    let rack_order = crate::load_rack_order(db, game_id, &user_id).await?;
    let exchange_available = state.bag.len() >= usize::from(rules.minimum_tiles_for_exchange);
    let completed = view.status == GameStatus::Completed;
    Ok(AuthorizedGamePage {
        user_id,
        viewer_player: player,
        game_id,
        view,
        rules,
        history,
        final_score_adjustments,
        rack_order,
        exchange_available,
        viewer_username,
        opponent_username,
        latest_action,
        latest_play_coordinates,
        viewer_play_coordinates,
        completion_reason,
        state,
        dictionary,
        completed,
    })
}

async fn username_for_user(db: &dyn Database, user_id: &str) -> Result<String, PresentationError> {
    db.select("users")
        .where_eq("user_id", user_id)
        .execute(db)
        .await?
        .first()
        .and_then(|row| row.get("username_display"))
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(PresentationError::Malformed)
}

fn player_play_coordinates(
    events: &[words_with_spouses_game_domain::GameEvent],
    player: words_with_spouses_game_domain::PlayerId,
) -> std::collections::BTreeSet<words_with_spouses_game_domain::Coordinate> {
    events
        .iter()
        .filter_map(|event| match event {
            words_with_spouses_game_domain::GameEvent::TilesPlayed {
                player_id,
                placements,
                ..
            } if *player_id == player => Some(placements.keys().copied()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn latest_public_action(
    events: &[words_with_spouses_game_domain::GameEvent],
    viewer_username: &str,
    opponent_username: &str,
    viewer: words_with_spouses_game_domain::PlayerId,
) -> (
    Option<String>,
    std::collections::BTreeSet<words_with_spouses_game_domain::Coordinate>,
) {
    use words_with_spouses_game_domain::GameEvent;

    let name = |player| {
        if player == viewer {
            viewer_username
        } else {
            opponent_username
        }
    };
    for event in events.iter().rev() {
        match event {
            GameEvent::TilesPlayed {
                player_id,
                placements,
                score,
                ..
            } => {
                return (
                    Some(format!("{} played for {score} points.", name(*player_id))),
                    placements.keys().copied().collect(),
                );
            }
            GameEvent::TilesExchanged {
                player_id,
                returned,
                ..
            } => {
                return (
                    Some(format!(
                        "{} exchanged {} tile(s).",
                        name(*player_id),
                        returned.len()
                    )),
                    std::collections::BTreeSet::new(),
                );
            }
            GameEvent::TurnPassed { player_id } => {
                return (
                    Some(format!("{} passed.", name(*player_id))),
                    std::collections::BTreeSet::new(),
                );
            }
            GameEvent::GameResigned { player_id, .. } => {
                return (
                    Some(format!("{} resigned.", name(*player_id))),
                    std::collections::BTreeSet::new(),
                );
            }
            GameEvent::GameCompleted { .. } | GameEvent::GameStarted { .. } => {}
        }
    }
    (None, std::collections::BTreeSet::new())
}

fn completion_reason(
    events: &[words_with_spouses_game_domain::GameEvent],
    scoreless_turn_limit: u8,
) -> Option<String> {
    use words_with_spouses_game_domain::GameEvent;

    if events
        .iter()
        .any(|event| matches!(event, GameEvent::GameResigned { .. }))
    {
        return Some("Resignation".to_string());
    }
    let completion_index = events
        .iter()
        .rposition(|event| matches!(event, GameEvent::GameCompleted { .. }))?;
    let scoreless_turns = events[..completion_index]
        .iter()
        .rev()
        .take_while(|event| {
            matches!(
                event,
                GameEvent::TurnPassed { .. } | GameEvent::TilesExchanged { .. }
            ) || matches!(event, GameEvent::TilesPlayed { score: 0, .. })
        })
        .count();
    if scoreless_turns >= usize::from(scoreless_turn_limit) {
        Some("Scoreless-turn limit".to_string())
    } else {
        Some("A player emptied their rack".to_string())
    }
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
    #[error("this game uses an unsupported dictionary")]
    UnsupportedDictionary,
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
    MoveHistory(#[from] crate::MoveHistoryError),
    #[error(transparent)]
    RackPreference(#[from] crate::RackPreferenceError),
    #[error(transparent)]
    Replay(#[from] words_with_spouses_game_domain::ReplayError),
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use time::Duration;

    use words_with_spouses_game_domain::{BoardTile, GameEvent, Tile, TileFace, TileId};

    use super::*;
    use crate::{accept_challenge, create_challenge, create_session, migrate_app, register};

    #[test]
    fn player_play_coordinates_attribute_only_that_players_tiles() {
        let viewer = words_with_spouses_game_domain::PlayerId::new();
        let opponent = words_with_spouses_game_domain::PlayerId::new();
        let viewer_coordinate = words_with_spouses_game_domain::Coordinate::new(7, 7);
        let opponent_coordinate = words_with_spouses_game_domain::Coordinate::new(8, 7);
        let board_tile = |id, letter| BoardTile {
            tile: Tile {
                id: TileId::new(id),
                face: TileFace::Letter(letter),
                points: 1,
            },
            letter,
        };
        let events = vec![
            GameEvent::TilesPlayed {
                player_id: viewer,
                placements: std::collections::BTreeMap::from([(
                    viewer_coordinate,
                    board_tile(1, 'A'),
                )]),
                score: 1,
                drawn: Vec::new(),
            },
            GameEvent::TilesPlayed {
                player_id: opponent,
                placements: std::collections::BTreeMap::from([(
                    opponent_coordinate,
                    board_tile(2, 'B'),
                )]),
                score: 1,
                drawn: Vec::new(),
            },
        ];

        assert_eq!(
            player_play_coordinates(&events, viewer),
            std::collections::BTreeSet::from([viewer_coordinate])
        );
    }

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
