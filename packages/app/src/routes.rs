//! Renderer-neutral routed product pages backed by durable presentation services.

use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use hyperchad::{
    actions::ActionType,
    renderer::{Content, ResponseCookie, ResponseMetadata, View},
    router::{Container, RoutePath, RouteRequest, Router},
    shared_state_models::{
        CommandEnvelope, CommandId, IdempotencyKey, ParticipantId, PayloadBlob, Revision,
        TransportInbound, TransportOutbound,
    },
    shared_state_transport::{AuthenticatedTransportContext, SharedStateTransportDispatcher as _},
    template::{LayoutOverflow, container},
};
use serde::Deserialize;
use switchy_database::Database;
use time::{Duration, OffsetDateTime};
use wwmtf_game_domain::{
    Coordinate, GameCommand, GameError, Placement, PlacementGuidance, PremiumSquare, TileId,
};

use crate::{
    AuthenticatedDashboard, AuthorizedGamePage, GoogleOidcClient, OidcAttemptPurpose,
    PresentationError, ProductWorkflowError, UserScoreTotals, accept_pending_challenge,
    cancel_pending_challenge, challenge_username, claim_oidc_attempt, cleanup_oidc_attempts,
    clear_move_plan, complete_legacy_google_migration, consume_oidc_attempt, create_oidc_attempt,
    create_shareable_invitation, decline_pending_challenge, error_component,
    find_or_create_development_user, google_login_and_create_session, load_authenticated_dashboard,
    load_authorized_game_page, load_move_plan, logout_session, move_history_component,
    redeem_shareable_invitation, redeem_shareable_invitation_by_id, revoke_shareable_invitation,
    save_move_plan,
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
    #[serde(default)]
    display_name: String,
}

