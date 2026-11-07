//! Multiplayer lobby and lobby creation pages.

use hyperchad::{
    actions::ActionType,
    router::Container,
    template::{LayoutOverflow, container},
};

use crate::{FirstPlayerPolicy, GameLobby, theme};

pub fn lobby_invitation_url(public_base_url: &str, lobby_id: &str) -> String {
    format!(
        "{}/lobbies/join/{lobby_id}",
        public_base_url.trim_end_matches('/')
    )
}

#[allow(clippy::too_many_lines, clippy::large_stack_frames)]
pub fn lobby_page(lobby: &GameLobby, viewer_user_id: &str, public_base_url: &str) -> Container {
    let is_creator = lobby.creator_user_id == viewer_user_id;
    let first_player = match &lobby.settings.first_player {
        FirstPlayerPolicy::Random => "Random".to_string(),
        FirstPlayerPolicy::Creator => "Creator".to_string(),
        FirstPlayerPolicy::Chosen(user_id) => format!("Chosen member: {user_id}"),
    };
    let invitation_url =
        (lobby.status == "OPEN").then(|| lobby_invitation_url(public_base_url, &lobby.lobby_id));
    let dashboard_channel = format!("dashboard:{viewer_user_id}");
    let lobby_path = format!("/lobbies/{}", lobby.lobby_id);
    // Page-specific handler elements must disappear on navigation. Idiomorph preserves #app-page,
    // and removing its event attribute alone does not unregister HyperChad's window listener.
    let refresh_lobby = ActionType::Navigate { url: lobby_path };
    container! {
        div id="app-page" font-family=(theme::FONT_UI) data-shared-state-channel=(dashboard_channel.as_str())
            min-height="100vh" background=(theme::MENU_BG) padding=24 align-items="center" {
            span id="lobby-live-refresh" hidden fx-global-shared-state-event=(refresh_lobby) { }
            main id="multiplayer-lobby" width="100%" max-width="760px" background=(theme::WHITE)
                border=((theme::BORDER, 1)) border-radius="18px" padding="28px" gap="18px" {
                @if let Some(game_id) = lobby.started_game_id {
                    span hidden fx-immediate=(ActionType::Navigate { url: format!("/games/{game_id}") }) { }
                }
                anchor href="/" color=(theme::LINK) { "← Dashboard" }
                h1 { "Game lobby" }
                span color=(theme::INK_MUTED) { (lobby.members.len()) " of " (lobby.settings.max_players) " seats joined" }
                section id="lobby-settings" gap="5px" {
                    h2 { "Game settings" }
                    span { (lobby.settings.board_size) " × " (lobby.settings.board_size) " board" }
                    span { (lobby.settings.tile_set_count) " complete tile set(s)" }
                    span { "Remaining tile count: " (if lobby.settings.visibility.show_remaining_tile_count { "Shown" } else { "Hidden" }) }
                    span { "Remaining tile faces: " (if lobby.settings.visibility.show_remaining_tile_faces { "Shown" } else { "Hidden" }) }
                    span { "First player: " (first_player) }
                }
                @if let Some(invitation_url) = invitation_url.as_deref() {
                    section id="lobby-invitation" background=(theme::PRIMARY_SOFT) padding=14 border-radius="10px" gap="5px" {
                        span font-weight=bold { "Private invitation" }
                        anchor href=(invitation_url) overflow-wrap="anywhere" color=(theme::LINK) { (invitation_url) }
                        span color=(theme::INK_MUTED) { "Share this private link, or invite players by username. Players can join while the lobby is open." }
                    }
                }
                @if is_creator && lobby.status == "OPEN" {
                    form hx-post=(format!("/lobbies/{}/action", lobby.lobby_id)) hx-target="#app-page" gap="8px" {
                        input type=hidden name="action" value="INVITE";
                        h2 { "Invite by username" }
                        input type=text name="username" placeholder="Exact @username" required=true padding="12px" border=((theme::INPUT_BORDER, 1)) border-radius="8px";
                        button type=submit background=(theme::PRIMARY) color=(theme::ON_PRIMARY) padding="12px" border-radius="8px" { "Send invite" }
                    }
                }
                @if !lobby.invited_usernames.is_empty() {
                    section gap="6px" {
                        h2 { "Invites sent" }
                        @for username in &lobby.invited_usernames {
                            span { "@" (username) " — awaiting response" }
                        }
                    }
                }
                section id="lobby-members" gap="6px" {
                    h2 { "Members" }
                    @for member in &lobby.members {
                        div direction="row" gap="8px" border-bottom=((theme::BORDER, 1)) padding-y="6px" {
                            span font-weight=bold { "Seat " (member.seat + 1) }
                            span { "@" (lobby.usernames.get(&member.user_id).map_or(member.user_id.as_str(), String::as_str)) }
                            @if member.user_id == lobby.creator_user_id { span color=(theme::LINK) { "Creator" } }
                        }
                    }
                }
                @if lobby.status == "STARTED" {
                    @if let Some(game_id) = lobby.started_game_id {
                        anchor href=(format!("/games/{game_id}")) background=(theme::PRIMARY) color=(theme::ON_PRIMARY) padding-y=12 padding-x=16 border-radius="10px" { "Open game" }
                    }
                } @else if is_creator {
                    form hx-post=(format!("/lobbies/{}/action", lobby.lobby_id)) hx-target="#app-page" gap="8px" {
                        input type=hidden name="action" value="UPDATE";
                        h2 { "Edit lobby settings" }
                        div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap="10px" {
                            div flex=1 min-width="180px" gap="4px" {
                                span font-weight=bold { "Maximum players" }
                                input type=text name="max_players" value=(lobby.settings.max_players) padding-y=10 padding-x=12 border=((theme::INPUT_BORDER, 1)) border-radius="8px";
                            }
                            div flex=1 min-width="180px" gap="4px" {
                                span font-weight=bold { "Board size" }
                                input type=text name="board_size" value=(lobby.settings.board_size) padding-y=10 padding-x=12 border=((theme::INPUT_BORDER, 1)) border-radius="8px";
                            }
                            div flex=1 min-width="180px" gap="4px" {
                                span font-weight=bold { "Tile sets" }
                                input type=text name="tile_set_count" value=(lobby.settings.tile_set_count) padding-y=10 padding-x=12 border=((theme::INPUT_BORDER, 1)) border-radius="8px";
                            }
                        }
                        div gap="4px" {
                            span font-weight=bold { "Remaining tile count" }
                            select name="show_remaining_tile_count" {
                                @if lobby.settings.visibility.show_remaining_tile_count {
                                    option value="true" { "Show" }
                                    option value="false" { "Hide" }
                                } @else {
                                    option value="false" { "Hide" }
                                    option value="true" { "Show" }
                                }
                            }
                        }
                        div gap="4px" {
                            span font-weight=bold { "Remaining tile faces" }
                            span color=(theme::INK_MUTED) font-size="12px" { "Shows an unordered inventory, never future draw order. Showing faces also reveals the total." }
                            select name="show_remaining_tile_faces" {
                                @if lobby.settings.visibility.show_remaining_tile_faces {
                                    option value="true" { "Show" }
                                    option value="false" { "Hide" }
                                } @else {
                                    option value="false" { "Hide" }
                                    option value="true" { "Show" }
                                }
                            }
                        }
                        div gap="4px" {
                            span font-weight=bold { "Who takes the first turn?" }
                            select name="first_player_policy" {
                                option value="CREATOR" { "Creator starts" }
                                option value="RANDOM" { "Random member starts" }
                                option value="CHOSEN" { "Choose a joined member" }
                            }
                        }
                        div gap="4px" {
                            span font-weight=bold { "Chosen member" }
                            span color=(theme::INK_MUTED) font-size="12px" { "Only used when ‘Choose a joined member’ is selected." }
                            input type=text name="chosen_first_user_id" placeholder="Joined member user ID" padding-y=10 padding-x=12 border=((theme::INPUT_BORDER, 1)) border-radius="8px";
                        }
                        button type=submit background=(theme::PRIMARY) color=(theme::ON_PRIMARY) padding-y=10 padding-x=14 border-radius="9px" { "Save settings" }
                    }
                    div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap="8px" {
                        form hx-post=(format!("/lobbies/{}/action", lobby.lobby_id)) hx-target="#app-page" {
                            input type=hidden name="action" value="START";
                            button type=submit background=(theme::PRIMARY) color=(theme::ON_PRIMARY) padding-y=10 padding-x=14 border-radius="9px" { "Start game" }
                        }
                        form hx-post=(format!("/lobbies/{}/action", lobby.lobby_id)) hx-target="#app-page" {
                            input type=hidden name="action" value="CANCEL";
                            button type=submit background=(theme::WHITE) color=(theme::DANGER_INK) padding-y=10 padding-x=14 border=((theme::DANGER_EDGE, 1)) border-radius="9px" { "Cancel lobby" }
                        }
                    }
                } @else {
                    form hx-post=(format!("/lobbies/{}/action", lobby.lobby_id)) hx-target="#app-page" {
                        input type=hidden name="action" value="LEAVE";
                        button type=submit background=(theme::WHITE) color=(theme::DANGER_INK) padding-y=10 padding-x=14 border=((theme::DANGER_EDGE, 1)) border-radius="9px" { "Leave lobby" }
                    }
                }
            }
        }
    }
    .into()
}

