//! Renderer-neutral routed product pages backed by durable presentation services.

use std::sync::Arc;

use hyperchad::{
    actions::ActionType,
    renderer::{ResponseCookie, ResponseMetadata, View},
    router::{Container, RoutePath, RouteRequest, Router},
    shared_state_models::{
        CommandEnvelope, CommandId, IdempotencyKey, ParticipantId, PayloadBlob, Revision,
        TransportInbound, TransportOutbound,
    },
    shared_state_transport::{AuthenticatedTransportContext, SharedStateTransportDispatcher as _},
    template::container,
};
use serde::Deserialize;
use switchy_database::Database;
use time::{Duration, OffsetDateTime};
use words_with_spouses_game_domain::{Coordinate, GameCommand, Placement, TileId};

use crate::{
    AccountWorkflowError, AuthenticatedDashboard, AuthorizedGamePage, PendingMoveView,
    PresentationError, ProductWorkflowError, UserScoreTotals, accept_pending_challenge,
    board_component, cancel_pending_challenge, challenge_username, create_shareable_invitation,
    decline_pending_challenge, error_component, load_authenticated_dashboard,
    load_authorized_game_page, login_and_create_session, logout_session, move_history_component,
    rack_component, redeem_shareable_invitation, register_and_create_session,
    revoke_shareable_invitation, status_component,
};

#[derive(Debug, Deserialize)]
struct DashboardActionForm {
    action: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    challenge_id: String,
    #[serde(default)]
    invitation_id: String,
    #[serde(default)]
    invitation_token: String,
}

const fn product_error_message(error: &ProductWorkflowError) -> &'static str {
    match error {
        ProductWorkflowError::UnknownUser => "That username is not registered.",
        ProductWorkflowError::Challenge(crate::ChallengeError::Duplicate) => {
            "A pending challenge already exists for those players."
        }
        ProductWorkflowError::Challenge(crate::ChallengeError::Unauthorized) => {
            "That challenge is no longer available to this account."
        }
        ProductWorkflowError::Invitation(crate::InvitationError::Invalid) => {
            "That invitation is invalid, expired, revoked, or already used."
        }
        _ => "The dashboard action could not be completed. Please try again.",
    }
}

async fn dashboard_action_route(
    database: &dyn Database,
    request: &RouteRequest,
    now: OffsetDateTime,
) -> Container {
    let Ok(user_id) = crate::authenticated_user(database, &request.cookies, now).await else {
        return error_component("Your session expired. Sign in and try again.");
    };
    let form: DashboardActionForm = match request.parse_form() {
        Ok(form) => form,
        Err(_) => return error_component("The dashboard action was incomplete."),
    };
    let result = match form.action.as_str() {
        "CHALLENGE" => challenge_username(database, &user_id, &form.username, now)
            .await
            .map(|_| ()),
        "ACCEPT_CHALLENGE" => accept_pending_challenge(database, &form.challenge_id, &user_id, now)
            .await
            .map(|_| ()),
        "DECLINE_CHALLENGE" => {
            decline_pending_challenge(database, &form.challenge_id, &user_id, now).await
        }
        "CANCEL_CHALLENGE" => {
            cancel_pending_challenge(database, &form.challenge_id, &user_id, now).await
        }
        "CREATE_INVITATION" => {
            create_shareable_invitation(database, &user_id, now, Duration::days(30))
                .await
                .map(|_| ())
        }
        "REDEEM_INVITATION" => {
            redeem_shareable_invitation(database, &form.invitation_token, &user_id, now)
                .await
                .map(|_| ())
        }
        "REVOKE_INVITATION" => {
            revoke_shareable_invitation(database, &form.invitation_id, &user_id, now).await
        }
        _ => return error_component("The dashboard action is unknown."),
    };
    if let Err(error) = result {
        return error_component(product_error_message(&error));
    }
    match load_authenticated_dashboard(database, &request.cookies, now).await {
        Ok(dashboard) => dashboard_page(&dashboard),
        Err(PresentationError::Unauthenticated) => {
            error_component("Your session expired. Sign in and review your dashboard.")
        }
        Err(_) => {
            error_component("The action succeeded, but the dashboard could not be refreshed.")
        }
    }
}