#[derive(Debug)]
enum DashboardActionSuccess {
    Updated,
    InvitationCreated {
        invitation_id: String,
        token: String,
    },
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

async fn refreshed_dashboard(
    database: &dyn Database,
    request: &RouteRequest,
    now: OffsetDateTime,
) -> Container {
    load_authenticated_dashboard(database, &request.cookies, now)
        .await
        .map_or_else(
            |_| error_component("The profile changed, but the dashboard could not be refreshed."),
            |dashboard| dashboard_page(&dashboard),
        )
}

#[derive(Debug, Deserialize)]
struct AvatarUploadForm {
    avatar: String,
}

async fn custom_avatar_route(
    database: &dyn Database,
    request: &RouteRequest,
    now: OffsetDateTime,
) -> Container {
    let Ok(user_id) = crate::authenticated_user(database, &request.cookies, now).await else {
        return error_component("Your session expired. Sign in and try again.");
    };
    if request.method.as_ref() != "POST" {
        return error_component("Profile picture uploads require a POST request.");
    }
    let form: AvatarUploadForm = match request.parse_form() {
        Ok(form) => form,
        Err(_) => return error_component("Choose a profile picture to upload."),
    };
    let Ok(bytes) = BASE64.decode(form.avatar) else {
        return error_component("The profile picture upload was invalid.");
    };
    match crate::set_custom_avatar(database, &user_id, &bytes, now).await {
        Ok(_) => refreshed_dashboard(database, request, now).await,
        Err(_) => error_component("The profile picture was invalid or could not be saved."),
    }
}

#[allow(clippy::too_many_lines)]
async fn dashboard_action_route(
    database: &dyn Database,
    dispatcher: &crate::GameSharedStateDispatcher,
    public_base_url: &str,
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
    let result: Result<DashboardActionSuccess, ProductWorkflowError> = match form.action.as_str() {
        "CHALLENGE" => challenge_username(database, &user_id, &form.username, now)
            .await
            .map(|_| DashboardActionSuccess::Updated),
        "ACCEPT_CHALLENGE" => accept_pending_challenge(database, &form.challenge_id, &user_id, now)
            .await
            .map(|_| DashboardActionSuccess::Updated),
        "DECLINE_CHALLENGE" => {
            decline_pending_challenge(database, &form.challenge_id, &user_id, now)
                .await
                .map(|()| DashboardActionSuccess::Updated)
        }
        "CANCEL_CHALLENGE" => cancel_pending_challenge(database, &form.challenge_id, &user_id, now)
            .await
            .map(|()| DashboardActionSuccess::Updated),
        "CREATE_INVITATION" => {
            create_shareable_invitation(database, &user_id, now, Duration::days(30))
                .await
                .map(
                    |(invitation_id, token)| DashboardActionSuccess::InvitationCreated {
                        invitation_id,
                        token: token.expose().to_string(),
                    },
                )
        }
        "REDEEM_INVITATION" => {
            redeem_shareable_invitation(database, &form.invitation_token, &user_id, now)
                .await
                .map(|_| DashboardActionSuccess::Updated)
        }
        "REVOKE_INVITATION" => {
            revoke_shareable_invitation(database, &form.invitation_id, &user_id, now)
                .await
                .map(|()| DashboardActionSuccess::Updated)
        }
        "SET_DISPLAY_NAME" => {
            return match crate::set_custom_display_name(database, &user_id, &form.display_name, now)
                .await
            {
                Ok(()) => refreshed_dashboard(database, request, now).await,
                Err(_) => error_component("The display name was invalid or could not be saved."),
            };
        }
        "USE_GOOGLE_NAME" => {
            return match crate::use_google_display_name(database, &user_id, now).await {
                Ok(()) => refreshed_dashboard(database, request, now).await,
                Err(_) => error_component("Google name synchronization could not be restored."),
            };
        }
        "REMOVE_AVATAR" => {
            return match crate::remove_custom_avatar(database, &user_id, now).await {
                Ok(()) => refreshed_dashboard(database, request, now).await,
                Err(_) => error_component("The profile picture could not be removed."),
            };
        }
        "USE_GOOGLE_AVATAR" => {
            return match crate::use_google_avatar(database, &user_id, now).await {
                Ok(()) => refreshed_dashboard(database, request, now).await,
                Err(_) => error_component("Google picture synchronization could not be restored."),
            };
        }
        _ => return error_component("The dashboard action is unknown."),
    };
    let success = match result {
        Ok(success) => success,
        Err(error) => return error_component(product_error_message(&error)),
    };
    let publish_dashboard_refresh = matches!(success, DashboardActionSuccess::Updated);
    let dashboard = load_authenticated_dashboard(database, &request.cookies, now).await;
    // A newly generated invitation secret exists only in this response. Broadcasting an immediate
    // generic dashboard refresh to the creating tab could replace it before the user can share it.
    if publish_dashboard_refresh
        && dispatcher
            .refresh_dashboard_subscribers(
                now.unix_timestamp_nanos().try_into().unwrap_or(i64::MAX),
            )
            .await
            .is_err()
    {
        #[cfg(feature = "metrics")]
        crate::observability::record_database_failure("refresh_dashboard_subscribers");
    }
    match dashboard {
        Ok(dashboard) => match success {
            DashboardActionSuccess::Updated => dashboard_page(&dashboard),
            DashboardActionSuccess::InvitationCreated {
                invitation_id,
                token,
            } => {
                dashboard_page_with_invitation(&dashboard, &invitation_id, &token, public_base_url)
            }
        },
        Err(PresentationError::Unauthenticated) => {
            error_component("Your session expired. Sign in and review your dashboard.")
        }
        Err(_) => {
            error_component("The action succeeded, but the dashboard could not be refreshed.")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize, Default)]
struct TurnDraft {
    selected_tile: Option<u16>,
    selected_blank_letter: Option<char>,
    placements: Vec<DraftPlacement>,
    #[serde(default)]
    rack_tile: Option<u16>,
    #[serde(default)]
    exchange_tiles: Vec<u16>,
    #[serde(default)]
    mode: TurnMode,
    #[serde(default)]
    board_zoom: BoardZoom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
enum BoardZoom {
    Fit,
    Compact,
    #[default]
    Normal,
    Large,
}

impl BoardZoom {
    const fn square_size(self) -> u32 {
        match self {
            Self::Fit => 20,
            Self::Compact => 28,
            Self::Normal => 44,
            Self::Large => 56,
        }
    }

    const fn zoom_out(self) -> Self {
        match self {
            Self::Fit | Self::Compact => Self::Fit,
            Self::Normal => Self::Compact,
            Self::Large => Self::Normal,
        }
    }

    const fn zoom_in(self) -> Self {
        match self {
            Self::Fit => Self::Compact,
            Self::Compact => Self::Normal,
            Self::Normal | Self::Large => Self::Large,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
enum TurnMode {
    #[default]
    Play,
    Exchange,
    ConfirmExchange,
    ConfirmPass,
    ConfirmResign,
}

impl TurnDraft {
    fn has_composed_turn_input(&self) -> bool {
        self.selected_blank_letter.is_some()
            || !self.placements.is_empty()
            || !self.exchange_tiles.is_empty()
            || self.mode != TurnMode::Play
    }

    fn begin_action(&mut self, action: &str) {
        if !matches!(
            action,
            "PICK_RACK_TILE"
                | "SWAP_RACK_TILES"
                | "SHUFFLE_RACK"
                | "ZOOM_OUT"
                | "ZOOM_RESET"
                | "ZOOM_IN"
        ) {
            self.rack_tile = None;
        }
    }

    fn domain_placements(&self) -> Vec<Placement> {
        self.placements
            .iter()
            .map(|placement| Placement {
                tile_id: TileId::new(placement.tile_id),
                coordinate: Coordinate::new(placement.x, placement.y),
                blank_letter: placement.blank_letter,
            })
            .collect()
    }
}

fn is_zoom_action(action: &str) -> bool {
    matches!(action, "ZOOM_OUT" | "ZOOM_RESET" | "ZOOM_IN")
}

fn rack_action(draft: &TurnDraft) -> &'static str {
    if draft.mode == TurnMode::Exchange {
        "TOGGLE_EXCHANGE"
    } else if draft.rack_tile.is_some() {
        "SWAP_RACK_TILES"
    } else {
        "PICK_RACK_TILE"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DraftFeedback {
    candidate: Option<wwmtf_game_domain::CandidatePlayAnalysis>,
    guidance: PlacementGuidance,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
struct DraftPlacement {
    tile_id: u16,
    x: u8,
    y: u8,
    blank_letter: Option<char>,
}

#[derive(Debug, Deserialize)]
struct ComposeTurnForm {
    action: String,
    expected_revision: u64,
    draft: String,
    #[serde(default)]
    tile_id: Option<u16>,
    #[serde(default)]
    x: Option<u8>,
    #[serde(default)]
    y: Option<u8>,
    #[serde(default)]
    letter: Option<char>,
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

#[derive(Debug, Deserialize)]
struct DevelopmentLoginForm {
    username: String,
    #[serde(default)]
    invitation_token: String,
}

/// Account credential form accepted by renderer-neutral routes.
#[derive(Debug, Deserialize)]
struct GoogleCallbackQuery {
    code: String,
    state: String,
}

fn google_callback_query(request: &RouteRequest) -> Result<GoogleCallbackQuery, ()> {
    serde_json::from_value(serde_json::to_value(&request.query).unwrap_or_default()).map_err(|_| ())
}

#[derive(Debug, Deserialize)]
struct MigrationForm {
    username: String,
    password: String,
}

const fn profile_error_reason(error: &crate::ProfileError) -> &'static str {
    match error {
        crate::ProfileError::InvalidImageUrl => "invalid_url",
        crate::ProfileError::InvalidImage => "invalid_image",
        crate::ProfileError::Http(_) => "provider_unavailable",
        crate::ProfileError::Database(_) => "storage_unavailable",
        _ => "profile_invalid",
    }
}

#[allow(clippy::manual_let_else)]
async fn google_start_route(
    database: &dyn Database,
    oidc: &GoogleOidcClient,
    request: &RouteRequest,
    now: OffsetDateTime,
    secure_cookies: bool,
) -> View {
    let continuation_invitation_id = match request.query.get("invite") {
        Some(token) => match crate::active_invitation_id(database, token, now).await {
            Ok(invitation_id) => Some(invitation_id),
            Err(_) => {
                return View::from(login_page(Some(
                    "That invitation is invalid, expired, revoked, or already used.",
                )));
            }
        },
        None => None,
    };
    let _ = cleanup_oidc_attempts(database, now).await;
    let attempt = match create_oidc_attempt(
        database,
        OidcAttemptPurpose::Login,
        None,
        continuation_invitation_id.as_deref(),
        now,
        Duration::minutes(10),
    )
    .await
    {
        Ok(attempt) => attempt,
        Err(_) => {
            return View::from(login_page(Some(
                "Google sign-in could not be started. Please try again.",
            )));
        }
    };
    let mut binding = ResponseCookie::secure(
        crate::OIDC_BINDING_COOKIE_NAME,
        attempt.browser_binding.clone(),
    );
    binding.same_site = hyperchad::renderer::SameSite::Lax;
    binding.secure = secure_cookies;
    View::builder()
        .with_primary(login_page(None))
        .with_response(ResponseMetadata {
            cookies: vec![binding],
            navigation: Some(
                hyperchad::renderer::ResponseNavigation::external(oidc.authorization_url(&attempt))
                    .expect("OIDC authorization URL is a validated external URL"),
            ),
        })
        .build()
}

#[allow(clippy::manual_let_else)]
async fn google_callback_route(
    database: &dyn Database,
    oidc: &GoogleOidcClient,
    request: &RouteRequest,
    now: OffsetDateTime,
    csrf_token: &str,
    secure_cookies: bool,
) -> View {
    let query = match google_callback_query(request) {
        Ok(query) => query,
        Err(()) => return View::from(login_page(Some("Google sign-in response was invalid."))),
    };
    let Some(binding) = request.cookies.get(crate::OIDC_BINDING_COOKIE_NAME) else {
        return View::from(login_page(Some("Google sign-in session expired.")));
    };
    let attempt = match claim_oidc_attempt(database, &query.state, binding, now).await {
        Ok(attempt) => attempt,
        Err(_) => return View::from(login_page(Some("Google sign-in session expired."))),
    };
    let identity = match oidc.exchange_callback(&query.code, &attempt).await {
        Ok(identity) => identity,
        Err(_) => return View::from(login_page(Some("Google sign-in could not be verified."))),
    };
    let result = match attempt.purpose {
        OidcAttemptPurpose::Login => {
            google_login_and_create_session(database, &identity, now, Duration::days(30))
                .await
                .map(|(_, session)| session)
        }
        OidcAttemptPurpose::MigratePassword => {
            let Some(user_id) = attempt.existing_user_id.as_deref() else {
                return View::from(login_page(Some("Account migration session is invalid.")));
            };
            complete_legacy_google_migration(database, user_id, &identity, now, Duration::days(30))
                .await
        }
    };
    let (session, user_id) = match result {
        Ok(session) => {
            let user_id = match crate::resolve_session(database, session.expose(), now).await {
                Ok(user_id) => user_id,
                Err(_) => {
                    return View::from(login_page(Some("Google sign-in could not be completed.")));
                }
            };
            (session, user_id)
        }
        Err(_) => return View::from(login_page(Some("Google account could not be connected."))),
    };
    let joined_game = match attempt.continuation_invitation_id.as_deref() {
        Some(invitation_id) => {
            match redeem_shareable_invitation_by_id(database, invitation_id, &user_id, now).await {
                Ok(game_id) => Some(game_id),
                Err(_) => {
                    return View::from(login_page(Some(
                        "That invitation is invalid, expired, revoked, or already used.",
                    )));
                }
            }
        }
        None => None,
    };
    if let Some(picture_url) = identity.picture_url.as_deref()
        && let Ok(bytes) = oidc
            .download_avatar(picture_url, std::time::Duration::from_secs(5))
            .await
        && let Err(error) = crate::set_google_avatar(database, &user_id, &bytes, now).await
    {
        log::warn!(target: "wwmtf::profiles", "google_avatar_sync_failed reason={}", profile_error_reason(&error));
    }
    if consume_oidc_attempt(database, &attempt.attempt_id, now)
        .await
        .is_err()
    {
        return View::from(login_page(Some("Google sign-in could not be completed.")));
    }
    let mut response = authenticated_session_response(session.expose(), csrf_token, secure_cookies);
    let mut binding = ResponseCookie::expired(crate::OIDC_BINDING_COOKIE_NAME);
    binding.same_site = hyperchad::renderer::SameSite::Lax;
    binding.secure = secure_cookies;
    response.cookies.push(binding);
    response.navigation = Some(
        hyperchad::renderer::ResponseNavigation::internal("/")
            .expect("dashboard is a valid internal path"),
    );
    let primary = match joined_game {
        Some(game_id) => invitation_joined_page(game_id),
        None => dashboard_after_authentication(database, session.expose(), "", now).await,
    };
    View::builder()
        .with_primary(primary)
        .with_response(response)
        .build()
}

#[allow(clippy::manual_let_else)]
async fn migration_start_route(
    database: &dyn Database,
    oidc: &GoogleOidcClient,
    request: &RouteRequest,
    now: OffsetDateTime,
    secure_cookies: bool,
) -> View {
    if request.method.as_ref() != "POST" {
        return View::from(migration_page(None));
    }
    let form: MigrationForm = match request.parse_form() {
        Ok(form) => form,
        Err(_) => return View::from(migration_page(Some("Enter your existing credentials."))),
    };
    let user_id = match crate::prove_legacy_password_account(
        database,
        &form.username,
        &form.password,
    )
    .await
    {
        Ok(user_id) => user_id,
        Err(_) => return View::from(migration_page(Some("Existing credentials are incorrect."))),
    };
    let attempt = match create_oidc_attempt(
        database,
        OidcAttemptPurpose::MigratePassword,
        Some(&user_id),
        None,
        now,
        Duration::minutes(10),
    )
    .await
    {
        Ok(attempt) => attempt,
        Err(_) => return View::from(migration_page(Some("Migration could not be started."))),
    };
    let mut binding = ResponseCookie::secure(
        crate::OIDC_BINDING_COOKIE_NAME,
        attempt.browser_binding.clone(),
    );
    binding.same_site = hyperchad::renderer::SameSite::Lax;
    binding.secure = secure_cookies;
    View::builder()
        .with_primary(migration_page(None))
        .with_response(ResponseMetadata {
            cookies: vec![binding],
            navigation: Some(
                hyperchad::renderer::ResponseNavigation::external(oidc.authorization_url(&attempt))
                    .expect("OIDC authorization URL is a validated external URL"),
            ),
        })
        .build()
}

async fn login_route(
    database: &dyn Database,
    request: &RouteRequest,
    now: OffsetDateTime,
    csrf_token: &str,
    secure_cookies: bool,
    development_login: bool,
    google_login: bool,
) -> View {
    let invitation_token = request.query.get("invite").map_or("", String::as_str);
    if request.method.as_ref() != "POST" {
        return View::from(login_page_with_invitation(
            None,
            invitation_token,
            development_login,
            google_login,
        ));
    }
    if !development_login {
        return View::from(login_page_with_invitation(
            Some("Local username login is available only in development mode."),
            invitation_token,
            false,
            google_login,
        ));
    }
    let form: DevelopmentLoginForm = match request.parse_form() {
        Ok(form) => form,
        Err(_) => {
            return View::from(login_page_with_invitation(
                Some("Enter a username."),
                invitation_token,
                true,
                google_login,
            ));
        }
    };
    let user_id = match find_or_create_development_user(database, &form.username, now).await {
        Ok(user_id) => user_id,
        Err(crate::AccountError::InvalidUsername) => {
            return View::from(login_page_with_invitation(
                Some("Username must be 3–32 letters, numbers, underscores, or hyphens."),
                &form.invitation_token,
                true,
                google_login,
            ));
        }
        Err(_) => {
            return View::from(login_page_with_invitation(
                Some("Local login could not be completed."),
                &form.invitation_token,
                true,
                google_login,
            ));
        }
    };
    let Ok(session) = crate::create_session(database, &user_id, now, Duration::days(30)).await
    else {
        return View::from(login_page_with_invitation(
            Some("Local login could not be completed."),
            &form.invitation_token,
            true,
            google_login,
        ));
    };
    let dashboard =
        dashboard_after_authentication(database, session.expose(), &form.invitation_token, now)
            .await;
    View::builder()
        .with_primary(dashboard)
        .with_response(authenticated_session_response(
            session.expose(),
            csrf_token,
            secure_cookies,
        ))
        .build()
}

fn register_route(development_login: bool, google_login: bool) -> View {
    View::from(login_page_with_invitation(
        if development_login {
            Some("Enter any username to create or resume a local development account.")
        } else {
            Some("New accounts are created automatically when you continue with Google.")
        },
        "",
        development_login,
        google_login,
    ))
}

async fn logout_route(
    database: &dyn Database,
    request: &RouteRequest,
    now: OffsetDateTime,
    secure_cookies: bool,
) -> View {
    if request.method.as_ref() != "POST" {
        return View::from(logout_page());
    }
    if let Some(session) = request.cookies.get(crate::SESSION_COOKIE_NAME)
        && logout_session(database, session, now).await.is_err()
    {
        #[cfg(feature = "metrics")]
        crate::observability::record_database_failure("logout_session");
        return View::from(product_error_page(
            "Unable to sign out",
            "Your session could not be revoked. Please try again.",
        ));
    }
    View::builder()
        .with_primary(signed_out_page())
        .with_response(logged_out_response(secure_cookies))
        .build()
}

async fn dashboard_after_authentication(
    database: &dyn Database,
    session: &str,
    invitation_token: &str,
    now: OffsetDateTime,
) -> Container {
    if invitation_token.is_empty() {
        return dashboard_refresh_page(database, session, now).await;
    }
    let cookies = std::collections::BTreeMap::from([(
        crate::SESSION_COOKIE_NAME.to_string(),
        session.to_string(),
    )]);
    let Ok(user_id) = crate::authenticated_user(database, &cookies, now).await else {
        return product_error_page(
            "Unable to join game",
            "Your new session could not be verified. Sign in and open the invitation again.",
        );
    };
    match redeem_shareable_invitation(database, invitation_token, &user_id, now).await {
        Ok(game_id) => invitation_joined_page(game_id),
        Err(error) => product_error_page("Invitation unavailable", product_error_message(&error)),
    }
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

fn draft_token(draft: &TurnDraft) -> String {
    serde_json::to_vec(draft)
        .expect("turn draft is serializable")
        .into_iter()
        .fold(String::new(), |mut encoded, byte| {
            use std::fmt::Write as _;
            write!(encoded, "{byte:02x}").expect("writing to a string cannot fail");
            encoded
        })
}

fn parse_draft(token: &str) -> Option<TurnDraft> {
    if !token.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..token.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&token[index..index + 2], 16).ok())
        .collect::<Option<Vec<_>>>()?;
    serde_json::from_slice(&bytes).ok()
}

fn reconcile_draft(game: &AuthorizedGamePage, draft: &mut TurnDraft) -> bool {
    let rack = game
        .view
        .rack
        .iter()
        .map(|(tile_id, _, _)| *tile_id)
        .collect::<std::collections::BTreeSet<_>>();
    let occupied = game
        .view
        .board
        .iter()
        .map(|(coordinate, _, _)| *coordinate)
        .collect::<std::collections::BTreeSet<_>>();
    let previous = draft.placements.len();
    draft.placements.retain(|placement| {
        rack.contains(&placement.tile_id)
            && !occupied.contains(&Coordinate::new(placement.x, placement.y))
    });
    if draft
        .selected_tile
        .is_some_and(|tile_id| !rack.contains(&tile_id))
    {
        draft.selected_tile = None;
        draft.selected_blank_letter = None;
    }
    if draft
        .rack_tile
        .is_some_and(|tile_id| !rack.contains(&tile_id))
    {
        draft.rack_tile = None;
    }
    draft
        .exchange_tiles
        .retain(|tile_id| rack.contains(tile_id));
    if game.view.active_player != game.viewer_player && draft.mode != TurnMode::Play {
        draft.mode = TurnMode::Play;
        draft.exchange_tiles.clear();
    }
    previous != draft.placements.len()
}

async fn persist_draft(
    database: &dyn Database,
    game: &AuthorizedGamePage,
    draft: &TurnDraft,
    now: OffsetDateTime,
) -> Result<(), crate::MovePlanError> {
    if draft.has_composed_turn_input() {
        save_move_plan(
            database,
            game.game_id,
            &game.user_id,
            &draft_token(draft),
            game.view.revision,
            i64::try_from(now.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX),
        )
        .await
    } else {
        clear_move_plan(database, game.game_id, &game.user_id).await
    }
}

fn draft_feedback(game: &AuthorizedGamePage, draft: &TurnDraft) -> DraftFeedback {
    let placements = draft.domain_placements();
    let guidance = game
        .placement_guidance(&placements)
        .unwrap_or(PlacementGuidance {
            required: std::collections::BTreeSet::new(),
            eligible: std::collections::BTreeSet::new(),
        });
    if placements.is_empty() {
        let message = if game.view.board.is_empty() {
            "Start by covering the starred center square."
        } else {
            "Choose a rack tile, then place it on a highlighted connecting square."
        };
        return DraftFeedback {
            candidate: None,
            guidance,
            message: message.to_string(),
        };
    }
    match game.analyze_candidate_play(&placements) {
        Ok(candidate) => {
            let message = if candidate.is_valid() {
                if game.view.active_player == game.viewer_player {
                    "This draft is ready to play.".to_string()
                } else {
                    "Plan ready. You can play it when your turn begins if the board still allows it."
                        .to_string()
                }
            } else {
                format!(
                    "The dictionary does not accept: {}.",
                    candidate.invalid_words.join(", ")
                )
            };
            DraftFeedback {
                candidate: Some(candidate),
                guidance,
                message,
            }
        }
        Err(error) => DraftFeedback {
            candidate: None,
            guidance,
            message: draft_analysis_message(&error),
        },
    }
}

fn draft_analysis_message(error: &GameError) -> String {
    match error {
        GameError::EmptyTileSelection => "Choose at least one rack tile.".to_string(),
        GameError::NotLinear => "Keep all drafted tiles in one row or one column.".to_string(),
        GameError::Gap => "Fill the highlighted gap before playing this word.".to_string(),
        GameError::FirstMoveMustCoverStart => {
            "The first word must cover the highlighted center square.".to_string()
        }
        GameError::Disconnected => "Connect this draft to a tile already on the board.".to_string(),
        GameError::InvalidWords(words) => {
            format!("The dictionary does not accept: {}.", words.join(", "))
        }
        GameError::InvalidBlankLetter => {
            "Choose a letter for every drafted blank tile.".to_string()
        }
        error => error.to_string(),
    }
}

fn invalid_words_message(words: &[String]) -> String {
    if words.len() == 1 {
        format!("{} is not a valid word", words[0])
    } else {
        format!("{} are not valid words", words.join(", "))
    }
}

fn draft_feedback_component(
    game: &AuthorizedGamePage,
    feedback: &DraftFeedback,
    draft: &TurnDraft,
) -> Container {
    let candidate_valid = feedback
        .candidate
        .as_ref()
        .is_some_and(wwmtf_game_domain::CandidatePlayAnalysis::is_valid);
    let message = match draft.mode {
        TurnMode::Exchange => format!(
            "{} tile(s) selected for exchange",
            draft.exchange_tiles.len()
        ),
        TurnMode::ConfirmExchange => {
            format!("Exchange {} selected tile(s)?", draft.exchange_tiles.len())
        }
        TurnMode::ConfirmPass => "Pass this turn?".to_string(),
        TurnMode::ConfirmResign => "Resign this game?".to_string(),
        TurnMode::Play => feedback.candidate.as_ref().map_or_else(
            || feedback.message.clone(),
            |candidate| {
                let words = candidate
                    .play
                    .words
                    .iter()
                    .map(|word| word.text.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                if candidate_valid {
                    let status = if game.view.active_player == game.viewer_player {
                        "ready to play"
                    } else {
                        "planned for your turn"
                    };
                    format!("{words} · {} points · {status}", candidate.play.score)
                } else {
                    invalid_words_message(&candidate.invalid_words)
                }
            },
        ),
    };
    container! {
        section id="draft-preview" class="dock-message" width="100%" direction="row"
            align-items="center" justify-content="center" padding-y="4px" padding-x="8px"
            background=(if candidate_valid { "#f4c95d" } else if feedback.candidate.is_some() { "#f7d8ae" } else { "#214c38" })
            color=(if feedback.candidate.is_some() { "#2d2515" } else { "#f4f0df" })
            border=((if candidate_valid { "#ffe29a" } else if feedback.candidate.is_some() { "#d77a59" } else { "#376d53" }, 1))
            border-radius="10px" overflow-x="hidden" {
            span font-size="13px" font-weight=bold text-overflow="ellipsis" white-space="preserve" {
                (message)
            }
        }
    }
    .into()
}

fn draft_error_page(game: &AuthorizedGamePage, draft: &TurnDraft, message: &str) -> Container {
    visual_game_page(game, draft, Some(message))
}

#[allow(clippy::too_many_lines)]
async fn game_compose_route(
    database: &dyn Database,
    request: &RouteRequest,
    game_id: &str,
    now: OffsetDateTime,
) -> Container {
    let game = match load_authorized_game_page(database, &request.cookies, game_id, now).await {
        Ok(game) => game,
        Err(PresentationError::Unauthenticated) => return signed_out_page(),
        Err(error @ PresentationError::Forbidden) => {
            #[cfg(feature = "metrics")]
            {
                let request_id = request
                    .headers
                    .get("x-request-id")
                    .map_or("missing", String::as_str);
                crate::observability::record_compose_authorization_failure("forbidden", request_id);
            }
            return product_error_page("Unable to compose turn", &error.to_string());
        }
        Err(error) => return product_error_page("Unable to compose turn", &error.to_string()),
    };
    let form: ComposeTurnForm = match request.parse_form() {
        Ok(form) => form,
        Err(_) => {
            return draft_error_page(
                &game,
                &TurnDraft::default(),
                "The turn selection was incomplete.",
            );
        }
    };
    if form.expected_revision != game.view.revision {
        let mut draft = load_move_plan(database, game.game_id, &game.user_id)
            .await
            .ok()
            .flatten()
            .and_then(|(payload, _)| parse_draft(&payload))
            .unwrap_or_default();
        let removed_conflicts = reconcile_draft(&game, &mut draft);
        let _ = persist_draft(database, &game, &draft, now).await;
        let message = if removed_conflicts {
            "The board changed. Conflicting planned tiles returned to your rack; the rest of your plan was preserved and rescored."
        } else {
            "The board changed. Your plan was preserved and rescored against the latest board."
        };
        return draft_error_page(&game, &draft, message);
    }
    let mut draft = parse_draft(&form.draft).unwrap_or_default();
    draft.begin_action(&form.action);
    if game.completed && !is_zoom_action(&form.action) {
        return visual_game_page(&game, &draft, Some("This game is complete."));
    }
    let viewer_turn = game.view.active_player == game.viewer_player;
    if !viewer_turn
        && matches!(
            form.action.as_str(),
            "BEGIN_EXCHANGE"
                | "TOGGLE_EXCHANGE"
                | "REVIEW_EXCHANGE"
                | "CONFIRM_PASS"
                | "CONFIRM_RESIGN"
        )
    {
        return draft_error_page(
            &game,
            &draft,
            "You can plan tile placements while you wait, but turn-ending actions remain unavailable.",
        );
    }
    match form.action.as_str() {
        "CHOOSE_TILE" => {
            let Some(tile_id) = form.tile_id else {
                return draft_error_page(&game, &draft, "Choose a rack tile.");
            };
            if !game.view.rack.iter().any(|(id, _, _)| *id == tile_id) {
                return draft_error_page(&game, &draft, "That tile is not in your rack.");
            }
            draft.selected_tile = Some(tile_id);
            draft.selected_blank_letter = None;
        }
        "CHOOSE_BLANK_LETTER" => {
            let Some(letter) = form.letter.map(|letter| letter.to_ascii_uppercase()) else {
                return draft_error_page(&game, &draft, "Choose a letter for the blank tile.");
            };
            if !letter.is_ascii_uppercase() {
                return draft_error_page(&game, &draft, "Blank tiles must represent A through Z.");
            }
            draft.selected_blank_letter = Some(letter);
        }
        "CANCEL_TILE_PICK" => {
            draft.selected_tile = None;
            draft.selected_blank_letter = None;
            draft.rack_tile = None;
        }
        "PLACE_TILE" => {
            let Some(tile_id) = draft.selected_tile else {
                return draft_error_page(
                    &game,
                    &draft,
                    "Choose a rack tile, then choose a board square.",
                );
            };
            let (Some(x), Some(y)) = (form.x, form.y) else {
                return draft_error_page(&game, &draft, "Choose a board square.");
            };
            let coordinate = Coordinate::new(x, y);
            if game
                .view
                .board
                .iter()
                .any(|(placed, _, _)| *placed == coordinate)
            {
                return draft_error_page(
                    &game,
                    &draft,
                    "That square already contains a played tile.",
                );
            }
            if let Some(other) = draft
                .placements
                .iter()
                .find(|placed| placed.x == x && placed.y == y && placed.tile_id != tile_id)
            {
                return draft_error_page(
                    &game,
                    &draft,
                    &format!(
                        "That square already contains pending tile {}.",
                        other.tile_id
                    ),
                );
            }
            let is_blank = game
                .view
                .rack
                .iter()
                .any(|(id, letter, _)| *id == tile_id && *letter == ' ');
            let blank_letter = draft.selected_blank_letter;
            if is_blank && blank_letter.is_none() {
                return draft_error_page(
                    &game,
                    &draft,
                    "Choose a letter for the blank tile before placing it.",
                );
            }
            draft.placements.retain(|placed| placed.tile_id != tile_id);
            draft.placements.push(DraftPlacement {
                tile_id,
                x,
                y,
                blank_letter: is_blank.then_some(blank_letter.unwrap_or('A')),
            });
            draft.selected_tile = None;
            draft.selected_blank_letter = None;
        }
        "REMOVE_TILE" => {
            let Some(tile_id) = form.tile_id else {
                return draft_error_page(
                    &game,
                    &draft,
                    "Choose a pending tile to return to your rack.",
                );
            };
            draft.placements.retain(|placed| placed.tile_id != tile_id);
            draft.selected_tile = Some(tile_id);
            draft.selected_blank_letter = None;
        }
        "PICK_RACK_TILE" => {
            let Some(tile_id) = form.tile_id else {
                return draft_error_page(&game, &draft, "Choose a rack tile to reposition.");
            };
            if !game.rack_order.contains(&tile_id) {
                return draft_error_page(&game, &draft, "That tile is not in your rack.");
            }
            draft.rack_tile = Some(tile_id);
            draft.selected_tile = Some(tile_id);
        }
        "SWAP_RACK_TILES" => {
            let selected_tile = draft.rack_tile;
            let (Some(selected_tile), Some(target_tile)) = (selected_tile, form.tile_id) else {
                return draft_error_page(&game, &draft, "Choose two rack tiles to swap.");
            };
            if selected_tile == target_tile {
                draft.rack_tile = Some(selected_tile);
                draft.selected_tile = Some(selected_tile);
                return visual_game_page(&game, &draft, None);
            }
            if !game.rack_order.contains(&selected_tile) || !game.rack_order.contains(&target_tile)
            {
                return draft_error_page(&game, &draft, "That tile is not in your rack.");
            }
            let order = crate::swap_rack_tiles(
                &game.rack_order,
                TileId::new(selected_tile),
                TileId::new(target_tile),
            );
            if crate::save_rack_order(
                database,
                game.game_id,
                &game.user_id,
                &order,
                now.unix_timestamp_nanos().try_into().unwrap_or(i64::MAX),
            )
            .await
            .is_err()
            {
                return draft_error_page(&game, &draft, "Your rack order could not be saved.");
            }
            draft.rack_tile = None;
            draft.selected_tile = None;
            let game =
                match load_authorized_game_page(database, &request.cookies, game_id, now).await {
                    Ok(game) => game,
                    Err(error) => {
                        return product_error_page("Unable to arrange rack", &error.to_string());
                    }
                };
            return visual_game_page(&game, &draft, None);
        }
        "BEGIN_EXCHANGE" => {
            draft.mode = TurnMode::Exchange;
            draft.selected_tile = None;
            draft.placements.clear();
        }
        "TOGGLE_EXCHANGE" => {
            let Some(tile_id) = form.tile_id else {
                return draft_error_page(&game, &draft, "Choose a tile to exchange.");
            };
            if !game.view.rack.iter().any(|(id, _, _)| *id == tile_id) {
                return draft_error_page(&game, &draft, "That tile is not in your rack.");
            }
            if let Some(index) = draft.exchange_tiles.iter().position(|id| *id == tile_id) {
                draft.exchange_tiles.remove(index);
            } else {
                draft.exchange_tiles.push(tile_id);
                draft.exchange_tiles.sort_unstable();
            }
        }
        "REVIEW_EXCHANGE" if draft.exchange_tiles.is_empty() => {
            return draft_error_page(&game, &draft, "Select at least one tile to exchange.");
        }
        "REVIEW_EXCHANGE" => draft.mode = TurnMode::ConfirmExchange,
        "CONFIRM_PASS" => draft.mode = TurnMode::ConfirmPass,
        "CONFIRM_RESIGN" => draft.mode = TurnMode::ConfirmResign,
        "CANCEL_MODE" => {
            draft.mode = TurnMode::Play;
            draft.exchange_tiles.clear();
        }
        "SHUFFLE_RACK" => {
            let order = crate::shuffle_rack_order(&game.rack_order);
            let updated_at_ms =
                i64::try_from(now.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX);
            if crate::save_rack_order(database, game.game_id, &game.user_id, &order, updated_at_ms)
                .await
                .is_err()
            {
                return draft_error_page(&game, &draft, "Your rack could not be shuffled.");
            }
            let game =
                match load_authorized_game_page(database, &request.cookies, game_id, now).await {
                    Ok(game) => game,
                    Err(error) => {
                        return product_error_page("Unable to shuffle rack", &error.to_string());
                    }
                };
            let _ = persist_draft(database, &game, &draft, now).await;
            return visual_game_page(&game, &draft, None);
        }
        "ZOOM_OUT" => draft.board_zoom = draft.board_zoom.zoom_out(),
        "ZOOM_RESET" => draft.board_zoom = BoardZoom::Fit,
        "ZOOM_IN" => draft.board_zoom = draft.board_zoom.zoom_in(),
        "CLEAR" => {
            let board_zoom = draft.board_zoom;
            draft = TurnDraft {
                board_zoom,
                ..TurnDraft::default()
            };
        }
        _ => return draft_error_page(&game, &draft, "That turn action is unavailable."),
    }
    if persist_draft(database, &game, &draft, now).await.is_err() {
        return draft_error_page(&game, &draft, "Your move plan could not be saved.");
    }
    visual_game_page(&game, &draft, None)
}

async fn game_turn_route(
    dispatcher: &crate::GameSharedStateDispatcher,
    database: &dyn Database,
    request: &RouteRequest,
    game_id: &str,
    now: OffsetDateTime,
) -> View {
    let Ok(user_id) = crate::authenticated_user(database, &request.cookies, now).await else {
        return View::from(error_component(
            "Your session expired. Sign in and review the game again.",
        ));
    };
    let Ok(game_id) = game_id.parse() else {
        return View::from(error_component("The game route is invalid."));
    };
    let form: PendingMoveForm = match request.parse_form() {
        Ok(form) => form,
        Err(_) => return turn_rejection("Select tiles and provide valid coordinates."),
    };
    let command = match form.game_command() {
        Ok(command) => command,
        Err(message) => return turn_rejection(message),
    };
    let Ok(payload) = PayloadBlob::from_serializable(&command) else {
        return turn_rejection("The turn could not be encoded. Try again.");
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
            let _ = clear_move_plan(database, game_id, &user_id).await;
            match load_authorized_game_page(database, &request.cookies, &game_id.to_string(), now)
                .await
            {
                Ok(game) => View::from(game_page(&game)),
                Err(PresentationError::Unauthenticated) => View::from(error_component(
                    "Your session expired. Sign in and review the accepted turn.",
                )),
                Err(_) => View::from(error_component(
                    "The turn was accepted, but the updated game could not be rendered. Reload the game.",
                )),
            }
        }
        Ok(result) => {
            let reason = result.iter().find_map(|response| match response {
                TransportInbound::CommandRejected { reason, .. } => Some(reason.as_str()),
                _ => None,
            });
            turn_rejection(reason.unwrap_or("The turn was not accepted."))
        }
        Err(_) => turn_rejection("The turn could not be persisted. Try again."),
    }
}

fn turn_feedback(message: Option<&str>) -> Container {
    container! {
        @if let Some(message) = message {
            section id="turn-feedback" width="100%" {
                div id="game-error" background=#fff3e8 border=(("#e2b98f", 1))
                    border-radius="12px" padding="14px" {
                    span color=#7a3f16 { (message) }
                }
            }
        }
    }
    .into()
}

fn turn_rejection(reason: &str) -> View {
    let message = if reason.contains("revision") {
        "This game changed in another tab. Review the latest board and resubmit."
    } else if reason.contains("authorized") || reason.contains("member") {
        "You are not authorized to act in this game."
    } else if reason.contains("rules") || reason.contains("dictionary version") {
        "This game requires an unsupported rules or dictionary version."
    } else if reason.contains("dictionary rejected") {
        reason
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
    View::builder()
        .with_fragment(turn_feedback(Some(message)))
        .build()
}

fn invitation_joined_page(game_id: wwmtf_game_domain::GameId) -> Container {
    let game_href = format!("/games/{game_id}");
    container! {
        div id="app-page" direction="column" align-items="center" min-height="100vh" background=#f4f1e8 padding-y=48 padding-x=24 {
            main width="100%" max-width="560px"
                background=#ffffff border=(("#ded8c9", 1)) border-radius="18px" padding="32px" gap="16px" {
                span color=#3f5735 font-weight=bold { "INVITATION ACCEPTED" }
                h1 { "Your game is ready" }
                span color=#5d6258 { "The invitation was redeemed and the game was created." }
                anchor href=(game_href) color=#ffffff background=#526243 border=(("#526243", 1))
                    border-radius="10px" padding-y=13 padding-x=18 { "Open game" }
            }
        }
    }
    .into()
}

fn invitation_page(invitation_token: &str, signed_in: bool) -> Container {
    let login_href = format!("/login?invite={invitation_token}");
    container! {
        div id="app-page" direction="column" align-items="center" min-height="100vh" background=#f4f1e8 padding-y=48 padding-x=24 {
            main width="100%" max-width="560px"
                background=#ffffff border=(("#ded8c9", 1)) border-radius="18px" padding="32px" gap="18px" {
                span color=#7b6240 font-weight=bold { "PRIVATE GAME INVITATION" }
                h1 { "You’ve been invited to play" }
                span color=#5d6258 { "This invitation creates a private two-player game. It can be used once." }
                @if signed_in {
                    form hx-post="/join" hx-target="#app-page" gap="10px" {
                        input type=hidden name="action" value="REDEEM_INVITATION";
                        input type=hidden name="invitation_token" value=(invitation_token);
                        button type=submit padding-y=13 padding-x=18 background=#526243 color=#ffffff
                            border=(("#526243", 1)) border-radius="10px" cursor=pointer { "Accept invitation" }
                    }
                    anchor href="/" color=#526243 { "Back to dashboard" }
                } @else {
                    span { "Continue with Google to accept this invitation." }
                    div direction="row" gap="10px" {
                        anchor href=(login_href) color=#ffffff background=#526243 border=(("#526243", 1))
                            border-radius="10px" padding-y=13 padding-x=18 { "Continue with Google" }
                    }
                }
            }
        }
    }
    .into()
}

async fn invitation_route(
    database: &dyn Database,
    request: &RouteRequest,
    now: OffsetDateTime,
) -> Container {
    let token = request
        .query
        .get("invite")
        .cloned()
        .or_else(|| {
            request
                .parse_form::<DashboardActionForm>()
                .ok()
                .map(|form| form.invitation_token)
        })
        .unwrap_or_default();
    if token.is_empty() {
        return product_error_page(
            "Invitation unavailable",
            "This invitation link is incomplete.",
        );
    }
    let user_id = crate::authenticated_user(database, &request.cookies, now).await;
    if request.method.as_ref() != "POST" {
        return invitation_page(&token, user_id.is_ok());
    }
    let Ok(user_id) = user_id else {
        return invitation_page(&token, false);
    };
    match redeem_shareable_invitation(database, &token, &user_id, now).await {
        Ok(game_id) => invitation_joined_page(game_id),
        Err(error) => product_error_page("Invitation unavailable", product_error_message(&error)),
    }
}

/// Builds the database-backed renderer-neutral application router.
#[must_use]
#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
pub fn create_product_router(
    database: Arc<dyn Database>,
    dispatcher: Arc<crate::GameSharedStateDispatcher>,
    definition_provider: Option<Arc<dyn crate::DefinitionProvider>>,
    google_oidc: Option<Arc<GoogleOidcClient>>,
    development_login: bool,
    csrf_token: String,
    public_base_url: String,
    secure_cookies: bool,
) -> Router {
    let router = Router::new();
    router.add_route_result("/health/live", |_request: RouteRequest| async move {
        Ok(Content::Raw {
            data: b"ok\n".to_vec().into(),
            content_type: "text/plain; charset=utf-8".to_string(),
        }) as Result<Content, Box<dyn std::error::Error>>
    });
    #[cfg(feature = "metrics")]
    router.add_route_result("/metrics", |_request: RouteRequest| async move {
        let metrics = crate::app_metrics_snapshot();
        let body = format!(
            concat!(
                "wwmtf_authentication_failures_total {}\n",
                "wwmtf_command_conflicts_total {}\n",
                "wwmtf_projection_rebuilds_total {}\n",
                "wwmtf_live_subscribers {}\n",
                "wwmtf_database_failures_total {}\n",
                "wwmtf_compose_authorization_failures_total {}\n",
                "wwmtf_live_subscription_failures_total {}\n"
            ),
            metrics.authentication_failures,
            metrics.command_conflicts,
            metrics.projection_rebuilds,
            metrics.live_subscribers,
            metrics.database_failures,
            metrics.compose_authorization_failures,
            metrics.live_subscription_failures,
        );
        Ok(Content::Raw {
            data: body.into_bytes().into(),
            content_type: "text/plain; version=0.0.4; charset=utf-8".to_string(),
        }) as Result<Content, Box<dyn std::error::Error>>
    });
    let readiness_database = database.clone();
    router.add_route_result("/health/ready", move |_request: RouteRequest| {
        let database = readiness_database.clone();
        async move {
            if !database.table_exists("__wwmtf_migrations").await?
                || !database.table_exists("game_journal").await?
            {
                return Err("application schema is not ready".into());
            }
            database.select("users").execute_first(&*database).await?;
            Ok(Content::Raw {
                data: b"ready\n".to_vec().into(),
                content_type: "text/plain; charset=utf-8".to_string(),
            }) as Result<Content, Box<dyn std::error::Error>>
        }
    });
    let dashboard_database = database.clone();
    router.add_route_result("/", move |request: RouteRequest| {
        let database = dashboard_database.clone();
        async move {
            Ok(dashboard_route(&*database, &request, OffsetDateTime::now_utc()).await)
                as Result<Container, Box<dyn std::error::Error>>
        }
    });
    let dashboard_action_database = database.clone();
    let dashboard_action_dispatcher = dispatcher.clone();
    let dashboard_public_base_url = Arc::new(public_base_url);
    router.add_route_result("/dashboard/action", move |request: RouteRequest| {
        let database = dashboard_action_database.clone();
        let dispatcher = dashboard_action_dispatcher.clone();
        let public_base_url = dashboard_public_base_url.clone();
        async move {
            Ok(dashboard_action_route(
                &*database,
                &dispatcher,
                public_base_url.as_str(),
                &request,
                OffsetDateTime::now_utc(),
            )
            .await) as Result<Container, Box<dyn std::error::Error>>
        }
    });
    let join_database = database.clone();
    router.add_route_result("/join", move |request: RouteRequest| {
        let database = join_database.clone();
        async move {
            Ok(invitation_route(&*database, &request, OffsetDateTime::now_utc()).await)
                as Result<Container, Box<dyn std::error::Error>>
        }
    });
    let csrf_token = Arc::new(csrf_token);
    let google_login = google_oidc.is_some();
    if let Some(oidc) = google_oidc {
        let google_start_database = database.clone();
        let google_start_oidc = oidc.clone();
        router.add_route_result("/auth/google/start", move |request: RouteRequest| {
            let database = google_start_database.clone();
            let oidc = google_start_oidc.clone();
            async move {
                Ok(google_start_route(
                    &*database,
                    &oidc,
                    &request,
                    OffsetDateTime::now_utc(),
                    secure_cookies,
                )
                .await) as Result<View, Box<dyn std::error::Error>>
            }
        });
        let google_callback_database = database.clone();
        let google_callback_oidc = oidc.clone();
        let google_callback_csrf = csrf_token.clone();
        router.add_route_result("/auth/google/callback", move |request: RouteRequest| {
            let database = google_callback_database.clone();
            let oidc = google_callback_oidc.clone();
            let csrf = google_callback_csrf.clone();
            async move {
                Ok(google_callback_route(
                    &*database,
                    &oidc,
                    &request,
                    OffsetDateTime::now_utc(),
                    &csrf,
                    secure_cookies,
                )
                .await) as Result<View, Box<dyn std::error::Error>>
            }
        });
        let migration_database = database.clone();
        let migration_oidc = oidc;
        router.add_route_result("/account/migrate", move |request: RouteRequest| {
            let database = migration_database.clone();
            let oidc = migration_oidc.clone();
            async move {
                Ok(migration_start_route(
                    &*database,
                    &oidc,
                    &request,
                    OffsetDateTime::now_utc(),
                    secure_cookies,
                )
                .await) as Result<View, Box<dyn std::error::Error>>
            }
        });
    }
    let login_database = database.clone();
    let login_csrf = csrf_token;
    router.add_route_result("/login", move |request: RouteRequest| {
        let database = login_database.clone();
        let csrf = login_csrf.clone();
        async move {
            Ok(login_route(
                &*database,
                &request,
                OffsetDateTime::now_utc(),
                &csrf,
                secure_cookies,
                development_login,
                google_login,
            )
            .await) as Result<View, Box<dyn std::error::Error>>
        }
    });
    router.add_route_result("/register", move |_request: RouteRequest| async move {
        Ok(register_route(development_login, google_login))
            as Result<View, Box<dyn std::error::Error>>
    });
    let logout_database = database.clone();
    router.add_route_result("/logout", move |request: RouteRequest| {
        let database = logout_database.clone();
        async move {
            Ok(logout_route(
                &*database,
                &request,
                OffsetDateTime::now_utc(),
                secure_cookies,
            )
            .await) as Result<View, Box<dyn std::error::Error>>
        }
    });
    let avatar_upload_database = database.clone();
    router.add_route_result("/profile/avatar", move |request: RouteRequest| {
        let database = avatar_upload_database.clone();
        async move {
            Ok(custom_avatar_route(&*database, &request, OffsetDateTime::now_utc()).await)
                as Result<Container, Box<dyn std::error::Error>>
        }
    });
    let avatar_database = database.clone();
    router.add_route_result(
        RoutePath::LiteralPrefix("/profiles/".to_string()),
        move |request: RouteRequest| {
            let database = avatar_database.clone();
            async move {
                let segments = request
                    .path
                    .strip_prefix("/profiles/")
                    .unwrap_or_default()
                    .split('/')
                    .collect::<Vec<_>>();
                let image = match segments.as_slice() {
                    [profile_user_id, "avatar", content_hash] => {
                        let viewer = crate::authenticated_user(
                            &*database,
                            &request.cookies,
                            OffsetDateTime::now_utc(),
                        )
                        .await
                        .ok();
                        if let Some(viewer) = viewer
                            && crate::can_view_profile_avatar(&*database, &viewer, profile_user_id)
                                .await?
                        {
                            crate::load_profile_image(&*database, profile_user_id, content_hash)
                                .await?
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                let (data, content_type) = image.map_or_else(
                    || {
                        (
                            b"not found\n".to_vec(),
                            "text/plain; charset=utf-8".to_string(),
                        )
                    },
                    |image| (image.bytes, image.content_type),
                );
                Ok(Content::Raw {
                    data: data.into(),
                    content_type,
                }) as Result<Content, Box<dyn std::error::Error>>
            }
        },
    );
    let game_dispatcher = dispatcher;
    let game_definition_provider = definition_provider;
    router.add_route_result(
        RoutePath::LiteralPrefix("/games/".to_string()),
        move |request: RouteRequest| {
            let database = database.clone();
            let dispatcher = game_dispatcher.clone();
            let definition_provider = game_definition_provider.clone();
            async move {
                let game_path = request.path.strip_prefix("/games/").unwrap_or_default();
                if let Some(game_id) = game_path.strip_suffix("/compose") {
                    Ok(View::from(
                        game_compose_route(
                            &*database,
                            &request,
                            game_id,
                            OffsetDateTime::now_utc(),
                        )
                        .await,
                    )) as Result<View, Box<dyn std::error::Error>>
                } else if let Some(game_id) = game_path.strip_suffix("/turn") {
                    Ok(game_turn_route(
                        &dispatcher,
                        &*database,
                        &request,
                        game_id,
                        OffsetDateTime::now_utc(),
                    )
                    .await) as Result<View, Box<dyn std::error::Error>>
                } else if let Some((game_id, word)) = game_path.split_once("/word-panels/") {
                    Ok(View::from(
                        game_word_panel_route(
                            &*database,
                            definition_provider.as_deref(),
                            &request,
                            game_id,
                            word,
                            OffsetDateTime::now_utc(),
                        )
                        .await,
                    )) as Result<View, Box<dyn std::error::Error>>
                } else if let Some((game_id, word)) = game_path.split_once("/words/") {
                    Ok(View::from(
                        game_word_route(
                            &*database,
                            definition_provider.as_deref(),
                            &request,
                            game_id,
                            word,
                            OffsetDateTime::now_utc(),
                        )
                        .await,
                    )) as Result<View, Box<dyn std::error::Error>>
                } else {
                    Ok(View::from(
                        game_route(&*database, &request, OffsetDateTime::now_utc()).await,
                    )) as Result<View, Box<dyn std::error::Error>>
                }
            }
        },
    );
    router
}

async fn played_word_definition(
    database: &dyn Database,
    provider: Option<&dyn crate::DefinitionProvider>,
    request: &RouteRequest,
    game_id: &str,
    word: &str,
    now: OffsetDateTime,
) -> Result<(wwmtf_game_domain::GameId, String, crate::DefinitionLookup), &'static str> {
    let game = load_authorized_game_page(database, &request.cookies, game_id, now)
        .await
        .map_err(|error| match error {
            PresentationError::Unauthenticated => "Your session expired. Sign in and try again.",
            PresentationError::Forbidden => "You are not authorized for this game.",
            _ => "The game could not be loaded.",
        })?;
    let word = wwmtf_game_domain::normalize_word(word).ok_or("That played word is invalid.")?;
    if !game.has_played_word(&word) {
        return Err("That word does not occur in this game's accepted move history.");
    }
    let now_ms = i64::try_from(now.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX);
    let lookup = crate::lookup_definition(database, provider, &word, now_ms)
        .await
        .unwrap_or_else(|error| {
            let reason = crate::DefinitionUnavailableReason::from(&error);
            log::warn!(
                target: "wwmtf::definitions",
                "definition_lookup_failed reason={}",
                reason.log_reason()
            );
            crate::DefinitionLookup::Unavailable(reason)
        });
    Ok((game.game_id, word, lookup))
}

/// Loads one played-word definition after authenticating game membership and occurrence.
async fn game_word_route(
    database: &dyn Database,
    provider: Option<&dyn crate::DefinitionProvider>,
    request: &RouteRequest,
    game_id: &str,
    word: &str,
    now: OffsetDateTime,
) -> Container {
    match played_word_definition(database, provider, request, game_id, word, now).await {
        Ok((game_id, word, lookup)) => definition_page(game_id, &word, &lookup),
        Err(message) => product_error_page("Definition unavailable", message),
    }
}

async fn game_word_panel_route(
    database: &dyn Database,
    provider: Option<&dyn crate::DefinitionProvider>,
    request: &RouteRequest,
    game_id: &str,
    word: &str,
    now: OffsetDateTime,
) -> Container {
    match played_word_definition(database, provider, request, game_id, word, now).await {
        Ok((_game_id, word, lookup)) => definition_panel(&word, &lookup),
        Err(message) => definition_panel_error(message),
    }
}

fn definition_content(word: &str, lookup: &crate::DefinitionLookup) -> Container {
    container! {
        h1 { (word) }
        @match lookup {
            crate::DefinitionLookup::Found(definition) => {
                @for meaning in &definition.meanings {
                    section gap="6px" {
                        h2 font-size="18px" { (meaning.part_of_speech.as_str()) }
                        @for text in &meaning.definitions {
                            span { "• " (text.as_str()) }
                        }
                    }
                }
                span color=#5d6258 font-size="12px" {
                    "Source: " anchor href=(definition.source_url.as_str()) color=#526243 { (definition.source_url.as_str()) }
                }
                span color=#5d6258 font-size="12px" {
                    "License: " anchor href=(definition.license_url.as_str()) color=#526243 { (definition.license_name.as_str()) }
                }
            },
            crate::DefinitionLookup::Missing => {
                span { "No definition is available from the configured provider." }
            },
            crate::DefinitionLookup::Unavailable(reason) => {
                span { (reason.user_message()) }
            },
        }
    }
    .into()
}

fn definition_page(
    game_id: wwmtf_game_domain::GameId,
    word: &str,
    lookup: &crate::DefinitionLookup,
) -> Container {
    let game_href = format!("/games/{game_id}");
    let content = definition_content(word, lookup);
    container! {
        div id="app-page" direction="column" align-items="center" min-height="100vh"
            background=#f4f1e8 padding-y=32 padding-x=18 {
            main id="word-definition" width="100%" max-width="720px" background=#ffffff
                border=(("#ded8c9", 1)) border-radius="18px" padding="24px" gap="14px" {
                anchor href=(game_href) color=#526243 { "← Back to game" }
                (content)
            }
        }
    }
    .into()
}

fn definition_panel(word: &str, lookup: &crate::DefinitionLookup) -> Container {
    let content = definition_content(word, lookup);
    container! {
        aside id="game-definition-layer" class="game-definition-panel" position="fixed"
            top=68 right="2%" width="440px" max-width="96%" max-height="70vh" overflow-y="auto"
            background=#f4f1e8 color=#26382d border=(("#8e7651", 3)) border-radius="18px"
            padding="18px" gap="12px" {
            div direction="row" justify-content="space-between" align-items="center" gap="12px" {
                span color=#5d6e62 font-size="11px" font-weight=bold { "WORD DEFINITION" }
                button type=button fx-click=(ActionType::no_display_by_id("game-definition-layer"))
                    background=#173326 color=#f4f0df border=(("#436854", 1)) border-radius="999px"
                    padding-y="6px" padding-x="10px" cursor=pointer { "Close" }
            }
            (content)
        }
    }
    .into()
}

fn definition_panel_error(message: &str) -> Container {
    container! {
        aside id="game-definition-layer" class="game-definition-panel" position="fixed"
            top=68 right="2%" width="440px" max-width="96%" max-height="70vh" overflow-y="auto"
            background=#f4f1e8 color=#26382d border=(("#8e7651", 3)) border-radius="18px"
            padding="18px" gap="12px" {
            div direction="row" justify-content="space-between" align-items="center" gap="12px" {
                h2 { "Definition unavailable" }
                button type=button fx-click=(ActionType::no_display_by_id("game-definition-layer"))
                    background=#173326 color=#f4f0df border=(("#436854", 1)) border-radius="999px"
                    padding-y="6px" padding-x="10px" cursor=pointer { "Close" }
            }
            span { (message) }
        }
    }
    .into()
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
        Ok(game) => {
            let stored = load_move_plan(database, game.game_id, &game.user_id)
                .await
                .ok()
                .flatten();
            let mut draft = stored
                .as_ref()
                .and_then(|(payload, _)| parse_draft(payload))
                .unwrap_or_default();
            let source_revision = stored.as_ref().map(|(_, revision)| *revision);
            let removed_conflicts = reconcile_draft(&game, &mut draft);
            let board_changed =
                source_revision.is_some_and(|revision| revision < game.view.revision);
            if board_changed {
                let _ = persist_draft(database, &game, &draft, now).await;
            }
            let message = if removed_conflicts {
                Some(
                    "The board changed. Conflicting planned tiles returned to your rack; the rest of your plan was preserved and rescored.",
                )
            } else if board_changed {
                Some(
                    "The board changed. Your plan was preserved and rescored against the latest board.",
                )
            } else {
                None
            };
            visual_game_page(&game, &draft, message)
        }
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
    login_page_with_invitation(error, "", false, true)
}

fn login_page_with_invitation(
    error: Option<&str>,
    invitation_token: &str,
    development_login: bool,
    google_login: bool,
) -> Container {
    let message = error.unwrap_or_default();
    let google_href = if invitation_token.is_empty() {
        "/auth/google/start".to_string()
    } else {
        format!("/auth/google/start?invite={invitation_token}")
    };
    container! {
        div id="app-page" direction="column" align-items="center" min-height="100vh" background=#f4f1e8 padding-y=48 padding-x=24 {
            main width="100%" max-width="480px"
                background=#ffffff border=(("#ded8c9", 1)) border-radius="18px" padding="32px" gap="20px" {
                anchor href="/" color=#526243 { "← Home" }
                div gap="6px" {
                    span color=#7b6240 font-weight=bold { "WORDS WITH MORE THAN FRIENDS" }
                    h1 { "Welcome back" }
                    span color=#5d6258 { "Sign in to continue your private games." }
                }
                @if development_login {
                    section background=#eef5e9 border=(("#b7c8ad", 1)) border-radius="10px" padding="14px" gap="12px" {
                        span color=#3f5735 font-weight=bold { "Local development login" }
                        span color=#5d6258 { "Enter any username. It will be created automatically if needed." }
                        form method="post" gap="12px" {
                            input type=hidden name="invitation_token" value=(invitation_token);
                            input type=text name="username" placeholder="Username" autofocus=true padding-y=13 padding-x=14
                                border=(("#cfc8b8", 1)) border-radius="10px";
                            button type=submit padding-y=13 padding-x=18 background=#526243 color=#ffffff
                                border=(("#526243", 1)) border-radius="10px" cursor=pointer { "Continue" }
                        }
                    }
                }
                @if google_login {
                    anchor href=(google_href) target="_top" color=#ffffff background=#526243 border=(("#526243", 1))
                        border-radius="10px" padding-y=13 padding-x=18 text-align="center" { "Continue with Google" }
                    span color=#5d6258 { "Already have a username/password account? "
                        anchor href="/account/migrate" color=#526243 { "Migrate it to Google" }
                    }
                }
                @if !development_login && !google_login {
                    span color=#5d6258 { "No login method is configured for this server." }
                }
                @if !message.is_empty() {
                    section id="account-result" background=#fff3e8 border=(("#e2b98f", 1))
                        border-radius="10px" padding="12px" { span color=#7a3f16 { (message) } }
                }
            }
        }
    }
    .into()
}

