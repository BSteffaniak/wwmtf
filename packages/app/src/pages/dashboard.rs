//! Signed-in dashboard page.

use hyperchad::{
    actions::ActionType,
    router::Container,
    template::{LayoutOverflow, container},
};

use crate::{AuthenticatedDashboard, UserScoreTotals, theme};

/// Renders the complete signed-in dashboard projection.
#[must_use]
pub fn dashboard_page(dashboard: &AuthenticatedDashboard) -> Container {
    dashboard_page_content(dashboard)
}

pub fn start_game_component() -> Container {
    container! {
        section id="new-game-actions" width="100%" gap="14px" {
            h2 { "Start a game" }
            span color=(theme::INK_MUTED) { "Choose your settings, invite players by username or link, and start from the lobby. For two players or more." }
            anchor href="/lobbies/new" background=(theme::PRIMARY) color=(theme::ON_PRIMARY) padding-y=12 padding-x=16
                border=((theme::PRIMARY_EDGE, 1)) border-radius="10px" { "Create game" }
        }
    }
    .into()
}

#[allow(clippy::too_many_lines, clippy::large_stack_frames)]
pub fn dashboard_page_content(dashboard: &AuthenticatedDashboard) -> Container {
    let user_id = dashboard.user_id.as_str();
    let username = dashboard.username.as_str();
    let display_name = dashboard.display_name.as_str();
    let avatar_url = dashboard.avatar_url.as_deref();
    let totals = score_totals_label(dashboard.score_totals.as_ref());
    let dashboard_channel = format!("dashboard:{}", dashboard.user_id);
    let refresh_dashboard = ActionType::Navigate {
        url: "/".to_string(),
    };
    container! {
        div id="app-page" font-family=(theme::FONT_UI) data-shared-state-channel=(dashboard_channel.as_str())
            direction="column" align-items="center"
            min-height="100vh" background=(theme::MENU_BG) color=(theme::INK)
            padding-y=24 padding-x=16 {
            span id="dashboard-live-refresh" hidden fx-global-shared-state-event=(refresh_dashboard) { }
            div id="dashboard-shell" width="100%" max-width="1080px" gap="28px" {
                header id="dashboard-header" direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) justify-content="space-between" align-items="center"
                    background=(theme::WHITE) border=((theme::BORDER, 1)) border-radius="18px" padding-y=22 padding-x=26 gap="16px" {
                    div direction="row" align-items="center" gap="12px" {
                        @if let Some(avatar_url) = avatar_url {
                            image src=(avatar_url) alt="Profile avatar" width="48" height="48" border-radius="999px";
                        }
                        div gap="4px" {
                            span color=(theme::PRIMARY_INK) font-weight=bold { "WORDS WITH MORE THAN FRIENDS" }
                            h1 { "Your games" }
                            span color=(theme::INK_MUTED) { "Signed in as " (display_name) }
                            @if display_name != username {
                                span color=(theme::INK_SOFT) { "@" (username) }
                            }
                        }
                    }
                    anchor href="/logout" color=(theme::LINK) { "Sign out" }
                }
                section id="active-games" data-dashboard-order="1" background=(theme::WHITE) border=((theme::BORDER, 1))
                    border-radius="16px" padding="24px" gap="4px" {
                    h2 { "Games" }
                    @if dashboard.projection.games.is_empty() {
                        div gap="8px" {
                            h3 { "Your first game starts here" }
                            span color=(theme::INK_SOFT) { "Challenge someone by username or create a private link to send them." }
                            anchor href="#new-game-actions" color=(theme::LINK) font-weight=bold { "Start a private game" }
                        }
                    }
                    @for game in &dashboard.projection.games {
                        @let href = format!("/games/{}", game.game_id);
                        @let viewer = game.participants.iter().find(|participant| participant.viewer);
                        @let others = game.participants.iter().filter(|participant| !participant.viewer).collect::<Vec<_>>();
                        @let state = if game.status == "COMPLETED" {
                            match viewer.and_then(|participant| participant.outcome.as_deref()) {
                                Some("WIN") => "You won",
                                Some("TIE") => "Tie game",
                                Some("LOSS") => "You lost",
                                _ => "Game complete",
                            }
                        } else if game.active_player_user_id.as_deref() == Some(user_id) {
                            "Your turn"
                        } else {
                            "Waiting for another player"
                        };
                        @let opponents = others.iter().map(|participant| participant.display_name.as_str()).collect::<Vec<_>>().join(", ");
                        div id=(format!("game-summary-{}", game.game_id)) class="game-summary"
                            direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) justify-content="space-between"
                            border-bottom=((theme::BORDER, 1)) padding-y=16 padding-x=4 gap="12px" {
                            div gap="3px" {
                                anchor href=(href) color=(theme::LINK) font-weight=bold { "Game with " (opponents) }
                                span color=(theme::PRIMARY_INK) font-weight=bold { (state) }
                                span color=(theme::INK_SOFT) {
                                    "You " (viewer.map_or(0, |participant| participant.score))
                                    @for participant in &others {
                                        " – " (participant.score) " " (participant.display_name.as_str())
                                    }
                                }
                            }
                            span color=(theme::INK_SOFT) { (game.latest_activity.as_str()) }
                        }
                    }
                }
                section id="pending-games" data-dashboard-order="2" background=(theme::WHITE) border=((theme::BORDER, 1))
                    border-radius="16px" padding="24px" gap="4px" {
                    div gap="5px" {
                        h2 { "Challenges & invitations" }
                        span color=(theme::INK_MUTED) { "Invitations are private and single-use. Old invite secrets cannot be displayed again." }
                    }
                    @if dashboard.projection.pending.is_empty() {
                        div gap="6px" {
                            span color=(theme::INK_SOFT) { "No challenges or invitations are waiting." }
                            anchor href="#new-game-actions" color=(theme::LINK) font-weight=bold { "Start a game" }
                        }
                    }
                    @for item in &dashboard.projection.pending {
                        @let counterparty = item.counterparty_display_name.as_deref()
                            .or(item.counterparty_username.as_deref())
                            .unwrap_or("Private invite");
                        @let heading = if item.kind == "LOBBY" {
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
                        div id=(format!("pending-item-{}", item.id)) class="pending-item" data-direction=(item.direction.as_str())
                            direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) justify-content="space-between" align-items="center"
                            border-bottom=((theme::BORDER, 1)) padding-y=14 padding-x=4 gap="12px" {
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
                                        span color=(theme::INK_SOFT) { "@" (handle) }
                                    }
                                    @if item.kind == "INVITATION" {
                                        span color=(theme::INK_SOFT) { "Link hidden after creation for security." }
                                    }
                                }
                            }
                            div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap="8px" {
                                @if item.kind == "LOBBY" {
                                    anchor href=(format!("/lobbies/{}", item.id)) { "Open lobby" }
                                } @else if item.kind == "LOBBY_INVITATION" {
                                    form hx-post=(format!("/lobbies/{}/action", item.id)) hx-target="#app-page" {
                                        input type=hidden name="action" value="ACCEPT_INVITE";
                                        button type=submit padding="10px" background=(theme::PRIMARY) color=(theme::ON_PRIMARY) border-radius="8px" { "Accept invite" }
                                    }
                                    form hx-post=(format!("/lobbies/{}/action", item.id)) hx-target="#app-page" {
                                        input type=hidden name="action" value="DECLINE_INVITE";
                                        button type=submit padding="10px" { "Decline" }
                                    }
                                } @else if item.kind == "CHALLENGE" && item.direction == "INCOMING" {
                                    form hx-post="/dashboard/action" hx-target="#app-page" {
                                        input type=hidden name="action" value="ACCEPT_CHALLENGE";
                                        input type=hidden name="challenge_id" value=(item.id.as_str());
                                        button type=submit padding-y=9 padding-x=13 background=(theme::PRIMARY) color=(theme::ON_PRIMARY)
                                            border=((theme::PRIMARY_EDGE, 1)) border-radius="8px" cursor=pointer { "Accept" }
                                    }
                                    form hx-post="/dashboard/action" hx-target="#app-page" {
                                        input type=hidden name="action" value="DECLINE_CHALLENGE";
                                        input type=hidden name="challenge_id" value=(item.id.as_str());
                                        button type=submit padding-y=9 padding-x=13 border=((theme::SECONDARY_EDGE, 1))
                                            border-radius="8px" cursor=pointer { "Decline" }
                                    }
                                } @else if item.kind == "CHALLENGE" {
                                    form hx-post="/dashboard/action" hx-target="#app-page" {
                                        input type=hidden name="action" value="CANCEL_CHALLENGE";
                                        input type=hidden name="challenge_id" value=(item.id.as_str());
                                        button type=submit padding-y=9 padding-x=13 border=((theme::SECONDARY_EDGE, 1))
                                            border-radius="8px" cursor=pointer { "Cancel" }
                                    }
                                } @else {
                                    form hx-post="/dashboard/action" hx-target="#app-page" {
                                        input type=hidden name="action" value="REVOKE_INVITATION";
                                        input type=hidden name="invitation_id" value=(item.id.as_str());
                                        button type=submit padding-y=9 padding-x=13 color=(theme::DANGER_INK) border=((theme::DANGER_EDGE, 1))
                                            border-radius="8px" cursor=pointer { "Revoke" }
                                    }
                                }
                            }
                        }
                    }
                }
                main id="dashboard-main" data-dashboard-order="3" direction="column" gap="16px" align-items="start" {
                    details id="new-game-panel" width="100%" background=(theme::WHITE) border=((theme::BORDER, 1))
                        border-radius="16px" padding="20px" {
                        summary cursor="pointer" font-weight=bold { "New game" }
                        div padding-top="14px" { (start_game_component()) }
                    }
                    details id="profile-settings" width="100%" background=(theme::WHITE) border=((theme::BORDER, 1))
                        border-radius="16px" padding="20px" {
                        summary cursor="pointer" font-weight=bold { "Profile" }
                        div padding-top="14px" gap="12px" {
                            form method="post" action="/dashboard/action" gap="8px" {
                                input type="hidden" name="action" value="SET_DISPLAY_NAME";
                                span { "Display name" }
                                input type="text" id="profile-display-name" name="display_name" value=(display_name)
                                    required=true padding="10px" border=((theme::INPUT_BORDER, 1)) border-radius="8px";
                                button type="submit" background=(theme::PRIMARY) color=(theme::ON_PRIMARY) border-radius="8px" padding="10px" { "Save display name" }
                            }
                            form method="post" action="/dashboard/action" {
                                input type="hidden" name="action" value="USE_GOOGLE_NAME";
                                button type="submit" background=(theme::WHITE) color=(theme::LINK) border=((theme::PRIMARY_EDGE, 1)) border-radius="8px" padding="10px" { "Use Google name again" }
                            }
                            form method="post" action="/profile/avatar" enctype="multipart/form-data" gap="8px" {
                                span { "Upload a profile picture" }
                                input type="file" name="avatar";
                                button type="submit" background=(theme::WHITE) color=(theme::LINK) border=((theme::PRIMARY_EDGE, 1)) border-radius="8px" padding="10px" { "Upload picture" }
                            }
                            form method="post" action="/dashboard/action" {
                                input type="hidden" name="action" value="REMOVE_AVATAR";
                                button type="submit" background=(theme::WHITE) color=(theme::DANGER_INK) border=((theme::DANGER_EDGE, 1)) border-radius="8px" padding="10px" { "Remove profile picture" }
                            }
                            form method="post" action="/dashboard/action" {
                                input type="hidden" name="action" value="USE_GOOGLE_AVATAR";
                                button type="submit" background=(theme::WHITE) color=(theme::LINK) border=((theme::PRIMARY_EDGE, 1)) border-radius="8px" padding="10px" { "Use Google photo again" }
                            }
                        }
                    }
                    details id="score-totals" width="100%" background=(theme::WHITE) border=((theme::BORDER, 1))
                        border-radius="16px" padding="20px" {
                        summary cursor="pointer" font-weight=bold { "Score history" }
                        span padding-top="12px" color=(theme::INK_MUTED) { (totals) }
                    }
                }
            }
        }
    }
    .into()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_has_one_game_creation_path() {
        let page = start_game_component().to_string();
        assert!(page.contains("/lobbies/new"));
        assert!(page.contains("invite players by username"));
        assert!(!page.contains("CHALLENGE"));
        assert!(!page.contains("CREATE_INVITATION"));
    }
}