#[derive(Debug, Deserialize)]
struct PendingMoveForm {
    command: String,
    command_id: String,
    idempotency_key: String,
    expected_revision: u64,
    #[serde(default)]
    tile_0: Option<u16>,
    #[serde(default)]
    tile_1: Option<u16>,
    #[serde(default)]
    tile_2: Option<u16>,
    #[serde(default)]
    tile_3: Option<u16>,
    #[serde(default)]
    tile_4: Option<u16>,
    #[serde(default)]
    tile_5: Option<u16>,
    #[serde(default)]
    tile_6: Option<u16>,
    #[serde(default)]
    x_0: Option<u8>,
    #[serde(default)]
    x_1: Option<u8>,
    #[serde(default)]
    x_2: Option<u8>,
    #[serde(default)]
    x_3: Option<u8>,
    #[serde(default)]
    x_4: Option<u8>,
    #[serde(default)]
    x_5: Option<u8>,
    #[serde(default)]
    x_6: Option<u8>,
    #[serde(default)]
    y_0: Option<u8>,
    #[serde(default)]
    y_1: Option<u8>,
    #[serde(default)]
    y_2: Option<u8>,
    #[serde(default)]
    y_3: Option<u8>,
    #[serde(default)]
    y_4: Option<u8>,
    #[serde(default)]
    y_5: Option<u8>,
    #[serde(default)]
    y_6: Option<u8>,
    #[serde(default)]
    blank_0: Option<String>,
    #[serde(default)]
    blank_1: Option<String>,
    #[serde(default)]
    blank_2: Option<String>,
    #[serde(default)]
    blank_3: Option<String>,
    #[serde(default)]
    blank_4: Option<String>,
    #[serde(default)]
    blank_5: Option<String>,
    #[serde(default)]
    blank_6: Option<String>,
}

type SelectedTile<'a> = (u16, Option<u8>, Option<u8>, Option<&'a str>);

impl PendingMoveForm {
    fn selected_tiles(&self) -> Vec<SelectedTile<'_>> {
        [
            (self.tile_0, self.x_0, self.y_0, self.blank_0.as_deref()),
            (self.tile_1, self.x_1, self.y_1, self.blank_1.as_deref()),
            (self.tile_2, self.x_2, self.y_2, self.blank_2.as_deref()),
            (self.tile_3, self.x_3, self.y_3, self.blank_3.as_deref()),
            (self.tile_4, self.x_4, self.y_4, self.blank_4.as_deref()),
            (self.tile_5, self.x_5, self.y_5, self.blank_5.as_deref()),
            (self.tile_6, self.x_6, self.y_6, self.blank_6.as_deref()),
        ]
        .into_iter()
        .filter_map(|(tile, x, y, blank)| tile.map(|tile| (tile, x, y, blank)))
        .collect()
    }

    fn game_command(&self) -> Result<GameCommand, &'static str> {
        let selected = self.selected_tiles();
        match self.command.as_str() {
            "PLAY" => {
                let placements = selected
                    .iter()
                    .map(|(tile_id, x, y, value)| {
                        let coordinate = Coordinate::new(
                            x.ok_or("Every selected tile needs one board coordinate.")?,
                            y.ok_or("Every selected tile needs one board coordinate.")?,
                        );
                        let blank_letter = value.and_then(|value| {
                            value
                                .trim()
                                .chars()
                                .next()
                                .map(|letter| letter.to_ascii_uppercase())
                        });
                        Ok::<Placement, &'static str>(Placement {
                            tile_id: TileId::new(*tile_id),
                            coordinate,
                            blank_letter,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if placements.is_empty() {
                    return Err("Select at least one tile to play.");
                }
                Ok(GameCommand::Play { placements })
            }
            "EXCHANGE" if !selected.is_empty() => Ok(GameCommand::Exchange {
                tile_ids: selected
                    .iter()
                    .map(|(tile_id, ..)| TileId::new(*tile_id))
                    .collect(),
            }),
            "EXCHANGE" => Err("Select at least one tile to exchange."),
            "PASS" => Ok(GameCommand::Pass),
            "RESIGN" => Ok(GameCommand::Resign),
            _ => Err("Unknown turn action."),
        }
    }
}

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
        Ok((_, session)) => {
            let dashboard = dashboard_refresh_page(database, session.expose(), now).await;
            View::builder()
                .with_primary(dashboard)
                .with_response(authenticated_session_response(session.expose(), csrf_token))
                .build()
        }
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
        Ok((_, session)) => {
            let dashboard = dashboard_refresh_page(database, session.expose(), now).await;
            View::builder()
                .with_primary(dashboard)
                .with_response(authenticated_session_response(session.expose(), csrf_token))
                .build()
        }
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