/// Renders the explicit legacy-account migration form.
#[must_use]
pub fn migration_page(error: Option<&str>) -> Container {
    let message = error.unwrap_or_default();
    container! {
        div id="app-page" direction="column" align-items="center" min-height="100vh" background=#f4f1e8 padding-y=48 padding-x=24 {
            main width="100%" max-width="480px" background=#ffffff border=(("#ded8c9", 1)) border-radius="18px" padding="32px" gap="20px" {
                anchor href="/login" color=#526243 { "← Back to sign in" }
                h1 { "Migrate your existing account" }
                span color=#5d6258 { "Confirm your existing credentials, then connect Google without losing your games." }
                form method="post" gap="12px" {
                    span font-weight=bold { "Username" }
                    input type=text name="username" placeholder="Username" padding-y=13 padding-x=14 border=(("#cfc8b8", 1)) border-radius="10px";
                    span font-weight=bold { "Password" }
                    input type=password name="password" placeholder="Password" padding-y=13 padding-x=14 border=(("#cfc8b8", 1)) border-radius="10px";
                    button type=submit padding-y=13 padding-x=18 background=#526243 color=#ffffff border=(("#526243", 1)) border-radius="10px" cursor=pointer { "Continue with Google" }
                }
                @if !message.is_empty() {
                    section background=#fff3e8 border=(("#e2b98f", 1)) border-radius="10px" padding="12px" { (message) }
                }
            }
        }
    }
    .into()
}

