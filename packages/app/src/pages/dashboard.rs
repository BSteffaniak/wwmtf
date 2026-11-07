//! Signed-in dashboard page.
//!
//! Games are grouped into "Your turn", "Waiting on others", and "Finished" from the existing
//! dashboard projection. Every visual is built from `HyperChad` template attributes and the
//! shared `crate::ui` building blocks; no stylesheets or application scripts are involved.

use std::fmt::Write as _;

use hyperchad::{
    actions::ActionType,
    router::Container,
    template::{LayoutOverflow, container},
};
use time::OffsetDateTime;

use crate::{
    AuthenticatedDashboard, GameParticipantSummary, GameSummary, PendingItem, UserScoreTotals,
    theme::{self, SeatColor},
    ui::{self, ButtonSize, ButtonVariant, Tone},
};

/// Minimum card width before game cards wrap onto their own row.
const GAME_CARD_MIN_WIDTH: i32 = 280;
/// Avatar size on "Your turn" cards.
const HERO_AVATAR_SIZE: i32 = 44;
/// Avatar size on compact game rows and invitation cards.
const COMPACT_AVATAR_SIZE: i32 = 36;
/// How far stacked opponent avatars overlap each other.
const AVATAR_OVERLAP: i32 = -10;

/// Renders the complete signed-in dashboard projection.
///
/// `now` is only used to render relative timestamps ("5m ago").
#[must_use]
pub fn dashboard_page(dashboard: &AuthenticatedDashboard, now: OffsetDateTime) -> Container {
    dashboard_page_content(dashboard, unix_ms(now))
}

/// The always-visible "start a game" hero with the single game-creation path.
pub fn start_game_component() -> Container {
    let new_game = ui::link_button(
        "/lobbies/new",
        "+ New game",
        ButtonVariant::Primary,
        ButtonSize::Large,
    );
    let tiles = tile_word("GO", 34);
    one(container! {
        section id="new-game-actions" width="100%" direction="row"
            overflow-x=(LayoutOverflow::Wrap { grid: false }) align-items="center" justify-content="space-between"
            background=(theme::ACCENT) color=(theme::ON_ACCENT)
            border=((theme::ACCENT_EDGE, theme::EDGE_THIN)) border-bottom=((theme::ACCENT_EDGE, theme::EDGE_DEPTH + 1))
            border-radius=(theme::RADIUS_XL) padding-y=(theme::SPACE_6) padding-x=(theme::SPACE_6) gap=(theme::SPACE_5) {
            div flex=1 min-width=240 gap=(theme::SPACE_2) {
                (ui::eyebrow("READY FOR A WORD BATTLE?", theme::TILE_HI))
                h2 margin=0 font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XXL) font-weight=bold { "Start a game" }
                span color=(theme::SURFACE_LAVENDER) font-size=(theme::TEXT_MD) {
                    "Choose your settings, invite players by username or link, and start from the lobby. For two players or more."
                }
            }
            div align-items="center" gap=(theme::SPACE_4) {
                (tiles)
                (new_game)
            }
        }
    })
}