async fn dashboard_refresh_page(
    database: &dyn Database,
    session: &str,
    now: OffsetDateTime,
) -> Container {
    let cookies = std::collections::BTreeMap::from([(
        crate::SESSION_COOKIE_NAME.to_string(),
        session.to_string(),
    )]);
    load_authenticated_dashboard(database, &cookies, now)
        .await
        .map_or_else(
            |_| {
                product_error_page(
                    "Unable to load dashboard",
                    "Your account is signed in, but the dashboard could not be loaded. Reload the page.",
                )
            },
            |dashboard| dashboard_page(&dashboard),
        )
}

async fn game_turn_route(
    dispatcher: &crate::GameSharedStateDispatcher,
    database: &dyn Database,
    request: &RouteRequest,
    game_id: &str,
    now: OffsetDateTime,
) -> Container {
    let Ok(user_id) = crate::authenticated_user(database, &request.cookies, now).await else {
        return error_component("Your session expired. Sign in and review the game again.");
    };
    let Ok(game_id) = game_id.parse() else {
        return error_component("The game route is invalid.");
    };
    let form: PendingMoveForm = match request.parse_form() {
        Ok(form) => form,
        Err(_) => return error_component("Select tiles and provide valid coordinates."),
    };
    let command = match form.game_command() {
        Ok(command) => command,
        Err(message) => return error_component(message),
    };
    let Ok(payload) = PayloadBlob::from_serializable(&command) else {
        return error_component("The turn could not be encoded. Try again.");
    };
    let context = AuthenticatedTransportContext {
        participant_id: ParticipantId::new(&user_id),
        identity_binding: request
            .cookies
            .get(crate::SESSION_COOKIE_NAME)
            .cloned()
            .unwrap_or_default(),
    };
    let envelope = CommandEnvelope {
        command_id: CommandId::new(form.command_id),
        channel_id: crate::game_channel(game_id),
        participant_id: context.participant_id.clone(),
        idempotency_key: IdempotencyKey::new(form.idempotency_key),
        expected_revision: Revision::new(form.expected_revision),
        command_name: form.command,
        payload,
        metadata: std::collections::BTreeMap::new(),
        created_at_ms: now.unix_timestamp_nanos().try_into().unwrap_or(i64::MAX),
    };
    match dispatcher
        .ingest_outbound(&context, TransportOutbound::Command(envelope))
        .await
    {
        Ok(result)
            if matches!(
                result.as_slice(),
                [TransportInbound::CommandAccepted { .. }]
            ) =>
        {
            match load_authorized_game_page(database, &request.cookies, &game_id.to_string(), now)
                .await
            {
                Ok(game) => game_page(&game),
                Err(PresentationError::Unauthenticated) => {
                    error_component("Your session expired. Sign in and review the accepted turn.")
                }
                Err(_) => error_component(
                    "The turn was accepted, but the updated game could not be rendered. Reload the game.",
                ),
            }
        }
        Ok(result) => {
            let reason = result.iter().find_map(|response| match response {
                TransportInbound::CommandRejected { reason, .. } => Some(reason.as_str()),
                _ => None,
            });
            turn_rejection(reason.unwrap_or("The turn was not accepted."))
        }
        Err(_) => error_component("The turn could not be persisted. Try again."),
    }
}