/// Renders logout confirmation. Session revocation is performed by the authenticated workflow.
#[must_use]
pub fn logout_page() -> Container {
    container! {
        div id="app-page" direction="column" align-items="center" min-height="100vh"
            background=#f4f1e8 padding=24 {
            main width="100%" max-width="480px" background=#ffffff border=(("#ded8c9", 1))
                border-radius=18 padding=32 gap=16 {
                h1 { "Sign out" }
                span color=#5d6258 { "Signing out revokes the current durable session." }
                form hx-post="/logout" hx-target="#app-page" {
                    button type=submit padding-y=12 padding-x=16 background=#526243 color=#ffffff
                        border=(("#526243", 1)) border-radius=10 cursor="pointer" { "Sign out" }
                }
                section id="account-result" {}
            }
        }
    }
    .into()
}

/// Renders signed-out navigation and account entry points without exposing state.
#[must_use]
pub fn signed_out_page() -> Container {
    container! {
        div id="app-page" direction="column" align-items="center" min-height="100vh"
            background=#f4f1e8 padding=24 {
            main width="100%" max-width="560px" background=#ffffff border=(("#ded8c9", 1))
                border-radius=18 padding=32 gap=18 {
                span color=#7b6240 font-weight=bold { "WORDS WITH MORE THAN FRIENDS" }
                h1 { "Sign in required" }
                span color=#5d6258 { "A valid secure session is required to view games." }
                div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=10 {
                    anchor href="/login" color=#ffffff background=#526243 border=(("#526243", 1))
                        border-radius=10 padding-y=12 padding-x=16 { "Continue with Google" }
                }
            }
        }
    }
    .into()
}

/// Renders the complete signed-in dashboard projection.
#[must_use]
pub fn dashboard_page(dashboard: &AuthenticatedDashboard) -> Container {
    dashboard_page_content(dashboard, None)
}

fn dashboard_page_with_invitation(
    dashboard: &AuthenticatedDashboard,
    invitation_id: &str,
    token: &str,
    public_base_url: &str,
) -> Container {
    dashboard_page_content(dashboard, Some((invitation_id, token, public_base_url)))
}

fn dashboard_request_before() -> ActionType {
    ActionType::Multi(vec![
        ActionType::display_by_id("dashboard-action-progress"),
        ActionType::no_display_by_id("dashboard-action-error"),
    ])
}

fn dashboard_request_after() -> ActionType {
    ActionType::no_display_by_id("dashboard-action-progress")
}

fn dashboard_request_error() -> ActionType {
    ActionType::display_by_id("dashboard-action-error")
}