#[allow(clippy::too_many_lines)]
pub fn dashboard_page_content(dashboard: &AuthenticatedDashboard, now_ms: i64) -> Container {
    let user_id = dashboard.user_id.as_str();
    let dashboard_channel = format!("dashboard:{}", dashboard.user_id);
    let refresh_dashboard = ActionType::Navigate {
        url: "/".to_string(),
    };

    let mut your_turn = Vec::new();
    let mut waiting = Vec::new();
    let mut finished = Vec::new();
    for game in &dashboard.projection.games {
        match game_state(game, user_id) {
            GameState::YourTurn => your_turn.push(game),
            GameState::Waiting => waiting.push(game),
            GameState::Finished(_) => finished.push(game),
        }
    }
    let has_games = !dashboard.projection.games.is_empty();

    let header = dashboard_header(dashboard);
    let hero = start_game_component();
    let your_turn_cards = your_turn
        .iter()
        .map(|game| your_turn_card(game, now_ms))
        .collect::<Vec<_>>();
    let waiting_cards = waiting
        .iter()
        .map(|game| compact_game_card(game, user_id, now_ms))
        .collect::<Vec<_>>();
    let finished_cards = finished
        .iter()
        .map(|game| compact_game_card(game, user_id, now_ms))
        .collect::<Vec<_>>();
    let pending_cards = dashboard
        .projection
        .pending
        .iter()
        .map(|item| pending_card(item, now_ms))
        .collect::<Vec<_>>();
    let stats = stats_panel(dashboard.score_totals.as_ref());
    let profile = profile_panel(dashboard);

    container! {
        div id="app-page" font-family=(theme::FONT_UI) data-shared-state-channel=(dashboard_channel.as_str())
            direction="column" align-items="center"
            min-height="100vh" background=(theme::MENU_BG) color=(theme::INK)
            padding-y=(theme::SPACE_6) padding-x=(theme::SPACE_4) {
            span id="dashboard-live-refresh" hidden fx-global-shared-state-event=(refresh_dashboard) { }
            div id="dashboard-shell" width="100%" max-width=(theme::PAGE_WIDE) gap=(theme::SPACE_6) {
                (header)
                (hero)
                section id="active-games" data-dashboard-order="1" gap=(theme::SPACE_6) {
                    @if has_games {
                        div gap=(theme::SPACE_3) {
                            (group_header("Your turn", your_turn.len(), theme::PRIMARY, theme::ON_PRIMARY))
                            @if your_turn_cards.is_empty() {
                                div direction="row" align-items="center" gap=(theme::SPACE_3)
                                    background=(theme::PRIMARY_SOFT) color=(theme::PRIMARY_INK)
                                    border=((theme::PRIMARY, theme::EDGE_THIN)) border-radius=(theme::RADIUS_LG)
                                    padding-y=(theme::SPACE_3) padding-x=(theme::SPACE_4) font-weight=bold {
                                    (ui::tile_face_colored("✓", None, 32, MINT_TILE))
                                    span { "All caught up! Your friends are thinking about their next move." }
                                }
                            } @else {
                                div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_4) {
                                    (your_turn_cards)
                                }
                            }
                        }
                        @if !waiting_cards.is_empty() {
                            div gap=(theme::SPACE_3) {
                                (group_header("Waiting on others", waiting.len(), theme::SURFACE_LAVENDER, theme::ACCENT_EDGE))
                                div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_3) {
                                    (waiting_cards)
                                }
                            }
                        }
                        @if !finished_cards.is_empty() {
                            details id="finished-games" gap=(theme::SPACE_3) {
                                summary cursor=pointer direction="row" align-items="center" gap=(theme::SPACE_2)
                                    padding-y=(theme::SPACE_2) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold
                                    color=(theme::INK_MUTED) {
                                    "▸ Finished games"
                                    (ui::pill(&finished.len().to_string(), theme::MENU_BG_DEEP, theme::INK_MUTED))
                                }
                                div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_3) {
                                    (finished_cards)
                                }
                            }
                        }
                    } @else {
                        (first_game_empty_state())
                    }
                }
                section id="pending-games" data-dashboard-order="2" gap=(theme::SPACE_3) {
                    div direction="row" align-items="end" justify-content="space-between"
                        overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_2) {
                        (section_heading(
                            "Challenges & invitations",
                            Some("Invitations are private and single-use. Old invite secrets cannot be displayed again."),
                        ))
                    }
                    @if pending_cards.is_empty() {
                        div direction="row" align-items="center" gap=(theme::SPACE_3)
                            background=(theme::SURFACE_ALT) border=((theme::BORDER, theme::EDGE_THIN))
                            border-radius=(theme::RADIUS_LG) padding-y=(theme::SPACE_3) padding-x=(theme::SPACE_4) {
                            (ui::tile_face_colored("✉", None, 32, LAVENDER_TILE))
                            span color=(theme::INK_MUTED) { "No challenges or invitations are waiting. Start a game to invite your friends!" }
                        }
                    } @else {
                        div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_3) {
                            (pending_cards)
                        }
                    }
                }
                main id="dashboard-main" data-dashboard-order="3" direction="row"
                    overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_4) align-items="start" {
                    (stats)
                    (profile)
                }
            }
        }
    }
    .into()
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

fn dashboard_header(dashboard: &AuthenticatedDashboard) -> Container {
    let username = dashboard.username.as_str();
    let display_name = dashboard.display_name.as_str();
    let seat = theme::seat_color_for(&dashboard.user_id);
    let avatar = profile_avatar(display_name, dashboard.avatar_url.as_deref(), seat);
    let sign_out = ui::link_button(
        "/logout",
        "Sign\u{a0}out",
        ButtonVariant::Secondary,
        ButtonSize::Small,
    );
    one(container! {
        header id="dashboard-header" direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false })
            justify-content="space-between" align-items="center"
            background=(theme::SURFACE) border=((theme::BORDER, theme::EDGE_THIN)) border-bottom=((theme::BORDER_STRONG, theme::EDGE_DEPTH))
            border-radius=(theme::RADIUS_XL) padding-y=(theme::SPACE_4) padding-x=(theme::SPACE_5) gap=(theme::SPACE_4) {
            (ui::brand_logo(30, false))
            div direction="row" align-items="center" gap=(theme::SPACE_3) {
                (avatar)
                div gap=2 overflow-wrap="anywhere" {
                    span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold { "Hey " (display_name) "!" }
                    span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) { "Signed in as " (display_name) }
                    @if display_name != username {
                        span color=(theme::ACCENT) font-size=(theme::TEXT_SM) font-weight=bold { "@" (username) }
                    }
                }
                (sign_out)
            }
        }
    })
}

