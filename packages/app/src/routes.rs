//! Renderer-neutral routed product pages backed by durable presentation services.

use std::sync::Arc;

use hyperchad::{
    renderer::{ResponseCookie, ResponseMetadata, View},
    router::{Container, RoutePath, RouteRequest, Router},
    template::container,
};
use serde::Deserialize;
use switchy_database::Database;
use time::{Duration, OffsetDateTime};

use crate::{
    AccountWorkflowError, AuthenticatedDashboard, AuthorizedGamePage, PresentationError,
    UserScoreTotals, board_component, error_component, load_authenticated_dashboard,
    load_authorized_game_page, login_and_create_session, logout_session, move_history_component,
    rack_component, register_and_create_session, status_component,
};

/// Account credential form accepted by renderer-neutral routes.
#[derive(Debug, Deserialize)]
struct AccountForm {
    username: String,
    password: String,
}

const fn account_error_message(error: &AccountWorkflowError) -> &'static str {
    match error {
        AccountWorkflowError::Account(crate::AccountError::InvalidCredentials) => {
            "Username or password is incorrect."
        }
        AccountWorkflowError::Account(crate::AccountError::InvalidUsername) => {
            "Username must be 3–32 letters, numbers, underscores, or hyphens."
        }
        AccountWorkflowError::Account(crate::AccountError::WeakPassword) => {
            "Password must contain at least 12 characters."
        }
        AccountWorkflowError::Account(crate::AccountError::UsernameTaken) => {
            "That username is already registered."
        }
        _ => "Account request could not be completed. Please try again.",
    }
}

async fn login_route(
    database: &dyn Database,
    request: &RouteRequest,
    now: OffsetDateTime,
    csrf_token: &str,
) -> View {
    if request.method.as_ref() != "POST" {
        return View::from(login_page(None));
    }
    let form: AccountForm = match request.parse_form() {
        Ok(form) => form,
        Err(_) => return View::from(login_page(Some("Enter a username and password."))),
    };
    match login_and_create_session(
        database,
        &form.username,
        &form.password,
        now,
        Duration::days(30),
    )
    .await
    {
        Ok((_, session)) => View::builder()
            .with_primary(dashboard_refresh_page())
            .with_response(authenticated_session_response(session.expose(), csrf_token))
            .build(),
        Err(error) => View::from(login_page(Some(account_error_message(&error)))),
    }
}

async fn register_route(
    database: &dyn Database,
    request: &RouteRequest,
    now: OffsetDateTime,
    csrf_token: &str,
) -> View {
    if request.method.as_ref() != "POST" {
        return View::from(register_page(None));
    }
    let form: AccountForm = match request.parse_form() {
        Ok(form) => form,
        Err(_) => return View::from(register_page(Some("Enter a username and password."))),
    };
    match register_and_create_session(
        database,
        &form.username,
        &form.password,
        now,
        Duration::days(30),
    )
    .await
    {
        Ok((_, session)) => View::builder()
            .with_primary(dashboard_refresh_page())
            .with_response(authenticated_session_response(session.expose(), csrf_token))
            .build(),
        Err(error) => View::from(register_page(Some(account_error_message(&error)))),
    }
}

async fn logout_route(
    database: &dyn Database,
    request: &RouteRequest,
    now: OffsetDateTime,
) -> View {
    if request.method.as_ref() != "POST" {
        return View::from(logout_page());
    }
    if let Some(session) = request.cookies.get(crate::SESSION_COOKIE_NAME)
        && logout_session(database, session, now).await.is_err()
    {
        crate::observability::record_database_failure("logout_session");
        return View::from(product_error_page(
            "Unable to sign out",
            "Your session could not be revoked. Please try again.",
        ));
    }
    View::builder()
        .with_primary(signed_out_page())
        .with_response(logged_out_response())
        .build()
}

fn dashboard_refresh_page() -> Container {
    container! {
        section id="account-result" {
            span { "Signed in. Loading your dashboard…" }
        }
    }
    .into()
}