fn start_game_component() -> Container {
    container! {
        section id="new-game-actions" width="100%" gap="14px" {
            div gap="5px" {
                h2 { "Start a game" }
                span color=#5d6258 { "Challenge another player using their exact stable @handle, shown on their profile and game views, or make a one-time private invite." }
            }
            div id="dashboard-action-status" min-height="48px" {
                div id="dashboard-action-progress" hidden background=#e8f1e3 border=(("#a9bf9c", 1))
                    border-radius="10px" padding="12px" { span { "Working…" } }
                div id="dashboard-action-error" hidden background=#fff3e8 border=(("#e2b98f", 1))
                    border-radius="10px" padding="12px" { span { "The request did not complete. Check your connection and try again." } }
            }
            form hx-post="/dashboard/action" hx-target="#app-page" gap="10px"
                fx-http-before-request=(dashboard_request_before())
                fx-http-after-request=(dashboard_request_after())
                fx-http-error=(dashboard_request_error()) {
                input type=hidden name="action" value="CHALLENGE";
                input type=text name="username" placeholder="Exact @handle" padding-y=13 padding-x=14
                    border=(("#cfc8b8", 1)) border-radius="10px";
                button type=submit padding-y=12 padding-x=16 background=#526243 color=#ffffff
                    border=(("#526243", 1)) border-radius="10px" cursor=pointer { "Send challenge" }
            }
            form hx-post="/dashboard/action" hx-target="#app-page" gap="8px"
                fx-http-before-request=(dashboard_request_before())
                fx-http-after-request=(dashboard_request_after())
                fx-http-error=(dashboard_request_error()) {
                input type=hidden name="action" value="CREATE_INVITATION";
                button type=submit padding-y=12 padding-x=16 background=#f4ead7 color=#664f2e
                    border=(("#cfb98e", 1)) border-radius="10px" cursor=pointer { "Create private invite link" }
            }
            form hx-post="/dashboard/action" hx-target="#app-page" gap="10px"
                fx-http-before-request=(dashboard_request_before())
                fx-http-after-request=(dashboard_request_after())
                fx-http-error=(dashboard_request_error()) {
                input type=hidden name="action" value="REDEEM_INVITATION";
                input type=text name="invitation_token" placeholder="Paste an invite token" padding-y=13 padding-x=14
                    border=(("#cfc8b8", 1)) border-radius="10px";
                button type=submit padding-y=12 padding-x=16 background=#ffffff color=#526243
                    border=(("#839276", 1)) border-radius="10px" cursor=pointer { "Join game" }
            }
        }
    }
    .into()
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::large_stack_frames)]
fn dashboard_page_content(
    dashboard: &AuthenticatedDashboard,
    created_invitation: Option<(&str, &str, &str)>,
) -> Container {
    let user_id = dashboard.user_id.as_str();
    let username = dashboard.username.as_str();
    let display_name = dashboard.display_name.as_str();
    let avatar_url = dashboard.avatar_url.as_deref();
    let totals = score_totals_label(dashboard.score_totals.as_ref());
    let dashboard_channel = format!("dashboard:{}", dashboard.user_id);
    let created_invitation_id = created_invitation.map(|(id, _, _)| id).unwrap_or_default();
    let created_invitation_path = created_invitation
        .map(|(_, token, base_url)| format!("{base_url}/join?invite={token}"))
        .unwrap_or_default();
    let refresh_dashboard = ActionType::Navigate {
        url: "/".to_string(),
    };
    container! {
        div id="app-page" data-shared-state-channel=(dashboard_channel.as_str())
            fx-global-shared-state-event=(refresh_dashboard)
            direction="column" align-items="center"
            min-height="100vh" background=#e9efe8 color=#24352c
            padding-y=24 padding-x=16 {
            div id="dashboard-shell" width="100%" max-width="1080px" gap="28px" {
                header id="dashboard-header" direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) justify-content="space-between" align-items="center"
                    background=#ffffff border=(("#ded8c9", 1)) border-radius="18px" padding-y=22 padding-x=26 gap="16px" {
                    div direction="row" align-items="center" gap="12px" {
                        @if let Some(avatar_url) = avatar_url {
                            image src=(avatar_url) alt="Profile avatar" width="48" height="48" border-radius="999px";
                        }
                        div gap="4px" {
                            span color=#2f8a57 font-weight=bold { "WORDS WITH MORE THAN FRIENDS" }
                            h1 { "Your games" }
                            span color=#5d6258 { "Signed in as " (display_name) }
                            @if display_name != username {
                                span color=#747a71 { "@" (username) }
                            }
                        }
                    }
                    anchor href="/logout" color=#526243 { "Sign out" }
                }
                @if let Some((_, token, _)) = created_invitation {
                    section id="created-invitation" background=#e8f1e3 border=(("#a9bf9c", 1))
                        border-radius="16px" padding="22px" gap="10px" {
                        span color=#3f5735 font-weight=bold { "Invitation ready" }
                        h2 { "Send this private link to your opponent" }
                        span color=#4f594a { "This is the only time the secret link can be shown. It expires in 30 days and can be used once." }
                        anchor href=(created_invitation_path.as_str()) color="#36512e" overflow-wrap="anywhere" {
                            (created_invitation_path.as_str())
                        }
                        span color=#6b7267 { "Invite token (for manual entry):" }
                        span overflow-wrap="anywhere" font-weight=bold { (token) }
                    }
                }
                section id="active-games" data-dashboard-order="1" background=#ffffff border=(("#ded8c9", 1))
                    border-radius="16px" padding="24px" gap="4px" {
                    h2 { "Games" }
                    @if dashboard.projection.games.is_empty() {
                        div gap="8px" {
                            h3 { "Your first game starts here" }
                            span color=#777b73 { "Challenge someone by username or create a private link to send them." }
                            anchor href="#new-game-actions" color=#526243 font-weight=bold { "Start a private game" }
                        }
                    }
                    @for game in &dashboard.projection.games {
                        @let href = format!("/games/{}", game.game_id);
                        @let state = if game.status == "COMPLETED" {
                            if game.winner_user_id.as_deref() == Some(user_id) {
                                "You won"
                            } else if game.winner_user_id.is_none() {
                                "Tie game"
                            } else {
                                "You lost"
                            }
                        } else if game.active_player_user_id.as_deref() == Some(user_id) {
                            "Your turn"
                        } else {
                            "Waiting for opponent"
                        };
                        div id=(format!("game-summary-{}", game.game_id)) class="game-summary"
                            direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) justify-content="space-between"
                            border-bottom=(("#e3ded2", 1)) padding-y=16 padding-x=4 gap="12px" {
                            div gap="3px" {
                                anchor href=(href) color=#526243 font-weight=bold { "Game with " (game.opponent_display_name.as_str()) }
                                @if game.opponent_display_name != game.opponent_username {
                                    span color=#777b73 { "@" (game.opponent_display_name.as_str()) }
                                }
                                span color=#3f5735 font-weight=bold { (state) }
                                span color=#777b73 { "You " (game.viewer_score) " – " (game.opponent_score) " " (game.opponent_display_name.as_str()) }
                            }
                            span color=#777b73 { (game.latest_activity.as_str()) }
                        }
                    }
                }
                section id="pending-games" data-dashboard-order="2" background=#ffffff border=(("#ded8c9", 1))
                    border-radius="16px" padding="24px" gap="4px" {
                    div gap="5px" {
                        h2 { "Challenges & invitations" }
                        span color=#5d6258 { "Invitations are private and single-use. Old invite secrets cannot be displayed again." }
                    }
                    @if dashboard.projection.pending.is_empty() {
                        div gap="6px" {
                            span color=#777b73 { "No challenges or invitations are waiting." }
                            anchor href="#new-game-actions" color=#526243 font-weight=bold { "Start a game" }
                        }
                    }
                    @for item in &dashboard.projection.pending {
                        @let counterparty = item.counterparty_display_name.as_deref()
                            .or(item.counterparty_username.as_deref())
                            .unwrap_or("Private invite");
                        @let heading = if item.kind == "CHALLENGE" && item.direction == "INCOMING" {
                            format!("Challenge from {counterparty}")
                        } else if item.kind == "CHALLENGE" {
                            format!("Challenge sent to {counterparty}")
                        } else if item.id == created_invitation_id {
                            "New private invitation".to_string()
                        } else {
                            "Active private invitation".to_string()
                        };
                        div id=(format!("pending-item-{}", item.id)) class="pending-item" data-direction=(item.direction.as_str())
                            direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) justify-content="space-between" align-items="center"
                            border-bottom=(("#e3ded2", 1)) padding-y=14 padding-x=4 gap="12px" {
                            div direction="row" align-items="center" gap="10px" {
                                @if let Some(avatar_url) = item.counterparty_avatar_url.as_deref() {
                                    image src=(avatar_url) alt="Challenge profile avatar" width="40" height="40" border-radius="999px";
                                }
                                div gap="3px" {
                                    span font-weight=bold { (heading) }
                                    @if let (Some(display_name), Some(handle)) = (
                                        item.counterparty_display_name.as_deref(),
                                        item.counterparty_username.as_deref(),
                                    ) && display_name != handle {
                                        span color=#777b73 { "@" (handle) }
                                    }
                                    @if item.kind == "INVITATION" && item.id != created_invitation_id {
                                        span color=#777b73 { "Link hidden after creation for security." }
                                    }
                                }
                            }
                            div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap="8px" {
                                @if item.kind == "CHALLENGE" && item.direction == "INCOMING" {
                                    form hx-post="/dashboard/action" hx-target="#app-page" {
                                        input type=hidden name="action" value="ACCEPT_CHALLENGE";
                                        input type=hidden name="challenge_id" value=(item.id.as_str());
                                        button type=submit padding-y=9 padding-x=13 background=#526243 color=#ffffff
                                            border=(("#526243", 1)) border-radius="8px" cursor=pointer { "Accept" }
                                    }
                                    form hx-post="/dashboard/action" hx-target="#app-page" {
                                        input type=hidden name="action" value="DECLINE_CHALLENGE";
                                        input type=hidden name="challenge_id" value=(item.id.as_str());
                                        button type=submit padding-y=9 padding-x=13 border=(("#c9c2b4", 1))
                                            border-radius="8px" cursor=pointer { "Decline" }
                                    }
                                } @else if item.kind == "CHALLENGE" {
                                    form hx-post="/dashboard/action" hx-target="#app-page" {
                                        input type=hidden name="action" value="CANCEL_CHALLENGE";
                                        input type=hidden name="challenge_id" value=(item.id.as_str());
                                        button type=submit padding-y=9 padding-x=13 border=(("#c9c2b4", 1))
                                            border-radius="8px" cursor=pointer { "Cancel" }
                                    }
                                } @else {
                                    form hx-post="/dashboard/action" hx-target="#app-page" {
                                        input type=hidden name="action" value="REVOKE_INVITATION";
                                        input type=hidden name="invitation_id" value=(item.id.as_str());
                                        button type=submit padding-y=9 padding-x=13 color=#814434 border=(("#d3a99d", 1))
                                            border-radius="8px" cursor=pointer { "Revoke" }
                                    }
                                }
                            }
                        }
                    }
                }
                main id="dashboard-main" data-dashboard-order="3" direction="column" gap="16px" align-items="start" {
                    details id="new-game-panel" width="100%" background=#ffffff border=(("#ded8c9", 1))
                        border-radius="16px" padding="20px" {
                        summary cursor="pointer" font-weight=bold { "New game" }
                        div padding-top="14px" { (start_game_component()) }
                    }
                    details id="profile-settings" width="100%" background=#ffffff border=(("#ded8c9", 1))
                        border-radius="16px" padding="20px" {
                        summary cursor="pointer" font-weight=bold { "Profile" }
                        div padding-top="14px" gap="12px" {
                            form method="post" action="/dashboard/action" gap="8px" {
                                input type="hidden" name="action" value="SET_DISPLAY_NAME";
                                span { "Display name" }
                                input type="text" id="profile-display-name" name="display_name" value=(display_name)
                                    required=true padding="10px" border=(("#b9b3a5", 1)) border-radius="8px";
                                button type="submit" background=#526243 color=#ffffff border-radius="8px" padding="10px" { "Save display name" }
                            }
                            form method="post" action="/dashboard/action" {
                                input type="hidden" name="action" value="USE_GOOGLE_NAME";
                                button type="submit" background=#ffffff color=#526243 border=(("#526243", 1)) border-radius="8px" padding="10px" { "Use Google name again" }
                            }
                            form method="post" action="/profile/avatar" enctype="multipart/form-data" gap="8px" {
                                span { "Upload a profile picture" }
                                input type="file" name="avatar";
                                button type="submit" background=#ffffff color=#526243 border=(("#526243", 1)) border-radius="8px" padding="10px" { "Upload picture" }
                            }
                            form method="post" action="/dashboard/action" {
                                input type="hidden" name="action" value="REMOVE_AVATAR";
                                button type="submit" background=#ffffff color=#7c3f38 border=(("#b57a73", 1)) border-radius="8px" padding="10px" { "Remove profile picture" }
                            }
                            form method="post" action="/dashboard/action" {
                                input type="hidden" name="action" value="USE_GOOGLE_AVATAR";
                                button type="submit" background=#ffffff color=#526243 border=(("#526243", 1)) border-radius="8px" padding="10px" { "Use Google photo again" }
                            }
                        }
                    }
                    details id="score-totals" width="100%" background=#ffffff border=(("#ded8c9", 1))
                        border-radius="16px" padding="20px" {
                        summary cursor="pointer" font-weight=bold { "Score history" }
                        span padding-top="12px" color=#5d6258 { (totals) }
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

/// Renders the compact fallback command controls beneath the visual board.
#[must_use]
pub fn turn_composer(game: &AuthorizedGamePage) -> Container {
    let action = format!("/games/{}/turn", game.game_id);
    let command_id = uuid::Uuid::new_v4().to_string();
    let idempotency_key = uuid::Uuid::new_v4().to_string();
    container! {
        section id="turn-composer" gap="12px" {
            h2 { "Turn controls" }
            span color=#5d6258 { "Tap a rack tile, then tap an open board square. Use these controls for non-placement turns." }
            div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap="10px" {
                form hx-post=(action.as_str()) hx-target="#app-page" {
                    input type=hidden name="command" value="PASS";
                    input type=hidden name="command_id" value=(command_id.as_str());
                    input type=hidden name="idempotency_key" value=(idempotency_key.as_str());
                    input type=hidden name="expected_revision" value=(game.view.revision);
                    button type=submit padding-y=10 padding-x=14 background=#ffffff color=#526243
                        border=(("#839276", 1)) border-radius="9px" cursor=pointer { "Pass" }
                }
                form hx-post=(action.as_str()) hx-target="#app-page" {
                    input type=hidden name="command" value="RESIGN";
                    input type=hidden name="command_id" value=(command_id.as_str());
                    input type=hidden name="idempotency_key" value=(idempotency_key.as_str());
                    input type=hidden name="expected_revision" value=(game.view.revision);
                    button type=submit padding-y=10 padding-x=14 background=#ffffff color=#814434
                        border=(("#d3a99d", 1)) border-radius="9px" cursor=pointer { "Resign" }
                }
            }
        }
    }
    .into()
}

fn compose_form_fields(game: &AuthorizedGamePage, draft: &TurnDraft, action: &str) -> Container {
    let encoded = draft_token(draft);
    container! {
        input type=hidden name="action" value=(action);
        input type=hidden name="expected_revision" value=(game.view.revision);
        input type=hidden name="draft" value=(encoded);
    }
    .into()
}

#[allow(clippy::too_many_lines)]
fn visual_board(
    game: &AuthorizedGamePage,
    draft: &TurnDraft,
    feedback: &DraftFeedback,
) -> Container {
    let action = format!("/games/{}/compose", game.game_id);
    let occupied = game
        .view
        .board
        .iter()
        .map(|(coordinate, letter, points)| (*coordinate, (*letter, *points)))
        .collect::<std::collections::BTreeMap<_, _>>();
    let pending = draft
        .placements
        .iter()
        .map(|placement| {
            (
                Coordinate::new(placement.x, placement.y),
                (placement.tile_id, placement.blank_letter),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let score_anchor = feedback.candidate.as_ref().and_then(|candidate| {
        candidate
            .play
            .words
            .iter()
            .flat_map(|word| word.coordinates.iter().copied())
            .filter(|coordinate| pending.contains_key(coordinate))
            .max_by_key(|coordinate| (coordinate.y, coordinate.x))
    });
    let score = feedback
        .candidate
        .as_ref()
        .map(|candidate| candidate.play.score);
    let invalid_score = feedback
        .candidate
        .as_ref()
        .is_some_and(|candidate| !candidate.is_valid());
    let square_size = draft.board_zoom.square_size();
    let score_bubble_left = score_anchor.map(|coordinate| {
        u32::from(coordinate.x) * (square_size + 2) + square_size.saturating_sub(12) + 6
    });
    let score_bubble_top = score_anchor
        .map(|coordinate| (u32::from(coordinate.y) * (square_size + 2) + 6).saturating_sub(16));
    let tile_font_size = if square_size < 36 { 15 } else { 20 };
    let premium_font_size = if square_size < 36 { 10 } else { 16 };
    let board_grid_width = u32::from(game.rules.board_size) * square_size
        + u32::from(game.rules.board_size.saturating_sub(1)) * 2;
    let board_frame_width = board_grid_width + 12;
    container! {
        section id="game-board" data-revision=(game.view.revision) data-board-zoom=(format!("{:?}", draft.board_zoom))
            position="absolute" top=0 right=0 bottom=0 left=0 direction="column" {
            div class="board-zoom-controls" width="100%" flex="0 0 auto" direction="row"
                justify-content="center" gap="7px" padding-y="6px" {
                form hx-post=(action.as_str()) hx-target="#app-page" {
                    (compose_form_fields(game, draft, "ZOOM_OUT"))
                    button type=submit padding-y="6px" padding-x="11px"
                        background=#173d2c color=#ffffff border=(("#35674e", 1)) border-radius="999px" cursor=pointer { "−" }
                }
                form hx-post=(action.as_str()) hx-target="#app-page" {
                    (compose_form_fields(game, draft, "ZOOM_RESET"))
                    button type=submit padding-y="6px" padding-x="11px" background=#173d2c color=#ffffff
                        border=(("#35674e", 1)) border-radius="999px" cursor=pointer { "Fit" }
                }
                form hx-post=(action.as_str()) hx-target="#app-page" {
                    (compose_form_fields(game, draft, "ZOOM_IN"))
                    button type=submit padding-y="6px" padding-x="11px"
                        background=#173d2c color=#ffffff border=(("#35674e", 1)) border-radius="999px" cursor=pointer { "+" }
                }
            }
            div class="board-viewport" width="100%" min-height=0 flex=1
                overflow-x="auto" overflow-y="auto" {
                div class="board-scroll-content" width="100%" height="100%" min-width=(board_frame_width)
                    min-height=(board_frame_width) align-items="center" justify-content="center" {
                    div data-board-grid-width=(board_grid_width) data-board-frame-width=(board_frame_width)
                        width=(board_frame_width) height=(board_frame_width) position="relative"
                        flex="0 0 auto" background=#594933 border=(("#493a28", 6)) border-radius="8px" gap="2px" {
                    @for y in 0..game.rules.board_size {
                        div direction="row" gap="2px" {
                            @for x in 0..game.rules.board_size {
                                @let coordinate = Coordinate::new(x, y);
                                @let committed = occupied.get(&coordinate).copied();
                                @let drafted = pending.get(&coordinate).copied();
                                @let premium = game.rules.premiums.get(&coordinate).copied();
                                @let required = feedback.guidance.required.contains(&coordinate);
                                @let eligible = feedback.guidance.eligible.contains(&coordinate);
                                @let latest = game.latest_play_coordinates.contains(&coordinate);
                                @let viewer_owned = game.viewer_play_coordinates.contains(&coordinate);
                                @let (background, label, color) = if let Some((letter, _)) = committed {
                                    ("#f6d47f", letter.to_string(), "#302515")
                                } else if let Some((tile_id, blank_letter)) = drafted {
                                    let letter = game.view.rack.iter().find(|(id, _, _)| *id == tile_id)
                                        .map(|(_, letter, _)| blank_letter.unwrap_or(*letter)).unwrap_or('?');
                                    ("#d8ecff", letter.to_string(), "#193751")
                                } else if coordinate == game.rules.start {
                                    ("#f5aaa7", "★".to_string(), "#6b2929")
                                } else {
                                    match premium {
                                        Some(PremiumSquare::Letter(2)) => ("#a9d9f0", "DL".to_string(), "#24546a"),
                                        Some(PremiumSquare::Letter(_)) => ("#5fb5dc", "TL".to_string(), "#123b4e"),
                                        Some(PremiumSquare::Word(2)) => ("#f2b5b8", "DW".to_string(), "#743236"),
                                        Some(PremiumSquare::Word(_)) => ("#e46e75", "TW".to_string(), "#ffffff"),
                                        None => ("#eee7d5", String::new(), "#756f64"),
                                    }
                                };
                                @let draft_points = drafted.and_then(|(tile_id, _)| {
                                    game.view.rack.iter().find(|(id, _, _)| *id == tile_id)
                                        .map(|(_, _, points)| *points)
                                });
                                @if let Some((tile_id, _)) = drafted {
                                    form hx-post=(action.as_str()) hx-target="#app-page" {
                                        (compose_form_fields(game, draft, "REMOVE_TILE"))
                                        input type=hidden name="tile_id" value=(tile_id);
                                        button type=submit class="board-square pending-square" width=(square_size) height=(square_size)
                                            background=#d8ecff color=#193751 border=(("#4381b3", 3)) border-radius="4px"
                                            align-items="center" justify-content="center" font-weight=bold position="relative" cursor=pointer {
                                            span font-size=(tile_font_size) { (label) }
                                            @if let Some(points) = draft_points {
                                                span class="board-tile-points" position="absolute" right="4px" bottom="2px" font-size="10px" { (points) }
                                            }
                                        }
                                    }
                                } @else if committed.is_some() {
                                    div class=(if latest { "board-square committed-square latest-move-square" } else if viewer_owned { "board-square committed-square viewer-owned-square" } else { "board-square committed-square" }) width=(square_size) height=(square_size)
                                        background=(background) color=(color) border=((if latest { "#2f8a57" } else { "#9a7d45" }, if latest { 3 } else { 1 }))
                                        border-radius="4px" align-items="center" justify-content="center" font-weight=bold position="relative" {
                                        @if viewer_owned {
                                            span class="viewer-tile-marker" position="absolute" top="4px" left="4px"
                                                width="5px" height="5px" background=#7f93a8 opacity=0.55 border-radius="999px" { }
                                        }
                                        span font-size=(tile_font_size) { (label) }
                                        @if let Some((_, points)) = committed {
                                            span class="board-tile-points" position="absolute" right="4px" bottom="2px" font-size="10px" { (points) }
                                        }
                                    }
                                } @else {
                                    form hx-post=(action.as_str()) hx-target="#app-page" {
                                        (compose_form_fields(game, draft, "PLACE_TILE"))
                                        input type=hidden name="x" value=(x);
                                        input type=hidden name="y" value=(y);
                                        button type=submit class="board-square open-square" data-x=(x) data-y=(y)
                                            width=(square_size) height=(square_size) background=(background) color=(color)
                                            border=(if required { ("#b96d2b", 3) } else if eligible { ("#527a4888", 3) } else { ("#aa9e85", 1) })
                                            position="relative" align-items="center" justify-content="center"
                                            font-weight=bold cursor=pointer {
                                            @if required {
                                                span class="required-square-highlight" position="absolute" top=0 left=0
                                                    width="100%" height="100%" background=#f3a64b opacity=0.3 { }
                                            } @else if eligible {
                                                span class="eligible-square-highlight" position="absolute" top=0 left=0
                                                    width="100%" height="100%" background=#7f9a78 opacity=0.3 { }
                                            }
                                            span position="relative" font-size=(premium_font_size) { (label) }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    @if let (Some(score), Some(left), Some(top)) = (score, score_bubble_left, score_bubble_top) {
                        span class=(if invalid_score { "draft-score-bubble invalid-draft-score" } else { "draft-score-bubble" })
                            position="absolute" left=(left) top=(top) min-width="31px" height="25px"
                            background=(if invalid_score { "#b4452f" } else { "#2f8a57" }) color=#ffffff
                            border=(if invalid_score { ("#7a2d20", 2) } else { ("#246d45", 2) }) border-radius="999px"
                            align-items="center" justify-content="center" font-size="12px" font-weight=bold {
                            (score)
                        }
                    }
                }
            }
        }
    }
    }
    .into()
}

fn visual_rack(game: &AuthorizedGamePage, draft: &TurnDraft) -> Container {
    let action = format!("/games/{}/compose", game.game_id);
    let can_compose = !game.completed;
    let rack = game
        .rack_order
        .iter()
        .filter_map(|ordered| {
            game.view
                .rack
                .iter()
                .find(|(tile_id, _, _)| tile_id == ordered)
        })
        .collect::<Vec<_>>();
    container! {
        section id="player-rack" width="100%" max-width="100%" position="relative" {
            div class="rack-tray" width="100%" max-width="100%" direction="row" overflow-x="hidden" overflow-y="hidden"
                justify-content="center" gap="3px"
                background=#6b4528 border=(("#3f2919", 4)) border-radius="14px" padding-y="8px" padding-x="6px" {
                @for (tile_id, letter, points) in rack {
                    @let placed = draft.placements.iter().any(|placement| placement.tile_id == *tile_id);
                    @let selected = draft.selected_tile == Some(*tile_id) || draft.rack_tile == Some(*tile_id);
                    @let exchange_selected = draft.exchange_tiles.contains(tile_id);
                    @let face = if *letter == ' ' { "?".to_string() } else { letter.to_string() };
                    @if can_compose || draft.mode == TurnMode::Play {
                        @let rack_action = rack_action(draft);
                        form hx-post=(action.as_str()) hx-target="#app-page"
                            width=calc(min(44, (dvw(100) - 80) / 7)) height=calc(min(44, (dvw(100) - 80) / 7))
                            min-width=0 flex-shrink=0 {
                            (compose_form_fields(game, draft, rack_action))
                            input type=hidden name="tile_id" value=(tile_id);
                            button type=submit class=(if selected || exchange_selected { "rack-tile rack-tile-selected" } else { "rack-tile" }) data-tile-id=(tile_id)
                                width=calc(min(44, (dvw(100) - 80) / 7)) height=calc(min(44, (dvw(100) - 80) / 7))
                                min-width=0 overflow-x="hidden" overflow-y="hidden"
                                background=(if selected { "#fff0a8" } else if exchange_selected { "#f7b9a9" } else if placed { "#b99e66" } else { "#f7d67f" }) color=#2e291f
                                border=((if selected { "#ffffff" } else if exchange_selected { "#ff796b" } else { "#b88a31" }, if selected || exchange_selected { 4 } else { 2 })) border-radius="7px" align-items="center" justify-content="center"
                                position="relative" font-weight=bold opacity=(if placed { 0.45 } else { 1.0 }) cursor=pointer {
                                span class="rack-tile-face" font-size=calc(min(18, (dvw(100) - 80) / 24)) { (face) }
                                span class="rack-tile-points" position="absolute" right="2px" bottom="1px"
                                    font-size=calc(min(8, (dvw(100) - 80) / 48)) { (points) }
                            }
                        }
                    } @else {
                        div class="rack-tile" data-tile-id=(tile_id)
                            width=calc(min(44, (dvw(100) - 80) / 7)) height=calc(min(44, (dvw(100) - 80) / 7))
                            min-width=0 flex-shrink=0 overflow-x="hidden" overflow-y="hidden"
                            background=#f7d67f color=#2e291f border=(("#b88a31", 2)) border-radius="7px"
                            align-items="center" justify-content="center" position="relative" font-weight=bold {
                            span class="rack-tile-face" font-size=calc(min(18, (dvw(100) - 80) / 24)) { (face) }
                            span class="rack-tile-points" position="absolute" right="2px" bottom="1px"
                                font-size=calc(min(8, (dvw(100) - 80) / 48)) { (points) }
                        }
                    }
                }
            }
            @if let Some(tile_id) = draft.selected_tile {
                @let selected_blank = game.view.rack.iter().any(|(id, letter, _)| *id == tile_id && *letter == ' ');
                @if selected_blank {
                    section class="blank-letter-layer" position="fixed" left="2vw" bottom="2dvh" width="96vw"
                        max-height="72dvh" overflow-y="auto"
                        background=#f4f1e8 color=#26382d border=(("#8e7651", 2)) border-radius="12px"
                        padding="10px" gap="7px" {
                        div width="100%" direction="row" justify-content="space-between" align-items="center" gap="8px" {
                            span font-weight=bold { "Choose the blank tile’s letter:" }
                            form hx-post=(action.as_str()) hx-target="#app-page" {
                                (compose_form_fields(game, draft, "CANCEL_TILE_PICK"))
                                button type=submit background=#ffffff color=#526243 border=(("#839276", 1))
                                    border-radius="999px" padding-y="5px" padding-x="9px" cursor=pointer { "Cancel" }
                            }
                        }
                        div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) justify-content="center" gap="5px" {
                            @for letter in 'A'..='Z' {
                                form hx-post=(action.as_str()) hx-target="#app-page" {
                                    (compose_form_fields(game, draft, "CHOOSE_BLANK_LETTER"))
                                    input type=hidden name="letter" value=(letter.to_string());
                                    button type=submit data-blank-letter=(letter.to_string()) width="38px" height="38px" border=(("#aa9e85", 1))
                                        background=(if draft.selected_blank_letter == Some(letter) { "#e8f1e3" } else { "#ffffff" })
                                        border-radius="7px" font-weight=bold cursor=pointer { (letter) }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    .into()
}

#[allow(clippy::too_many_lines)]
fn visual_turn_actions(game: &AuthorizedGamePage, draft: &TurnDraft) -> Container {
    let turn_action = format!("/games/{}/turn", game.game_id);
    let compose_action = format!("/games/{}/compose", game.game_id);
    let command_id = uuid::Uuid::new_v4().to_string();
    let idempotency_key = uuid::Uuid::new_v4().to_string();
    let viewer_turn = !game.completed && game.view.active_player == game.viewer_player;
    let play_score = draft_feedback(game, draft)
        .candidate
        .filter(wwmtf_game_domain::CandidatePlayAnalysis::is_valid)
        .map(|candidate| candidate.play.score);
    container! {
        section id="turn-actions" class="turn-composer action-hud" max-width="100%" position="relative" padding-y="4px" padding-x="6px"
            background=(if matches!(draft.mode, TurnMode::ConfirmExchange | TurnMode::ConfirmPass | TurnMode::ConfirmResign) { "#f7d8ae" } else { "#173d2c" })
            color=(if matches!(draft.mode, TurnMode::ConfirmExchange | TurnMode::ConfirmPass | TurnMode::ConfirmResign) { "#402c1e" } else { "#f4f0df" })
            border=((if matches!(draft.mode, TurnMode::ConfirmExchange | TurnMode::ConfirmPass | TurnMode::ConfirmResign) { "#f3b66e" } else { "#35674e" }, 1))
            border-radius="10px" {
            @if draft.mode == TurnMode::Exchange {
                div class="primary-action-row" direction="row" overflow-x="auto" overflow-y="hidden"
                    align-items="center" gap="6px" {
                    form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                        (compose_form_fields(game, draft, "REVIEW_EXCHANGE"))
                        button type=submit class="primary-turn-action" padding-y=10 padding-x=14 background=#2f8a57 color=#ffffff
                            border=(("#246d45", 2)) border-radius="11px" font-weight=bold cursor=pointer { "Review exchange" }
                    }
                    form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                        (compose_form_fields(game, draft, "CANCEL_MODE"))
                        button type=submit padding-y=10 padding-x=14 background=#ffffff color=#526243
                            border=(("#839276", 1)) border-radius="9px" cursor=pointer { "Cancel" }
                    }
                }
            } @else if draft.mode == TurnMode::ConfirmExchange {
                div class="primary-action-row" direction="row" overflow-x="auto" overflow-y="hidden" gap="6px" {
                    form hx-post=(turn_action.as_str()) hx-target="#app-page" {
                        input type=hidden name="command" value="EXCHANGE";
                        input type=hidden name="command_id" value=(command_id.as_str());
                        input type=hidden name="idempotency_key" value=(idempotency_key.as_str());
                        input type=hidden name="expected_revision" value=(game.view.revision);
                        @for (index, tile_id) in draft.exchange_tiles.iter().enumerate() {
                            input type=hidden name=(format!("tile_{index}")) value=(tile_id);
                        }
                        button type=submit class="primary-turn-action" padding-y=10 padding-x=14 background=#2f8a57 color=#ffffff
                            border=(("#246d45", 2)) border-radius="11px" font-weight=bold cursor=pointer { "Confirm exchange" }
                    }
                    form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                        (compose_form_fields(game, draft, "CANCEL_MODE"))
                        button type=submit padding-y=10 padding-x=14 background=#ffffff color=#526243
                            border=(("#839276", 1)) border-radius="9px" cursor=pointer { "Cancel" }
                    }
                }
            } @else if draft.mode == TurnMode::ConfirmPass {
                (confirmed_command_forms(game, draft, "PASS", "Confirm pass", &command_id, &idempotency_key))
            } @else if draft.mode == TurnMode::ConfirmResign {
                (confirmed_command_forms(game, draft, "RESIGN", "Confirm resignation", &command_id, &idempotency_key))
            } @else {
                div class="primary-action-row" direction="row" overflow-x="auto" overflow-y="hidden"
                    align-items="center" justify-content="space-between" gap="6px" {
                    @if !game.completed {
                        form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                            (compose_form_fields(game, draft, "SHUFFLE_RACK"))
                            button type=submit padding-y=7 padding-x=10 background=#ffffff color=#526243
                                border=(("#839276", 1)) border-radius="9px" cursor=pointer { "Shuffle" }
                        }
                    }
                    @if !draft.placements.is_empty() {
                        form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                            (compose_form_fields(game, draft, "CLEAR"))
                            button type=submit padding-y=7 padding-x=10 background=#ffffff color=#526243
                                border=(("#839276", 1)) border-radius="9px" cursor=pointer { "Recall" }
                        }
                    }
                    @if !draft.placements.is_empty() && viewer_turn {
                        form hx-post=(turn_action.as_str()) hx-target="#app-page" {
                            input type=hidden name="command" value="PLAY";
                            input type=hidden name="command_id" value=(command_id.as_str());
                            input type=hidden name="idempotency_key" value=(idempotency_key.as_str());
                            input type=hidden name="expected_revision" value=(game.view.revision);
                            @for (index, placement) in draft.placements.iter().enumerate() {
                                input type=hidden name=(format!("tile_{index}")) value=(placement.tile_id);
                                input type=hidden name=(format!("x_{index}")) value=(placement.x);
                                input type=hidden name=(format!("y_{index}")) value=(placement.y);
                                @if let Some(letter) = placement.blank_letter {
                                    input type=hidden name=(format!("blank_{index}")) value=(letter.to_string());
                                }
                            }
                            button type=submit class="primary-turn-action" padding-y=7 padding-x=12 background=#2f8a57 color=#ffffff
                                border=(("#246d45", 2)) border-radius="9px" font-weight=bold cursor=pointer {
                                @if let Some(score) = play_score { "Play · " (score) } @else { "Play" }
                            }
                        }
                    }
                    @if !game.completed && viewer_turn {
                        button type=button fx-click=(ActionType::toggle_display_by_id("more-turn-actions-menu"))
                            color=#f4f0df font-weight=bold padding-y="7px" padding-x="6px"
                            cursor=pointer { "More ···" }
                    }
                }
                @if !game.completed && viewer_turn {
                    div id="more-turn-actions-menu" hidden position="absolute" bottom="100%" right=0
                        background=#ffffff color=#26382d border=(("#d8d1c1", 1)) border-radius="12px"
                        padding="12px" gap="8px" {
                        @if game.exchange_available {
                            form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                                (compose_form_fields(game, draft, "BEGIN_EXCHANGE"))
                                button type=submit padding-y=8 padding-x=12 background=#ffffff color=#526243
                                    border=(("#839276", 1)) border-radius="9px" cursor=pointer { "Exchange" }
                            }
                        }
                        form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                            (compose_form_fields(game, draft, "CONFIRM_PASS"))
                            button type=submit padding-y=8 padding-x=12 background=#ffffff color=#526243
                                border=(("#839276", 1)) border-radius="9px" cursor=pointer { "Pass" }
                        }
                        form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                            (compose_form_fields(game, draft, "CONFIRM_RESIGN"))
                            button type=submit padding-y=8 padding-x=12 background=#ffffff color=#814434
                                border=(("#d3a99d", 1)) border-radius="9px" cursor=pointer { "Resign" }
                        }
                    }
                }
            }
        }
    }
    .into()
}

fn confirmed_command_forms(
    game: &AuthorizedGamePage,
    draft: &TurnDraft,
    command: &str,
    label: &str,
    command_id: &str,
    idempotency_key: &str,
) -> Container {
    let turn_action = format!("/games/{}/turn", game.game_id);
    let compose_action = format!("/games/{}/compose", game.game_id);
    container! {
        div class="primary-action-row" direction="row" overflow-x="auto" overflow-y="hidden" gap="6px" {
            form hx-post=(turn_action.as_str()) hx-target="#app-page" {
                input type=hidden name="command" value=(command);
                input type=hidden name="command_id" value=(command_id);
                input type=hidden name="idempotency_key" value=(idempotency_key);
                input type=hidden name="expected_revision" value=(game.view.revision);
                button type=submit class="primary-turn-action" padding-y=10 padding-x=14 background=#2f8a57 color=#ffffff
                    border=(("#246d45", 2)) border-radius="11px" font-weight=bold cursor=pointer { (label) }
            }
            form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                (compose_form_fields(game, draft, "CANCEL_MODE"))
                button type=submit padding-y=10 padding-x=14 background=#ffffff color=#526243
                    border=(("#839276", 1)) border-radius="9px" cursor=pointer { "Cancel" }
            }
        }
    }
    .into()
}

fn player_scoreboard_component(game: &AuthorizedGamePage) -> Container {
    let viewer_score = game
        .view
        .scores
        .iter()
        .find(|(player, _)| *player == game.viewer_player)
        .map_or(0, |(_, score)| *score);
    let opponent_score = game
        .view
        .scores
        .iter()
        .find(|(player, _)| *player != game.viewer_player)
        .map_or(0, |(_, score)| *score);
    let opponent_active = !game.completed && game.view.active_player != game.viewer_player;
    let viewer_active = !game.completed && game.view.active_player == game.viewer_player;
    let turn = if game.completed {
        "Game complete"
    } else if viewer_active {
        "Your move"
    } else {
        "Their move"
    };
    let viewer_initial = game
        .viewer_username
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    let opponent_initial = game
        .opponent_username
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    container! {
        section id="game-awareness" class="player-hud scoreboard-hud" min-width=0 flex=1
            overflow-x="hidden" background=#f4c95d color=#2d2515
            border=(("#ffe29a", 2)) border-radius="999px" padding-y="6px" padding-x="8px" {
            div direction="row" justify-content="space-between" align-items="center" gap="5px" min-width=0 {
                div id="viewer-scoreboard" direction="row" align-items="center" gap="5px" min-width=0 flex=1 overflow-x="hidden" {
                    @if let Some(avatar_url) = game.viewer_avatar_url.as_deref() {
                        image src=(avatar_url) alt="Your profile avatar" width="34" height="34" border-radius="999px";
                    } @else {
                        span width="34px" height="34px" border-radius="999px"
                            background=(if viewer_active { "#2e7049" } else { "#d6b361" }) color=#ffffff
                            border=((if viewer_active { "#73ba8d" } else { "#f2d98d" }, 2))
                            align-items="center" justify-content="center" font-size="16px" font-weight=bold { (viewer_initial) }
                    }
                    span min-width="34px" align-items="center" justify-content="center"
                        background=(if viewer_active { "#fff1bd" } else { "#e1bd63" })
                        border-radius="999px" padding-y="3px" padding-x="6px"
                        font-size="19px" font-weight=bold { (viewer_score) }
                }
                div min-width=0 align-items="center" overflow-x="hidden" {
                    span id="named-turn-status" font-size="11px" font-weight=bold white-space="preserve" { (turn) }
                    div id="live-status" font-size="10px" overflow-x="hidden" {
                        span id="live-status-connecting"
                            fx-global-shared-state-connecting=(live_status_action("live-status-connecting")) { "● Connecting" }
                        span id="live-status-connected" hidden
                            fx-global-shared-state-connected=(live_status_action("live-status-connected")) { "● Live" }
                        span hidden fx-global-shared-state-subscribed=(live_status_action("live-status-connected")) { }
                        span id="live-status-reconnecting" hidden color=#9a651f
                            fx-global-shared-state-reconnecting=(live_status_action("live-status-reconnecting")) { "● Reconnecting" }
                        span id="live-status-disconnected" hidden color=#9b3f35
                            fx-global-shared-state-disconnected=(live_status_action("live-status-disconnected")) { "● Offline" }
                    }
                }
                div id="opponent-scoreboard" direction="row" align-items="center" justify-content="end"
                    gap="5px" min-width=0 flex=1 overflow-x="hidden" {
                    span min-width="34px" align-items="center" justify-content="center"
                        background=(if opponent_active { "#fff1bd" } else { "#e1bd63" })
                        border-radius="999px" padding-y="3px" padding-x="6px"
                        font-size="19px" font-weight=bold { (opponent_score) }
                    @if let Some(avatar_url) = game.opponent_avatar_url.as_deref() {
                        image src=(avatar_url) alt="Opponent profile avatar" width="34" height="34" border-radius="999px";
                    } @else {
                        span width="34px" height="34px" border-radius="999px"
                            background=(if opponent_active { "#4d3821" } else { "#d6b361" }) color=#ffffff
                            border=((if opponent_active { "#7b5a35" } else { "#f2d98d" }, 2))
                            align-items="center" justify-content="center" font-size="16px" font-weight=bold { (opponent_initial) }
                    }
                }
            }
        }
    }
    .into()
}

fn completed_game_summary(game: &AuthorizedGamePage) -> Container {
    let viewer_score = game
        .view
        .scores
        .iter()
        .find(|(player, _)| *player == game.viewer_player)
        .map_or(0, |(_, score)| *score);
    let opponent = game
        .view
        .scores
        .iter()
        .find(|(player, _)| *player != game.viewer_player)
        .copied();
    let opponent_score = opponent.map_or(0, |(_, score)| score);
    let outcome = match game.view.winner {
        None => "Tie game".to_string(),
        Some(winner) if winner == game.viewer_player => {
            format!("{} won", game.viewer_display_name)
        }
        Some(_) => format!("{} won", game.opponent_display_name),
    };
    let viewer_adjustment = game
        .final_score_adjustments
        .get(&game.viewer_player)
        .copied()
        .unwrap_or_default();
    let opponent_adjustment = opponent
        .and_then(|(player, _)| game.final_score_adjustments.get(&player).copied())
        .unwrap_or_default();
    container! {
        section id="completed-game-summary" width="100%" background=#ffffff color=#26382d border=(("#c8b88f", 2))
            border-radius="12px" padding-y="8px" padding-x="10px" gap="6px" {
            div width="100%" direction="row" justify-content="space-between" align-items="center" gap="8px" {
                span font-size="18px" font-weight=bold { (outcome) }
                button type=button fx-click=(ActionType::no_display_by_id("completed-game-summary"))
                    background=#ffffff color=#526243 border=(("#839276", 1)) border-radius="999px"
                    padding-y="4px" padding-x="8px" cursor=pointer { "Close" }
            }
            div direction="row" justify-content="center" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap="12px" {
                span font-weight=bold { (game.viewer_display_name.as_str()) ": " (viewer_score) }
                span font-weight=bold { (game.opponent_display_name.as_str()) ": " (opponent_score) }
            }
            @if game.completion_reason.is_some() || viewer_adjustment != 0 || opponent_adjustment != 0 {
                details width="100%" {
                    summary cursor=pointer color=#5d6258 { "Game details" }
                    div padding-top="5px" gap="4px" {
                        @if let Some(reason) = &game.completion_reason {
                            span color=#5d6258 { "Completed by: " (reason.as_str()) }
                        }
                        @if viewer_adjustment != 0 || opponent_adjustment != 0 {
                            span color=#5d6258 {
                                "Final adjustments — " (game.viewer_display_name.as_str()) ": " (format!("{viewer_adjustment:+}"))
                                ", " (game.opponent_display_name.as_str()) ": " (format!("{opponent_adjustment:+}"))
                            }
                        }
                    }
                }
            }
        }
    }
    .into()
}

fn rules_component(game: &AuthorizedGamePage) -> Container {
    let exchange_requirement = game.rules.minimum_tiles_for_exchange;
    let rack_size = game.rules.rack_size;
    let full_rack_bonus = game.rules.full_rack_bonus;
    let scoreless_turn_limit = game.rules.scoreless_turn_limit;
    container! {
        details id="game-rules" width="100%" {
            summary cursor="pointer" font-weight=bold { "Rules and board key" }
            div padding-top="12px" gap="9px" color=#5d6258 {
                span { "Place tiles in one row or column to form connected words. The opening play must cover the center star. All formed words must be accepted by this game’s pinned dictionary." }
                span { "A tile scores its printed value. DL and TL multiply a newly placed tile; DW and TW multiply the whole word. Premium squares apply only when first covered." }
                span { "Playing all " (rack_size) " rack tiles adds a " (full_rack_bonus) "-point full-rack bonus." }
                span { "Exchange replaces selected tiles and ends your turn. It is available only while at least " (exchange_requirement) " tiles remain in the reserve; exchanged tile identities stay private." }
                span { "Passing ends your turn without playing. After each player passes three times consecutively (" (scoreless_turn_limit) " total consecutive passes), the game ends and the player with the higher current score wins." }
                span { "Resigning immediately completes the game for your opponent. Otherwise, emptying a rack after the reserve is exhausted completes the game and applies the remaining-tile score adjustments." }
            }
        }
    }
    .into()
}

fn live_status_action(visible_id: &str) -> ActionType {
    ActionType::Multi(vec![
        ActionType::no_display_by_id("live-status-connecting"),
        ActionType::no_display_by_id("live-status-connected"),
        ActionType::no_display_by_id("live-status-reconnecting"),
        ActionType::no_display_by_id("live-status-disconnected"),
        ActionType::display_by_id(visible_id),
    ])
}

#[allow(clippy::too_many_lines)]
fn visual_game_page(
    game: &AuthorizedGamePage,
    draft: &TurnDraft,
    error: Option<&str>,
) -> Container {
    let game_id = game.game_id.to_string();
    let short_game_id = game_id.chars().take(8).collect::<String>();
    let feedback = draft_feedback(game, draft);
    let board = visual_board(game, draft, &feedback);
    let draft_preview = draft_feedback_component(game, &feedback, draft);
    let scoreboard = player_scoreboard_component(game);
    let completed_summary = game.completed.then(|| completed_game_summary(game));
    let rack = visual_rack(game, draft);
    let actions = visual_turn_actions(game, draft);
    let history = move_history_component(game.game_id, &game.history);
    let game_channel = format!("game:{}", game.game_id);
    let game_path = if draft.has_composed_turn_input() {
        format!("/games/{game_id}?draft_revision={}", game.view.revision)
    } else {
        format!("/games/{game_id}")
    };
    let refresh_game = ActionType::Navigate { url: game_path };
    let turn_feedback_view = turn_feedback(error);
    container! {
        div id="app-page" class="game-scene" data-shared-state-channel=(game_channel.as_str())
            fx-global-shared-state-event=(refresh_game)
            direction="column" width=vw100 height=dvh100 min-height=dvh100
            position="fixed" top=0 right=0 bottom=0 left=0
            overflow-x="hidden" overflow-y="hidden"
            background=#123b2a color=#f4f0df gap="6px" {
            header id="scene-controls" width="100%" min-width=0 direction="row"
                align-items="center" gap="6px" padding-y="8px" padding-x="6px" {
                div class="header-action-slot header-action-left" min-width=0 flex=1 direction="row" justify-content="start" {
                    anchor href="/" flex="0 0 auto" background=#173326 color=#f4f0df border=(("#436854", 1))
                        border-radius="999px" padding-y="7px" padding-x="12px" font-weight=bold { "← Leave" }
                }
                (scoreboard)
                div class="header-action-slot header-action-right" min-width=0 flex=1 direction="row" justify-content="end" {
                    button type=button fx-click=(ActionType::toggle_display_by_id("activity-rail"))
                        background=#173326 color=#f4f0df border=(("#436854", 1))
                        border-radius="999px" padding-y="7px" padding-x="12px" font-weight=bold cursor=pointer { "Menu ···" }
                }
            }
            main id="game-layout" class="game-arena" width="100%" min-height=0
                flex=1 position="relative" overflow-x="hidden" overflow-y="hidden" {
                section id="board-region" position="absolute" top=0 right=0 bottom=0 left=0
                    overflow-x="hidden" overflow-y="hidden" {
                    (board)
                }
            }
            section id="turn-dock-layer" width="100%" flex="0 0 auto" align-items="center" padding-x="10px" {
                section id="play-console" class="game-console turn-dock" max-width="100%" align-items="center"
                    background=#2a523c border=(("#8e6b3d", 3)) border-radius="16px"
                    padding-y="6px" padding-x="8px" gap="5px" {
                    @if let Some(completed_summary) = completed_summary {
                        (completed_summary)
                    } @else if error.is_some() {
                        (turn_feedback_view)
                    } @else if game.view.active_player == game.viewer_player {
                        (draft_preview)
                    } @else {
                        section class="dock-message" width="100%" direction="row" justify-content="center"
                            background=#214c38 border=(("#376d53", 1)) border-radius="10px" padding-y="4px" padding-x="8px" {
                            span font-size="13px" font-weight=bold white-space="preserve" text-overflow="ellipsis" {
                                (game.opponent_display_name.as_str()) " is playing · rearrange your rack while you wait"
                            }
                        }
                    }
                    (rack)
                    @if !game.completed && game.view.active_player == game.viewer_player {
                        (actions)
                    } @else if !game.completed {
                        @let compose_action = format!("/games/{}/compose", game.game_id);
                        section id="turn-actions" class="primary-action-row" max-width="100%" direction="row"
                            justify-content="center" gap="6px" overflow-x="auto" overflow-y="hidden" {
                            form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                                (compose_form_fields(game, draft, "SHUFFLE_RACK"))
                                button type=submit padding-y=7 padding-x=12 background=#ffffff color=#526243
                                    border=(("#839276", 1)) border-radius="9px" cursor=pointer { "Shuffle" }
                            }
                            @if !draft.placements.is_empty() {
                                form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                                    (compose_form_fields(game, draft, "CLEAR"))
                                    button type=submit padding-y=7 padding-x=12 background=#ffffff color=#526243
                                        border=(("#839276", 1)) border-radius="9px" cursor=pointer { "Recall" }
                                }
                            }
                        }
                    }
                }
            }
            aside id="activity-rail" hidden position="fixed" top=62 right=6 width="340px" max-width="92vw"
                max-height="78vh" overflow-y="auto" background=#f6f0df color=#26382d
                border=(("#8e7651", 3)) border-radius="18px" padding-y="16px" padding-x="16px" gap="14px" {
                div direction="row" justify-content="space-between" align-items="start" gap="8px" {
                    div gap="2px" {
                        span color=#5d6e62 font-size="11px" font-weight=bold { "MATCH " (short_game_id) }
                        h2 { (game.opponent_display_name.as_str()) " vs " (game.viewer_display_name.as_str()) }
                    }
                    button type=button fx-click=(ActionType::no_display_by_id("activity-rail"))
                        background=#ffffff color=#526243 border=(("#839276", 1)) border-radius="999px"
                        padding-y="5px" padding-x="9px" cursor=pointer { "Close" }
                }
                @if let Some(latest) = &game.latest_action {
                    section id="latest-game-action" background=#e5d6ad border=(("#c7aa68", 1))
                        border-radius="12px" padding="12px" gap="3px" {
                        span color=#6d5727 font-size="11px" font-weight=bold { "LATEST" }
                        span font-weight=bold { (latest.as_str()) }
                    }
                }
                section id="recent-activity" gap="8px" {
                    h3 { "Move history" }
                    (history)
                }
                section id="game-reference" width="100%" border-top=(("#d8c9a7", 1))
                    padding-top="12px" gap="10px" {
                    (rules_component(game))
                }
            }
            aside id="game-definition-layer" hidden position="fixed" top=68 right="2%"
                width="440px" max-width="96%" max-height="70vh" overflow-y="auto"
                background=#f4f1e8 color=#26382d border=(("#8e7651", 3)) border-radius="18px"
                padding="18px" gap="12px" {
                span color=#5d6e62 font-size="11px" font-weight=bold { "WORD DEFINITION" }
                span { "Loading definition…" }
            }
        }
    }
    .into()
}

/// Renders public board/status/history and only the authorized viewer's rack.
#[must_use]
pub fn game_page(game: &AuthorizedGamePage) -> Container {
    visual_game_page(game, &TurnDraft::default(), None)
}

fn product_error_page(title: &str, message: &str) -> Container {
    let error = error_component(message);
    container! {
        div id="app-page" direction="column" align-items="center" min-height="100vh"
            background=#f4f1e8 padding=24 {
            main width="100%" max-width="560px" background=#ffffff border=(("#ded8c9", 1))
                border-radius=18 padding=32 gap=16 {
                anchor href="/" color=#526243 { "← Dashboard" }
                h1 { (title) }
                (error)
            }
        }
    }
    .into()
}