/// The viewer's own avatar. The image keeps the `Profile avatar` alt text that browser
/// acceptance uses to detect a mirrored or removed picture; the fallback is a seat-colored
/// initial without an image.
fn profile_avatar(display_name: &str, avatar_url: Option<&str>, seat: SeatColor) -> Container {
    let size = 52;
    avatar_url.map_or_else(
        || ui::avatar(display_name, None, seat, size),
        |avatar_url| {
            one(container! {
                image src=(avatar_url) alt="Profile avatar" width=(size) height=(size) min-width=(size)
                    border-radius=(theme::RADIUS_PILL) border=((seat.edge, 3));
            })
        },
    )
}

// ---------------------------------------------------------------------------
// Games
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Won,
    Lost,
    Tie,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GameState {
    YourTurn,
    Waiting,
    Finished(Outcome),
}

fn game_state(game: &GameSummary, user_id: &str) -> GameState {
    if game.status == "COMPLETED" {
        let viewer = game
            .participants
            .iter()
            .find(|participant| participant.viewer);
        GameState::Finished(
            match viewer.and_then(|participant| participant.outcome.as_deref()) {
                Some("WIN") => Outcome::Won,
                Some("TIE") => Outcome::Tie,
                Some("LOSS") => Outcome::Lost,
                _ => Outcome::Complete,
            },
        )
    } else if game.active_player_user_id.as_deref() == Some(user_id) {
        GameState::YourTurn
    } else {
        GameState::Waiting
    }
}

fn status_pill(state: GameState) -> Container {
    let (label, tone) = match state {
        GameState::YourTurn => ("Your turn", Tone::Success),
        GameState::Waiting => ("Waiting", Tone::Accent),
        GameState::Finished(Outcome::Won) => ("You won ★", Tone::Warning),
        GameState::Finished(Outcome::Lost) => ("You lost", Tone::Danger),
        GameState::Finished(Outcome::Tie) => ("Tie", Tone::Info),
        GameState::Finished(Outcome::Complete) => ("Game complete", Tone::Neutral),
    };
    ui::badge(label, tone)
}

fn opponents(game: &GameSummary) -> Vec<&GameParticipantSummary> {
    game.participants
        .iter()
        .filter(|participant| !participant.viewer)
        .collect()
}

fn opponent_names(game: &GameSummary) -> String {
    let names = opponents(game)
        .iter()
        .map(|participant| participant.display_name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if names.is_empty() {
        "yourself".to_string()
    } else {
        names
    }
}

/// "You 12 – 8 Bob – 3 Carol".
fn score_line(game: &GameSummary) -> String {
    let viewer_score = game
        .participants
        .iter()
        .find(|participant| participant.viewer)
        .map_or(0, |participant| participant.score);
    let mut line = format!("You {viewer_score}");
    for participant in opponents(game) {
        let _ = write!(
            line,
            " – {} {}",
            participant.score, participant.display_name
        );
    }
    line
}

/// Overlapping opponent avatars, colored by seat when no picture is available.
fn avatar_stack(game: &GameSummary, size: i32) -> Container {
    let avatars = opponents(game)
        .into_iter()
        .enumerate()
        .map(|(index, participant)| {
            let mut avatar = ui::avatar(
                &participant.display_name,
                participant.avatar_url.as_deref(),
                theme::seat_color(participant.seat),
                size,
            );
            if index > 0 {
                avatar.margin_left = Some(hyperchad::transformer::Number::Integer(i64::from(
                    AVATAR_OVERLAP,
                )));
            }
            avatar
        })
        .collect::<Vec<_>>();
    one(container! {
        div direction="row" align-items="center" min-width=(size) { (avatars) }
    })
}

/// A big, colorful, fully clickable card for games waiting on the viewer.
fn your_turn_card(game: &GameSummary, now_ms: i64) -> Container {
    let href = format!("/games/{}", game.game_id);
    let id = format!("game-summary-{}", game.game_id);
    let when = relative_time(game.updated_at_ms, now_ms);
    one(container! {
        anchor id=(id) class="game-summary" href=(href) flex=1 min-width=(GAME_CARD_MIN_WIDTH)
            direction="column" gap=(theme::SPACE_3) text-decoration="none" color=(theme::INK)
            background=(theme::SUNNY_SOFT) border=((theme::TILE_EDGE, theme::EDGE_THIN)) border-bottom=((theme::TILE_EDGE, theme::EDGE_DEPTH + 1))
            border-radius=(theme::RADIUS_XL) padding=(theme::SPACE_5) cursor=pointer
            fx-hover=(ActionType::set_background_self(theme::TILE_HI)) {
            div direction="row" align-items="center" justify-content="space-between" gap=(theme::SPACE_3) {
                div direction="row" align-items="center" gap=(theme::SPACE_3) {
                    (avatar_stack(game, HERO_AVATAR_SIZE))
                    div gap=2 overflow-wrap="anywhere" {
                        span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold { "Game with " (opponent_names(game)) }
                        span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) { (when) }
                    }
                }
                (status_pill(GameState::YourTurn))
            }
            span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XL) font-weight=bold color=(theme::ON_SUNNY) { (score_line(game)) }
            div direction="row" align-items="center" justify-content="space-between" gap=(theme::SPACE_3)
                overflow-x=(LayoutOverflow::Wrap { grid: false }) {
                span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) { (game.latest_activity.as_str()) }
                span background=(theme::PRIMARY) color=(theme::ON_PRIMARY)
                    border=((theme::PRIMARY_EDGE, theme::EDGE_THIN)) border-bottom=((theme::PRIMARY_EDGE, theme::EDGE_DEPTH - 1))
                    border-radius=(theme::RADIUS_PILL) padding-y=(theme::SPACE_2) padding-x=(theme::SPACE_4)
                    font-weight=bold white-space="preserve" { "Play your move →" }
            }
        }
    })
}

