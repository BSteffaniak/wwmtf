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
    template::container,
};
use serde::Deserialize;
use switchy_database::Database;
use time::{Duration, OffsetDateTime};
use wwmtf_game_domain::{Coordinate, GameCommand, GameError, Placement, PlacementGuidance, TileId};

use crate::{
    AuthorizedGamePage, FirstPlayerPolicy, GameCreationPolicy, GameVisibilitySettings,
    GoogleOidcClient, LobbySettings, OidcAttemptPurpose, PresentationError, ProductWorkflowError,
    accept_pending_challenge, cancel_lobby, cancel_pending_challenge, claim_oidc_attempt,
    cleanup_oidc_attempts, clear_move_plan, complete_legacy_google_migration, consume_oidc_attempt,
    create_lobby, create_oidc_attempt, decline_pending_challenge, error_component,
    find_or_create_development_user, google_login_and_create_session, join_lobby, join_lobby_by_id,
    leave_lobby, load_authenticated_dashboard, load_authorized_game_page, load_lobby,
    load_move_plan, logout_session,
    pages::{
        auth::{
            invitation_joined_page, invitation_page, login_page, login_page_with_invitation,
            logout_page, migration_page, product_error_page, signed_out_page, signed_out_page_for,
        },
        dashboard::dashboard_page,
        game::{
            definition_page, definition_panel, definition_panel_error, draft_error_page, game_page,
            turn_feedback, visual_game_page,
        },
        lobby::{lobby_creation_page, lobby_join_page, lobby_page},
    },
    redeem_shareable_invitation, redeem_shareable_invitation_by_id, revoke_shareable_invitation,
    save_move_plan, start_lobby, theme, update_lobby_settings,
};

#[derive(Debug, Deserialize)]
struct LobbyActionForm {
    action: String,
    #[serde(default)]
    invitation_token: String,
    #[serde(default)]
    username: String,
    #[serde(default = "default_lobby_max_players")]
    max_players: usize,
    #[serde(default = "default_lobby_board_size")]
    board_size: u8,
    #[serde(default = "default_lobby_tile_sets")]
    tile_set_count: u8,
    #[serde(default = "default_true")]
    show_remaining_tile_count: bool,
    #[serde(default = "default_true")]
    show_remaining_tile_faces: bool,
    #[serde(default)]
    first_player_policy: String,
    #[serde(default)]
    chosen_first_user_id: String,
}

const fn default_lobby_max_players() -> usize {
    4
}
const fn default_lobby_board_size() -> u8 {
    15
}
const fn default_lobby_tile_sets() -> u8 {
    1
}
const fn default_true() -> bool {
    true
}

fn lobby_settings(form: &LobbyActionForm, creator_user_id: &str) -> LobbySettings {
    let first_player = match form.first_player_policy.as_str() {
        "RANDOM" => FirstPlayerPolicy::Random,
        "CHOSEN" => FirstPlayerPolicy::Chosen(if form.chosen_first_user_id.is_empty() {
            creator_user_id.to_string()
        } else {
            form.chosen_first_user_id.clone()
        }),
        _ => FirstPlayerPolicy::Creator,
    };
    LobbySettings {
        max_players: form.max_players,
        board_size: form.board_size,
        tile_set_count: form.tile_set_count,
        first_player,
        visibility: GameVisibilitySettings {
            show_remaining_tile_count: form.show_remaining_tile_count,
            show_remaining_tile_faces: form.show_remaining_tile_faces,
        },
    }
}

