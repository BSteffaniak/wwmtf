//! Renderer-neutral routed product pages backed by durable presentation services.

use std::sync::Arc;

use hyperchad::{
    renderer::View,
    router::{Container, RoutePath, RouteRequest, Router},
    template::container,
};
use switchy_database::Database;
use time::OffsetDateTime;

use crate::{
    AuthenticatedDashboard, AuthorizedGamePage, PresentationError, UserScoreTotals,
    board_component, error_component, load_authenticated_dashboard, load_authorized_game_page,
    move_history_component, rack_component, status_component,
};

/// Builds the database-backed renderer-neutral application router.
#[must_use]
pub fn create_product_router(database: Arc<dyn Database>) -> Router {
    let router = Router::new();
    let dashboard_database = database.clone();
    router.add_route_result("/", move |request: RouteRequest| {
        let database = dashboard_database.clone();
        async move {
            Ok(dashboard_route(&*database, &request, OffsetDateTime::now_utc()).await)
                as Result<Container, Box<dyn std::error::Error>>
        }
    });
    router.add_route_result(
        RoutePath::LiteralPrefix("/games/".to_string()),
        move |request: RouteRequest| {
            let database = database.clone();
            async move {
                Ok(game_route(&*database, &request, OffsetDateTime::now_utc()).await)
                    as Result<Container, Box<dyn std::error::Error>>
            }
        },
    );
    router
}

/// Loads and renders the signed-in dashboard, or a recoverable authentication page.
pub async fn dashboard_route(
    database: &dyn Database,
    request: &RouteRequest,
    now: OffsetDateTime,
) -> Container {
    match load_authenticated_dashboard(database, &request.cookies, now).await {
        Ok(dashboard) => dashboard_page(&dashboard),
        Err(PresentationError::Unauthenticated) => signed_out_page(),
        Err(error) => product_error_page("Dashboard unavailable", &error.to_string()),
    }
}

/// Loads and renders one stable authorized game route.
pub async fn game_route(
    database: &dyn Database,
    request: &RouteRequest,
    now: OffsetDateTime,
) -> Container {
    let game_id = request.path.strip_prefix("/games/").unwrap_or_default();
    match load_authorized_game_page(database, &request.cookies, game_id, now).await {
        Ok(game) => game_page(&game),
        Err(PresentationError::Unauthenticated) => signed_out_page(),
        Err(error @ (PresentationError::Forbidden | PresentationError::UnknownGame)) => {
            product_error_page("Game unavailable", &error.to_string())
        }
        Err(error) => product_error_page("Unable to load game", &error.to_string()),
    }
}

/// Renders signed-out navigation and account entry points without exposing state.
#[must_use]
pub fn signed_out_page() -> Container {
    container! {
        div padding=32 gap=24 {
            header gap=8 {
                h1 { "Words with Spouses" }
                span { "Private asynchronous word-tile games" }
            }
            main gap=16 {
                h2 { "Sign in required" }
                span { "A valid secure session is required to view games." }
                div gap=8 {
                    anchor href="/login" { "Sign in" }
                    anchor href="/register" { "Create account" }
                }
            }
        }
    }
    .into()
}

/// Renders the complete signed-in dashboard projection.
#[must_use]
pub fn dashboard_page(dashboard: &AuthenticatedDashboard) -> Container {
    let user_id = dashboard.user_id.as_str();
    let totals = score_totals_label(dashboard.score_totals.as_ref());
    container! {
        div padding=32 gap=24 {
            header gap=8 {
                h1 { "Words with Spouses" }
                span { "Signed in as " (user_id) }
                anchor href="/logout" { "Sign out" }
            }
            main gap=24 {
                section id="score-totals" gap=8 {
                    h2 { "Score history" }
                    span { (totals) }
                }
                section id="pending-games" gap=8 {
                    h2 { "Challenges and invitations" }
                    @for item in &dashboard.projection.pending {
                        div class="pending-item" {
                            span { (item.kind) " " (item.direction) }
                            span { (item.counterparty_user_id.as_deref().unwrap_or("Shareable invitation")) }
                        }
                    }
                }
                section id="active-games" gap=8 {
                    h2 { "Games" }
                    @for game in &dashboard.projection.games {
                        @let href = format!("/games/{}", game.game_id);
                        @let turn = if game.active_player_user_id.as_deref() == Some(user_id) {
                            "Your turn"
                        } else {
                            game.status.as_str()
                        };
                        div class="game-summary" {
                            anchor href=(href) { (game.game_id.as_str()) }
                            span { (turn) }
                            span { "Revision " (game.canonical_revision) }
                        }
                    }
                }
            }
        }
    }
    .into()
}