/// A compact clickable card for games waiting on someone else or already finished.
fn compact_game_card(game: &GameSummary, user_id: &str, now_ms: i64) -> Container {
    let href = format!("/games/{}", game.game_id);
    let id = format!("game-summary-{}", game.game_id);
    let state = game_state(game, user_id);
    let when = relative_time(game.updated_at_ms, now_ms);
    let (edge, hover) = match state {
        GameState::Finished(_) => (theme::BORDER, theme::SURFACE_ALT),
        _ => (theme::SECONDARY_EDGE, theme::SECONDARY_HI),
    };
    one(container! {
        anchor id=(id) class="game-summary" href=(href) flex=1 min-width=(GAME_CARD_MIN_WIDTH)
            direction="column" gap=(theme::SPACE_2) text-decoration="none" color=(theme::INK)
            background=(theme::SURFACE) border=((edge, theme::EDGE_THIN)) border-bottom=((edge, theme::EDGE_DEPTH))
            border-radius=(theme::RADIUS_LG) padding-y=(theme::SPACE_3) padding-x=(theme::SPACE_4) cursor=pointer
            fx-hover=(ActionType::set_background_self(hover)) {
            div direction="row" align-items="center" justify-content="space-between" gap=(theme::SPACE_3) {
                div direction="row" align-items="center" gap=(theme::SPACE_3) {
                    (avatar_stack(game, COMPACT_AVATAR_SIZE))
                    div gap=2 overflow-wrap="anywhere" {
                        span font-weight=bold { "Game with " (opponent_names(game)) }
                        span color=(theme::INK_SOFT) font-size=(theme::TEXT_SM) { (score_line(game)) }
                    }
                }
                (status_pill(state))
            }
            div direction="row" justify-content="space-between" gap=(theme::SPACE_2)
                overflow-x=(LayoutOverflow::Wrap { grid: false }) font-size=(theme::TEXT_XS) color=(theme::INK_SOFT) {
                span { (game.latest_activity.as_str()) }
                span font-weight=bold { (when) }
            }
        }
    })
}

fn group_header(title: &str, count: usize, fill: &str, ink: &str) -> Container {
    one(container! {
        div direction="row" align-items="center" gap=(theme::SPACE_2) {
            h2 margin=0 font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XL) font-weight=bold { (title) }
            (ui::pill(&count.to_string(), fill, ink))
        }
    })
}

/// Section title with a supporting line; like `ui::section_header` but without the browser's
/// default heading margins so the subtitle sits snugly underneath.
fn section_heading(title: &str, subtitle: Option<&str>) -> Container {
    one(container! {
        div gap=(theme::SPACE_1) {
            h2 margin=0 color=(theme::INK) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XL) font-weight=bold { (title) }
            @if let Some(subtitle) = subtitle {
                span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) { (subtitle) }
            }
        }
    })
}

fn first_game_empty_state() -> Container {
    let new_game = ui::link_button(
        "/lobbies/new",
        "Create your first game",
        ButtonVariant::Primary,
        ButtonSize::Medium,
    );
    one(container! {
        div align-items="center" gap=(theme::SPACE_3) text-align="center"
            background=(theme::SURFACE) border=((theme::BORDER, theme::EDGE_THIN)) border-bottom=((theme::BORDER_STRONG, theme::EDGE_DEPTH))
            border-radius=(theme::RADIUS_XL) padding-y=(theme::SPACE_8) padding-x=(theme::SPACE_5) {
            (tile_word("PLAY", 48))
            h3 margin=0 font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XL) font-weight=bold { "Your first game starts here" }
            span color=(theme::INK_MUTED) max-width=420 {
                "Create a game, invite friends by username or private link, and it will show up right here."
            }
            (new_game)
        }
    })
}