/// Builds the database-backed renderer-neutral application router.
#[must_use]
pub fn create_product_router(database: Arc<dyn Database>, csrf_token: String) -> Router {
    let router = Router::new();
    let dashboard_database = database.clone();
    router.add_route_result("/", move |request: RouteRequest| {
        let database = dashboard_database.clone();
        async move {
            Ok(dashboard_route(&*database, &request, OffsetDateTime::now_utc()).await)
                as Result<Container, Box<dyn std::error::Error>>
        }
    });
    let csrf_token = Arc::new(csrf_token);
    let login_database = database.clone();
    let login_csrf = csrf_token.clone();
    router.add_route_result("/login", move |request: RouteRequest| {
        let database = login_database.clone();
        let csrf_token = login_csrf.clone();
        async move {
            Ok(login_route(
                &*database,
                &request,
                OffsetDateTime::now_utc(),
                csrf_token.as_str(),
            )
            .await) as Result<View, Box<dyn std::error::Error>>
        }
    });
    let register_database = database.clone();
    let register_csrf = csrf_token;
    router.add_route_result("/register", move |request: RouteRequest| {
        let database = register_database.clone();
        let csrf_token = register_csrf.clone();
        async move {
            Ok(register_route(
                &*database,
                &request,
                OffsetDateTime::now_utc(),
                csrf_token.as_str(),
            )
            .await) as Result<View, Box<dyn std::error::Error>>
        }
    });
    let logout_database = database.clone();
    router.add_route_result("/logout", move |request: RouteRequest| {
        let database = logout_database.clone();
        async move {
            Ok(logout_route(&*database, &request, OffsetDateTime::now_utc()).await)
                as Result<View, Box<dyn std::error::Error>>
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

/// Renders the renderer-neutral login form.
#[must_use]
pub fn login_page(error: Option<&str>) -> Container {
    let message = error.unwrap_or_default();
    container! {
        div padding=32 gap=16 {
            anchor href="/" { "Home" }
            h1 { "Sign in" }
            form hx-post="/login" hx-target="#account-result" gap=8 {
                input type=text name="username" placeholder="Username";
                input type=password name="password" placeholder="Password";
                button type=submit { "Sign in" }
            }
            section id="account-result" { span { (message) } }
            anchor href="/register" { "Create account" }
        }
    }
    .into()
}

/// Renders the renderer-neutral registration form.
#[must_use]
pub fn register_page(error: Option<&str>) -> Container {
    let message = error.unwrap_or_default();
    container! {
        div padding=32 gap=16 {
            anchor href="/" { "Home" }
            h1 { "Create account" }
            form hx-post="/register" hx-target="#account-result" gap=8 {
                input type=text name="username" placeholder="Username";
                input type=password name="password" placeholder="Password (12+ characters)";
                button type=submit { "Create account" }
            }
            section id="account-result" { span { (message) } }
            anchor href="/login" { "Sign in" }
        }
    }
    .into()
}

/// Renders logout confirmation. Session revocation is performed by the authenticated workflow.
#[must_use]
pub fn logout_page() -> Container {
    container! {
        div padding=32 gap=16 {
            h1 { "Sign out" }
            form hx-post="/logout" hx-target="#account-result" {
                button type=submit { "Sign out" }
            }
            section id="account-result" {
                span { "Signing out revokes the current durable session." }
            }
        }
    }
    .into()
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
    let game_channel = format!("game:{}", game.game_id);
    let pass_command_id = uuid::Uuid::new_v4().to_string();
    let pass_idempotency_key = uuid::Uuid::new_v4().to_string();
    let pass_payload = "\"Pass\"";
    let resign_command_id = uuid::Uuid::new_v4().to_string();
    let resign_idempotency_key = uuid::Uuid::new_v4().to_string();
    let resign_payload = "\"Resign\"";
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
                section id="game-live-state" data-shared-state-channel=(game_channel.as_str()) {
                    span { "Live updates use the authorized HyperChad game channel." }
                }
                @if !game.completed {
                    section id="turn-actions" gap=8 {
                        h2 { "Turn actions" }
                        button
                            data-shared-state-command="PASS"
                            data-shared-state-command-id=(pass_command_id)
                            data-shared-state-idempotency-key=(pass_idempotency_key)
                            data-shared-state-channel=(game_channel.as_str())
                            data-shared-state-participant=(game.user_id.as_str())
                            data-shared-state-expected-revision=(game.view.revision)
                            data-shared-state-command-payload=(pass_payload)
                        { "Pass" }
                        button
                            data-shared-state-command="RESIGN"
                            data-shared-state-command-id=(resign_command_id)
                            data-shared-state-idempotency-key=(resign_idempotency_key)
                            data-shared-state-channel=(game_channel.as_str())
                            data-shared-state-participant=(game.user_id.as_str())
                            data-shared-state-expected-revision=(game.view.revision)
                            data-shared-state-command-payload=(resign_payload)
                        { "Resign" }
                    }
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

/// Builds secure response effects for a newly authenticated browser session.
#[must_use]
pub fn authenticated_session_response(session: &str, csrf_token: &str) -> ResponseMetadata {
    let mut csrf_cookie = ResponseCookie::secure(crate::CSRF_COOKIE_NAME, csrf_token);
    csrf_cookie.http_only = false;
    ResponseMetadata {
        cookies: vec![
            ResponseCookie::secure(crate::SESSION_COOKIE_NAME, session),
            csrf_cookie,
        ],
        redirect: Some("/".to_string()),
    }
}

/// Builds secure cookie-expiration effects for logout.
#[must_use]
pub fn logged_out_response() -> ResponseMetadata {
    ResponseMetadata {
        cookies: vec![
            ResponseCookie::expired(crate::SESSION_COOKIE_NAME),
            ResponseCookie::expired(crate::CSRF_COOKIE_NAME),
        ],
        redirect: Some("/login".to_string()),
    }
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
            assert!(page.contains("data-shared-state-channel"));
            assert!(page.contains("data-shared-state-command=\"PASS\""));
            assert!(page.contains("data-shared-state-expected-revision=\"1\""));
            assert!(!page.contains("bag"));
        });
    }

    #[test]
    fn account_post_routes_issue_and_expire_secure_cookies() {
        block_on(async {
            let database = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens");
            migrate_app(&*database).await.expect("migrations run");
            let mut register = RouteRequest::from_path("/register", RequestInfo::default());
            register.method = "POST".parse().expect("POST parses");
            register.headers.insert(
                "content-type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            );
            register.body = Some(std::sync::Arc::new(
                "username=alice&password=correct+horse+battery+staple".into(),
            ));
            let response = register_route(
                &*database,
                &register,
                OffsetDateTime::UNIX_EPOCH,
                "csrf-test",
            )
            .await;
            assert_eq!(response.response.redirect.as_deref(), Some("/"));
            assert_eq!(response.response.cookies.len(), 2);
            let session = response.response.cookies[0].value.clone();
            assert!(response.response.cookies[0].http_only);
            assert!(response.response.cookies[0].secure);

            let mut logout = RouteRequest::from_path("/logout", RequestInfo::default());
            logout.method = "POST".parse().expect("POST parses");
            logout
                .cookies
                .insert(crate::SESSION_COOKIE_NAME.to_string(), session);
            let response = logout_route(&*database, &logout, OffsetDateTime::UNIX_EPOCH).await;
            assert_eq!(response.response.redirect.as_deref(), Some("/login"));
            assert!(
                response
                    .response
                    .cookies
                    .iter()
                    .all(|cookie| cookie.max_age_seconds == Some(0))
            );
        });
    }

    #[test]
    fn account_session_effects_are_secure_and_expirable() {
        let signed_in = authenticated_session_response("opaque-test-session", "csrf-test");
        assert_eq!(signed_in.redirect.as_deref(), Some("/"));
        assert_eq!(signed_in.cookies.len(), 2);
        assert!(signed_in.cookies[0].secure);
        assert!(signed_in.cookies[0].http_only);
        assert!(!signed_in.cookies[1].http_only);
        assert!(signed_in.cookies.iter().all(|cookie| cookie.secure));

        let signed_out = logged_out_response();
        assert_eq!(signed_out.redirect.as_deref(), Some("/login"));
        assert!(
            signed_out
                .cookies
                .iter()
                .all(|cookie| cookie.max_age_seconds == Some(0))
        );
    }

    #[test]
    fn account_pages_expose_renderer_neutral_forms() {
        for (page, route) in [
            (login_page(None), "/login"),
            (register_page(None), "/register"),
            (logout_page(), "/logout"),
        ] {
            let rendered = page
                .display_to_string(false, false)
                .expect("account page renders");
            assert!(rendered.contains(&format!("hx-post=\"{route}\"")));
            assert!(!rendered.contains("script"));
        }
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