fn score_totals_label(totals: Option<&UserScoreTotals>) -> String {
    totals.map_or_else(
        || "No completed games".to_string(),
        |totals| {
            format!(
                "{} completed, {} wins, {} ties, {} total points",
                totals.completed_games, totals.wins, totals.ties, totals.total_score
            )
        },
    )
}

/// Renders public board/status/history and only the authorized viewer's rack.
#[must_use]
pub fn game_page(game: &AuthorizedGamePage) -> Container {
    let game_id = game.game_id.to_string();
    let state_label = if game.completed {
        "Completed"
    } else {
        "Active"
    };
    let board = board_component(&game.view);
    let status = status_component(&game.view);
    let rack = rack_component(&game.view);
    let history = move_history_component(&game.history);
    let adjustments = game
        .final_score_adjustments
        .iter()
        .map(|(player, adjustment)| format!("{player:?}:{adjustment:+}"))
        .collect::<Vec<_>>()
        .join(" ");
    container! {
        div padding=32 gap=24 {
            header gap=8 {
                anchor href="/" { "Dashboard" }
                h1 { "Game " (game_id) }
                span { (state_label) }
            }
            main gap=16 {
                (board)
                (status)
                (rack)
                (history)
                section id="final-score-adjustments" {
                    h2 { "Final score adjustments" }
                    span { (adjustments) }
                }
                section id="game-live-state" data-channel=(format!("game:{}", game.game_id)) {
                    span { "Live updates use the authorized HyperChad game channel." }
                }
            }
        }
    }
    .into()
}

fn product_error_page(title: &str, message: &str) -> Container {
    let error = error_component(message);
    container! {
        div padding=32 gap=16 {
            anchor href="/" { "Dashboard" }
            h1 { (title) }
            (error)
        }
    }
    .into()
}

/// Returns the game page as a `HyperChad` view for target-scoped update composition.
#[must_use]
pub fn game_view_response(game: &AuthorizedGamePage) -> View {
    View::from(game_page(game))
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use hyperchad::router::RequestInfo;
    use time::Duration;

    use super::*;
    use crate::{
        SESSION_COOKIE_NAME, accept_challenge, create_challenge, create_session, migrate_app,
        register,
    };

    #[test]
    fn database_backed_routes_render_dashboard_and_private_game() {
        block_on(async {
            let database: Arc<dyn Database> = Arc::from(
                switchy_database_connection::builder()
                    .turso()
                    .with_in_memory()
                    .build()
                    .await
                    .expect("Turso opens"),
            );
            migrate_app(&*database).await.expect("migrations run");
            let now = OffsetDateTime::UNIX_EPOCH;
            let alice = register(&*database, "alice", "correct horse battery staple", now)
                .await
                .expect("Alice registers");
            let bob = register(&*database, "bob", "another correct horse battery", now)
                .await
                .expect("Bob registers");
            let challenge = create_challenge(&*database, &alice, &bob, now)
                .await
                .expect("challenge creates");
            let game_id = accept_challenge(&*database, &challenge, &bob, now, 5)
                .await
                .expect("game starts");
            let session = create_session(&*database, &alice, now, Duration::days(1))
                .await
                .expect("session creates");
            let mut dashboard_request = RouteRequest::from_path("/", RequestInfo::default());
            dashboard_request.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                session.expose().to_string(),
            );
            let dashboard = dashboard_route(&*database, &dashboard_request, now)
                .await
                .display_to_string(false, false)
                .expect("dashboard renders");
            assert!(dashboard.contains(&game_id.to_string()));
            assert!(dashboard.contains("Signed in as"));

            let mut game_request =
                RouteRequest::from_path(&format!("/games/{game_id}"), RequestInfo::default());
            game_request.cookies = dashboard_request.cookies;
            let page = game_route(&*database, &game_request, now)
                .await
                .display_to_string(false, false)
                .expect("game renders");
            assert!(page.contains("player-rack"));
            assert!(page.contains("move-history"));
            assert!(!page.contains("bag"));
        });
    }

    #[test]
    fn anonymous_route_renders_recoverable_sign_in_state() {
        block_on(async {
            let database = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens");
            migrate_app(&*database).await.expect("migrations run");
            let request = RouteRequest::from_path("/", RequestInfo::default());
            let page = dashboard_route(&*database, &request, OffsetDateTime::UNIX_EPOCH)
                .await
                .display_to_string(false, false)
                .expect("page renders");
            assert!(page.contains("Sign in required"));
        });
    }
}