// ---------------------------------------------------------------------------
// Challenges and invitations
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
fn pending_card(item: &PendingItem, now_ms: i64) -> Container {
    let counterparty = item
        .counterparty_display_name
        .as_deref()
        .or(item.counterparty_username.as_deref())
        .unwrap_or("Private invite");
    let heading = if item.kind == "LOBBY" {
        "Game lobby".to_string()
    } else if item.kind == "LOBBY_INVITATION" {
        format!("Lobby invite from {counterparty}")
    } else if item.kind == "CHALLENGE" && item.direction == "INCOMING" {
        format!("Challenge from {counterparty}")
    } else if item.kind == "CHALLENGE" {
        format!("Challenge sent to {counterparty}")
    } else {
        "Active private invitation".to_string()
    };
    let incoming = item.direction == "INCOMING";
    let (edge, glyph, glyph_colors) = if incoming {
        (theme::PRIMARY, "!", MINT_TILE)
    } else if item.kind == "LOBBY" {
        (theme::SECONDARY_EDGE, "L", LAVENDER_TILE)
    } else {
        (theme::SECONDARY_EDGE, "→", LAVENDER_TILE)
    };
    let seat = theme::seat_color_for(
        item.counterparty_user_id
            .as_deref()
            .unwrap_or(item.id.as_str()),
    );
    let identity = match (
        item.counterparty_avatar_url.as_deref(),
        item.counterparty_display_name.as_deref(),
    ) {
        (Some(avatar_url), _) => one(container! {
            image src=(avatar_url) alt="Challenge profile avatar"
                width=(COMPACT_AVATAR_SIZE) height=(COMPACT_AVATAR_SIZE) min-width=(COMPACT_AVATAR_SIZE)
                border-radius=(theme::RADIUS_PILL) border=((seat.edge, theme::EDGE_THIN));
        }),
        (None, Some(display_name)) => ui::avatar(display_name, None, seat, COMPACT_AVATAR_SIZE),
        (None, None) => ui::tile_face_colored(glyph, None, COMPACT_AVATAR_SIZE, glyph_colors),
    };
    let when = relative_time(item.created_at_ms, now_ms);
    let lobby_href = format!("/lobbies/{}", item.id);
    let lobby_action = format!("/lobbies/{}/action", item.id);
    one(container! {
        div id=(format!("pending-item-{}", item.id)) class="pending-item" data-direction=(item.direction.as_str())
            flex=1 min-width=(GAME_CARD_MIN_WIDTH) gap=(theme::SPACE_3)
            background=(theme::SURFACE) border=((edge, theme::EDGE_THIN)) border-bottom=((edge, theme::EDGE_DEPTH))
            border-radius=(theme::RADIUS_LG) padding-y=(theme::SPACE_3) padding-x=(theme::SPACE_4) {
            div direction="row" align-items="center" gap=(theme::SPACE_3) {
                (identity)
                div gap=2 flex=1 overflow-wrap="anywhere" {
                    span font-weight=bold { (heading) }
                    @if let (Some(display_name), Some(handle)) = (
                        item.counterparty_display_name.as_deref(),
                        item.counterparty_username.as_deref(),
                    ) && display_name != handle {
                        span color=(theme::ACCENT) font-size=(theme::TEXT_SM) font-weight=bold { "@" (handle) }
                    }
                    @if item.kind == "INVITATION" {
                        span color=(theme::INK_SOFT) font-size=(theme::TEXT_SM) { "Link hidden after creation for security." }
                    }
                    span color=(theme::INK_SOFT) font-size=(theme::TEXT_XS) { (when) }
                }
            }
            div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_2) {
                @if item.kind == "LOBBY" {
                    (ui::link_button(&lobby_href, "Open lobby →", ButtonVariant::Accent, ButtonSize::Medium))
                } @else if item.kind == "LOBBY_INVITATION" {
                    form hx-post=(lobby_action.as_str()) hx-target="#app-page" {
                        input type=hidden name="action" value="ACCEPT_INVITE";
                        (ui::submit_button("✓ Accept invite", ButtonVariant::Primary, ButtonSize::Medium))
                    }
                    form hx-post=(lobby_action.as_str()) hx-target="#app-page" {
                        input type=hidden name="action" value="DECLINE_INVITE";
                        (ui::submit_button("Decline", ButtonVariant::Secondary, ButtonSize::Medium))
                    }
                } @else if item.kind == "CHALLENGE" && incoming {
                    form hx-post="/dashboard/action" hx-target="#app-page" {
                        input type=hidden name="action" value="ACCEPT_CHALLENGE";
                        input type=hidden name="challenge_id" value=(item.id.as_str());
                        (ui::submit_button("✓ Accept", ButtonVariant::Primary, ButtonSize::Medium))
                    }
                    form hx-post="/dashboard/action" hx-target="#app-page" {
                        input type=hidden name="action" value="DECLINE_CHALLENGE";
                        input type=hidden name="challenge_id" value=(item.id.as_str());
                        (ui::submit_button("Decline", ButtonVariant::Secondary, ButtonSize::Medium))
                    }
                } @else if item.kind == "CHALLENGE" {
                    form hx-post="/dashboard/action" hx-target="#app-page" {
                        input type=hidden name="action" value="CANCEL_CHALLENGE";
                        input type=hidden name="challenge_id" value=(item.id.as_str());
                        (ui::submit_button("Cancel", ButtonVariant::Secondary, ButtonSize::Medium))
                    }
                } @else {
                    form hx-post="/dashboard/action" hx-target="#app-page" {
                        input type=hidden name="action" value="REVOKE_INVITATION";
                        input type=hidden name="invitation_id" value=(item.id.as_str());
                        (ui::submit_button("Revoke", ButtonVariant::Danger, ButtonSize::Medium))
                    }
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Stats and profile
// ---------------------------------------------------------------------------

fn stats_panel(totals: Option<&UserScoreTotals>) -> Container {
    let (completed, wins, ties, points) = totals.map_or((0, 0, 0, 0), |totals| {
        (
            totals.completed_games,
            totals.wins,
            totals.ties,
            totals.total_score,
        )
    });
    let subtitle = score_totals_label(totals);
    one(container! {
        section id="score-totals" flex=1 min-width=(GAME_CARD_MIN_WIDTH) gap=(theme::SPACE_4)
            background=(theme::SURFACE) border=((theme::BORDER, theme::EDGE_THIN)) border-bottom=((theme::BORDER_STRONG, theme::EDGE_DEPTH))
            border-radius=(theme::RADIUS_XL) padding=(theme::SPACE_5) {
            (section_heading("Score history", Some(&subtitle)))
            // Pairs keep the tiles in a tidy 2 × 2 grid on phones and one row on wide panels.
            div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_2) {
                div direction="row" flex=1 min-width=200 gap=(theme::SPACE_2) {
                    (ui::stat_tile("COMPLETED", &completed.to_string(), theme::SURFACE_LAVENDER, theme::ACCENT_EDGE))
                    (ui::stat_tile("WINS", &wins.to_string(), theme::PRIMARY_SOFT, theme::PRIMARY_INK))
                }
                div direction="row" flex=1 min-width=200 gap=(theme::SPACE_2) {
                    (ui::stat_tile("TIES", &ties.to_string(), theme::INFO_SOFT, theme::INK))
                    (ui::stat_tile("POINTS", &points.to_string(), theme::SUNNY_SOFT, theme::ON_SUNNY))
                }
            }
        }
    })
}

fn profile_panel(dashboard: &AuthenticatedDashboard) -> Container {
    let display_name = dashboard.display_name.as_str();
    one(container! {
        details id="profile-settings" flex=1 min-width=(GAME_CARD_MIN_WIDTH)
            background=(theme::SURFACE) border=((theme::BORDER, theme::EDGE_THIN)) border-bottom=((theme::BORDER_STRONG, theme::EDGE_DEPTH))
            border-radius=(theme::RADIUS_XL) padding=(theme::SPACE_5) {
            summary cursor=pointer direction="row" align-items="center" gap=(theme::SPACE_3) {
                (ui::tile_face_colored("⚙", None, 36, LAVENDER_TILE))
                div gap=2 {
                    span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XL) font-weight=bold { "Profile" }
                    span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) { "Change your name and picture" }
                }
            }
            div padding-top=(theme::SPACE_4) gap=(theme::SPACE_4) {
                form method="post" action="/dashboard/action" gap=(theme::SPACE_2) {
                    input type="hidden" name="action" value="SET_DISPLAY_NAME";
                    span font-weight=bold font-size=(theme::TEXT_SM) color=(theme::INK_MUTED) { "Display name" }
                    input type="text" id="profile-display-name" name="display_name" value=(display_name)
                        required=true padding-y=(theme::SPACE_2) padding-x=(theme::SPACE_3)
                        font-size=(theme::TEXT_MD) color=(theme::INK) background=(theme::SURFACE_ALT)
                        border=((theme::INPUT_BORDER, theme::EDGE_THIN)) border-radius=(theme::RADIUS_MD);
                    (ui::submit_button("Save display name", ButtonVariant::Primary, ButtonSize::Medium))
                }
                form method="post" action="/dashboard/action" {
                    input type="hidden" name="action" value="USE_GOOGLE_NAME";
                    (ui::submit_button("Use Google name again", ButtonVariant::Secondary, ButtonSize::Small))
                }
                div height=0 border-top=((theme::BORDER, theme::EDGE_THIN)) { }
                form method="post" action="/profile/avatar" enctype="multipart/form-data" gap=(theme::SPACE_2) {
                    span font-weight=bold font-size=(theme::TEXT_SM) color=(theme::INK_MUTED) { "Upload a profile picture" }
                    input type="file" name="avatar" padding=(theme::SPACE_2) background=(theme::SURFACE_ALT)
                        border=((theme::INPUT_BORDER, theme::EDGE_THIN)) border-radius=(theme::RADIUS_MD);
                    (ui::submit_button("Upload picture", ButtonVariant::Accent, ButtonSize::Medium))
                }
                div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_2) {
                    form method="post" action="/dashboard/action" {
                        input type="hidden" name="action" value="USE_GOOGLE_AVATAR";
                        (ui::submit_button("Use Google photo again", ButtonVariant::Secondary, ButtonSize::Small))
                    }
                    form method="post" action="/dashboard/action" {
                        input type="hidden" name="action" value="REMOVE_AVATAR";
                        (ui::submit_button("Remove profile picture", ButtonVariant::Danger, ButtonSize::Small))
                    }
                }
            }
        }
    })
}

pub fn score_totals_label(totals: Option<&UserScoreTotals>) -> String {
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

// ---------------------------------------------------------------------------
// Small local helpers (candidates for promotion into `crate::ui`)
// ---------------------------------------------------------------------------

/// Mint tile colors for celebratory glyphs.
const MINT_TILE: ui::TileColors = ui::TileColors {
    face: theme::PRIMARY_SOFT,
    edge: theme::PRIMARY_EDGE,
    ink: theme::PRIMARY_INK,
};

/// Lavender tile colors for quiet glyphs.
const LAVENDER_TILE: ui::TileColors = ui::TileColors {
    face: theme::SURFACE_LAVENDER,
    edge: theme::SECONDARY_EDGE,
    ink: theme::ACCENT_EDGE,
};

/// Unwraps a single-element `container!` result so helpers do not add wrapper `div`s.
fn one(mut items: Vec<Container>) -> Container {
    if items.len() == 1 {
        items.remove(0)
    } else {
        Container::from(items)
    }
}

/// Spells a word in sunny tiles with standard letter points and a playful bob.
fn tile_word(word: &str, size: i32) -> Container {
    let tiles = word
        .chars()
        .enumerate()
        .map(|(index, letter)| {
            let mut tile = ui::tile_face(&letter.to_string(), Some(letter_points(letter)), size);
            if index % 2 == 1 {
                tile.translate_y = Some(hyperchad::transformer::Number::Integer(i64::from(
                    size / 10,
                )));
            }
            tile
        })
        .collect::<Vec<_>>();
    one(container! {
        div direction="row" gap=(size / 8) align-items="start" { (tiles) }
    })
}

/// Classic English letter values for decorative tiles only (never used for scoring).
const fn letter_points(letter: char) -> u8 {
    match letter.to_ascii_uppercase() {
        'D' | 'G' => 2,
        'B' | 'C' | 'M' | 'P' => 3,
        'F' | 'H' | 'V' | 'W' | 'Y' => 4,
        'K' => 5,
        'J' | 'X' => 8,
        'Q' | 'Z' => 10,
        _ => 1,
    }
}

fn unix_ms(now: OffsetDateTime) -> i64 {
    i64::try_from(now.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
}

/// Compact relative time such as "just now", "5m ago", "2h ago", or "3d ago".
pub fn relative_time(then_ms: i64, now_ms: i64) -> String {
    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;
    let elapsed = now_ms.saturating_sub(then_ms);
    if elapsed < MINUTE {
        "just now".to_string()
    } else if elapsed < HOUR {
        format!("{}m ago", elapsed / MINUTE)
    } else if elapsed < DAY {
        format!("{}h ago", elapsed / HOUR)
    } else if elapsed < MONTH {
        format!("{}d ago", elapsed / DAY)
    } else if elapsed < YEAR {
        format!("{}mo ago", elapsed / MONTH)
    } else {
        format!("{}y ago", elapsed / YEAR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DashboardProjection;

    #[test]
    fn dashboard_has_one_game_creation_path() {
        let page = start_game_component().to_string();
        assert!(page.contains("/lobbies/new"));
        assert!(page.contains("invite players by username"));
        assert!(!page.contains("CHALLENGE"));
        assert!(!page.contains("CREATE_INVITATION"));
    }

    #[test]
    fn relative_time_uses_compact_units() {
        let now = 1_000_000_000_000;
        assert_eq!(relative_time(now - 5_000, now), "just now");
        assert_eq!(relative_time(now + 5_000, now), "just now");
        assert_eq!(relative_time(now - 5 * 60_000, now), "5m ago");
        assert_eq!(relative_time(now - 2 * 3_600_000, now), "2h ago");
        assert_eq!(relative_time(now - 3 * 86_400_000, now), "3d ago");
        assert_eq!(relative_time(now - 65 * 86_400_000, now), "2mo ago");
        assert_eq!(relative_time(now - 800 * 86_400_000, now), "2y ago");
    }

    fn participant(
        user_id: &str,
        name: &str,
        seat: usize,
        score: u32,
        viewer: bool,
        outcome: Option<&str>,
    ) -> GameParticipantSummary {
        GameParticipantSummary {
            user_id: user_id.to_string(),
            username: name.to_lowercase(),
            display_name: name.to_string(),
            avatar_url: None,
            seat,
            score,
            viewer,
            active: true,
            current_turn: false,
            outcome: outcome.map(ToOwned::to_owned),
        }
    }

    fn game(
        game_id: &str,
        status: &str,
        active: Option<&str>,
        updated_at_ms: i64,
        outcome: Option<&str>,
    ) -> GameSummary {
        GameSummary {
            game_id: game_id.to_string(),
            status: status.to_string(),
            active_player_user_id: active.map(ToOwned::to_owned),
            canonical_revision: 3,
            last_score: None,
            updated_at_ms,
            participants: vec![
                participant("user-a", "Alice", 0, 12, true, outcome),
                participant("user-b", "Bob", 1, 8, false, None),
            ],
            latest_activity: "Bob played for 8 points".to_string(),
        }
    }

    fn fixture(games: Vec<GameSummary>, pending: Vec<PendingItem>) -> AuthenticatedDashboard {
        AuthenticatedDashboard {
            user_id: "user-a".to_string(),
            username: "alice".to_string(),
            display_name: "Alice".to_string(),
            avatar_url: None,
            projection: DashboardProjection { pending, games },
            score_totals: Some(UserScoreTotals {
                user_id: "user-a".to_string(),
                completed_games: 4,
                wins: 3,
                ties: 1,
                total_score: 812,
            }),
        }
    }

    #[test]
    fn dashboard_groups_games_by_turn_and_outcome() {
        let now = 10_000_000_000;
        let dashboard = fixture(
            vec![
                game(
                    "g-wait",
                    "ACTIVE",
                    Some("user-b"),
                    now - 2 * 3_600_000,
                    None,
                ),
                game("g-mine", "ACTIVE", Some("user-a"), now - 5 * 60_000, None),
                game(
                    "g-won",
                    "COMPLETED",
                    None,
                    now - 3 * 86_400_000,
                    Some("WIN"),
                ),
            ],
            Vec::new(),
        );
        let page = dashboard_page_content(&dashboard, now)
            .display_to_string(false, false)
            .expect("dashboard renders");

        let header = page.find("id=\"dashboard-header\"").expect("header");
        let hero = page.find("id=\"new-game-actions\"").expect("hero");
        let mine = page.find("id=\"game-summary-g-mine\"").expect("your turn");
        let waiting = page.find("id=\"game-summary-g-wait\"").expect("waiting");
        let won = page.find("id=\"game-summary-g-won\"").expect("finished");
        assert!(header < hero && hero < mine && mine < waiting && waiting < won);

        assert!(page.contains("href=\"/games/g-mine\""));
        assert!(page.contains("class=\"game-summary\""));
        assert!(page.contains("Game with Bob"));
        assert!(page.contains("You 12 – 8 Bob"));
        assert!(page.contains("Play your move"));
        assert!(page.contains("Waiting on others"));
        assert!(page.contains("Finished games"));
        assert!(page.contains("You won"));
        assert!(page.contains("5m ago"));
        assert!(page.contains("2h ago"));
        assert!(page.contains("3d ago"));
        assert!(page.contains("Signed in as Alice"));
        assert!(page.contains("@alice"));
        assert!(page.contains("id=\"dashboard-live-refresh\""));
        assert!(page.contains("data-shared-state-channel=\"dashboard:user-a\""));
        assert!(page.contains("812"));
        // Without a picture the header shows an initial, never the acceptance image hook.
        assert!(!page.contains("alt=\"Profile avatar\""));
    }

    #[test]
    fn dashboard_empty_state_invites_a_first_game() {
        let page = dashboard_page_content(&fixture(Vec::new(), Vec::new()), 0)
            .display_to_string(false, false)
            .expect("dashboard renders");
        assert!(page.contains("Your first game starts here"));
        assert!(page.contains("No challenges or invitations are waiting"));
        assert!(!page.contains("class=\"game-summary\""));
    }

    #[test]
    fn pending_items_keep_their_forms_and_hooks() {
        let pending = vec![
            PendingItem {
                id: "c-in".to_string(),
                kind: "CHALLENGE".to_string(),
                direction: "INCOMING".to_string(),
                counterparty_user_id: Some("user-b".to_string()),
                counterparty_username: Some("bob".to_string()),
                counterparty_display_name: Some("Bob".to_string()),
                counterparty_avatar_url: None,
                created_at_ms: 0,
            },
            PendingItem {
                id: "l-1".to_string(),
                kind: "LOBBY".to_string(),
                direction: "OUTGOING".to_string(),
                counterparty_user_id: None,
                counterparty_username: None,
                counterparty_display_name: None,
                counterparty_avatar_url: None,
                created_at_ms: 0,
            },
        ];
        let mut dashboard = fixture(Vec::new(), pending);
        dashboard.avatar_url = Some("/profile/avatar/user-a".to_string());
        let page = dashboard_page_content(&dashboard, 0)
            .display_to_string(false, false)
            .expect("dashboard renders");
        assert!(page.contains("id=\"pending-item-c-in\""));
        assert!(page.contains("class=\"pending-item\""));
        assert!(page.contains("data-direction=\"INCOMING\""));
        assert!(page.contains("value=\"ACCEPT_CHALLENGE\""));
        assert!(page.contains("value=\"DECLINE_CHALLENGE\""));
        assert!(page.contains("name=\"challenge_id\" value=\"c-in\""));
        assert!(page.contains("Challenge from Bob"));
        assert!(page.contains("href=\"/lobbies/l-1\""));
        assert!(page.contains("Open lobby"));
        assert!(page.contains("alt=\"Profile avatar\""));
    }
}
