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
    template::{LayoutOverflow, container},
};
use serde::Deserialize;
use switchy_database::Database;
use time::{Duration, OffsetDateTime};
use words_with_spouses_game_domain::{
    Coordinate, GameCommand, GameError, Placement, PlacementGuidance, PlayAnalysis, PremiumSquare,
    TileId,
};

use crate::{
    AccountWorkflowError, AuthenticatedDashboard, AuthorizedGamePage, PresentationError,
    ProductWorkflowError, UserScoreTotals, accept_pending_challenge, cancel_pending_challenge,
    challenge_username, create_shareable_invitation, decline_pending_challenge, error_component,
    load_authenticated_dashboard, load_authorized_game_page, login_and_create_session,
    logout_session, move_history_component, redeem_shareable_invitation,
    register_and_create_session, revoke_shareable_invitation, viewer_turn_component,
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
    fn has_unsubmitted_input(&self) -> bool {
        self.selected_tile.is_some()
            || self.selected_blank_letter.is_some()
            || !self.placements.is_empty()
            || !self.exchange_tiles.is_empty()
            || self.mode != TurnMode::Play
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct DraftFeedback {
    analysis: Option<PlayAnalysis>,
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

/// Account credential form accepted by renderer-neutral routes.
#[derive(Debug, Deserialize)]
struct AccountForm {
    username: String,
    password: String,
    #[serde(default)]
    invitation_token: String,
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
    secure_cookies: bool,
) -> View {
    let invitation_token = request.query.get("invite").map_or("", String::as_str);
    if request.method.as_ref() != "POST" {
        return View::from(login_page_with_invitation(None, invitation_token));
    }
    let form: AccountForm = match request.parse_form() {
        Ok(form) => form,
        Err(_) => {
            return View::from(login_page_with_invitation(
                Some("Enter a username and password."),
                invitation_token,
            ));
        }
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
            let dashboard = dashboard_after_authentication(
                database,
                session.expose(),
                &form.invitation_token,
                now,
            )
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
        Err(error) => View::from(login_page_with_invitation(
            Some(account_error_message(&error)),
            &form.invitation_token,
        )),
    }
}

async fn register_route(
    database: &dyn Database,
    request: &RouteRequest,
    now: OffsetDateTime,
    csrf_token: &str,
    secure_cookies: bool,
) -> View {
    let invitation_token = request.query.get("invite").map_or("", String::as_str);
    if request.method.as_ref() != "POST" {
        return View::from(register_page_with_invitation(None, invitation_token));
    }
    let form: AccountForm = match request.parse_form() {
        Ok(form) => form,
        Err(_) => {
            return View::from(register_page_with_invitation(
                Some("Enter a username and password."),
                invitation_token,
            ));
        }
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
            let dashboard = dashboard_after_authentication(
                database,
                session.expose(),
                &form.invitation_token,
                now,
            )
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
        Err(error) => View::from(register_page_with_invitation(
            Some(account_error_message(&error)),
            &form.invitation_token,
        )),
    }
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
            analysis: None,
            guidance,
            message: message.to_string(),
        };
    }
    match game.analyze_play(&placements) {
        Ok(analysis) => DraftFeedback {
            analysis: Some(analysis),
            guidance,
            message: "This draft is ready to play.".to_string(),
        },
        Err(error) => DraftFeedback {
            analysis: None,
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
        GameError::InvalidWord(word) => format!("The dictionary does not accept {word}."),
        GameError::InvalidBlankLetter => {
            "Choose a letter for every drafted blank tile.".to_string()
        }
        error => error.to_string(),
    }
}

fn draft_feedback_component(feedback: &DraftFeedback) -> Container {
    let formed_words = feedback.analysis.as_ref().map(|analysis| {
        analysis
            .words
            .iter()
            .map(|word| word.text.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    });
    container! {
        section id="draft-preview" background=#ffffff border=(("#ded8c9", 1))
            border-radius="16px" padding="18px" gap="8px" {
            h2 { "Draft preview" }
            @if let Some(analysis) = &feedback.analysis {
                span color=#3f5735 font-weight=bold { "Word" (if analysis.words.len() == 1 { "" } else { "s" }) ": " (formed_words.unwrap_or_default()) }
                span font-size="22px" font-weight=bold { (analysis.score) " points" }
                span color=#3f5735 { (feedback.message.as_str()) }
                @if analysis.full_rack_bonus > 0 {
                    span color=#5d6258 { "Includes a " (analysis.full_rack_bonus) "-point full-rack bonus." }
                }
            } @else {
                span color=#5d6258 { (feedback.message.as_str()) }
                @if !feedback.guidance.required.is_empty() {
                    span color=#7a3f16 font-weight=bold { "Required squares are highlighted on the board." }
                }
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
        Err(error) => return product_error_page("Unable to compose turn", &error.to_string()),
    };
    if game.completed {
        return visual_game_page(&game, &TurnDraft::default(), Some("This game is complete."));
    }
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
        return draft_error_page(
            &game,
            &TurnDraft::default(),
            "The board changed. Your unsubmitted tiles were cleared; compose the move again.",
        );
    }
    let mut draft = parse_draft(&form.draft).unwrap_or_default();
    let viewer_turn = game.view.active_player == game.viewer_player;
    if !viewer_turn
        && !matches!(
            form.action.as_str(),
            "PICK_RACK_TILE" | "SWAP_RACK_TILES" | "CANCEL_MODE"
        )
    {
        return draft_error_page(
            &game,
            &draft,
            "It is not your turn. You can still arrange your rack while you wait.",
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
                .any(|(placed, _)| *placed == coordinate)
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
            let selected_tile = draft.rack_tile.or(draft.selected_tile);
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
        "CLEAR" => draft = TurnDraft::default(),
        _ => return draft_error_page(&game, &draft, "That turn action is unavailable."),
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
        section id="turn-feedback" width="100%" max-width="1120px"
            {
            @if let Some(message) = message {
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

fn invitation_joined_page(game_id: words_with_spouses_game_domain::GameId) -> Container {
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
    let register_href = format!("/register?invite={invitation_token}");
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
                    span { "Sign in or create an account to accept." }
                    div direction="row" gap="10px" {
                        anchor href=(login_href) color=#ffffff background=#526243 border=(("#526243", 1))
                            border-radius="10px" padding-y=13 padding-x=18 { "Sign in" }
                        anchor href=(register_href) color=#526243 border=(("#839276", 1))
                            border-radius="10px" padding-y=13 padding-x=18 { "Create account" }
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
pub fn create_product_router(
    database: Arc<dyn Database>,
    dispatcher: Arc<crate::GameSharedStateDispatcher>,
    csrf_token: String,
    public_base_url: String,
    secure_cookies: bool,
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
                secure_cookies,
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
                secure_cookies,
            )
            .await) as Result<View, Box<dyn std::error::Error>>
        }
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
    let game_dispatcher = dispatcher;
    router.add_route_result(
        RoutePath::LiteralPrefix("/games/".to_string()),
        move |request: RouteRequest| {
            let database = database.clone();
            let dispatcher = game_dispatcher.clone();
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
    let stale_message = request
        .query
        .contains_key("draft_stale")
        .then_some("The board changed while you were composing. Your unsubmitted tiles were cleared; your rack order was preserved.");
    match load_authorized_game_page(database, &request.cookies, game_id, now).await {
        Ok(game) => visual_game_page(&game, &TurnDraft::default(), stale_message),
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
    login_page_with_invitation(error, "")
}

fn login_page_with_invitation(error: Option<&str>, invitation_token: &str) -> Container {
    let message = error.unwrap_or_default();
    let register_href = if invitation_token.is_empty() {
        "/register".to_string()
    } else {
        format!("/register?invite={invitation_token}")
    };
    container! {
        div id="app-page" direction="column" align-items="center" min-height="100vh" background=#f4f1e8 padding-y=48 padding-x=24 {
            main width="100%" max-width="480px"
                background=#ffffff border=(("#ded8c9", 1)) border-radius="18px" padding="32px" gap="20px" {
                anchor href="/" color=#526243 { "← Home" }
                div gap="6px" {
                    span color=#7b6240 font-weight=bold { "WORDS WITH SPOUSES" }
                    h1 { "Welcome back" }
                    span color=#5d6258 { "Sign in to continue your private games." }
                }
                form hx-post="/login" hx-target="#app-page" gap="12px" {
                    input type=hidden name="invitation_token" value=(invitation_token);
                    span font-weight=bold { "Username" }
                    input type=text name="username" placeholder="Username" padding-y=13 padding-x=14
                        border=(("#cfc8b8", 1)) border-radius="10px";
                    span font-weight=bold { "Password" }
                    input type=password name="password" placeholder="Password" padding-y=13 padding-x=14
                        border=(("#cfc8b8", 1)) border-radius="10px";
                    button type=submit padding-y=13 padding-x=18 background=#526243 color=#ffffff
                        border=(("#526243", 1)) border-radius="10px" cursor=pointer { "Sign in" }
                }
                @if !message.is_empty() {
                    section id="account-result" background=#fff3e8 border=(("#e2b98f", 1))
                        border-radius="10px" padding="12px" { span color=#7a3f16 { (message) } }
                }
                span { "New here? " anchor href=(register_href) color=#526243 { "Create an account" } }
            }
        }
    }
    .into()
}

/// Renders the renderer-neutral registration form.
#[must_use]
pub fn register_page(error: Option<&str>) -> Container {
    register_page_with_invitation(error, "")
}

fn register_page_with_invitation(error: Option<&str>, invitation_token: &str) -> Container {
    let message = error.unwrap_or_default();
    let login_href = if invitation_token.is_empty() {
        "/login".to_string()
    } else {
        format!("/login?invite={invitation_token}")
    };
    container! {
        div id="app-page" direction="column" align-items="center" min-height="100vh" background=#f4f1e8 padding-y=48 padding-x=24 {
            main width="100%" max-width="480px"
                background=#ffffff border=(("#ded8c9", 1)) border-radius="18px" padding="32px" gap="20px" {
                anchor href="/" color=#526243 { "← Home" }
                div gap="6px" {
                    span color=#7b6240 font-weight=bold { "WORDS WITH SPOUSES" }
                    h1 { "Create your account" }
                    span color=#5d6258 { "Choose a username your opponent will recognize." }
                }
                form hx-post="/register" hx-target="#app-page" gap="12px" {
                    input type=hidden name="invitation_token" value=(invitation_token);
                    span font-weight=bold { "Username" }
                    input type=text name="username" placeholder="Username" padding-y=13 padding-x=14
                        border=(("#cfc8b8", 1)) border-radius="10px";
                    span font-weight=bold { "Password" }
                    input type=password name="password" placeholder="Password (12+ characters)" padding-y=13 padding-x=14
                        border=(("#cfc8b8", 1)) border-radius="10px";
                    button type=submit padding-y=13 padding-x=18 background=#526243 color=#ffffff
                        border=(("#526243", 1)) border-radius="10px" cursor=pointer { "Create account" }
                }
                @if !message.is_empty() {
                    section id="account-result" background=#fff3e8 border=(("#e2b98f", 1))
                        border-radius="10px" padding="12px" { span color=#7a3f16 { (message) } }
                }
                span { "Already have an account? " anchor href=(login_href) color=#526243 { "Sign in" } }
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
                span color=#7b6240 font-weight=bold { "WORDS WITH SPOUSES" }
                h1 { "Sign in required" }
                span color=#5d6258 { "A valid secure session is required to view games." }
                div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=10 {
                    anchor href="/login" color=#ffffff background=#526243 border=(("#526243", 1))
                        border-radius=10 padding-y=12 padding-x=16 { "Sign in" }
                    anchor href="/register" color=#526243 border=(("#839276", 1))
                        border-radius=10 padding-y=12 padding-x=16 { "Create account" }
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
        section id="new-game-actions" width="100%" background=#ffffff
            border=(("#ded8c9", 1)) border-radius="18px" padding="24px" gap="18px" {
            div gap="5px" {
                h2 { "Start a game" }
                span color=#5d6258 { "Challenge a username or make a one-time private invite." }
            }
            div id="dashboard-action-progress" hidden background=#e8f1e3 border=(("#a9bf9c", 1))
                border-radius="10px" padding="12px" { span { "Working…" } }
            div id="dashboard-action-error" hidden background=#fff3e8 border=(("#e2b98f", 1))
                border-radius="10px" padding="12px" { span { "The request did not complete. Check your connection and try again." } }
            form hx-post="/dashboard/action" hx-target="#app-page" gap="10px"
                fx-http-before-request=(dashboard_request_before())
                fx-http-after-request=(dashboard_request_after())
                fx-http-error=(dashboard_request_error()) {
                input type=hidden name="action" value="CHALLENGE";
                input type=text name="username" placeholder="Opponent username" padding-y=13 padding-x=14
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
fn dashboard_page_content(
    dashboard: &AuthenticatedDashboard,
    created_invitation: Option<(&str, &str, &str)>,
) -> Container {
    let user_id = dashboard.user_id.as_str();
    let username = dashboard.username.as_str();
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
            min-height="100vh" background=#f4f1e8 color=#293126
            padding-y=24 padding-x=16 {
            div id="dashboard-shell" width="100%" max-width="1080px" gap="28px" {
                header id="dashboard-header" direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) justify-content="space-between" align-items="center"
                    background=#ffffff border=(("#ded8c9", 1)) border-radius="18px" padding-y=22 padding-x=26 gap="16px" {
                    div gap="4px" {
                        span color=#7b6240 font-weight=bold { "WORDS WITH SPOUSES" }
                        h1 { "Your games" }
                        span color=#5d6258 { "Signed in as " (username) }
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
                main id="dashboard-main" direction="column" gap="24px" align-items="start" {
                    (start_game_component())
                    section id="score-totals" width="100%" background=#ffffff
                        border=(("#ded8c9", 1)) border-radius="18px" padding="24px" gap="10px" {
                        h2 { "Score history" }
                        span color=#5d6258 { (totals) }
                    }
                }
                section id="pending-games" background=#ffffff border=(("#ded8c9", 1))
                    border-radius="18px" padding="24px" gap="14px" {
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
                        @let counterparty = item.counterparty_username.as_deref().unwrap_or("Private invite");
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
                            border=(("#e3ded2", 1)) border-radius="12px" padding-y=14 padding-x=16 gap="12px" {
                            div gap="3px" {
                                span font-weight=bold { (heading) }
                                @if item.kind == "INVITATION" && item.id != created_invitation_id {
                                    span color=#777b73 { "Link hidden after creation for security." }
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
                section id="active-games" background=#ffffff border=(("#ded8c9", 1))
                    border-radius="18px" padding="24px" gap="14px" {
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
                            border=(("#e3ded2", 1)) border-radius="12px" padding-y=14 padding-x=16 gap="12px" {
                            div gap="3px" {
                                anchor href=(href) color=#526243 font-weight=bold { "Game with " (game.opponent_username.as_str()) }
                                span color=#3f5735 font-weight=bold { (state) }
                                span color=#777b73 { "You " (game.viewer_score) " – " (game.opponent_score) " " (game.opponent_username.as_str()) }
                            }
                            span color=#777b73 { (game.latest_activity.as_str()) }
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
        .copied()
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
    container! {
        section id="game-board" data-revision=(game.view.revision) gap="10px" {
            span color=#5d6258 { "Board key: gold tiles are committed; green outlines mark the latest move; blue tiles are your current draft; orange squares are required; green squares are eligible." }
            div overflow-x="auto" {
                div width="720px" background=#7c6547 border=(("#7c6547", 5)) gap="2px" {
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
                                @let (background, label, color) = if let Some(letter) = committed {
                                    ("#f2d79b", letter.to_string(), "#2e291f")
                                } else if let Some((tile_id, blank_letter)) = drafted {
                                    let letter = game.view.rack.iter().find(|(id, _, _)| *id == tile_id)
                                        .map(|(_, letter, _)| blank_letter.unwrap_or(*letter)).unwrap_or('?');
                                    ("#f7e4ae", letter.to_string(), "#2e291f")
                                } else if coordinate == game.rules.start {
                                    ("#e79b9b", "★".to_string(), "#6b3535")
                                } else {
                                    match premium {
                                        Some(PremiumSquare::Letter(2)) => ("#b9dbe8", "DL".to_string(), "#31596a"),
                                        Some(PremiumSquare::Letter(_)) => ("#77b6d1", "TL".to_string(), "#173f52"),
                                        Some(PremiumSquare::Word(2)) => ("#e9b2b2", "DW".to_string(), "#743d3d"),
                                        Some(PremiumSquare::Word(_)) => ("#d87f7f", "TW".to_string(), "#ffffff"),
                                        None => ("#ede6d4", String::new(), "#756f64"),
                                    }
                                };
                                @if let Some((tile_id, _)) = drafted {
                                    form hx-post=(action.as_str()) hx-target="#app-page" {
                                        (compose_form_fields(game, draft, "REMOVE_TILE"))
                                        input type=hidden name="tile_id" value=(tile_id);
                                        button type=submit class="board-square pending-square" width="44px" height="44px"
                                            background=#dce8f5 color=#2e291f border=(("#4f7298", 3))
                                            align-items="center" justify-content="center" font-weight=bold cursor=pointer {
                                            span font-size="20px" { (label) }
                                        }
                                    }
                                } @else if committed.is_some() {
                                    div class=(if latest { "board-square committed-square latest-move-square" } else { "board-square committed-square" }) width="44px" height="44px"
                                        background=(background) color=(color) border=((if latest { "#526243" } else { "#aa9e85" }, if latest { 3 } else { 1 }))
                                        align-items="center" justify-content="center" font-weight=bold {
                                        span font-size="20px" { (label) }
                                    }
                                } @else {
                                    form hx-post=(action.as_str()) hx-target="#app-page" {
                                        (compose_form_fields(game, draft, "PLACE_TILE"))
                                        input type=hidden name="x" value=(x);
                                        input type=hidden name="y" value=(y);
                                        button type=submit class="board-square open-square" data-x=(x) data-y=(y)
                                            width="44px" height="44px" background=(background) color=(color)
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
                                            span position="relative" { (label) }
                                        }
                                    }
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

fn visual_rack(game: &AuthorizedGamePage, draft: &TurnDraft) -> Container {
    let action = format!("/games/{}/compose", game.game_id);
    let can_compose = !game.completed && game.view.active_player == game.viewer_player;
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
        section id="player-rack" gap="10px" {
            h2 { "Your rack" }
            div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap="6px" background=#7c6547 border-radius="8px" padding="8px" {
                @for (tile_id, letter, points) in rack {
                    @let placed = draft.placements.iter().any(|placement| placement.tile_id == *tile_id);
                    @let selected = draft.selected_tile == Some(*tile_id) || draft.rack_tile == Some(*tile_id);
                    @let exchange_selected = draft.exchange_tiles.contains(tile_id);
                    @let face = if *letter == ' ' { "?".to_string() } else { letter.to_string() };
                    @if can_compose || draft.mode == TurnMode::Play {
                        @let rack_action = if draft.mode == TurnMode::Exchange {
                            "TOGGLE_EXCHANGE"
                        } else if draft.rack_tile.is_some() || draft.selected_tile.is_some() {
                            "SWAP_RACK_TILES"
                        } else if can_compose {
                            "CHOOSE_TILE"
                        } else {
                            "PICK_RACK_TILE"
                        };
                        form hx-post=(action.as_str()) hx-target="#app-page" {
                            (compose_form_fields(game, draft, rack_action))
                            input type=hidden name="tile_id" value=(tile_id);
                            button type=submit class=(if selected || exchange_selected { "rack-tile rack-tile-selected" } else { "rack-tile" }) data-tile-id=(tile_id) width="50px" height="56px"
                                background=(if placed { "#c8b88f" } else { "#f2d79b" }) color=#2e291f
                                border=(("#d1b36f", 2)) border-radius="6px" align-items="center" justify-content="center"
                                position="relative" font-weight=bold opacity=(if placed { 0.45 } else { 1.0 }) cursor=pointer {
                                span font-size="24px" { (face) }
                                span position="absolute" right="5px" bottom="3px" font-size="12px" { (points) }
                            }
                        }
                    } @else {
                        div class="rack-tile" data-tile-id=(tile_id) width="50px" height="56px"
                            background=#f2d79b color=#2e291f border=(("#d1b36f", 2)) border-radius="6px"
                            align-items="center" justify-content="center" position="relative" font-weight=bold {
                            span font-size="24px" { (face) }
                            span position="absolute" right="5px" bottom="3px" font-size="12px" { (points) }
                        }
                    }
                }
            }
            @if draft.rack_tile.is_some() || draft.selected_tile.is_some() {
                span color=#3f5735 font-weight=bold { "Tile selected — choose another rack tile to swap positions, or choose a board square." }
            } @else if draft.mode == TurnMode::Play {
                span color=#777b73 { "Select a rack tile to play it or swap it with another tile." }
            }
            @if let Some(tile_id) = draft.selected_tile {
                @let selected_blank = game.view.rack.iter().any(|(id, letter, _)| *id == tile_id && *letter == ' ');
                span color=#3f5735 font-weight=bold { "Tile selected — choose an open board square." }
                @if selected_blank {
                    div gap="7px" {
                        span { "Choose the blank tile’s letter first:" }
                        div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap="4px" {
                            @for letter in 'A'..='Z' {
                                form hx-post=(action.as_str()) hx-target="#app-page" {
                                    (compose_form_fields(game, draft, "CHOOSE_BLANK_LETTER"))
                                    input type=hidden name="letter" value=(letter.to_string());
                                    button type=submit data-blank-letter=(letter.to_string()) width="30px" height="30px" border=(("#aa9e85", 1))
                                        background=(if draft.selected_blank_letter == Some(letter) { "#e8f1e3" } else { "#ffffff" })
                                        border-radius="5px" cursor=pointer { (letter) }
                                }
                            }
                        }
                    }
                }
            } @else if draft.rack_tile.is_none() {
                span color=#777b73 { "Choose a tile, then choose its square on the board." }
            }
        }
    }
    .into()
}

fn visual_turn_actions(game: &AuthorizedGamePage, draft: &TurnDraft) -> Container {
    let turn_action = format!("/games/{}/turn", game.game_id);
    let compose_action = format!("/games/{}/compose", game.game_id);
    let command_id = uuid::Uuid::new_v4().to_string();
    let idempotency_key = uuid::Uuid::new_v4().to_string();
    container! {
        section id="turn-actions" class="turn-composer" background=#ffffff border=(("#ded8c9", 1))
            border-radius="16px" padding="18px" gap="12px" {
            @if draft.mode == TurnMode::Exchange {
                span font-weight=bold { (draft.exchange_tiles.len()) " tile(s) selected for exchange." }
                div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap="10px" {
                    form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                        (compose_form_fields(game, draft, "REVIEW_EXCHANGE"))
                        button type=submit padding-y=10 padding-x=14 background=#526243 color=#ffffff
                            border=(("#526243", 1)) border-radius="9px" cursor=pointer { "Review exchange" }
                    }
                    form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                        (compose_form_fields(game, draft, "CANCEL_MODE"))
                        button type=submit padding-y=10 padding-x=14 background=#ffffff color=#526243
                            border=(("#839276", 1)) border-radius="9px" cursor=pointer { "Cancel" }
                    }
                }
            } @else if draft.mode == TurnMode::ConfirmExchange {
                span font-weight=bold { "Exchange " (draft.exchange_tiles.len()) " selected tile(s)?" }
                span color=#5d6258 { "This ends your turn and draws the same number of replacements." }
                div direction="row" gap="10px" {
                    form hx-post=(turn_action.as_str()) hx-target="#app-page" {
                        input type=hidden name="command" value="EXCHANGE";
                        input type=hidden name="command_id" value=(command_id.as_str());
                        input type=hidden name="idempotency_key" value=(idempotency_key.as_str());
                        input type=hidden name="expected_revision" value=(game.view.revision);
                        @for (index, tile_id) in draft.exchange_tiles.iter().enumerate() {
                            input type=hidden name=(format!("tile_{index}")) value=(tile_id);
                        }
                        button type=submit padding-y=10 padding-x=14 background=#526243 color=#ffffff
                            border=(("#526243", 1)) border-radius="9px" cursor=pointer { "Confirm exchange" }
                    }
                    form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                        (compose_form_fields(game, draft, "CANCEL_MODE"))
                        button type=submit padding-y=10 padding-x=14 background=#ffffff color=#526243
                            border=(("#839276", 1)) border-radius="9px" cursor=pointer { "Cancel" }
                    }
                }
            } @else if draft.mode == TurnMode::ConfirmPass {
                span font-weight=bold { "Pass this turn?" }
                span color=#5d6258 { "You will score no points and your opponent will play next." }
                (confirmed_command_forms(game, draft, "PASS", "Confirm pass", &command_id, &idempotency_key))
            } @else if draft.mode == TurnMode::ConfirmResign {
                span font-weight=bold color=#814434 { "Resign this game?" }
                span color=#5d6258 { "The game ends immediately and your opponent wins." }
                (confirmed_command_forms(game, draft, "RESIGN", "Confirm resignation", &command_id, &idempotency_key))
            } @else {
                @if !draft.placements.is_empty() {
                form hx-post=(turn_action.as_str()) hx-target="#app-page" gap="8px" {
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
                    button type=submit padding-y=13 padding-x=18 background=#526243 color=#ffffff
                        border=(("#526243", 1)) border-radius="10px" cursor=pointer { "Play word" }
                }
                form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                    (compose_form_fields(game, draft, "CLEAR"))
                    button type=submit padding-y=10 padding-x=14 background=#ffffff color=#526243
                        border=(("#839276", 1)) border-radius="9px" cursor=pointer { "Recall tiles" }
                }
                }
                @if game.exchange_available {
                    form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                        (compose_form_fields(game, draft, "BEGIN_EXCHANGE"))
                        button type=submit padding-y=10 padding-x=14 background=#ffffff color=#526243
                            border=(("#839276", 1)) border-radius="9px" cursor=pointer { "Exchange tiles" }
                    }
                } @else {
                    span color=#777b73 { "Tile exchange is not available now." }
                }
                form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                    (compose_form_fields(game, draft, "CONFIRM_PASS"))
                    button type=submit padding-y=10 padding-x=14 background=#ffffff color=#526243
                        border=(("#839276", 1)) border-radius="9px" cursor=pointer { "Pass" }
                }
                form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                    (compose_form_fields(game, draft, "CONFIRM_RESIGN"))
                    button type=submit padding-y=10 padding-x=14 background=#ffffff color=#814434
                        border=(("#d3a99d", 1)) border-radius="9px" cursor=pointer { "Resign" }
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
        div direction="row" gap="10px" {
            form hx-post=(turn_action.as_str()) hx-target="#app-page" {
                input type=hidden name="command" value=(command);
                input type=hidden name="command_id" value=(command_id);
                input type=hidden name="idempotency_key" value=(idempotency_key);
                input type=hidden name="expected_revision" value=(game.view.revision);
                button type=submit padding-y=10 padding-x=14 background=#526243 color=#ffffff
                    border=(("#526243", 1)) border-radius="9px" cursor=pointer { (label) }
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

fn game_awareness_component(game: &AuthorizedGamePage) -> Container {
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
    let turn = if game.completed {
        "Game complete".to_string()
    } else if game.view.active_player == game.viewer_player {
        format!("{}’s turn (you)", game.viewer_username)
    } else {
        format!("{}’s turn", game.opponent_username)
    };
    container! {
        section id="game-awareness" gap="10px" {
            div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap="12px" {
                div flex=1 background=#ffffff border=(("#ded8c9", 1)) border-radius="12px" padding-y=14 padding-x=18 gap="4px" {
                    span color=#777b73 { (game.viewer_username.as_str()) " (you)" }
                    span font-size="26px" font-weight=bold { (viewer_score) }
                }
                div flex=1 background=#ffffff border=(("#ded8c9", 1)) border-radius="12px" padding-y=14 padding-x=18 gap="4px" {
                    span color=#777b73 { (game.opponent_username.as_str()) }
                    span font-size="26px" font-weight=bold { (opponent_score) }
                }
            }
            span id="named-turn-status" color=#3f5735 font-weight=bold { (turn) }
            @if let Some(latest) = &game.latest_action {
                span id="latest-game-action" color=#5d6258 { "Latest: " (latest.as_str()) }
            }
            div id="live-status" color=#5d6258 {
                span id="live-status-connecting"
                    fx-global-shared-state-connecting=(live_status_action("live-status-connecting")) {
                    "Live updates: connecting…"
                }
                span id="live-status-connected" hidden
                    fx-global-shared-state-connected=(live_status_action("live-status-connected")) {
                    "Live updates: connected"
                }
                span hidden
                    fx-global-shared-state-subscribed=(live_status_action("live-status-connected")) { }
                span id="live-status-reconnecting" hidden
                    fx-global-shared-state-reconnecting=(live_status_action("live-status-reconnecting")) {
                    "Live updates: reconnecting…"
                }
                span id="live-status-disconnected" hidden
                    fx-global-shared-state-disconnected=(live_status_action("live-status-disconnected")) {
                    "Live updates: disconnected. Retrying automatically."
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
            format!("{} won", game.viewer_username)
        }
        Some(_) => format!("{} won", game.opponent_username),
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
        section id="completed-game-summary" background=#ffffff border=(("#c8b88f", 2))
            border-radius="16px" padding="20px" gap="9px" {
            h2 { (outcome) }
            @if let Some(reason) = &game.completion_reason {
                span color=#5d6258 { "Completed by: " (reason.as_str()) }
            }
            div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap="16px" {
                span font-weight=bold { (game.viewer_username.as_str()) ": " (viewer_score) }
                span font-weight=bold { (game.opponent_username.as_str()) ": " (opponent_score) }
            }
            @if viewer_adjustment != 0 || opponent_adjustment != 0 {
                span color=#5d6258 {
                    "Final adjustments — " (game.viewer_username.as_str()) ": " (format!("{viewer_adjustment:+}"))
                    ", " (game.opponent_username.as_str()) ": " (format!("{opponent_adjustment:+}"))
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
        details id="game-rules" width="100%" background=#ffffff border=(("#ded8c9", 1))
            border-radius="14px" padding="16px" {
            summary cursor="pointer" font-weight=bold { "Rules and board key" }
            div padding-top="12px" gap="9px" color=#5d6258 {
                span { "Place tiles in one row or column to form connected words. The opening play must cover the center star. All formed words must be accepted by this game’s pinned dictionary." }
                span { "A tile scores its printed value. DL and TL multiply a newly placed tile; DW and TW multiply the whole word. Premium squares apply only when first covered." }
                span { "Playing all " (rack_size) " rack tiles adds a " (full_rack_bonus) "-point full-rack bonus." }
                span { "Exchange replaces selected tiles and ends your turn. It is available only while at least " (exchange_requirement) " tiles remain in the reserve; exchanged tile identities stay private." }
                span { "Passing ends your turn without playing. " (scoreless_turn_limit) " consecutive scoreless turns complete the game." }
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

fn visual_game_page(
    game: &AuthorizedGamePage,
    draft: &TurnDraft,
    error: Option<&str>,
) -> Container {
    let game_id = game.game_id.to_string();
    let short_game_id = game_id.chars().take(8).collect::<String>();
    let feedback = draft_feedback(game, draft);
    let board = visual_board(game, draft, &feedback);
    let draft_preview = draft_feedback_component(&feedback);
    let awareness = game_awareness_component(game);
    let completed_summary = game.completed.then(|| completed_game_summary(game));
    let viewer_turn = viewer_turn_component(&game.view, game.viewer_player);
    let rack = visual_rack(game, draft);
    let actions = visual_turn_actions(game, draft);
    let history = move_history_component(&game.history);
    let game_channel = format!("game:{}", game.game_id);
    let game_path = if draft.has_unsubmitted_input() {
        format!("/games/{game_id}?draft_stale=1")
    } else {
        format!("/games/{game_id}")
    };
    let refresh_game = ActionType::Navigate { url: game_path };
    let turn_feedback_view = turn_feedback(error);
    container! {
        div id="app-page" data-shared-state-channel=(game_channel.as_str())
            fx-global-shared-state-event=(refresh_game)
            direction="column" align-items="center"
            min-height="100vh" background=#f4f1e8 color=#293126
            padding-y=20 padding-x=10 gap="18px" {
            header id="game-header" width="100%" max-width="760px"
                direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) justify-content="space-between" align-items="center"
                background=#ffffff border=(("#ded8c9", 1)) border-radius="16px"
                padding-y=18 padding-x=22 gap="14px" {
                div gap="3px" {
                    anchor href="/" color=#526243 { "← Games" }
                    h1 { "Game " (short_game_id) }
                }
                (viewer_turn)
            }
            (turn_feedback_view)
            main id="game-layout" width="100%" max-width="760px" gap="16px" {
                (awareness)
                @if let Some(completed_summary) = completed_summary { (completed_summary) }
                @if !game.completed && game.view.active_player == game.viewer_player { (draft_preview) }
                section id="board-card" width="100%" background=#ffffff
                    border=(("#ded8c9", 1)) border-radius="18px" padding="14px" { (board) }
                (rack)
                @if !game.completed && game.view.active_player == game.viewer_player { (actions) }
                @else if !game.completed {
                    section id="turn-composer" background=#ffffff border=(("#ded8c9", 1))
                        border-radius="16px" padding="18px" {
                        span color=#777b73 { "Your rack is ready. The board will update when your opponent plays." }
                    }
                }
                details width="100%" background=#ffffff border=(("#ded8c9", 1))
                    border-radius="14px" padding="16px" {
                    summary cursor="pointer" font-weight=bold { "Move history" }
                    (history)
                }
                (rules_component(game))
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
    let mut csrf_cookie = ResponseCookie::secure(crate::CSRF_COOKIE_NAME, csrf_token);
    csrf_cookie.http_only = false;
    csrf_cookie.secure = secure_cookies;
    ResponseMetadata {
        cookies: vec![session_cookie, csrf_cookie],
        redirect: None,
    }
}

/// Builds cookie-expiration effects for logout using the runtime transport policy.
#[must_use]
pub fn logged_out_response(secure_cookies: bool) -> ResponseMetadata {
    let mut session_cookie = ResponseCookie::expired(crate::SESSION_COOKIE_NAME);
    session_cookie.secure = secure_cookies;
    let mut csrf_cookie = ResponseCookie::expired(crate::CSRF_COOKIE_NAME);
    csrf_cookie.secure = secure_cookies;
    ResponseMetadata {
        cookies: vec![session_cookie, csrf_cookie],
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
    use words_with_spouses_game_domain::Dictionary as _;

    use super::*;
    use crate::{
        SESSION_COOKIE_NAME, accept_challenge, create_challenge, create_session, migrate_app,
        register,
    };

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
        assert!(page.contains(&format!("/register?invite={token}")));

        let login = login_page_with_invitation(None, token)
            .display_to_string(false, false)
            .expect("login renders");
        assert!(login.contains("name=\"invitation_token\""));
        assert!(login.contains(token));
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
    fn turn_draft_round_trips_and_places_selected_tiles() {
        let draft = TurnDraft {
            selected_tile: Some(7),
            selected_blank_letter: Some('Q'),
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
            assert!(page.contains("id=\"turn-feedback\""));
            assert!(!page.contains("draft_stale=1"));
            let mut stale_request = RouteRequest::from_path(
                &format!("/games/{game_id}?draft_stale=1"),
                RequestInfo::default(),
            );
            stale_request.cookies = game_request.cookies.clone();
            let stale_page = game_route(&*database, &stale_request, now)
                .await
                .display_to_string(false, false)
                .expect("stale game renders");
            assert!(stale_page.contains("The board changed while you were composing"));
            assert!(stale_page.contains("your rack order was preserved"));
            assert!(page.contains("name=\"expected_revision\" value=\"1\""));
            assert!(page.contains("turn-actions"));
            assert!(page.contains("game-awareness"));
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
            assert!(page.contains("Board key: gold tiles are committed"));
            assert!(page.contains("rack-tile"));
            assert!(page.contains("DL"));
            assert!(page.contains("TW"));
            assert!(page.contains("eligible-square-highlight"));
            assert!(page.contains("game-rules"));
            assert!(page.contains("50-point full-rack bonus"));
            assert!(page.contains("6 consecutive scoreless turns"));
            assert!(page.contains("Choose a tile, then choose its square on the board."));
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
            let (active_player, session) = if state.active_player == alice_player {
                (alice_player, alice_session)
            } else {
                let bob_player = crate::player_for_user(&*database, game_id, &bob)
                    .await
                    .expect("Bob is seated");
                (bob_player, bob_session)
            };
            let rack = &state.racks[&active_player];
            let (first, second, word) = rack
                .iter()
                .enumerate()
                .find_map(|(first_index, first)| {
                    rack.iter().enumerate().find_map(|(second_index, second)| {
                        (first_index != second_index).then(|| {
                            let first_letter = match first.face {
                                words_with_spouses_game_domain::TileFace::Letter(letter) => letter,
                                words_with_spouses_game_domain::TileFace::Blank => return None,
                            };
                            let second_letter = match second.face {
                                words_with_spouses_game_domain::TileFace::Letter(letter) => letter,
                                words_with_spouses_game_domain::TileFace::Blank => return None,
                            };
                            let word = format!("{first_letter}{second_letter}");
                            words_with_spouses_game_domain::bundled_dictionary()
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
            assert!(rendered.contains("This draft is ready to play."));
            assert!(rendered.contains("draft_stale=1"));
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
                true,
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
            let response =
                logout_route(&*database, &logout, OffsetDateTime::UNIX_EPOCH, true).await;
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
        let signed_in = authenticated_session_response("opaque-test-session", "csrf-test", true);
        assert_eq!(signed_in.redirect, None);
        assert_eq!(signed_in.cookies.len(), 2);
        assert!(signed_in.cookies[0].secure);
        assert!(signed_in.cookies[0].http_only);
        assert!(!signed_in.cookies[1].http_only);
        assert!(signed_in.cookies.iter().all(|cookie| cookie.secure));

        let development = authenticated_session_response("opaque-test-session", "csrf-test", false);
        assert!(development.cookies.iter().all(|cookie| !cookie.secure));
        assert!(development.cookies[0].http_only);
        assert!(!development.cookies[1].http_only);

        let signed_out = logged_out_response(true);
        assert_eq!(signed_out.redirect.as_deref(), Some("/login"));
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