fn turn_rejection(reason: &str) -> Container {
    let message = if reason.contains("revision") {
        "This game changed in another tab. Review the latest board and resubmit."
    } else if reason.contains("authorized") || reason.contains("member") {
        "You are not authorized to act in this game."
    } else if reason.contains("rules") || reason.contains("dictionary version") {
        "This game requires an unsupported rules or dictionary version."
    } else if reason.contains("dictionary rejected") {
        "The dictionary rejected a word in this move. Adjust the placement and resubmit."
    } else if reason.contains("coordinate")
        || reason.contains("row or one column")
        || reason.contains("gap")
        || reason.contains("start square")
        || reason.contains("connect")
        || reason.contains("placement")
    {
        "The tile placement is not legal. Review the board coordinates and resubmit."
    } else if reason.contains("turn") {
        "It is not your turn. Review the latest game state before submitting."
    } else if reason.contains("complete") {
        "This game is complete and no longer accepts turns."
    } else {
        reason
    };
    error_component(message)
}

/// Builds the database-backed renderer-neutral application router.
#[must_use]
pub fn create_product_router(
    database: Arc<dyn Database>,
    dispatcher: Arc<crate::GameSharedStateDispatcher>,
    csrf_token: String,
) -> Router {
    let router = Router::new();
    let dashboard_database = database.clone();
    router.add_route_result("/", move |request: RouteRequest| {
        let database = dashboard_database.clone();
        async move {
            Ok(dashboard_route(&*database, &request, OffsetDateTime::now_utc()).await)
                as Result<Container, Box<dyn std::error::Error>>
        }
    });
    let dashboard_action_database = database.clone();
    router.add_route_result("/dashboard/action", move |request: RouteRequest| {
        let database = dashboard_action_database.clone();
        async move {
            Ok(dashboard_action_route(&*database, &request, OffsetDateTime::now_utc()).await)
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
    let game_dispatcher = dispatcher;
    router.add_route_result(
        RoutePath::LiteralPrefix("/games/".to_string()),
        move |request: RouteRequest| {
            let database = database.clone();
            let dispatcher = game_dispatcher.clone();
            async move {
                let game_path = request.path.strip_prefix("/games/").unwrap_or_default();
                if let Some(game_id) = game_path.strip_suffix("/turn") {
                    Ok(game_turn_route(
                        &dispatcher,
                        &*database,
                        &request,
                        game_id,
                        OffsetDateTime::now_utc(),
                    )
                    .await) as Result<Container, Box<dyn std::error::Error>>
                } else {
                    Ok(game_route(&*database, &request, OffsetDateTime::now_utc()).await)
                        as Result<Container, Box<dyn std::error::Error>>
                }
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
        div id="app-page" padding=32 gap=16 {
            anchor href="/" { "Home" }
            h1 { "Sign in" }
            form hx-post="/login" hx-target="#app-page" gap=8 {
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
        div id="app-page" padding=32 gap=16 {
            anchor href="/" { "Home" }
            h1 { "Create account" }
            form hx-post="/register" hx-target="#app-page" gap=8 {
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
        div id="app-page" padding=32 gap=16 {
            h1 { "Sign out" }
            form hx-post="/logout" hx-target="#app-page" {
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
        div id="app-page" padding=32 gap=24 {
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
    let dashboard_channel = format!("dashboard:{}", dashboard.user_id);
    container! {
        div id="app-page" data-shared-state-channel=(dashboard_channel.as_str()) padding=32 gap=24 {
            header gap=8 {
                h1 { "Words with Spouses" }
                span { "Signed in as " (user_id) }
                anchor href="/logout" { "Sign out" }
            }
            main gap=24 {
                section id="new-game-actions" gap=8 {
                    h2 { "Start a game" }
                    form hx-post="/dashboard/action" hx-target="#app-page" gap=4 {
                        input type=hidden name="action" value="CHALLENGE";
                        input type=text name="username" placeholder="Opponent username";
                        button type=submit { "Challenge" }
                    }
                    form hx-post="/dashboard/action" hx-target="#app-page" gap=4 {
                        input type=hidden name="action" value="CREATE_INVITATION";
                        button type=submit { "Create shareable invitation" }
                    }
                    form hx-post="/dashboard/action" hx-target="#app-page" gap=4 {
                        input type=hidden name="action" value="REDEEM_INVITATION";
                        input type=text name="invitation_token" placeholder="Invitation token";
                        button type=submit { "Join from invitation" }
                    }
                }
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
                            @if item.kind == "CHALLENGE" && item.direction == "INCOMING" {
                                form hx-post="/dashboard/action" hx-target="#app-page" {
                                    input type=hidden name="action" value="ACCEPT_CHALLENGE";
                                    input type=hidden name="challenge_id" value=(item.id.as_str());
                                    button type=submit { "Accept" }
                                }
                                form hx-post="/dashboard/action" hx-target="#app-page" {
                                    input type=hidden name="action" value="DECLINE_CHALLENGE";
                                    input type=hidden name="challenge_id" value=(item.id.as_str());
                                    button type=submit { "Decline" }
                                }
                            } @else if item.kind == "CHALLENGE" {
                                form hx-post="/dashboard/action" hx-target="#app-page" {
                                    input type=hidden name="action" value="CANCEL_CHALLENGE";
                                    input type=hidden name="challenge_id" value=(item.id.as_str());
                                    button type=submit { "Cancel" }
                                }
                            } @else {
                                form hx-post="/dashboard/action" hx-target="#app-page" {
                                    input type=hidden name="action" value="REVOKE_INVITATION";
                                    input type=hidden name="invitation_id" value=(item.id.as_str());
                                    button type=submit { "Revoke" }
                                }
                            }
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

/// Renders renderer-neutral pending placement/exchange form controls.
#[must_use]
pub fn turn_composer(game: &AuthorizedGamePage) -> Container {
    let action = format!("/games/{}/turn", game.game_id);
    let command_id = uuid::Uuid::new_v4().to_string();
    let idempotency_key = uuid::Uuid::new_v4().to_string();
    let rack = game.view.rack.clone();
    let rack_orders = rack
        .iter()
        .map(|(tile_id, _, _)| PendingMoveView::reorder_rack(&rack, *tile_id))
        .collect::<Vec<_>>();
    let show_rack_order = |target_index: usize| {
        ActionType::Multi(
            rack_orders
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    let order_id = format!("rack-order-{index}");
                    if index == target_index {
                        ActionType::display_by_id(order_id)
                    } else {
                        ActionType::no_display_by_id(order_id)
                    }
                })
                .collect(),
        )
    };
    container! {
        section id="turn-composer" gap=12 {
            h2 { "Compose turn" }
            span { "Select one or more rack tile IDs and provide matching board coordinates. Blank letters are optional and normalized server-side." }
            section id="local-rack-order" gap=4 {
                h3 { "Local rack order" }
                @for (order_index, ordered_rack) in rack_orders.iter().enumerate() {
                    @let order_id = format!("rack-order-{order_index}");
                    section id=(order_id.as_str()) gap=4
                        fx-immediate=(if order_index == 0 {
                            ActionType::display_by_id(order_id.clone())
                        } else {
                            ActionType::no_display_by_id(order_id.clone())
                        }) {
                        @for (tile_id, letter, points) in ordered_rack {
                            @let reorder_label = format!("Tile {tile_id}: {letter} ({points})");
                            @let target_order = rack
                                .iter()
                                .position(|(candidate_id, _, _)| candidate_id == tile_id)
                                .expect("ordered rack tiles originate in the projected rack");
                            button type=button
                                fx-click=(show_rack_order(target_order)) {
                                (reorder_label)
                            }
                        }
                    }
                }
                span { "Choose a tile to make it first in the local order. This does not submit a turn." }
            }
            form hx-post=(action.as_str()) hx-target="#app-page" gap=8 {
                input type=hidden name="command_id" value=(command_id);
                input type=hidden name="idempotency_key" value=(idempotency_key);
                input type=hidden name="expected_revision" value=(game.view.revision);
                select name="command" {
                    option value="PLAY" { "Play tiles" }
                    option value="EXCHANGE" { "Exchange tiles" }
                    option value="PASS" { "Pass" }
                    option value="RESIGN" { "Resign" }
                }
                @for (index, (tile_id, letter, points)) in rack.iter().enumerate() {
                    @let label = format!("Tile {tile_id}: {letter} ({points})");
                    @let tile_name = format!("tile_{index}");
                    @let x_name = format!("x_{index}");
                    @let y_name = format!("y_{index}");
                    @let blank_name = format!("blank_{index}");
                    @let editor_class = format!("pending-editor-{index}");
                    @let blank_class = format!("blank-letter-{index}");
                    div class="rack-tile-composer" gap=4 {
                        input type=checkbox name=(tile_name) value=(tile_id)
                            fx-click=(ActionType::toggle_display_str_class(editor_class.as_str()));
                        span { "Select / unplace " (label) }
                        div class=(editor_class.as_str()) gap=4
                            fx-immediate=(ActionType::no_display_class(editor_class.as_str())) {
                            span { "Place by entering board coordinates; edit them to move the pending tile." }
                            input type=text name=(x_name) placeholder="x";
                            input type=text name=(y_name) placeholder="y";
                            input class=(blank_class.as_str()) type=text name=(blank_name) placeholder="blank letter";
                            button type=button fx-click=(ActionType::no_display_class(editor_class.as_str())) {
                                "Unplace"
                            }
                            button type=button fx-click=(ActionType::select_class(blank_class.as_str())) {
                                "Choose blank letter"
                            }
                        }
                    }
                }
                button type=submit { "Submit turn" }
            }
            section id="turn-result" { span { "Pending placements are local until submitted." } }
        }
    }
    .into()
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
    let composer = turn_composer(game);
    let history = move_history_component(&game.history);
    let game_channel = format!("game:{}", game.game_id);
    let adjustments = game
        .final_score_adjustments
        .iter()
        .map(|(player, adjustment)| format!("{player:?}:{adjustment:+}"))
        .collect::<Vec<_>>()
        .join(" ");
    container! {
        div id="app-page" data-shared-state-channel=(game_channel.as_str())
            padding=32 gap=24 {
            header gap=8 {
                anchor href="/" { "Dashboard" }
                h1 { "Game " (game_id) }
                span { (state_label) }
            }
            main gap=16 {
                (board)
                (status)
                (rack)
                @if !game.completed {
                    (composer)
                }
                (history)
                section id="final-score-adjustments" {
                    h2 { "Final score adjustments" }
                    span { (adjustments) }
                }
                section id="game-live-state" data-shared-state-channel=(game_channel.as_str()) {
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
        div id="app-page" padding=32 gap=16 {
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
        redirect: None,
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
    fn turn_rejections_render_recoverable_product_guidance() {
        for (reason, expected) in [
            (
                "stale command revision: expected 2, actual 3",
                "changed in another tab",
            ),
            ("dictionary rejected word XYZ", "dictionary rejected"),
            (
                "new tiles must be placed in one row or one column",
                "not legal",
            ),
            ("the authenticated user is not a member", "not authorized"),
            ("it is not this player's turn", "not your turn"),
            ("the game is complete", "no longer accepts turns"),
        ] {
            let rendered = turn_rejection(reason)
                .display_to_string(false, false)
                .expect("error renders");
            assert!(rendered.contains(expected), "{rendered}");
            assert!(rendered.contains("game-error"));
        }
    }

    #[test]
    fn pending_move_forms_build_play_exchange_and_blank_commands() {
        let play = PendingMoveForm {
            command: "PLAY".to_string(),
            command_id: "play-command".to_string(),
            idempotency_key: "play-idempotency".to_string(),
            expected_revision: 1,
            tile_0: Some(3),
            tile_1: Some(4),
            tile_2: None,
            tile_3: None,
            tile_4: None,
            tile_5: None,
            tile_6: None,
            x_0: Some(7),
            x_1: Some(8),
            x_2: None,
            x_3: None,
            x_4: None,
            x_5: None,
            x_6: None,
            y_0: Some(7),
            y_1: Some(7),
            y_2: None,
            y_3: None,
            y_4: None,
            y_5: None,
            y_6: None,
            blank_0: Some("a".to_string()),
            blank_1: Some(String::new()),
            blank_2: None,
            blank_3: None,
            blank_4: None,
            blank_5: None,
            blank_6: None,
        }
        .game_command()
        .expect("play command builds");
        assert!(matches!(
            play,
            GameCommand::Play { placements }
                if placements[0].blank_letter == Some('A')
                    && placements[1].blank_letter.is_none()
        ));

        let exchange = PendingMoveForm {
            command: "EXCHANGE".to_string(),
            command_id: "exchange-command".to_string(),
            idempotency_key: "exchange-idempotency".to_string(),
            expected_revision: 1,
            tile_0: Some(3),
            tile_1: Some(4),
            tile_2: None,
            tile_3: None,
            tile_4: None,
            tile_5: None,
            tile_6: None,
            x_0: None,
            x_1: None,
            x_2: None,
            x_3: None,
            x_4: None,
            x_5: None,
            x_6: None,
            y_0: None,
            y_1: None,
            y_2: None,
            y_3: None,
            y_4: None,
            y_5: None,
            y_6: None,
            blank_0: None,
            blank_1: None,
            blank_2: None,
            blank_3: None,
            blank_4: None,
            blank_5: None,
            blank_6: None,
        }
        .game_command()
        .expect("exchange command builds");
        assert!(matches!(exchange, GameCommand::Exchange { tile_ids } if tile_ids.len() == 2));
    }

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
            assert!(dashboard.contains("new-game-actions"));
            assert!(dashboard.contains("name=\"action\" value=\"CHALLENGE\""));
            assert!(dashboard.contains("name=\"action\" value=\"CREATE_INVITATION\""));
            assert!(dashboard.contains("name=\"action\" value=\"REDEEM_INVITATION\""));

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
            assert!(page.contains("name=\"expected_revision\" value=\"1\""));
            assert!(page.contains("turn-composer"));
            assert!(page.contains("value=\"PLAY\""));
            assert!(page.contains("value=\"EXCHANGE\""));
            assert!(page.contains("value=\"PASS\""));
            assert!(page.contains("value=\"RESIGN\""));
            assert!(page.contains("name=\"command_id\""));
            assert!(page.contains("name=\"idempotency_key\""));
            assert!(page.contains("pending-editor-0"));
            assert!(page.contains("blank-letter-0"));
            assert!(page.contains("rack-order-0"));
            assert!(page.contains("rack-order-1"));
            assert!(page.contains("make it first in the local order"));
            assert!(page.contains("fx-click"));
            assert!(page.contains("Select"));
            assert!(page.contains("SetDisplay"));
            assert!(!page.contains("bag"));
        });
    }

    #[test]
    fn turn_route_uses_shared_dispatcher_and_publishes_authorized_updates() {
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
            let dispatcher = crate::GameSharedStateDispatcher::new(database.clone());
            let bob_context = AuthenticatedTransportContext {
                participant_id: ParticipantId::new(&bob),
                identity_binding: "bob-tab".to_string(),
            };
            let bob_updates = dispatcher
                .subscribe_channel(&bob_context, &crate::game_channel(game_id))
                .await
                .expect("Bob subscribes");
            let _ = bob_updates.recv_async().await.expect("initial update");

            let mut request =
                RouteRequest::from_path(&format!("/games/{game_id}/turn"), RequestInfo::default());
            request.method = "POST".parse().expect("POST parses");
            request.headers.insert(
                "content-type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            );
            request.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                session.expose().to_string(),
            );
            let state = crate::recover_game(&*database, game_id)
                .await
                .expect("game loads");
            let alice_player = crate::player_for_user(&*database, game_id, &alice)
                .await
                .expect("Alice is seated");
            let tile_id = state.racks[&alice_player][0].id.get();
            request.body = Some(std::sync::Arc::new(
                format!(
                    "command=EXCHANGE&command_id=exchange-1&idempotency_key=exchange-idem-1&expected_revision={}&tile_0={tile_id}",
                    state.revision
                )
                .into(),
            ));

            let response =
                game_turn_route(&dispatcher, &*database, &request, &game_id.to_string(), now)
                    .await
                    .display_to_string(false, false)
                    .expect("turn response renders");
            assert!(response.contains("game-board"), "{response}");
            assert!(response.contains(&format!("data-revision=\"{}\"", state.revision + 1)));
            let update = bob_updates.recv_async().await.expect("Bob receives update");
            let projected = dispatcher
                .project_event(&bob_context, &update)
                .expect("Bob update is authorized");
            let view: crate::GameView = projected.payload.deserialize().expect("view decodes");
            assert_eq!(view.revision, state.revision + 1);
        });
    }

    #[test]
    fn dashboard_action_route_creates_and_accepts_challenges_without_trusted_user_ids() {
        block_on(async {
            let database = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens");
            migrate_app(&*database).await.expect("migrations run");
            let now = OffsetDateTime::UNIX_EPOCH;
            let alice = register(&*database, "alice", "correct horse battery staple", now)
                .await
                .expect("Alice registers");
            let bob = register(&*database, "bob", "another correct horse battery", now)
                .await
                .expect("Bob registers");
            let alice_session = create_session(&*database, &alice, now, Duration::days(1))
                .await
                .expect("Alice session creates");
            let bob_session = create_session(&*database, &bob, now, Duration::days(1))
                .await
                .expect("Bob session creates");

            let mut challenge =
                RouteRequest::from_path("/dashboard/action", RequestInfo::default());
            challenge.method = "POST".parse().expect("POST parses");
            challenge.headers.insert(
                "content-type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            );
            challenge.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                alice_session.expose().to_string(),
            );
            challenge.body = Some(std::sync::Arc::new("action=CHALLENGE&username=bob".into()));
            let alice_dashboard = dashboard_action_route(&*database, &challenge, now)
                .await
                .display_to_string(false, false)
                .expect("dashboard renders");
            assert!(alice_dashboard.contains("OUTGOING"));

            let bob_dashboard = load_authenticated_dashboard(
                &*database,
                &std::collections::BTreeMap::from([(
                    SESSION_COOKIE_NAME.to_string(),
                    bob_session.expose().to_string(),
                )]),
                now,
            )
            .await
            .expect("Bob dashboard loads");
            let challenge_id = bob_dashboard.projection.pending[0].id.clone();
            let mut accept = challenge;
            accept.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                bob_session.expose().to_string(),
            );
            accept.body = Some(std::sync::Arc::new(
                format!("action=ACCEPT_CHALLENGE&challenge_id={challenge_id}").into(),
            ));
            let accepted = dashboard_action_route(&*database, &accept, now)
                .await
                .display_to_string(false, false)
                .expect("dashboard renders");
            assert!(accepted.contains("class=\"game-summary\""));
            assert!(accepted.contains("Your turn") || accepted.contains("ACTIVE"));
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
            assert_eq!(response.response.redirect, None);
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
        assert_eq!(signed_in.redirect, None);
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