pub fn lobby_creation_page(user_id: &str) -> Container {
    let channel = format!("dashboard:{user_id}");
    container! {
        div id="app-page" font-family=(theme::FONT_UI) data-shared-state-channel=(channel) min-height="100vh" background=(theme::MENU_BG) padding=24 align-items="center" {
            main width="100%" max-width="760px" background=(theme::WHITE) border=((theme::BORDER, 1))
                border-radius="18px" padding="28px" gap="18px" {
                anchor href="/" color=(theme::LINK) { "← Dashboard" }
                h1 { "Create game" }
                span color=(theme::INK_MUTED) { "Choose the initial settings. You can change them again before starting the game." }
                form hx-post="/lobbies/create" hx-target="#app-page" gap="14px" {
                    input type=hidden name="action" value="CREATE";
                    div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap="10px" {
                        div flex=1 min-width="180px" gap="4px" {
                            span font-weight=bold { "Maximum players" }
                            span color=(theme::INK_MUTED) font-size="12px" { "Total lobby capacity, including you." }
                            input type=text name="max_players" value="4" placeholder="For example, 4" padding-y=10 padding-x=12 border=((theme::INPUT_BORDER, 1)) border-radius="8px";
                        }
                        div flex=1 min-width="180px" gap="4px" {
                            span font-weight=bold { "Board size" }
                            span color=(theme::INK_MUTED) font-size="12px" { "Width and height of the square board." }
                            input type=text name="board_size" value="15" placeholder="For example, 15" padding-y=10 padding-x=12 border=((theme::INPUT_BORDER, 1)) border-radius="8px";
                        }
                        div flex=1 min-width="180px" gap="4px" {
                            span font-weight=bold { "Tile sets" }
                            span color=(theme::INK_MUTED) font-size="12px" { "Number of complete 100-tile distributions." }
                            input type=text name="tile_set_count" value="1" placeholder="For example, 1" padding-y=10 padding-x=12 border=((theme::INPUT_BORDER, 1)) border-radius="8px";
                        }
                    }
                    div gap="4px" {
                        span font-weight=bold { "Remaining tile count" }
                        select name="show_remaining_tile_count" {
                            option value="true" { "Show" }
                            option value="false" { "Hide" }
                        }
                    }
                    div gap="4px" {
                        span font-weight=bold { "Remaining tile faces" }
                        span color=(theme::INK_MUTED) font-size="12px" { "Shows an unordered inventory, never future draw order. Showing faces also reveals the total." }
                        select name="show_remaining_tile_faces" {
                            option value="true" { "Show" }
                            option value="false" { "Hide" }
                        }
                    }
                    div gap="4px" {
                        span font-weight=bold { "Who takes the first turn?" }
                        span color=(theme::INK_MUTED) font-size="12px" { "Choose yourself or select a random joined member when the game starts." }
                        select name="first_player_policy" {
                            option value="CREATOR" { "Creator starts" }
                            option value="RANDOM" { "Random member starts" }
                        }
                    }
                    button type=submit padding-y=12 padding-x=16 background=(theme::PRIMARY) color=(theme::ON_PRIMARY)
                        border=((theme::PRIMARY_EDGE, 1)) border-radius="10px" cursor=pointer { "Create game" }
                }
            }
        }
    }
    .into()
}