/// Builds session response effects using the runtime's cookie transport policy.
#[must_use]
pub fn authenticated_session_response(
    session: &str,
    csrf_token: &str,
    secure_cookies: bool,
) -> ResponseMetadata {
    let mut session_cookie = ResponseCookie::secure(crate::SESSION_COOKIE_NAME, session);
    session_cookie.secure = secure_cookies;
    session_cookie.same_site = hyperchad::renderer::SameSite::Lax;
    let mut csrf_cookie = ResponseCookie::secure(crate::CSRF_COOKIE_NAME, csrf_token);
    csrf_cookie.http_only = false;
    csrf_cookie.secure = secure_cookies;
    ResponseMetadata {
        cookies: vec![session_cookie, csrf_cookie],
        navigation: None,
    }
}

/// Builds cookie-expiration effects for logout using the runtime transport policy.
///
/// # Panics
///
/// Panics only if the static `/login` application path ceases to be a valid internal navigation.
#[must_use]
pub fn logged_out_response(secure_cookies: bool) -> ResponseMetadata {
    let mut session_cookie = ResponseCookie::expired(crate::SESSION_COOKIE_NAME);
    session_cookie.secure = secure_cookies;
    let mut csrf_cookie = ResponseCookie::expired(crate::CSRF_COOKIE_NAME);
    csrf_cookie.secure = secure_cookies;
    ResponseMetadata {
        cookies: vec![session_cookie, csrf_cookie],
        navigation: Some(
            hyperchad::renderer::ResponseNavigation::internal("/login")
                .expect("login is a valid internal navigation path"),
        ),
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
    use wwmtf_game_domain::Dictionary as _;

    use super::*;
    use crate::{
        SESSION_COOKIE_NAME, VerifiedExternalIdentity, accept_challenge, create_challenge,
        create_session, migrate_app, register,
    };

    async fn test_database() -> Arc<dyn Database> {
        let database: Arc<dyn Database> = Arc::from(
            switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens"),
        );
        migrate_app(&*database).await.expect("migrations run");
        database
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn google_profile_callback_failures_do_not_block_login_or_overwrite_customization() {
        block_on(async {
            let database = test_database().await;
            let now = OffsetDateTime::UNIX_EPOCH;
            let identity = VerifiedExternalIdentity::google(
                "https://accounts.google.com",
                "profile-lifecycle-subject",
                "Provider Name",
                Some("https://lh3.googleusercontent.com/avatar-one".to_string()),
            )
            .expect("identity validates");
            let (user_id, first_session) = crate::google_login_and_create_session(
                &*database,
                &identity,
                now,
                Duration::days(1),
            )
            .await
            .expect("first Google login succeeds");
            assert_eq!(
                crate::resolve_session(&*database, first_session.expose(), now)
                    .await
                    .expect("session resolves"),
                user_id
            );

            crate::set_custom_display_name(&*database, &user_id, "Custom Name", now)
                .await
                .expect("custom name saves");
            crate::remove_custom_avatar(&*database, &user_id, now)
                .await
                .expect("custom avatar removal saves");
            let refreshed = VerifiedExternalIdentity::google(
                "https://accounts.google.com",
                "profile-lifecycle-subject",
                "Changed Provider Name",
                Some("https://lh3.googleusercontent.com/avatar-two".to_string()),
            )
            .expect("refreshed identity validates");
            let (returning_user, returning_session) = crate::google_login_and_create_session(
                &*database,
                &refreshed,
                now + Duration::minutes(1),
                Duration::days(1),
            )
            .await
            .expect("returning Google login succeeds despite independent avatar sync");
            assert_eq!(returning_user, user_id);
            assert!(
                crate::resolve_session(
                    &*database,
                    returning_session.expose(),
                    now + Duration::minutes(1)
                )
                .await
                .is_ok()
            );
            let profile = crate::load_profile(&*database, &user_id)
                .await
                .expect("profile loads")
                .expect("profile exists");
            assert_eq!(profile.display_name, "Custom Name");
            assert_eq!(profile.avatar_source, crate::AvatarSource::CustomNone);

            crate::use_google_display_name(&*database, &user_id, now + Duration::minutes(2))
                .await
                .expect("name synchronization restores");
            crate::use_google_avatar(&*database, &user_id, now + Duration::minutes(2))
                .await
                .expect("avatar synchronization restores");
            let restored = VerifiedExternalIdentity::google(
                "https://accounts.google.com",
                "profile-lifecycle-subject",
                "Restored Provider Name",
                Some("https://lh3.googleusercontent.com/avatar-three".to_string()),
            )
            .expect("restored identity validates");
            crate::google_login_and_create_session(
                &*database,
                &restored,
                now + Duration::minutes(3),
                Duration::days(1),
            )
            .await
            .expect("restored provider ownership refreshes");
            let profile = crate::load_profile(&*database, &user_id)
                .await
                .expect("profile reloads")
                .expect("profile exists");
            assert_eq!(profile.display_name, "Restored Provider Name");
            assert_eq!(profile.avatar_source, crate::AvatarSource::Google);

            let invalid_avatar = crate::download_google_avatar(
                "https://example.com/not-google.png",
                std::time::Duration::from_millis(10),
            )
            .await;
            assert!(invalid_avatar.is_err());
            assert!(
                crate::resolve_session(
                    &*database,
                    returning_session.expose(),
                    now + Duration::minutes(3)
                )
                .await
                .is_ok(),
                "avatar enrichment failure cannot invalidate authentication"
            );
        });
    }

    #[test]
    fn dashboard_explains_exact_stable_handle_challenges() {
        let page = start_game_component().to_string();
        assert!(page.contains("exact stable @handle"));
        assert!(page.contains("Exact @handle"));
    }

    #[test]
    fn google_callback_query_requires_code_and_state_without_reflecting_values() {
        let malformed = RouteRequest::from_path("/auth/google/callback", RequestInfo::default());
        assert!(google_callback_query(&malformed).is_err());

        let mut valid = malformed;
        valid
            .query
            .insert("code".to_string(), "private-code".to_string());
        valid
            .query
            .insert("state".to_string(), "private-state".to_string());
        let parsed = google_callback_query(&valid).expect("complete callback query parses");
        assert_eq!(parsed.code, "private-code");
        assert_eq!(parsed.state, "private-state");
    }

    #[test]
    fn custom_avatar_route_accepts_multipart_file_content() {
        block_on(async {
            let database = test_database().await;
            let now = OffsetDateTime::UNIX_EPOCH;
            let user = register(
                &*database,
                "avatar-route-user",
                "correct horse battery staple",
                now,
            )
            .await
            .expect("user registers");
            crate::create_google_profile(&*database, &user, "Avatar User", None, now)
                .await
                .expect("profile creates");
            let session = create_session(&*database, &user, now, Duration::days(1))
                .await
                .expect("session creates");
            let source = image::DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2));
            let mut png = Vec::new();
            source
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .expect("PNG encodes");
            let boundary = "wwmtf-avatar-boundary";
            let mut body = format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"avatar\"; filename=\"avatar.png\"\r\nContent-Type: image/png\r\n\r\n"
            )
            .into_bytes();
            body.extend_from_slice(&png);
            body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
            let mut request = RouteRequest::from_path("/profile/avatar", RequestInfo::default());
            request.method = "POST".parse().expect("POST parses");
            request.headers.insert(
                "content-type".to_string(),
                format!("multipart/form-data; boundary={boundary}"),
            );
            request.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                session.expose().to_string(),
            );
            request.body = Some(std::sync::Arc::new(body.into()));

            let rendered = custom_avatar_route(&*database, &request, now)
                .await
                .display_to_string(false, false)
                .expect("dashboard renders");
            assert!(rendered.contains("Profile"));
            let profile = crate::load_profile(&*database, &user)
                .await
                .expect("profile loads")
                .expect("profile exists");
            assert_eq!(profile.avatar_source, crate::AvatarSource::Custom);
        });
    }

    #[test]
    fn invitation_creation_returns_the_only_shareable_link_and_uses_display_name() {
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
            let session = create_session(&*database, &alice, now, Duration::days(1))
                .await
                .expect("session creates");
            let dispatcher = crate::GameSharedStateDispatcher::new(database.clone());
            let mut request = RouteRequest::from_path("/dashboard/action", RequestInfo::default());
            request.method = "POST".parse().expect("POST parses");
            request.headers.insert(
                "content-type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            );
            request.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                session.expose().to_string(),
            );
            request.body = Some(std::sync::Arc::new("action=CREATE_INVITATION".into()));

            let rendered = dashboard_action_route(
                &*database,
                &dispatcher,
                "https://games.example.test",
                &request,
                now,
            )
            .await
            .display_to_string(false, false)
            .expect("dashboard renders");

            assert!(rendered.contains("Invitation ready"));
            assert!(rendered.contains("https://games.example.test/join?invite="));
            assert!(rendered.contains("Signed in as alice"));
            assert!(!rendered.contains(&format!("Signed in as {alice}")));
        });
    }

    #[test]
    fn invitation_link_preserves_token_through_account_entry() {
        let token = "private-invitation-token";
        let request =
            RouteRequest::from_path(&format!("/join?invite={token}"), RequestInfo::default());
        let page = invitation_page(token, false)
            .display_to_string(false, false)
            .expect("invitation renders");
        assert_eq!(request.query.get("invite").map(String::as_str), Some(token));
        assert!(page.contains(&format!("/login?invite={token}")));
        assert!(!page.contains("/register"));

        let login = login_page_with_invitation(None, token, false, true)
            .display_to_string(false, false)
            .expect("login renders");
        assert!(login.contains(&format!("/auth/google/start?invite={token}")));
        assert!(login.contains(token));

        block_on(async {
            let database = test_database().await;
            let owner = crate::register(
                &*database,
                "invitation-owner",
                "correct horse battery staple",
                OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .expect("owner registers");
            let (invitation_id, token) = crate::create_invitation(
                &*database,
                &owner,
                OffsetDateTime::UNIX_EPOCH,
                Duration::days(1),
            )
            .await
            .expect("invitation creates");
            let request = RouteRequest::from_path(
                &format!("/auth/google/start?invite={}", token.expose()),
                RequestInfo::default(),
            );
            let continuation =
                crate::active_invitation_id(&*database, token.expose(), OffsetDateTime::UNIX_EPOCH)
                    .await
                    .expect("continuation resolves");
            assert_eq!(continuation, invitation_id);
            let query = request
                .query
                .get("invite")
                .expect("query retains token at entry");
            assert_ne!(query, &continuation);
        });
    }

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
            let view = turn_rejection(reason);
            let rendered = view
                .fragments
                .first()
                .expect("rejection has a feedback fragment")
                .container
                .display_to_string(false, false)
                .expect("error renders");
            assert!(view.primary.is_none());
            assert!(rendered.contains(expected), "{rendered}");
            assert!(rendered.contains("game-error"));
        }
    }

    #[test]
    fn invalid_word_status_only_names_rejected_words() {
        assert_eq!(
            invalid_words_message(&["BAR".to_string()]),
            "BAR is not a valid word"
        );
        assert_eq!(
            invalid_words_message(&["ABC".to_string(), "BAZ".to_string()]),
            "ABC, BAZ are not valid words"
        );
    }

    #[test]
    fn turn_draft_round_trips_and_places_selected_tiles() {
        let draft = TurnDraft {
            selected_tile: Some(7),
            selected_blank_letter: Some('Q'),
            board_zoom: BoardZoom::Large,
            placements: vec![DraftPlacement {
                tile_id: 3,
                x: 7,
                y: 7,
                blank_letter: None,
            }],
            ..TurnDraft::default()
        };
        assert_eq!(parse_draft(&draft_token(&draft)), Some(draft));
    }

    #[test]
    fn board_zoom_is_bounded_and_defaults_for_older_drafts() {
        assert_eq!(BoardZoom::Fit.zoom_out(), BoardZoom::Fit);
        assert_eq!(BoardZoom::Fit.zoom_in(), BoardZoom::Compact);
        assert_eq!(BoardZoom::Compact.zoom_out(), BoardZoom::Fit);
        assert_eq!(BoardZoom::Compact.zoom_in(), BoardZoom::Normal);
        assert_eq!(BoardZoom::Normal.zoom_in(), BoardZoom::Large);
        assert_eq!(BoardZoom::Large.zoom_in(), BoardZoom::Large);
        assert_eq!(BoardZoom::Fit.square_size(), 20);
        assert_eq!(BoardZoom::Compact.square_size(), 28);
        assert_eq!(BoardZoom::Normal.square_size(), 44);
        assert_eq!(BoardZoom::Large.square_size(), 56);

        let legacy = r#"{"selected_tile":null,"selected_blank_letter":null,"placements":[],"rack_tile":null,"exchange_tiles":[],"mode":"Play"}"#;
        let draft: TurnDraft = serde_json::from_str(legacy).expect("legacy draft parses");
        assert_eq!(draft.board_zoom, BoardZoom::Normal);
    }

    #[test]
    fn non_rack_actions_break_the_consecutive_rack_click_sequence() {
        for action in [
            "PLACE_TILE",
            "REMOVE_TILE",
            "CHOOSE_BLANK_LETTER",
            "BEGIN_EXCHANGE",
            "CANCEL_MODE",
        ] {
            let mut draft = TurnDraft {
                selected_tile: Some(7),
                rack_tile: Some(7),
                ..TurnDraft::default()
            };

            draft.begin_action(action);

            assert_eq!(draft.rack_tile, None, "{action}");
            assert_eq!(draft.selected_tile, Some(7), "{action}");
        }
    }

    #[test]
    fn consecutive_rack_actions_preserve_the_swap_anchor() {
        for action in [
            "PICK_RACK_TILE",
            "SWAP_RACK_TILES",
            "SHUFFLE_RACK",
            "ZOOM_OUT",
            "ZOOM_RESET",
            "ZOOM_IN",
        ] {
            let mut draft = TurnDraft {
                selected_tile: Some(7),
                rack_tile: Some(7),
                ..TurnDraft::default()
            };

            draft.begin_action(action);

            assert_eq!(draft.rack_tile, Some(7), "{action}");
        }
    }

    #[test]
    fn rack_click_only_swaps_after_an_uninterrupted_rack_click() {
        let mut draft = TurnDraft {
            selected_tile: Some(7),
            rack_tile: Some(7),
            ..TurnDraft::default()
        };
        assert_eq!(rack_action(&draft), "SWAP_RACK_TILES");

        draft.begin_action("PLACE_TILE");

        assert_eq!(rack_action(&draft), "PICK_RACK_TILE");
        assert_eq!(draft.selected_tile, Some(7));
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
    #[allow(clippy::too_many_lines)]
    fn dashboard_actions_publish_private_refreshes_for_waiting_users() {
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
            let alice_session = create_session(&*database, &alice, now, Duration::days(1))
                .await
                .expect("Alice session creates");
            let bob_session = create_session(&*database, &bob, now, Duration::days(1))
                .await
                .expect("Bob session creates");
            let dispatcher = crate::GameSharedStateDispatcher::new(database.clone());
            let bob_context = AuthenticatedTransportContext {
                participant_id: ParticipantId::new(&bob),
                identity_binding: "bob-browser".to_string(),
            };
            let bob_events = dispatcher
                .subscribe_channel(&bob_context, &crate::dashboard_channel(&bob))
                .await
                .expect("Bob dashboard subscribes");
            let initial = bob_events
                .recv_async()
                .await
                .expect("initial dashboard arrives");

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
            let rendered = dashboard_action_route(
                &*database,
                &dispatcher,
                "http://localhost:8343",
                &challenge,
                now,
            )
            .await
            .display_to_string(false, false)
            .expect("dashboard renders");
            assert!(rendered.contains("OUTGOING"));

            let refresh = bob_events.recv_async().await.expect("Bob refresh arrives");
            assert!(refresh.revision.value() > initial.revision.value());
            let view: crate::DashboardLiveView = refresh
                .payload
                .deserialize()
                .expect("dashboard view decodes");
            assert!(
                view.projection
                    .pending
                    .iter()
                    .any(|item| { item.kind == "CHALLENGE" && item.direction == "INCOMING" })
            );
            let challenge_id = view
                .projection
                .pending
                .iter()
                .find(|item| item.kind == "CHALLENGE" && item.direction == "INCOMING")
                .expect("incoming challenge is projected")
                .id
                .clone();
            let alice_context = AuthenticatedTransportContext {
                participant_id: ParticipantId::new(&alice),
                identity_binding: "alice-browser".to_string(),
            };
            let alice_events = dispatcher
                .subscribe_channel(&alice_context, &crate::dashboard_channel(&alice))
                .await
                .expect("Alice dashboard subscribes");
            let alice_initial = alice_events
                .recv_async()
                .await
                .expect("initial Alice dashboard arrives");

            let mut accept = RouteRequest::from_path("/dashboard/action", RequestInfo::default());
            accept.method = "POST".parse().expect("POST parses");
            accept.headers.insert(
                "content-type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            );
            accept.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                bob_session.expose().to_string(),
            );
            accept.body = Some(std::sync::Arc::new(
                format!("action=ACCEPT_CHALLENGE&challenge_id={challenge_id}").into(),
            ));
            dashboard_action_route(
                &*database,
                &dispatcher,
                "http://localhost:8343",
                &accept,
                now,
            )
            .await;

            let alice_refresh = alice_events
                .recv_async()
                .await
                .expect("Alice acceptance refresh arrives");
            assert!(alice_refresh.revision.value() > alice_initial.revision.value());
            let alice_view: crate::DashboardLiveView = alice_refresh
                .payload
                .deserialize()
                .expect("Alice dashboard view decodes");
            assert!(alice_view.projection.pending.is_empty());
            assert_eq!(alice_view.projection.games.len(), 1);
        });
    }

    #[test]
    #[allow(clippy::too_many_lines)]
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
            assert!(dashboard.contains("Game with bob"));
            assert!(dashboard.contains("You 0 – 0 bob"));
            assert!(dashboard.contains("Signed in as"));
            assert!(dashboard.contains("new-game-actions"));
            assert!(dashboard.contains("name=\"action\" value=\"CHALLENGE\""));
            assert!(dashboard.contains("name=\"action\" value=\"CREATE_INVITATION\""));
            assert!(dashboard.contains("name=\"action\" value=\"REDEEM_INVITATION\""));
            assert!(dashboard.contains("dashboard-action-progress"));
            assert!(dashboard.contains("dashboard-action-error"));
            assert!(dashboard.contains("dashboard-action-status"));
            let active_position = dashboard
                .find("id=\"active-games\"")
                .expect("games section");
            let pending_position = dashboard
                .find("id=\"pending-games\"")
                .expect("pending section");
            let actions_position = dashboard
                .find("id=\"dashboard-main\"")
                .expect("actions section");
            assert!(active_position < pending_position && pending_position < actions_position);
            assert!(!dashboard.contains("data-shared-state-refresh-"));

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
            assert!(!page.contains("data-shared-state-refresh-"));
            assert!(!page.contains("id=\"turn-feedback\""));
            assert!(!page.contains("draft_revision="));
            assert!(page.contains("name=\"expected_revision\" value=\"1\""));
            assert!(page.contains("turn-actions"));
            assert!(page.contains("game-awareness"));
            assert!(page.contains("viewer-scoreboard"));
            assert!(page.contains("opponent-scoreboard"));
            assert!(page.contains("game-scene"));
            assert!(page.contains("game-arena"));
            assert!(page.contains("player-hud"));
            assert!(page.contains("action-hud"));
            assert!(page.contains("header-action-left"));
            assert!(page.contains("header-action-right"));
            assert!(page.contains("activity-rail"));
            assert!(page.contains("turn-dock"));
            assert!(page.contains("id=\"turn-dock-layer\""));
            assert!(page.contains("sx-flex-grow=\"0\""));
            assert!(page.contains("sx-flex-shrink=\"0\""));
            assert!(!page.contains("sx-padding-bottom=\"330\""));
            assert!(!page.contains("sx-max-height=\"40vh\""));
            assert!(!page.contains("sx-max-height=\"300px\""));
            assert!(page.contains("sx-flex-grow=\"1\""));
            assert!(page.contains("sx-position=\"fixed\""));
            assert!(page.contains("sx-height=\"100dvh\""));
            assert!(page.contains("sx-min-height=\"100dvh\""));
            assert!(page.contains("sx-position=\"absolute\""));
            assert!(page.contains("sx-top=\"0\""));
            assert!(page.contains("sx-bottom=\"0\""));
            assert!(page.contains("sx-max-height=\"78vh\""));
            assert!(page.contains("sx-overflow-y=\"auto\""));
            assert!(page.contains("board-viewport"));
            assert!(page.contains("board-scroll-content"));
            assert!(page.contains("board-zoom-controls"));
            assert!(page.contains("value=\"ZOOM_OUT\""));
            assert!(page.contains("value=\"ZOOM_RESET\""));
            assert!(page.contains("value=\"ZOOM_IN\""));
            assert!(page.contains("value=\"SHUFFLE_RACK\""));
            assert!(page.contains("Shuffle"));
            assert!(page.contains("Menu ···"));
            assert!(!page.contains("primary-turn-action"));
            assert!(page.contains("alice"));
            assert!(page.contains("bob"));
            assert!(page.contains("named-turn-status"));
            assert!(page.contains("live-status-connecting"));
            assert!(page.contains("live-status-connected"));
            assert!(page.contains("live-status-reconnecting"));
            assert!(page.contains("live-status-disconnected"));
            assert!(page.contains("draft-preview"));
            assert!(page.contains("Start by covering the starred center square."));
            assert!(page.contains("value=\"CONFIRM_PASS\""));
            assert!(page.contains("value=\"CONFIRM_RESIGN\""));
            assert!(!page.contains("name=\"command\" value=\"PASS\""));
            assert!(!page.contains("name=\"command\" value=\"RESIGN\""));
            assert!(page.contains("open-square"));
            assert!(page.contains("rack-tile"));
            assert!(!page.contains("board-tile-points"));
            assert!(page.contains("data-board-grid-width=\"688\""));
            assert!(page.contains("data-board-frame-width=\"700\""));
            assert!(!page.contains("width:720px"));
            assert!(page.contains("DL"));
            assert!(page.contains("TW"));
            assert!(page.contains("eligible-square-highlight"));
            assert!(page.contains("game-rules"));
            assert!(page.contains("50-point full-rack bonus"));
            assert!(page.contains("6 total consecutive passes"));
            assert!(page.contains("dock-message"));
            assert!(!page.contains("provide matching board coordinates"));
            assert!(!page.contains("pending-editor-0"));
            assert!(!page.contains("bag"));
        });
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn compose_route_renders_server_derived_ready_preview() {
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
            let alice_session = create_session(&*database, &alice, now, Duration::days(1))
                .await
                .expect("Alice session creates");
            let bob_session = create_session(&*database, &bob, now, Duration::days(1))
                .await
                .expect("Bob session creates");
            let state = crate::recover_game(&*database, game_id)
                .await
                .expect("game loads");
            let alice_player = crate::player_for_user(&*database, game_id, &alice)
                .await
                .expect("Alice is seated");
            let bob_player = crate::player_for_user(&*database, game_id, &bob)
                .await
                .expect("Bob is seated");
            let (active_player, session, inactive_player, inactive_session) =
                if state.active_player == alice_player {
                    (alice_player, alice_session, bob_player, bob_session)
                } else {
                    (bob_player, bob_session, alice_player, alice_session)
                };
            let rack = &state.racks[&active_player];
            let (first, second, word) = rack
                .iter()
                .enumerate()
                .find_map(|(first_index, first)| {
                    rack.iter().enumerate().find_map(|(second_index, second)| {
                        (first_index != second_index).then(|| {
                            let first_letter = match first.face {
                                wwmtf_game_domain::TileFace::Letter(letter) => letter,
                                wwmtf_game_domain::TileFace::Blank => return None,
                            };
                            let second_letter = match second.face {
                                wwmtf_game_domain::TileFace::Letter(letter) => letter,
                                wwmtf_game_domain::TileFace::Blank => return None,
                            };
                            let word = format!("{first_letter}{second_letter}");
                            wwmtf_game_domain::bundled_dictionary()
                                .contains(&word)
                                .then_some((first, second, word))
                        })?
                    })
                })
                .expect("seeded rack has a two-letter dictionary word");
            let draft = TurnDraft {
                selected_tile: None,
                selected_blank_letter: None,
                placements: vec![
                    DraftPlacement {
                        tile_id: first.id.get(),
                        x: 7,
                        y: 7,
                        blank_letter: None,
                    },
                    DraftPlacement {
                        tile_id: second.id.get(),
                        x: 8,
                        y: 7,
                        blank_letter: None,
                    },
                ],
                ..TurnDraft::default()
            };
            let cookies = std::collections::BTreeMap::from([(
                SESSION_COOKIE_NAME.to_string(),
                session.expose().to_string(),
            )]);
            let game = load_authorized_game_page(&*database, &cookies, &game_id.to_string(), now)
                .await
                .expect("active player's game loads");
            let rendered = visual_game_page(&game, &draft, None)
                .display_to_string(false, false)
                .expect("compose response renders");

            assert!(rendered.contains("draft-preview"));
            assert!(rendered.contains(&word));
            assert!(rendered.contains("points"));
            assert!(rendered.contains("ready to play"));
            assert_eq!(rendered.matches("primary-turn-action").count(), 1);
            assert!(rendered.contains("Play ·"));
            assert!(rendered.contains("board-tile-points"));
            assert!(rendered.contains("draft-score-bubble"));
            assert!(rendered.contains("sx-position=\"absolute\""));
            assert!(rendered.contains(&format!("draft_revision={}", game.view.revision)));

            let inactive_rack = &state.racks[&inactive_player];
            let (inactive_first, inactive_second) = inactive_rack
                .iter()
                .enumerate()
                .find_map(|(first_index, first)| {
                    inactive_rack
                        .iter()
                        .enumerate()
                        .find_map(|(second_index, second)| {
                            (first_index != second_index).then(|| {
                                let first_letter = match first.face {
                                    wwmtf_game_domain::TileFace::Letter(letter) => letter,
                                    wwmtf_game_domain::TileFace::Blank => return None,
                                };
                                let second_letter = match second.face {
                                    wwmtf_game_domain::TileFace::Letter(letter) => letter,
                                    wwmtf_game_domain::TileFace::Blank => return None,
                                };
                                wwmtf_game_domain::bundled_dictionary()
                                    .contains(&format!("{first_letter}{second_letter}"))
                                    .then_some((first, second))
                            })?
                        })
                })
                .expect("inactive seeded rack has a two-letter dictionary word");
            let inactive_draft = TurnDraft {
                placements: vec![
                    DraftPlacement {
                        tile_id: inactive_first.id.get(),
                        x: 7,
                        y: 7,
                        blank_letter: None,
                    },
                    DraftPlacement {
                        tile_id: inactive_second.id.get(),
                        x: 8,
                        y: 7,
                        blank_letter: None,
                    },
                ],
                ..TurnDraft::default()
            };
            let inactive_cookies = std::collections::BTreeMap::from([(
                SESSION_COOKIE_NAME.to_string(),
                inactive_session.expose().to_string(),
            )]);
            let inactive_game =
                load_authorized_game_page(&*database, &inactive_cookies, &game_id.to_string(), now)
                    .await
                    .expect("inactive player's game loads");
            assert_ne!(
                inactive_game.view.active_player,
                inactive_game.viewer_player
            );
            let inactive_candidate = inactive_game
                .analyze_candidate_play(&inactive_draft.domain_placements())
                .expect("inactive plan analyzes");
            assert!(inactive_candidate.is_valid());
            let inactive_feedback = draft_feedback(&inactive_game, &inactive_draft);
            assert!(inactive_feedback.candidate.is_some());
            let inactive_preview =
                draft_feedback_component(&inactive_game, &inactive_feedback, &inactive_draft)
                    .display_to_string(false, false)
                    .expect("inactive preview renders");
            assert!(inactive_preview.contains("planned for your turn"));
            let inactive_rendered = visual_game_page(&inactive_game, &inactive_draft, None)
                .display_to_string(false, false)
                .expect("inactive plan renders");
            assert!(inactive_rendered.contains("pending-square"));
            assert!(inactive_rendered.contains("points"));
            assert!(inactive_rendered.contains("Recall"));
            assert!(inactive_rendered.contains("value=\"CLEAR\""));
            assert!(!inactive_rendered.contains("Play ·"));
            assert!(!inactive_rendered.contains("value=\"CONFIRM_PASS\""));
            assert_eq!(inactive_rendered.matches("primary-turn-action").count(), 0);

            let initial_order = game.rack_order.clone();
            let initial_revision = game.view.revision;
            let mut shuffle_request = RouteRequest::from_path(
                &format!("/games/{game_id}/compose"),
                RequestInfo::default(),
            );
            shuffle_request.method = "POST".parse().expect("POST parses");
            shuffle_request.headers.insert(
                "content-type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            );
            shuffle_request.cookies = cookies.clone();
            shuffle_request.body = Some(std::sync::Arc::new(
                format!(
                    "action=SHUFFLE_RACK&expected_revision={}&draft={}",
                    game.view.revision,
                    draft_token(&draft)
                )
                .into(),
            ));
            let shuffled =
                game_compose_route(&*database, &shuffle_request, &game_id.to_string(), now)
                    .await
                    .display_to_string(false, false)
                    .expect("shuffle response renders");
            let reloaded =
                load_authorized_game_page(&*database, &cookies, &game_id.to_string(), now)
                    .await
                    .expect("shuffled game reloads");
            let mut original_members = initial_order.clone();
            let mut shuffled_members = reloaded.rack_order.clone();
            original_members.sort_unstable();
            shuffled_members.sort_unstable();
            assert_eq!(original_members, shuffled_members);
            assert_eq!(reloaded.view.revision, initial_revision);
            assert!(shuffled.contains("pending-square"));
            assert!(shuffled.contains("data-board-zoom=\"Normal\""));

            let rack_only_draft = TurnDraft {
                selected_tile: Some(first.id.get()),
                rack_tile: Some(first.id.get()),
                ..TurnDraft::default()
            };
            let rack_only = visual_game_page(&game, &rack_only_draft, None)
                .display_to_string(false, false)
                .expect("rack-only response renders");
            assert!(!rack_only.contains("draft_revision="));
            assert!(!rack_only.contains("The board changed while you were composing"));
        });
    }

    #[test]
    fn turn_action_controls_require_review_before_destructive_commands() {
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
            let state = crate::recover_game(&*database, game_id)
                .await
                .expect("game loads");
            let active_user = if crate::player_for_user(&*database, game_id, &alice)
                .await
                .expect("Alice is seated")
                == state.active_player
            {
                alice
            } else {
                bob
            };
            let session = create_session(&*database, &active_user, now, Duration::days(1))
                .await
                .expect("active session creates");
            let cookies = std::collections::BTreeMap::from([(
                SESSION_COOKIE_NAME.to_string(),
                session.expose().to_string(),
            )]);
            let game = load_authorized_game_page(&*database, &cookies, &game_id.to_string(), now)
                .await
                .expect("game loads");

            let initial = visual_turn_actions(&game, &TurnDraft::default())
                .display_to_string(false, false)
                .expect("actions render");
            assert!(initial.contains("value=\"CONFIRM_PASS\""));
            assert!(initial.contains("value=\"CONFIRM_RESIGN\""));
            assert!(initial.contains("more-turn-actions-menu"));
            assert!(initial.contains("fx-click"));
            assert!(!initial.contains("<details"));
            assert!(!initial.contains("<summary"));
            assert!(!initial.contains("name=\"command\" value=\"PASS\""));
            assert!(!initial.contains("name=\"command\" value=\"RESIGN\""));

            let pass = visual_turn_actions(
                &game,
                &TurnDraft {
                    mode: TurnMode::ConfirmPass,
                    ..TurnDraft::default()
                },
            )
            .display_to_string(false, false)
            .expect("confirmation renders");
            assert!(pass.contains("name=\"command\" value=\"PASS\""));
            assert!(pass.contains("Confirm pass"));
            assert!(pass.contains("value=\"CANCEL_MODE\""));

            let exchange = visual_turn_actions(
                &game,
                &TurnDraft {
                    exchange_tiles: game.rack_order[..2].to_vec(),
                    mode: TurnMode::ConfirmExchange,
                    ..TurnDraft::default()
                },
            )
            .display_to_string(false, false)
            .expect("exchange confirmation renders");
            assert!(exchange.contains("name=\"command\" value=\"EXCHANGE\""));
            assert!(exchange.contains("Confirm exchange"));
            assert!(!exchange.contains("blank_0"));
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
                game_turn_route(&dispatcher, &*database, &request, &game_id.to_string(), now).await;
            let response = response
                .primary
                .expect("accepted turn returns the updated game")
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
            let database: Arc<dyn Database> = Arc::from(database);
            let dispatcher = crate::GameSharedStateDispatcher::new(database.clone());
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
            let alice_dashboard = dashboard_action_route(
                &*database,
                &dispatcher,
                "http://localhost:8343",
                &challenge,
                now,
            )
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
            let accepted = dashboard_action_route(
                &*database,
                &dispatcher,
                "http://localhost:8343",
                &accept,
                now,
            )
            .await
            .display_to_string(false, false)
            .expect("dashboard renders");
            assert!(accepted.contains("class=\"game-summary\""));
            assert!(
                accepted.contains("Your turn") || accepted.contains("Waiting for opponent"),
                "{accepted}"
            );
        });
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn definition_route_authorizes_participants_and_only_canonical_played_words() {
        block_on(async {
            let database = test_database().await;
            let now = OffsetDateTime::UNIX_EPOCH;
            let alice = register(&*database, "alice", "correct horse battery staple", now)
                .await
                .expect("Alice registers");
            let bob = register(&*database, "bob", "another correct horse battery", now)
                .await
                .expect("Bob registers");
            let mallory = register(&*database, "mallory", "third correct horse battery", now)
                .await
                .expect("Mallory registers");
            let challenge = create_challenge(&*database, &alice, &bob, now)
                .await
                .expect("challenge creates");
            let game_id = accept_challenge(&*database, &challenge, &bob, now, 5)
                .await
                .expect("game starts");
            let state = crate::recover_game(&*database, game_id)
                .await
                .expect("game loads");
            let alice_player = crate::player_for_user(&*database, game_id, &alice)
                .await
                .expect("Alice is seated");
            let (active_user, rack) = if state.active_player == alice_player {
                (&alice, &state.racks[&alice_player])
            } else {
                (&bob, &state.racks[&state.active_player])
            };
            let (first, second, word) = rack
                .iter()
                .enumerate()
                .find_map(|(first_index, first)| {
                    rack.iter().enumerate().find_map(|(second_index, second)| {
                        if first_index == second_index {
                            return None;
                        }
                        let wwmtf_game_domain::TileFace::Letter(first_letter) = first.face else {
                            return None;
                        };
                        let wwmtf_game_domain::TileFace::Letter(second_letter) = second.face else {
                            return None;
                        };
                        let word = format!("{first_letter}{second_letter}");
                        wwmtf_game_domain::bundled_dictionary()
                            .contains(&word)
                            .then_some((*first, *second, word))
                    })
                })
                .expect("rack contains a legal two-letter word");
            crate::submit_game_command(
                &*database,
                game_id,
                active_user,
                "definition-play-command",
                "definition-play-idempotency",
                state.revision,
                &GameCommand::Play {
                    placements: vec![
                        Placement {
                            tile_id: first.id,
                            coordinate: Coordinate::new(7, 7),
                            blank_letter: None,
                        },
                        Placement {
                            tile_id: second.id,
                            coordinate: Coordinate::new(8, 7),
                            blank_letter: None,
                        },
                    ],
                },
                1,
            )
            .await
            .expect("word is played");

            for user in [&alice, &bob] {
                let session = create_session(&*database, user, now, Duration::days(1))
                    .await
                    .expect("participant session creates");
                let mut request = RouteRequest::from_path(
                    &format!("/games/{game_id}/words/{word}"),
                    RequestInfo::default(),
                );
                request.cookies.insert(
                    SESSION_COOKIE_NAME.to_string(),
                    session.expose().to_string(),
                );
                let page =
                    game_word_route(&*database, None, &request, &game_id.to_string(), &word, now)
                        .await
                        .display_to_string(false, false)
                        .expect("participant definition renders");
                assert!(
                    page.contains("disabled by the server administrator"),
                    "{page}"
                );
            }

            let mallory_session = create_session(&*database, &mallory, now, Duration::days(1))
                .await
                .expect("Mallory session creates");
            let mut forbidden = RouteRequest::from_path(
                &format!("/games/{game_id}/words/{word}"),
                RequestInfo::default(),
            );
            forbidden.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                mallory_session.expose().to_string(),
            );
            let forbidden = game_word_route(
                &*database,
                None,
                &forbidden,
                &game_id.to_string(),
                &word,
                now,
            )
            .await
            .display_to_string(false, false)
            .expect("forbidden result renders");
            assert!(forbidden.contains("not authorized"));

            let signed_out = game_word_route(
                &*database,
                None,
                &RouteRequest::from_path(
                    &format!("/games/{game_id}/words/{word}"),
                    RequestInfo::default(),
                ),
                &game_id.to_string(),
                &word,
                now,
            )
            .await
            .display_to_string(false, false)
            .expect("signed-out result renders");
            assert!(signed_out.contains("Sign in"));

            let active_session = create_session(&*database, active_user, now, Duration::days(1))
                .await
                .expect("active session creates");
            let mut active_request = RouteRequest::from_path(
                &format!("/games/{game_id}/words/WORD"),
                RequestInfo::default(),
            );
            active_request.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                active_session.expose().to_string(),
            );
            let unplayed = game_word_route(
                &*database,
                None,
                &active_request,
                &game_id.to_string(),
                "WORD",
                now,
            )
            .await
            .display_to_string(false, false)
            .expect("unplayed result renders");
            assert!(unplayed.contains("does not occur"));
            let malformed = game_word_route(
                &*database,
                None,
                &active_request,
                &game_id.to_string(),
                "not%20a%20word",
                now,
            )
            .await
            .display_to_string(false, false)
            .expect("malformed result renders");
            assert!(
                malformed.contains("played word is invalid")
                    || malformed.contains("word is invalid")
            );

            let second_challenge = create_challenge(&*database, &alice, &mallory, now)
                .await
                .expect("second challenge creates");
            let second_game = accept_challenge(&*database, &second_challenge, &mallory, now, 6)
                .await
                .expect("second game starts");
            let cross_game = game_word_route(
                &*database,
                None,
                &active_request,
                &second_game.to_string(),
                &word,
                now,
            )
            .await
            .display_to_string(false, false)
            .expect("cross-game result renders");
            assert!(cross_game.contains("does not occur"));
        });
    }

    #[test]
    fn definition_route_requires_a_played_word_and_handles_disabled_provider() {
        block_on(async {
            let database = test_database().await;
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
            let mut request = RouteRequest::from_path(
                &format!("/games/{game_id}/words/WORD"),
                RequestInfo::default(),
            );
            request.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                session.expose().to_string(),
            );

            let unplayed = game_word_route(
                &*database,
                None,
                &request,
                &game_id.to_string(),
                "WORD",
                now,
            )
            .await
            .display_to_string(false, false)
            .expect("unplayed result renders");
            assert!(unplayed.contains("does not occur"));

            let mut game =
                load_authorized_game_page(&*database, &request.cookies, &game_id.to_string(), now)
                    .await
                    .expect("game loads");
            game.history.push(crate::MoveHistoryView {
                revision: 2,
                kind: "TILES_PLAYED".to_string(),
                description: "alice played WORD.".to_string(),
                score_summary: "alice 4 – bob 0".to_string(),
                played_words: vec![crate::PlayedWordView {
                    text: "WORD".to_string(),
                    score: 4,
                }],
            });
            assert!(game.has_played_word("WORD"));
            let unavailable_lookup = crate::DefinitionLookup::Unavailable(
                crate::DefinitionUnavailableReason::ProviderUnavailable,
            );
            let unavailable = definition_page(game_id, "WORD", &unavailable_lookup)
                .display_to_string(false, false)
                .expect("unavailable result renders");
            assert!(unavailable.contains("temporarily unavailable"));
            assert!(unavailable.contains(&format!("/games/{game_id}")));

            let panel_lookup = crate::DefinitionLookup::Unavailable(
                crate::DefinitionUnavailableReason::RateLimited,
            );
            let panel = definition_panel("WORD", &panel_lookup)
                .display_to_string(false, false)
                .expect("panel renders");
            assert!(panel.contains("id=\"game-definition-layer\""));
            assert!(panel.contains("temporarily rate limited"));
            assert!(!panel.contains("id=\"app-page\""));
            assert!(panel.contains("fx-click"));
            assert!(!panel.contains("Open definition page"));

            let panel_error = definition_panel_error("You are not authorized for this game.")
                .display_to_string(false, false)
                .expect("panel error renders");
            assert!(panel_error.contains("game-definition-layer"));
            assert!(panel_error.contains("not authorized"));
        });
    }

    #[test]
    fn health_routes_report_process_and_database_readiness() {
        block_on(async {
            let database = test_database().await;
            let dispatcher = Arc::new(crate::GameSharedStateDispatcher::new(database.clone()));
            let router = create_product_router(
                database,
                dispatcher,
                None,
                None,
                false,
                "csrf-test".to_string(),
                "https://games.example.test".to_string(),
                true,
            );

            let live = router
                .navigate(("/health/live", RequestInfo::default()))
                .await
                .expect("liveness route resolves")
                .expect("liveness returns content");
            let ready = router
                .navigate(("/health/ready", RequestInfo::default()))
                .await
                .expect("readiness route resolves")
                .expect("readiness returns content");

            assert!(matches!(live, Content::Raw { .. }));
            assert!(matches!(ready, Content::Raw { .. }));
            #[cfg(not(feature = "metrics"))]
            assert!(
                router
                    .navigate(("/metrics", RequestInfo::default()))
                    .await
                    .is_err(),
                "metrics route must be absent unless explicitly enabled"
            );
        });
    }

    #[cfg(feature = "metrics")]
    #[test]
    fn metrics_route_reports_secret_safe_counters() {
        block_on(async {
            let database = test_database().await;
            let dispatcher = Arc::new(crate::GameSharedStateDispatcher::new(database.clone()));
            let router = create_product_router(
                database,
                dispatcher,
                None,
                None,
                false,
                "csrf-test".to_string(),
                "https://games.example.test".to_string(),
                true,
            );
            let metrics = router
                .navigate(("/metrics", RequestInfo::default()))
                .await
                .expect("metrics route resolves")
                .expect("metrics returns content");

            let Content::Raw { data, .. } = metrics else {
                panic!("metrics should be raw content");
            };
            let metrics = std::str::from_utf8(&data).expect("metrics should be UTF-8");
            assert!(metrics.contains("wwmtf_authentication_failures_total"));
            assert!(metrics.contains("wwmtf_live_subscribers"));
        });
    }

    #[test]
    fn username_only_login_is_development_only() {
        block_on(async {
            let database = test_database().await;
            let mut login = RouteRequest::from_path("/login", RequestInfo::default());
            login.method = "POST".parse().expect("POST parses");
            login.headers.insert(
                "content-type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            );
            login.body = Some(std::sync::Arc::new("username=local-player".into()));

            let production = login_route(
                &*database,
                &login,
                OffsetDateTime::UNIX_EPOCH,
                "csrf-test",
                true,
                false,
                true,
            )
            .await;
            assert!(production.response.cookies.is_empty());
            let rendered = production
                .primary
                .expect("response has primary content")
                .display_to_string(false, false)
                .expect("login response renders");
            assert!(rendered.contains("only in development mode"));

            let development = login_route(
                &*database,
                &login,
                OffsetDateTime::UNIX_EPOCH,
                "csrf-test",
                false,
                true,
                false,
            )
            .await;
            assert_eq!(
                development.response.cookies.len(),
                2,
                "{}",
                development
                    .primary
                    .as_ref()
                    .expect("response has primary content")
                    .display_to_string(false, false)
                    .expect("development login renders")
            );
            assert!(
                development
                    .response
                    .cookies
                    .iter()
                    .all(|cookie| !cookie.secure)
            );
            let user = crate::find_or_create_development_user(
                &*database,
                "local-player",
                OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .expect("development user resolves");
            assert_eq!(
                crate::resolve_session(
                    &*database,
                    &development.response.cookies[0].value,
                    OffsetDateTime::UNIX_EPOCH,
                )
                .await
                .expect("development session resolves"),
                user
            );
        });
    }

    #[test]
    fn account_session_effects_are_secure_and_expirable() {
        let signed_in = authenticated_session_response("opaque-test-session", "csrf-test", true);
        assert_eq!(signed_in.navigation, None);
        assert_eq!(signed_in.cookies.len(), 2);
        assert!(signed_in.cookies[0].secure);
        assert!(signed_in.cookies[0].http_only);
        assert_eq!(
            signed_in.cookies[0].same_site,
            hyperchad::renderer::SameSite::Lax
        );
        assert!(!signed_in.cookies[1].http_only);
        assert!(signed_in.cookies.iter().all(|cookie| cookie.secure));

        let development = authenticated_session_response("opaque-test-session", "csrf-test", false);
        assert!(development.cookies.iter().all(|cookie| !cookie.secure));
        assert!(development.cookies[0].http_only);
        assert!(!development.cookies[1].http_only);

        let signed_out = logged_out_response(true);
        assert_eq!(
            signed_out
                .navigation
                .as_ref()
                .map(hyperchad::renderer::ResponseNavigation::location),
            Some("/login")
        );
        assert!(
            signed_out
                .cookies
                .iter()
                .all(|cookie| cookie.max_age_seconds == Some(0))
        );
        assert!(signed_out.cookies.iter().all(|cookie| cookie.secure));

        let development_signed_out = logged_out_response(false);
        assert!(
            development_signed_out
                .cookies
                .iter()
                .all(|cookie| cookie.max_age_seconds == Some(0) && !cookie.secure)
        );
    }

    #[test]
    fn account_pages_expose_renderer_neutral_forms() {
        let login = login_page(None)
            .display_to_string(false, false)
            .expect("login page renders");
        assert!(login.contains("href=\"/auth/google/start\""));
        assert!(login.contains("target=\"_top\""));
        assert!(login.contains("Continue with Google"));
        assert!(login.contains("href=\"/account/migrate\""));
        assert!(!login.contains("script"));

        let logout = logout_page()
            .display_to_string(false, false)
            .expect("logout page renders");
        assert!(logout.contains("hx-post=\"/logout\""));
        assert!(!logout.contains("script"));
        let migration = migration_page(None)
            .display_to_string(false, false)
            .expect("migration page renders");
        assert!(migration.contains("method=\"post\""));
        assert!(!migration.contains("action="));
        assert!(!migration.contains("hx-post"));
        assert!(!migration.contains("script"));
    }

    #[test]
    fn account_pages_do_not_serialize_authentication_secrets() {
        let secrets = [
            "secret-subject",
            "https://lh3.googleusercontent.com/secret-picture",
            "secret-state",
            "secret-nonce",
            "secret-pkce-verifier",
            "secret-code",
            "secret-token",
            "secret-password",
            "secret-session",
            "secret-invitation",
        ];
        for page in [
            login_page(Some("Google sign-in could not be verified.")),
            migration_page(Some("Existing credentials are incorrect.")),
            signed_out_page(),
            logout_page(),
        ] {
            let rendered = page
                .display_to_string(false, false)
                .expect("account page renders");
            for secret in secrets {
                assert!(!rendered.contains(secret));
            }
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