#[derive(Debug, Deserialize)]
struct DashboardActionForm {
    action: String,
    #[serde(default)]
    challenge_id: String,
    #[serde(default)]
    invitation_id: String,
    #[serde(default)]
    invitation_token: String,
    #[serde(default)]
    display_name: String,
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
            |dashboard| dashboard_page(&dashboard, now),
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
    _public_base_url: &str,
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
    let result: Result<(), ProductWorkflowError> = match form.action.as_str() {
        "ACCEPT_CHALLENGE" => accept_pending_challenge(database, &form.challenge_id, &user_id, now)
            .await
            .map(|_| ()),
        "DECLINE_CHALLENGE" => {
            decline_pending_challenge(database, &form.challenge_id, &user_id, now).await
        }
        "CANCEL_CHALLENGE" => {
            cancel_pending_challenge(database, &form.challenge_id, &user_id, now).await
        }
        "REDEEM_INVITATION" => {
            redeem_shareable_invitation(database, &form.invitation_token, &user_id, now)
                .await
                .map(|_| ())
        }
        "REVOKE_INVITATION" => {
            revoke_shareable_invitation(database, &form.invitation_id, &user_id, now).await
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
    if let Err(error) = result {
        return error_component(product_error_message(&error));
    }
    let dashboard = load_authenticated_dashboard(database, &request.cookies, now).await;
    if dispatcher
        .refresh_dashboard_subscribers(now.unix_timestamp_nanos().try_into().unwrap_or(i64::MAX))
        .await
        .is_err()
    {
        #[cfg(feature = "metrics")]
        crate::observability::record_database_failure("refresh_dashboard_subscribers");
    }
    match dashboard {
        Ok(dashboard) => dashboard_page(&dashboard, now),
        Err(PresentationError::Unauthenticated) => {
            error_component("Your session expired. Sign in and review your dashboard.")
        }
        Err(_) => {
            error_component("The action succeeded, but the dashboard could not be refreshed.")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize, Default)]
pub struct TurnDraft {
    pub selected_tile: Option<u16>,
    pub selected_blank_letter: Option<char>,
    pub placements: Vec<DraftPlacement>,
    #[serde(default)]
    pub rack_tile: Option<u16>,
    #[serde(default)]
    pub exchange_tiles: Vec<u16>,
    #[serde(default)]
    pub mode: TurnMode,
    #[serde(default)]
    pub board_zoom: BoardZoom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum BoardZoom {
    Fit,
    Compact,
    #[default]
    Normal,
    Large,
}

impl BoardZoom {
    /// Board square size as a percentage of the size that fits the visible play area.
    ///
    /// `Normal` (the default) fits the whole board; zooming in enlarges squares for touch,
    /// zooming out leaves breathing room around the board.
    pub const fn fit_percent(self) -> u32 {
        match self {
            Self::Fit => 78,
            Self::Compact => 89,
            Self::Normal => 100,
            Self::Large => 200,
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
pub enum TurnMode {
    #[default]
    Play,
    Exchange,
    ConfirmExchange,
    ConfirmPass,
    ConfirmResign,
}

impl TurnDraft {
    pub fn has_composed_turn_input(&self) -> bool {
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

pub fn rack_action(draft: &TurnDraft) -> &'static str {
    if draft.mode == TurnMode::Exchange {
        "TOGGLE_EXCHANGE"
    } else if draft.rack_tile.is_some() {
        "SWAP_RACK_TILES"
    } else {
        "PICK_RACK_TILE"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftFeedback {
    pub candidate: Option<wwmtf_game_domain::CandidatePlayAnalysis>,
    pub guidance: PlacementGuidance,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct DraftPlacement {
    pub tile_id: u16,
    pub x: u8,
    pub y: u8,
    pub blank_letter: Option<char>,
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
            |dashboard| dashboard_page(&dashboard, now),
        )
}

pub fn draft_token(draft: &TurnDraft) -> String {
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

pub fn draft_feedback(game: &AuthorizedGamePage, draft: &TurnDraft) -> DraftFeedback {
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
            "Pick a tile, then tap a highlighted square."
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
    let action = form.action.as_str();
    draft.begin_action(action);
    if game.completed && !is_zoom_action(action) {
        return visual_game_page(&game, &draft, Some("This game is complete."));
    }
    let viewer_turn = game.view.active_player == game.viewer_player;
    if !viewer_turn
        && matches!(
            action,
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
    match action {
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

fn view_with_internal_navigation(path: String, user_id: &str) -> View {
    // Keep the subscription alive through the navigation response; a channel-less intermediate
    // page otherwise races an unsubscribe against the destination's subscribe.
    let channel = format!("dashboard:{user_id}");
    let navigate = ActionType::Navigate { url: path.clone() };
    View::builder()
        .with_primary(container! {
            div id="app-page" font-family=(theme::FONT_UI) data-shared-state-channel=(channel) fx-immediate=(navigate)
                min-height="100vh" align-items="center" justify-content="center" gap=(theme::SPACE_3)
                background=(theme::MENU_BG) color=(theme::INK_MUTED) {
                (crate::ui::tile_face("…", None, 56))
                span font-weight=bold { "Loading…" }
            }
        })
        .with_response(ResponseMetadata {
            cookies: Vec::new(),
            navigation: Some(
                hyperchad::renderer::ResponseNavigation::internal(path)
                    .expect("application lobby paths are valid internal navigation"),
            ),
        })
        .build()
}

async fn refresh_lobby_dashboards(
    dispatcher: &crate::GameSharedStateDispatcher,
    now: OffsetDateTime,
    actor: Option<&str>,
) {
    // The actor receives the new page in the HTTP response. A simultaneous event for their old
    // page can race that navigation (for example, accepting an invite from the dashboard).
    if dispatcher
        .refresh_dashboard_subscribers_except(
            now.unix_timestamp_nanos().try_into().unwrap_or(i64::MAX),
            actor,
        )
        .await
        .is_err()
    {
        #[cfg(feature = "metrics")]
        crate::observability::record_database_failure("refresh_lobby_dashboards");
    }
}

async fn lobby_create_route(
    database: &dyn Database,
    dispatcher: &crate::GameSharedStateDispatcher,
    request: &RouteRequest,
    policy: GameCreationPolicy,
    _public_base_url: &str,
    now: OffsetDateTime,
) -> View {
    let Ok(user_id) = crate::authenticated_user(database, &request.cookies, now).await else {
        return View::from(signed_out_page_for(
            "NEW GAME",
            "Sign in to create a game and invite your friends.",
            None,
        ));
    };
    if request.method.as_ref() != "POST" {
        return View::from(lobby_creation_page(&user_id, policy));
    }
    let form = request
        .parse_form::<LobbyActionForm>()
        .unwrap_or_else(|_| LobbyActionForm {
            action: "CREATE".to_string(),
            username: String::new(),
            invitation_token: String::new(),
            max_players: 4,
            board_size: 15,
            tile_set_count: 1,
            show_remaining_tile_count: true,
            show_remaining_tile_faces: true,
            first_player_policy: "CREATOR".to_string(),
            chosen_first_user_id: String::new(),
        });
    match create_lobby(
        database,
        &user_id,
        lobby_settings(&form, &user_id),
        policy,
        now,
        Duration::days(30),
    )
    .await
    {
        Ok((lobby_id, _token)) => match load_lobby(database, &lobby_id, &user_id).await {
            Ok(_lobby) => {
                refresh_lobby_dashboards(dispatcher, now, Some(&user_id)).await;
                view_with_internal_navigation(format!("/lobbies/{lobby_id}"), &user_id)
            }
            Err(error) => View::from(product_error_page("Lobby unavailable", &error.to_string())),
        },
        Err(error) => View::from(product_error_page(
            "Lobby could not be created",
            &error.to_string(),
        )),
    }
}

async fn lobby_join_route(
    database: &dyn Database,
    dispatcher: &crate::GameSharedStateDispatcher,
    request: &RouteRequest,
    policy: GameCreationPolicy,
    _public_base_url: &str,
    now: OffsetDateTime,
) -> View {
    let lobby_id = request
        .path
        .strip_prefix("/lobbies/join/")
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let legacy_token = request.query.get("invite").cloned().or_else(|| {
        request
            .parse_form::<LobbyActionForm>()
            .ok()
            .map(|form| form.invitation_token)
    });
    let Ok(user_id) = crate::authenticated_user(database, &request.cookies, now).await else {
        return View::from(signed_out_page_for(
            "YOU'RE INVITED",
            "A friend saved you a seat at the board. Sign in to claim it.",
            Some("After signing in, open your invite link again to join the game."),
        ));
    };
    if request.method.as_ref() != "POST" {
        let action = lobby_id.as_ref().map_or_else(
            || "/lobbies/join".to_string(),
            |lobby_id| format!("/lobbies/join/{lobby_id}"),
        );
        let preview = match lobby_id.as_deref() {
            Some(lobby_id) => crate::preview_lobby_invite(database, lobby_id, &user_id, now)
                .await
                .ok(),
            None => None,
        };
        if let (Some(lobby_id), Some(preview)) = (lobby_id.as_deref(), preview.as_ref())
            && preview.already_joined
        {
            return view_with_internal_navigation(format!("/lobbies/{lobby_id}"), &user_id);
        }
        return View::from(lobby_join_page(
            &user_id,
            &action,
            legacy_token.as_deref(),
            preview.as_ref(),
        ));
    }
    let result = if let Some(lobby_id) = lobby_id {
        join_lobby_by_id(database, &lobby_id, &user_id, policy, now).await
    } else if let Some(token) = legacy_token {
        join_lobby(database, &token, &user_id, policy, now).await
    } else {
        Err(crate::LobbyError::Unavailable)
    };
    match result {
        Ok(lobby_id) => match load_lobby(database, &lobby_id, &user_id).await {
            Ok(_lobby) => {
                refresh_lobby_dashboards(dispatcher, now, Some(&user_id)).await;
                view_with_internal_navigation(format!("/lobbies/{lobby_id}"), &user_id)
            }
            Err(error) => View::from(product_error_page("Lobby unavailable", &error.to_string())),
        },
        Err(error) => View::from(product_error_page("Lobby unavailable", &error.to_string())),
    }
}

async fn lobby_route(
    database: &dyn Database,
    request: &RouteRequest,
    policy: GameCreationPolicy,
    public_base_url: &str,
    now: OffsetDateTime,
) -> Container {
    let lobby_id = request
        .path
        .strip_prefix("/lobbies/")
        .unwrap_or_default()
        .trim_end_matches("/action");
    let Ok(user_id) = crate::authenticated_user(database, &request.cookies, now).await else {
        return signed_out_page_for("LOBBY", "Sign in to see this lobby.", None);
    };
    match load_lobby(database, lobby_id, &user_id).await {
        Ok(lobby) => lobby_page(&lobby, &user_id, public_base_url, policy),
        Err(error) => product_error_page("Lobby unavailable", &error.to_string()),
    }
}

async fn lobby_action_route(
    database: &dyn Database,
    dispatcher: &crate::GameSharedStateDispatcher,
    request: &RouteRequest,
    policy: GameCreationPolicy,
    _public_base_url: &str,
    now: OffsetDateTime,
) -> View {
    let lobby_id = request
        .path
        .strip_prefix("/lobbies/")
        .unwrap_or_default()
        .trim_end_matches("/action");
    let Ok(user_id) = crate::authenticated_user(database, &request.cookies, now).await else {
        return View::from(signed_out_page_for(
            "LOBBY",
            "Sign in to update this lobby.",
            None,
        ));
    };
    let Ok(form) = request.parse_form::<LobbyActionForm>() else {
        return View::from(product_error_page(
            "Lobby unavailable",
            "The lobby action was invalid.",
        ));
    };
    let action = form.action.as_str();
    let result = match action {
        "INVITE" => {
            crate::lobbies::invite_lobby_username(database, lobby_id, &user_id, &form.username, now)
                .await
                .map(|()| None)
        }
        "ACCEPT_INVITE" | "DECLINE_INVITE" => crate::lobbies::respond_lobby_invitation(
            database,
            lobby_id,
            &user_id,
            action == "ACCEPT_INVITE",
            policy,
            now,
        )
        .await
        .map(|()| None),
        "UPDATE" => update_lobby_settings(
            database,
            lobby_id,
            &user_id,
            lobby_settings(&form, &user_id),
            policy,
            now,
        )
        .await
        .map(|()| None),
        "START" => start_lobby(
            database,
            lobby_id,
            &user_id,
            policy,
            now,
            u64::try_from(now.unix_timestamp_nanos()).unwrap_or_default(),
        )
        .await
        .map(Some),
        "CANCEL" => cancel_lobby(database, lobby_id, &user_id, now)
            .await
            .map(|()| None),
        "LEAVE" => leave_lobby(database, lobby_id, &user_id, now)
            .await
            .map(|()| None),
        _ => {
            return View::from(product_error_page(
                "Lobby unavailable",
                "The lobby action is unknown.",
            ));
        }
    };
    match result {
        Ok(Some(game_id)) => {
            refresh_lobby_dashboards(dispatcher, now, Some(&user_id)).await;
            view_with_internal_navigation(format!("/games/{game_id}"), &user_id)
        }
        Ok(None) if matches!(action, "CANCEL" | "LEAVE" | "DECLINE_INVITE") => {
            refresh_lobby_dashboards(dispatcher, now, Some(&user_id)).await;
            view_with_internal_navigation("/".to_string(), &user_id)
        }
        Ok(None) => match load_lobby(database, lobby_id, &user_id).await {
            Ok(_lobby) => {
                refresh_lobby_dashboards(dispatcher, now, Some(&user_id)).await;
                view_with_internal_navigation(format!("/lobbies/{lobby_id}"), &user_id)
            }
            Err(error) => View::from(product_error_page("Lobby unavailable", &error.to_string())),
        },
        Err(error) => View::from(product_error_page(
            "Lobby action failed",
            &error.to_string(),
        )),
    }
}

// ---- Rematch route ---------------------------------------------------------------------------

/// Creates a rematch lobby from a completed game and navigates its requester there.
///
/// Only `POST` creates a lobby; any other method returns the viewer to the game page.
async fn game_rematch_route(
    dispatcher: &crate::GameSharedStateDispatcher,
    database: &dyn Database,
    request: &RouteRequest,
    game_id: &str,
    policy: GameCreationPolicy,
    now: OffsetDateTime,
) -> View {
    let Ok(user_id) = crate::authenticated_user(database, &request.cookies, now).await else {
        return View::from(signed_out_page_for(
            "REMATCH",
            "Sign in to challenge your opponents to another round.",
            None,
        ));
    };
    let Ok(parsed_game_id) = game_id.parse::<wwmtf_game_domain::GameId>() else {
        return View::from(product_error_page(
            "Rematch unavailable",
            &crate::LobbyError::NotGameParticipant.to_string(),
        ));
    };
    if request.method.as_ref() != "POST" {
        return view_with_internal_navigation(format!("/games/{parsed_game_id}"), &user_id);
    }
    match crate::lobbies::create_rematch_lobby(
        database,
        parsed_game_id,
        &user_id,
        policy,
        now,
        Duration::days(30),
    )
    .await
    {
        Ok(lobby_id) => {
            refresh_lobby_dashboards(dispatcher, now, Some(&user_id)).await;
            view_with_internal_navigation(format!("/lobbies/{lobby_id}"), &user_id)
        }
        Err(error) => View::from(product_error_page(
            "Rematch unavailable",
            &error.to_string(),
        )),
    }
}

// ---- End rematch route -----------------------------------------------------------------------

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
    game_creation_policy: crate::GameCreationPolicy,
    csrf_token: String,
    public_base_url: String,
    secure_cookies: bool,
) -> Router {
    let router = Router::new();
    let lobby_policy = game_creation_policy;
    let lobby_public_base_url = Arc::new(public_base_url.clone());
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
    let lobby_new_database = database.clone();
    let lobby_new_dispatcher = dispatcher.clone();
    let lobby_new_public_base_url = lobby_public_base_url.clone();
    router.add_route_result("/lobbies/new", move |request: RouteRequest| {
        let database = lobby_new_database.clone();
        let dispatcher = lobby_new_dispatcher.clone();
        let public_base_url = lobby_new_public_base_url.clone();
        async move {
            Ok(lobby_create_route(
                &*database,
                &dispatcher,
                &request,
                lobby_policy,
                &public_base_url,
                OffsetDateTime::now_utc(),
            )
            .await) as Result<View, Box<dyn std::error::Error>>
        }
    });
    let lobby_create_database = database.clone();
    let lobby_create_dispatcher = dispatcher.clone();
    let lobby_create_public_base_url = lobby_public_base_url.clone();
    router.add_route_result("/lobbies/create", move |request: RouteRequest| {
        let database = lobby_create_database.clone();
        let dispatcher = lobby_create_dispatcher.clone();
        let public_base_url = lobby_create_public_base_url.clone();
        async move {
            Ok(lobby_create_route(
                &*database,
                &dispatcher,
                &request,
                lobby_policy,
                &public_base_url,
                OffsetDateTime::now_utc(),
            )
            .await) as Result<View, Box<dyn std::error::Error>>
        }
    });
    let join_database = database.clone();
    let join_dispatcher = dispatcher.clone();
    let join_public_base_url = lobby_public_base_url.clone();
    router.add_route_result(
        RoutePath::LiteralPrefix("/lobbies/join".to_string()),
        move |request: RouteRequest| {
            let database = join_database.clone();
            let dispatcher = join_dispatcher.clone();
            let public_base_url = join_public_base_url.clone();
            async move {
                Ok(lobby_join_route(
                    &*database,
                    &dispatcher,
                    &request,
                    lobby_policy,
                    &public_base_url,
                    OffsetDateTime::now_utc(),
                )
                .await) as Result<View, Box<dyn std::error::Error>>
            }
        },
    );
    let lobby_database = database.clone();
    let lobby_dispatcher = dispatcher.clone();
    let lobby_action_public_base_url = lobby_public_base_url;
    router.add_route_result(
        RoutePath::LiteralPrefix("/lobbies/".to_string()),
        move |request: RouteRequest| {
            let database = lobby_database.clone();
            let dispatcher = lobby_dispatcher.clone();
            let public_base_url = lobby_action_public_base_url.clone();
            async move {
                let page = if request.path.ends_with("/action") {
                    lobby_action_route(
                        &*database,
                        &dispatcher,
                        &request,
                        lobby_policy,
                        &public_base_url,
                        OffsetDateTime::now_utc(),
                    )
                    .await
                } else {
                    View::from(
                        lobby_route(
                            &*database,
                            &request,
                            lobby_policy,
                            &public_base_url,
                            OffsetDateTime::now_utc(),
                        )
                        .await,
                    )
                };
                Ok(page) as Result<View, Box<dyn std::error::Error>>
            }
        },
    );
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
    if !google_login {
        // Keep the migration link target recoverable when Google sign-in is not configured.
        router.add_route_result("/account/migrate", |_request: RouteRequest| async move {
            Ok(View::from(product_error_page(
                "Migration unavailable",
                "Moving an account to Google sign-in needs Google, which is not set up on this server.",
            ))) as Result<View, Box<dyn std::error::Error>>
        });
    }
    // Browsers request a favicon on every page; answer with an empty icon instead of a routing
    // error.
    router.add_route_result("/favicon.ico", |_request: RouteRequest| async move {
        Ok(Content::Raw {
            data: Vec::new().into(),
            content_type: "image/x-icon".to_string(),
        }) as Result<Content, Box<dyn std::error::Error>>
    });
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
                } else if let Some(game_id) = game_path.strip_suffix("/rematch") {
                    // Checked after the word routes so a played word spelled "rematch" keeps
                    // resolving to its definition.
                    Ok(game_rematch_route(
                        &dispatcher,
                        &*database,
                        &request,
                        game_id,
                        lobby_policy,
                        OffsetDateTime::now_utc(),
                    )
                    .await) as Result<View, Box<dyn std::error::Error>>
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

/// Loads and renders the signed-in dashboard, or a recoverable authentication page.
pub async fn dashboard_route(
    database: &dyn Database,
    request: &RouteRequest,
    now: OffsetDateTime,
) -> Container {
    match load_authenticated_dashboard(database, &request.cookies, now).await {
        Ok(dashboard) => dashboard_page(&dashboard, now),
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
    use hyperchad::{
        router::RequestInfo,
        shared_state_models::ParticipantId,
        shared_state_transport::{
            AuthenticatedTransportContext, SharedStateTransportDispatcher as _,
        },
    };
    use switchy_database::query::FilterableQuery as _;
    use time::Duration;
    use wwmtf_game_domain::Dictionary as _;

    use super::*;
    use crate::pages::{
        game::{blank_letter_picker_open, draft_feedback_component},
        lobby::lobby_invitation_url,
    };
    use crate::{
        GameSharedStateDispatcher, SESSION_COOKIE_NAME, VerifiedExternalIdentity, accept_challenge,
        create_challenge, create_session, migrate_app, register,
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
    fn lobby_join_and_start_publish_live_refreshes_to_waiting_members() {
        block_on(async {
            let database = test_database().await;
            let now = OffsetDateTime::UNIX_EPOCH;
            let creator = register(
                &*database,
                "live-creator",
                "correct horse battery staple",
                now,
            )
            .await
            .expect("creator registers");
            let joiner = register(
                &*database,
                "live-joiner",
                "another correct horse battery",
                now,
            )
            .await
            .expect("joiner registers");
            let creator_session = create_session(&*database, &creator, now, Duration::days(1))
                .await
                .expect("session creates");
            let joiner_session = create_session(&*database, &joiner, now, Duration::days(1))
                .await
                .expect("session creates");
            let policy = GameCreationPolicy::new(16, 64, 16).expect("policy");
            let dispatcher = GameSharedStateDispatcher::new(database.clone());
            let context = AuthenticatedTransportContext {
                participant_id: ParticipantId::new(&creator),
                identity_binding: "creator-browser".to_string(),
            };
            let events = dispatcher
                .subscribe_channel(&context, &crate::dashboard_channel(&creator))
                .await
                .expect("creator subscribes");
            events.recv_async().await.expect("initial dashboard");
            let (lobby_id, _) = create_lobby(
                &*database,
                &creator,
                LobbySettings {
                    max_players: 4,
                    board_size: 15,
                    tile_set_count: 1,
                    first_player: FirstPlayerPolicy::Creator,
                    visibility: GameVisibilitySettings::default(),
                },
                policy,
                now,
                Duration::days(1),
            )
            .await
            .expect("lobby creates");

            let mut join = RouteRequest::from_path(
                &format!("/lobbies/join/{lobby_id}"),
                RequestInfo::default(),
            );
            join.method = "POST".parse().expect("POST parses");
            join.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                joiner_session.expose().to_string(),
            );
            lobby_join_route(
                &*database,
                &dispatcher,
                &join,
                policy,
                "https://games.example.test",
                now,
            )
            .await;
            events.try_recv().expect("join refresh publishes");
            let joiner_events = dispatcher
                .subscribe_channel(
                    &AuthenticatedTransportContext {
                        participant_id: ParticipantId::new(&joiner),
                        identity_binding: "joiner-browser".to_string(),
                    },
                    &crate::dashboard_channel(&joiner),
                )
                .await
                .expect("joiner subscribes");
            joiner_events.try_recv().expect("initial joiner dashboard");

            let mut start = RouteRequest::from_path(
                &format!("/lobbies/{lobby_id}/action"),
                RequestInfo::default(),
            );
            start.method = "POST".parse().expect("POST parses");
            start.headers.insert(
                "content-type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            );
            start.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                creator_session.expose().to_string(),
            );
            start.body = Some(Arc::new("action=START".into()));
            let started = lobby_action_route(
                &*database,
                &dispatcher,
                &start,
                policy,
                "https://games.example.test",
                now,
            )
            .await;
            joiner_events
                .try_recv()
                .expect("start refresh publishes to waiting member");
            assert!(
                events.try_recv().is_err(),
                "actor navigation must not race an event refresh"
            );
            assert!(
                started
                    .response
                    .navigation
                    .as_ref()
                    .expect("game navigation")
                    .location()
                    .starts_with("/games/")
            );
        });
    }

    // ---- Rematch route tests -----------------------------------------------------------------

    #[test]
    #[allow(clippy::too_many_lines)]
    fn rematch_route_creates_invited_lobby_only_for_completed_participants() {
        block_on(async {
            let database = test_database().await;
            let now = OffsetDateTime::UNIX_EPOCH;
            let mut users = Vec::new();
            for name in ["route-rematch-a", "route-rematch-b", "route-rematch-x"] {
                let user = register(&*database, name, "correct horse battery staple", now)
                    .await
                    .expect("user registers");
                let session = create_session(&*database, &user, now, Duration::days(1))
                    .await
                    .expect("session creates");
                users.push((user, session));
            }
            let [
                (alice, alice_session),
                (bob, bob_session),
                (_, outsider_session),
            ] = users.as_slice()
            else {
                panic!("three users")
            };
            let policy = GameCreationPolicy::new(16, 64, 16).expect("policy");
            let dispatcher = GameSharedStateDispatcher::new(database.clone());
            let challenge = create_challenge(&*database, alice, bob, now)
                .await
                .expect("challenge");
            let game_id = accept_challenge(&*database, &challenge, bob, now, 11)
                .await
                .expect("game starts");
            let request = |session: &crate::SessionToken, method: &str| {
                let mut request = RouteRequest::from_path(
                    &format!("/games/{game_id}/rematch"),
                    RequestInfo::default(),
                );
                request.method = method.parse().expect("method parses");
                request.cookies.insert(
                    SESSION_COOKIE_NAME.to_string(),
                    session.expose().to_string(),
                );
                request
            };
            let id = game_id.to_string();
            let text = |view: &View| {
                view.primary
                    .as_ref()
                    .expect("page")
                    .display_to_string(false, false)
                    .expect("renders")
            };

            let active = game_rematch_route(
                &dispatcher,
                &*database,
                &request(alice_session, "POST"),
                &id,
                policy,
                now,
            )
            .await;
            assert!(active.response.navigation.is_none());
            assert!(text(&active).contains("A rematch is available after the game ends."));

            let state = crate::recover_game(&*database, game_id)
                .await
                .expect("recover");
            let active_user = if crate::player_for_user(&*database, game_id, alice)
                .await
                .expect("alice plays")
                == state.active_player
            {
                alice
            } else {
                bob
            };
            crate::submit_game_command(
                &*database,
                game_id,
                active_user,
                "route-rematch-resign",
                "route-rematch-resign",
                state.revision,
                &GameCommand::Resign,
                0,
            )
            .await
            .expect("resignation completes the game");

            let outsider = game_rematch_route(
                &dispatcher,
                &*database,
                &request(outsider_session, "POST"),
                &id,
                policy,
                now,
            )
            .await;
            assert!(outsider.response.navigation.is_none());
            assert!(text(&outsider).contains("Only players in this game can request a rematch."));

            let get = game_rematch_route(
                &dispatcher,
                &*database,
                &request(alice_session, "GET"),
                &id,
                policy,
                now,
            )
            .await;
            assert_eq!(
                get.response
                    .navigation
                    .as_ref()
                    .expect("GET returns to the game")
                    .location(),
                format!("/games/{game_id}")
            );
            assert!(
                crate::dashboard_projection(&*database, bob)
                    .await
                    .expect("dashboard")
                    .pending
                    .is_empty(),
                "GET must not create a lobby"
            );

            let bob_events = dispatcher
                .subscribe_channel(
                    &AuthenticatedTransportContext {
                        participant_id: ParticipantId::new(bob),
                        identity_binding: "bob-browser".to_string(),
                    },
                    &crate::dashboard_channel(bob),
                )
                .await
                .expect("bob subscribes");
            bob_events.recv_async().await.expect("initial dashboard");
            let created = game_rematch_route(
                &dispatcher,
                &*database,
                &request(alice_session, "POST"),
                &id,
                policy,
                now,
            )
            .await;
            let location = created
                .response
                .navigation
                .as_ref()
                .expect("lobby navigation")
                .location()
                .to_string();
            let lobby_id = location
                .strip_prefix("/lobbies/")
                .expect("navigates to the new lobby");
            bob_events
                .try_recv()
                .expect("invitee dashboard refreshes live");
            let pending = crate::dashboard_projection(&*database, bob)
                .await
                .expect("dashboard")
                .pending;
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].kind, "LOBBY_INVITATION");
            let lobby = load_lobby(&*database, lobby_id, alice)
                .await
                .expect("creator loads rematch lobby");
            assert_eq!(lobby.invited_usernames, ["route-rematch-b"]);
            assert_eq!(lobby.settings.max_players, 2);

            // The invitee's completed game offers to join the rematch instead of creating another.
            let bob_cookies = std::collections::BTreeMap::from([(
                SESSION_COOKIE_NAME.to_string(),
                bob_session.expose().to_string(),
            )]);
            let bob_game = load_authorized_game_page(&*database, &bob_cookies, &id, now)
                .await
                .expect("bob loads the completed game");
            let summary = crate::pages::game::completed_game_summary(&bob_game)
                .display_to_string(false, false)
                .expect("results render");
            assert!(
                summary.contains("route-rematch-a wants a rematch!"),
                "{summary}"
            );
            assert!(summary.contains(&format!("hx-post=\"/lobbies/{lobby_id}/action\"")));
            assert!(summary.contains("value=\"ACCEPT_INVITE\""));
            assert!(!summary.contains(&format!("hx-post=\"/games/{game_id}/rematch\"")));
        });
    }

    // ---- End rematch route tests -------------------------------------------------------------

    #[test]
    fn lobby_creation_uses_refreshable_get_and_canonical_navigation() {
        block_on(async {
            let database = test_database().await;
            let now = OffsetDateTime::UNIX_EPOCH;
            let creator = register(
                &*database,
                "refreshable-creator",
                "correct horse battery staple",
                now,
            )
            .await
            .expect("creator registers");
            let session = create_session(&*database, &creator, now, Duration::days(1))
                .await
                .expect("session creates");
            let policy = GameCreationPolicy::new(16, 64, 16).expect("policy");
            let dispatcher = GameSharedStateDispatcher::new(database.clone());
            let mut get = RouteRequest::from_path("/lobbies/new", RequestInfo::default());
            get.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                session.expose().to_string(),
            );
            let page = lobby_create_route(
                &*database,
                &dispatcher,
                &get,
                policy,
                "https://games.example.test",
                now,
            )
            .await;
            assert!(page.response.navigation.is_none());
            assert!(
                page.primary
                    .as_ref()
                    .expect("creation page")
                    .display_to_string(false, false)
                    .expect("renders")
                    .contains("Create game")
            );

            let mut post = get;
            post.path = "/lobbies/create".to_string();
            post.method = "POST".parse().expect("POST parses");
            post.headers.insert(
                "content-type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            );
            post.body = Some(Arc::new(
                "action=CREATE&max_players=4&board_size=15&tile_set_count=1&first_player_policy=CREATOR".into(),
            ));
            let created = lobby_create_route(
                &*database,
                &dispatcher,
                &post,
                policy,
                "https://games.example.test",
                now,
            )
            .await;
            let canonical = created
                .response
                .navigation
                .as_ref()
                .expect("canonical navigation")
                .location();
            assert!(canonical.starts_with("/lobbies/"));
            let rendered = created
                .primary
                .as_ref()
                .expect("navigation marker")
                .display_to_string(false, false)
                .expect("renders");
            assert!(
                rendered.contains("fx-immediate") || rendered.contains("v-onload"),
                "{rendered}"
            );
            assert!(rendered.contains(canonical));
            assert!(!rendered.contains("?invite="));
        });
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn creator_can_cancel_lobby_after_settings_update_and_share_link_remains_visible() {
        block_on(async {
            let database = test_database().await;
            let now = OffsetDateTime::UNIX_EPOCH;
            let creator = register(
                &*database,
                "lobby-creator",
                "correct horse battery staple",
                now,
            )
            .await
            .expect("creator registers");
            let session = create_session(&*database, &creator, now, Duration::days(1))
                .await
                .expect("session creates");
            let policy = GameCreationPolicy::new(16, 64, 16).expect("policy");
            let dispatcher = GameSharedStateDispatcher::new(database.clone());
            let (lobby_id, _token) = create_lobby(
                &*database,
                &creator,
                LobbySettings {
                    max_players: 4,
                    board_size: 15,
                    tile_set_count: 1,
                    first_player: FirstPlayerPolicy::Creator,
                    visibility: GameVisibilitySettings::default(),
                },
                policy,
                now,
                Duration::days(1),
            )
            .await
            .expect("lobby creates");
            let share_url = lobby_invitation_url("https://games.example.test", &lobby_id);
            let lobby = load_lobby(&*database, &lobby_id, &creator)
                .await
                .expect("lobby loads");
            let canonical_page = lobby_page(&lobby, &creator, "https://games.example.test", policy)
                .display_to_string(false, false)
                .expect("canonical lobby renders");
            assert!(canonical_page.contains(&share_url));
            let mut update = RouteRequest::from_path(
                &format!("/lobbies/{lobby_id}/action"),
                RequestInfo::default(),
            );
            update.method = "POST".parse().expect("POST parses");
            update.headers.insert(
                "content-type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            );
            update.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                session.expose().to_string(),
            );
            update.body = Some(Arc::new(
                "action=UPDATE&max_players=6&board_size=21&tile_set_count=2&first_player_policy=CREATOR".into(),
            ));
            let updated = lobby_action_route(
                &*database,
                &dispatcher,
                &update,
                policy,
                "https://games.example.test",
                now,
            )
            .await;
            let updated_html = updated
                .primary
                .as_ref()
                .expect("navigation marker")
                .display_to_string(false, false)
                .expect("navigation renders");
            let expected_lobby_path = format!("/lobbies/{lobby_id}");
            assert!(updated_html.contains("fx-immediate") || updated_html.contains("v-onload"));
            assert!(updated_html.contains(&expected_lobby_path));
            assert_eq!(
                updated
                    .response
                    .navigation
                    .as_ref()
                    .map(hyperchad::renderer::ResponseNavigation::location),
                Some(expected_lobby_path.as_str())
            );

            let mut cancel = update;
            cancel.body = Some(Arc::new("action=CANCEL".into()));
            let cancelled = lobby_action_route(
                &*database,
                &dispatcher,
                &cancel,
                policy,
                "https://games.example.test",
                now,
            )
            .await;
            let cancelled_html = cancelled
                .primary
                .as_ref()
                .expect("navigation marker")
                .display_to_string(false, false)
                .expect("navigation renders");
            assert!(cancelled_html.contains("fx-immediate") || cancelled_html.contains("v-onload"));
            assert!(cancelled_html.contains('/'));
            assert_eq!(
                cancelled
                    .response
                    .navigation
                    .as_ref()
                    .map(hyperchad::renderer::ResponseNavigation::location),
                Some("/")
            );
            assert_eq!(
                database
                    .select("game_lobbies")
                    .where_eq("lobby_id", lobby_id)
                    .execute(&*database)
                    .await
                    .expect("lobby loads")[0]
                    .get("status")
                    .and_then(|value| value.as_str().map(ToOwned::to_owned))
                    .as_deref(),
                Some("CANCELLED")
            );
        });
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
    fn legacy_invitation_creation_is_not_available() {
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

            assert!(rendered.contains("The dashboard action is unknown"));
            assert!(
                crate::dashboard_projection(&*database, &alice)
                    .await
                    .expect("projection")
                    .pending
                    .is_empty()
            );
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
    fn invite_landing_page_previews_the_lobby_and_keeps_the_join_form() {
        block_on(async {
            let database = test_database().await;
            let now = OffsetDateTime::UNIX_EPOCH;
            let host = register(
                &*database,
                "invite-host",
                "correct horse battery staple",
                now,
            )
            .await
            .expect("host registers");
            let guest = register(
                &*database,
                "invite-guest",
                "another correct horse battery",
                now,
            )
            .await
            .expect("guest registers");
            let guest_session = create_session(&*database, &guest, now, Duration::days(1))
                .await
                .expect("session creates");
            let policy = GameCreationPolicy::new(16, 64, 16).expect("policy");
            let dispatcher = GameSharedStateDispatcher::new(database.clone());
            let (lobby_id, _) = create_lobby(
                &*database,
                &host,
                LobbySettings {
                    max_players: 3,
                    board_size: 15,
                    tile_set_count: 1,
                    first_player: FirstPlayerPolicy::Creator,
                    visibility: GameVisibilitySettings::default(),
                },
                policy,
                now,
                Duration::days(1),
            )
            .await
            .expect("lobby creates");
            let path = format!("/lobbies/join/{lobby_id}");

            let anonymous = RouteRequest::from_path(&path, RequestInfo::default());
            let signed_out = lobby_join_route(&*database, &dispatcher, &anonymous, policy, "", now)
                .await
                .primary
                .expect("signed-out page renders")
                .display_to_string(false, false)
                .expect("signed-out page serializes");
            assert!(signed_out.contains("Sign in required"), "{signed_out}");
            assert!(signed_out.contains("href=\"/login\""));
            assert!(!signed_out.contains("WHOOPS"));

            let mut landing = RouteRequest::from_path(&path, RequestInfo::default());
            landing.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                guest_session.expose().to_string(),
            );
            let page = lobby_join_route(&*database, &dispatcher, &landing, policy, "", now)
                .await
                .primary
                .expect("landing renders")
                .display_to_string(false, false)
                .expect("landing serializes");
            assert!(page.contains("id=\"app-page\""), "{page}");
            assert!(page.contains(&format!("hx-post=\"{path}\"")), "{page}");
            assert!(page.contains("hx-target=\"#app-page\""));
            assert!(page.contains("Join game"));
            assert!(page.contains("@invite-host"));
            assert!(page.contains("1 of 3 seats taken"));
            assert!(page.contains("15 × 15 board"));
            assert!(page.contains("brand-logo"));
        });
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn off_turn_submit_replaces_the_waiting_dock_feedback_slot() {
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
            let state = crate::recover_game(&*database, game_id)
                .await
                .expect("game loads");
            let waiting_user = if crate::player_for_user(&*database, game_id, &alice)
                .await
                .expect("Alice is seated")
                == state.active_player
            {
                bob
            } else {
                alice
            };
            let session = create_session(&*database, &waiting_user, now, Duration::days(1))
                .await
                .expect("session creates");

            let mut page_request =
                RouteRequest::from_path(&format!("/games/{game_id}"), RequestInfo::default());
            page_request.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                session.expose().to_string(),
            );
            let page = game_route(&*database, &page_request, now)
                .await
                .display_to_string(false, false)
                .expect("waiting page renders");
            assert!(page.contains("waiting-banner"), "{page}");
            assert!(page.contains("id=\"turn-feedback\""), "{page}");
            assert!(page.contains("plan your next word"));

            let dispatcher = GameSharedStateDispatcher::new(database.clone());
            let mut submit =
                RouteRequest::from_path(&format!("/games/{game_id}/turn"), RequestInfo::default());
            submit.method = "POST".parse().expect("POST parses");
            submit.headers.insert(
                "content-type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            );
            submit.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                session.expose().to_string(),
            );
            submit.body = Some(std::sync::Arc::new(
                format!(
                    "command=PASS&command_id={}&idempotency_key={}&expected_revision={}",
                    uuid::Uuid::new_v4(),
                    uuid::Uuid::new_v4(),
                    state.revision
                )
                .into(),
            ));
            let view =
                game_turn_route(&dispatcher, &*database, &submit, &game_id.to_string(), now).await;
            assert!(view.primary.is_none());
            let fragment = view
                .fragments
                .first()
                .expect("off-turn submit returns a feedback fragment")
                .container
                .display_to_string(false, false)
                .expect("fragment renders");
            assert!(fragment.contains("id=\"turn-feedback\""), "{fragment}");
            assert!(fragment.contains("not your turn"), "{fragment}");
        });
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
        assert_eq!(BoardZoom::Fit.fit_percent(), 78);
        assert_eq!(BoardZoom::Compact.fit_percent(), 89);
        assert_eq!(BoardZoom::Normal.fit_percent(), 100);
        assert_eq!(BoardZoom::Large.fit_percent(), 200);

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
    fn selecting_a_blank_letter_closes_the_picker_and_preserves_the_selection() {
        let rack = [(7, ' ', 0)];
        let mut draft = TurnDraft {
            selected_tile: Some(7),
            ..TurnDraft::default()
        };

        assert!(blank_letter_picker_open(&draft, &rack));

        draft.selected_blank_letter = Some('Q');
        assert!(!blank_letter_picker_open(&draft, &rack));
        assert_eq!(draft.selected_tile, Some(7));
        assert_eq!(draft.selected_blank_letter, Some('Q'));
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
            // Existing challenges remain actionable, but cannot be created through the dashboard.
            crate::challenge_username(&*database, &alice, "bob", now)
                .await
                .expect("legacy challenge");
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
            assert!(rendered.contains("The dashboard action is unknown"));
            refresh_lobby_dashboards(&dispatcher, now, None).await;

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
            assert!(!dashboard.contains("name=\"action\" value=\"CHALLENGE\""));
            assert!(!dashboard.contains("name=\"action\" value=\"CREATE_INVITATION\""));
            assert!(!dashboard.contains("name=\"action\" value=\"REDEEM_INVITATION\""));
            assert!(!dashboard.contains("dashboard-action-progress"));
            assert!(!dashboard.contains("dashboard-action-error"));
            assert!(!dashboard.contains("dashboard-action-status"));
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
            assert!(page.contains("hx-target=\"#app-page\""));
            assert!(page.contains("move-history"));
            assert!(page.contains("data-shared-state-channel"));
            assert!(!page.contains("data-shared-state-refresh-"));
            assert!(page.contains("id=\"turn-feedback\""));
            assert!(!page.contains("id=\"game-error\""));
            assert!(!page.contains("draft_revision="));
            assert!(page.contains("name=\"expected_revision\" value=\"1\""));
            assert!(page.contains("turn-actions"));
            assert!(page.contains("game-awareness"));
            assert!(page.contains("participant-scoreboard"));
            assert!(page.contains("player-scoreboard-"));
            assert!(page.contains("game-scene"));
            assert!(page.contains("game-arena"));
            assert!(page.contains("player-hud"));
            assert!(page.contains("action-hud"));
            assert!(page.contains("id=\"leave-game\""));
            assert!(page.contains("id=\"scene-controls\""));
            assert!(page.contains("activity-rail"));
            assert!(page.contains("game-info-sheet"));
            assert!(page.contains("id=\"rail-tab-moves\""));
            assert!(page.contains("id=\"rail-pane-moves\""));
            assert!(page.contains("id=\"rail-pane-info\""));
            assert!(page.contains("turn-dock"));
            assert!(page.contains("id=\"dock-controls\""));
            assert!(page.contains("id=\"dock-status\""));
            assert!(page.contains("id=\"board-frame\""));
            assert!(!page.contains("sx-padding-bottom=\"330\""));
            assert!(!page.contains("sx-max-height=\"40vh\""));
            assert!(!page.contains("sx-max-height=\"300px\""));
            assert!(page.contains("sx-position=\"fixed\""));
            assert!(page.contains("sx-height=\"100dvh\""));
            assert!(page.contains("sx-min-height=\"100dvh\""));
            assert!(page.contains("sx-position=\"absolute\""));
            assert!(page.contains("sx-top=\"0\""));
            assert!(page.contains("sx-bottom=\"0\""));
            assert!(
                page.contains("sx-translate-x=\"-50%\""),
                "the dock centers over the board"
            );
            assert!(page.contains("sx-overflow-y=\"auto\""));
            assert!(page.contains("board-viewport"));
            assert!(page.contains("board-scroll-content"));
            assert!(page.contains("board-zoom-controls"));
            assert!(page.contains("value=\"ZOOM_OUT\""));
            assert!(page.contains("value=\"ZOOM_RESET\""));
            assert!(page.contains("value=\"ZOOM_IN\""));
            assert!(page.contains("value=\"SHUFFLE_RACK\""));
            assert!(page.contains("Shuffle"));
            assert!(page.contains("Menu"));
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
            assert!(page.contains("data-board-size=\"15\""));
            assert!(page.contains("data-board-fit-percent=\"100\""));
            assert!(
                page.contains("dvh"),
                "board squares fit the dynamic viewport"
            );
            assert!(!page.contains("width:720px"));
            assert!(page.contains("2L"));
            assert!(page.contains("3W"));
            assert!(page.contains("board-zoom-controls"));
            assert!(page.contains("Place tiles to play"));
            assert!(page.contains("eligible-square-highlight"));
            assert!(page.contains("game-rules"));
            assert!(page.contains("50-point full-rack bonus"));
            assert!(page.contains("Three uninterrupted complete rounds"));
            assert!(page.contains("dock-message"));
            assert!(!page.contains("provide matching board coordinates"));
            assert!(!page.contains("pending-editor-0"));
            assert!(page.contains("remaining-tiles-button"));
            assert!(page.contains("remaining-tiles-panel"));
            assert!(page.contains("tiles in the bag"));
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
            let mut placement_request = RouteRequest::from_path(
                &format!("/games/{game_id}/compose"),
                RequestInfo::default(),
            );
            placement_request.method = "POST".parse().expect("POST parses");
            placement_request.headers.insert(
                "content-type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            );
            placement_request.cookies = cookies.clone();
            let placement_draft = TurnDraft {
                selected_tile: Some(first.id.get()),
                ..TurnDraft::default()
            };
            placement_request.body = Some(std::sync::Arc::new(
                format!(
                    "action=PLACE_TILE&expected_revision={}&draft={}&x=7&y=7",
                    game.view.revision,
                    draft_token(&placement_draft)
                )
                .into(),
            ));
            let placement_response =
                game_compose_route(&*database, &placement_request, &game_id.to_string(), now)
                    .await
                    .display_to_string(false, false)
                    .expect("placement response renders");
            assert!(placement_response.contains("id=\"app-page\""));
            assert!(placement_response.contains("pending-square"));
            assert!(placement_response.contains("activity-rail"));

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
            // Existing challenges remain actionable, but cannot be created through the dashboard.
            crate::challenge_username(&*database, &alice, "bob", now)
                .await
                .expect("legacy challenge");
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
            assert!(alice_dashboard.contains("The dashboard action is unknown"));

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
                accepted.contains("Your turn") || accepted.contains("Waiting for another player"),
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
                actor: None,
                points: Some(4),
                headline: "played".to_string(),
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
    fn lobby_new_route_shows_creation_page_instead_of_treating_new_as_an_id() {
        block_on(async {
            let database = test_database().await;
            let now = OffsetDateTime::now_utc();
            let creator = register(
                &*database,
                "new-route-creator",
                "correct horse battery staple",
                now,
            )
            .await
            .expect("creator registers");
            let session = create_session(&*database, &creator, now, Duration::days(1))
                .await
                .expect("session creates");
            let dispatcher = Arc::new(crate::GameSharedStateDispatcher::new(database.clone()));
            let router = create_product_router(
                database,
                dispatcher,
                None,
                None,
                false,
                crate::GameCreationPolicy::new(16, 64, 16).expect("policy"),
                "csrf-test".to_string(),
                "https://games.example.test".to_string(),
                true,
            );
            let mut request = RouteRequest::from_path("/lobbies/new", RequestInfo::default());
            request.cookies.insert(
                SESSION_COOKIE_NAME.to_string(),
                session.expose().to_string(),
            );
            let content = router
                .navigate(request)
                .await
                .expect("new lobby route resolves")
                .expect("new lobby route returns content");
            let Content::View(view) = content else {
                panic!("new lobby route should return a view");
            };
            let page = view
                .primary
                .as_ref()
                .expect("creation page is primary")
                .display_to_string(false, false)
                .expect("creation page renders");
            assert!(page.contains("Create game"));
            assert!(!page.contains("Lobby unavailable"));
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
                crate::GameCreationPolicy::new(16, 64, 16).expect("policy"),
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
                crate::GameCreationPolicy::new(16, 64, 16).expect("policy"),
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
