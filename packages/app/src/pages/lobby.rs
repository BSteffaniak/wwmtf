//! Multiplayer lobby and lobby creation pages.
//!
//! Both pages are styled only with template attributes and `fx-*` actions. Setting controls are
//! selects whose field names and value formats match `LobbyActionForm`; the server still validates
//! every submitted value against [`GameCreationPolicy`].

use std::collections::{BTreeMap, BTreeSet};

use hyperchad::{
    actions::ActionType,
    router::Container,
    template::{LayoutOverflow, container},
};

use crate::{
    FirstPlayerPolicy, GameCreationPolicy, GameLobby, LobbyInvitePreview, LobbyMember,
    LobbySettings, theme,
    ui::{self, ButtonSize, ButtonVariant, Tone},
};

/// Square board sizes offered in the picker (odd sizes keep a centered start square).
const BOARD_SIZE_CHOICES: [u8; 7] = [9, 11, 13, 15, 17, 19, 21];
/// Upper bound on list lengths so a generous server policy never renders a huge select.
const MAX_LISTED_PLAYERS: usize = 16;
/// Upper bound on listed tile-set counts.
const MAX_LISTED_TILE_SETS: u8 = 8;
/// Default seat count on the creation form.
const DEFAULT_PLAYERS: usize = 4;
/// Default board size on the creation form.
const DEFAULT_BOARD_SIZE: u8 = 15;

pub fn lobby_invitation_url(public_base_url: &str, lobby_id: &str) -> String {
    format!(
        "{}/lobbies/join/{lobby_id}",
        public_base_url.trim_end_matches('/')
    )
}

// ---------------------------------------------------------------------------
// Option lists
// ---------------------------------------------------------------------------

fn player_options(policy: GameCreationPolicy, current: usize) -> Vec<(String, String)> {
    let mut counts: BTreeSet<usize> =
        (2..=policy.max_players.clamp(2, MAX_LISTED_PLAYERS)).collect();
    counts.insert(current);
    counts
        .into_iter()
        .map(|count| {
            let label = match count {
                2 => "2 · head to head".to_string(),
                count => format!("{count} players"),
            };
            (count.to_string(), label)
        })
        .collect()
}

fn board_options(policy: GameCreationPolicy, current: u8) -> Vec<(String, String)> {
    let mut sizes: BTreeSet<u8> = BOARD_SIZE_CHOICES
        .into_iter()
        .filter(|size| *size <= policy.max_board_size)
        .collect();
    if sizes.is_empty() {
        sizes.insert(policy.max_board_size);
    }
    sizes.insert(current);
    sizes
        .into_iter()
        .map(|size| {
            let flavor = match size {
                0..=11 => " · quick & cozy",
                12..=14 => " · snappy",
                15 => " · classic",
                16..=19 => " · roomy",
                _ => " · epic",
            };
            (size.to_string(), format!("{size} × {size}{flavor}"))
        })
        .collect()
}

fn tile_set_options(policy: GameCreationPolicy, current: u8) -> Vec<(String, String)> {
    let mut counts: BTreeSet<u8> =
        (1..=policy.max_tile_sets.clamp(1, MAX_LISTED_TILE_SETS)).collect();
    counts.insert(current);
    counts
        .into_iter()
        .map(|count| {
            let tiles = u32::from(count) * 100;
            let label = if count == 1 {
                format!("1 set · {tiles} tiles")
            } else {
                format!("{count} sets · {tiles} tiles")
            };
            (count.to_string(), label)
        })
        .collect()
}

fn visibility_options() -> Vec<(String, String)> {
    vec![
        ("true".to_string(), "Show it".to_string()),
        ("false".to_string(), "Keep it secret".to_string()),
    ]
}

const fn bool_value(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

// ---------------------------------------------------------------------------
// Private building blocks
// ---------------------------------------------------------------------------

/// A labeled setting tile: glyph badge, label, one-line hint, and the control underneath.
fn setting_field(
    glyph: &str,
    tint: &str,
    label: &str,
    hint: &str,
    control: Container,
) -> Container {
    let control = vec![control];
    ui::one(container! {
        div flex=1 min-width=200 gap=(theme::SPACE_2) padding=(theme::SPACE_4)
            background=(theme::SURFACE_ALT) border=((theme::BORDER, theme::EDGE_THIN))
            border-radius=(theme::RADIUS_LG) {
            div direction="row" align-items="center" gap=(theme::SPACE_2) {
                span width=32 height=32 min-width=32 border-radius=(theme::RADIUS_SM)
                    background=(tint) color=(theme::INK) align-items="center" justify-content="center"
                    font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold {
                    (glyph)
                }
                span font-weight=bold font-size=(theme::TEXT_MD) { (label) }
            }
            span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) { (hint) }
            (control)
        }
    })
}

/// A wrapping row of setting tiles.
fn setting_row(children: impl Into<Vec<Container>>) -> Container {
    let children: Vec<Container> = children.into();
    ui::one(container! {
        div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_3) {
            (children)
        }
    })
}

/// A small group heading inside a form.
fn group_heading(glyph: &str, title: &str) -> Container {
    ui::one(container! {
        div direction="row" align-items="center" gap=(theme::SPACE_2) {
            span color=(theme::ACCENT) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold { (glyph) }
            h2 margin=0 color=(theme::INK) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold { (title) }
        }
    })
}

/// Page title block with the tile-spelled kicker.
fn page_title(kicker: &str, title: &str, subtitle: &str) -> Container {
    let tiles: Vec<Container> = kicker
        .chars()
        .filter(|character| !character.is_whitespace())
        .enumerate()
        .map(|(index, letter)| {
            let colors = if index % 2 == 0 {
                ui::TILE_SUNNY
            } else {
                ui::TILE_PENDING
            };
            let mut letter_tile = ui::tile_face_colored(&letter.to_string(), None, 30, colors);
            letter_tile.translate_y =
                Some(hyperchad::transformer::Number::Integer(if index % 2 == 0 {
                    -2
                } else {
                    2
                }));
            letter_tile
        })
        .collect();
    ui::one(container! {
        div gap=(theme::SPACE_2) {
            div direction="row" gap=(theme::SPACE_1) padding-y=(theme::SPACE_1) { (tiles) }
            h1 margin=0 color=(theme::INK) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XXL) font-weight=bold { (title) }
            span color=(theme::INK_MUTED) font-size=(theme::TEXT_MD) { (subtitle) }
        }
    })
}

fn display_username<'a>(lobby: &'a GameLobby, user_id: &'a str) -> &'a str {
    lobby.usernames.get(user_id).map_or(user_id, String::as_str)
}

// ---------------------------------------------------------------------------
// Lobby page
// ---------------------------------------------------------------------------

fn seat_card(
    lobby: &GameLobby,
    viewer_user_id: &str,
    seat: usize,
    member: Option<&LobbyMember>,
) -> Container {
    let Some(member) = member else {
        return ui::one(container! {
            div flex=1 min-width=140 align-items="center" gap=(theme::SPACE_2)
                padding=(theme::SPACE_4) background=(theme::MENU_BG)
                border=((theme::BORDER_STRONG, theme::EDGE_THIN)) border-radius=(theme::RADIUS_LG)
                color=(theme::INK_SOFT) text-align="center" {
                span width=52 height=52 min-width=52 border-radius=(theme::RADIUS_PILL)
                    border=((theme::BORDER_STRONG, theme::EDGE_THIN)) background=(theme::SURFACE_ALT)
                    align-items="center" justify-content="center"
                    font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XL) font-weight=bold { "+" }
                span font-weight=bold font-size=(theme::TEXT_MD) { "Open seat" }
                span font-size=(theme::TEXT_XS) font-weight=bold { "Seat " (seat + 1) }
            }
        });
    };
    let colors = theme::seat_color(member.seat);
    let username = display_username(lobby, &member.user_id);
    let handle = format!("@{username}");
    let avatar = ui::avatar(username, None, colors, 52);
    let is_creator = member.user_id == lobby.creator_user_id;
    let is_viewer = member.user_id == viewer_user_id;
    ui::one(container! {
        div flex=1 min-width=140 align-items="center" gap=(theme::SPACE_2)
            padding=(theme::SPACE_4) background=(theme::SURFACE)
            border=((colors.edge, theme::EDGE_THIN)) border-bottom=((colors.edge, theme::EDGE_DEPTH))
            border-radius=(theme::RADIUS_LG) text-align="center" {
            (avatar)
            span font-weight=bold font-size=(theme::TEXT_MD) color=(theme::INK) overflow-wrap="anywhere" { (handle) }
            span font-size=(theme::TEXT_XS) font-weight=bold color=(theme::INK_SOFT) { "Seat " (seat + 1) }
            @if is_creator || is_viewer {
                div direction="row" gap=(theme::SPACE_1) justify-content="center" {
                    @if is_creator { (ui::badge("Creator", Tone::Accent)) }
                    @if is_viewer { (ui::badge("You", Tone::Success)) }
                }
            }
        }
    })
}

fn seat_pips(joined: usize, total: usize) -> Container {
    let pips: Vec<Container> = (0..total)
        .map(|index| {
            let (fill, edge) = if index < joined {
                (theme::PRIMARY, theme::PRIMARY_EDGE)
            } else {
                (theme::SURFACE_ALT, theme::BORDER_STRONG)
            };
            ui::one(container! {
                span width=18 height=18 min-width=18 border-radius=(theme::RADIUS_SM / 2)
                    background=(fill) border=((edge, theme::EDGE_THIN)) { }
            })
        })
        .collect();
    ui::one(container! {
        div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_1) { (pips) }
    })
}

fn first_player_label(lobby: &GameLobby) -> String {
    match &lobby.settings.first_player {
        FirstPlayerPolicy::Random => "Random".to_string(),
        FirstPlayerPolicy::Creator => "Creator".to_string(),
        FirstPlayerPolicy::Chosen(user_id) => {
            format!("Chosen member: @{}", display_username(lobby, user_id))
        }
    }
}

fn settings_chips(lobby: &GameLobby) -> Vec<Container> {
    settings_chips_for(&lobby.settings, &first_player_label(lobby))
}

/// Settings chips for any lobby view; `first_player` is the already-resolved display label.
fn settings_chips_for(settings: &LobbySettings, first_player: &str) -> Vec<Container> {
    let tile_sets = if settings.tile_set_count == 1 {
        "1 complete tile set".to_string()
    } else {
        format!("{} complete tile sets", settings.tile_set_count)
    };
    let shown = |value: bool| if value { "Shown" } else { "Hidden" };
    vec![
        ui::pill(
            &format!("▦ {} × {} board", settings.board_size, settings.board_size),
            theme::PREMIUM_DL,
            theme::PREMIUM_DL_INK,
        ),
        ui::pill(
            &format!("★ {tile_sets}"),
            theme::SUNNY_SOFT,
            theme::ON_SUNNY,
        ),
        ui::pill(
            &format!(
                "# Remaining tile count: {}",
                shown(settings.visibility.show_remaining_tile_count)
            ),
            theme::SURFACE_LAVENDER,
            theme::ACCENT_EDGE,
        ),
        ui::pill(
            &format!(
                "◇ Remaining tile faces: {}",
                shown(settings.visibility.show_remaining_tile_faces)
            ),
            theme::PREMIUM_DW,
            theme::PREMIUM_DW_INK,
        ),
        ui::pill(
            &format!("→ First player: {first_player}"),
            theme::PRIMARY_SOFT,
            theme::PRIMARY_INK,
        ),
    ]
}

#[allow(clippy::too_many_lines)]
fn lobby_settings_form(
    lobby: &GameLobby,
    policy: GameCreationPolicy,
    action_url: &str,
) -> Container {
    let settings = &lobby.settings;
    let (first_policy, chosen_user) = match &settings.first_player {
        FirstPlayerPolicy::Creator => ("CREATOR", None),
        FirstPlayerPolicy::Random => ("RANDOM", None),
        FirstPlayerPolicy::Chosen(user_id) => ("CHOSEN", Some(user_id.as_str())),
    };
    let members: Vec<(String, String)> = lobby
        .members
        .iter()
        .map(|member| {
            (
                member.user_id.clone(),
                format!(
                    "@{} · seat {}",
                    display_username(lobby, &member.user_id),
                    member.seat + 1
                ),
            )
        })
        .collect();
    let chosen_selected = chosen_user
        .filter(|user_id| members.iter().any(|(id, _)| id == user_id))
        .unwrap_or(lobby.creator_user_id.as_str());
    let first_options = vec![
        ("CREATOR".to_string(), "Creator starts".to_string()),
        ("RANDOM".to_string(), "Random member starts".to_string()),
        ("CHOSEN".to_string(), "Choose a joined member".to_string()),
    ];
    let basics = setting_row(vec![
        setting_field(
            "☺",
            theme::PREMIUM_DW,
            "Maximum players",
            "Total seats, including you.",
            ui::select(
                "max_players",
                &settings.max_players.to_string(),
                &player_options(policy, settings.max_players),
            ),
        ),
        setting_field(
            "▦",
            theme::PREMIUM_DL,
            "Board size",
            "Width and height of the square board.",
            ui::select(
                "board_size",
                &settings.board_size.to_string(),
                &board_options(policy, settings.board_size),
            ),
        ),
        setting_field(
            "★",
            theme::SUNNY_SOFT,
            "Tile sets",
            "Complete 100-tile bags shuffled together.",
            ui::select(
                "tile_set_count",
                &settings.tile_set_count.to_string(),
                &tile_set_options(policy, settings.tile_set_count),
            ),
        ),
    ]);
    let visibility = setting_row(vec![
        setting_field(
            "#",
            theme::SURFACE_LAVENDER,
            "Remaining tile count",
            "Show how many tiles are left in the bag.",
            ui::select(
                "show_remaining_tile_count",
                bool_value(settings.visibility.show_remaining_tile_count),
                &visibility_options(),
            ),
        ),
        setting_field(
            "◇",
            theme::PREMIUM_START,
            "Remaining tile faces",
            "Unordered inventory, never draw order. Showing faces also reveals the total.",
            ui::select(
                "show_remaining_tile_faces",
                bool_value(settings.visibility.show_remaining_tile_faces),
                &visibility_options(),
            ),
        ),
    ]);
    let turn_order = setting_row(vec![
        setting_field(
            "→",
            theme::PRIMARY_SOFT,
            "Who takes the first turn?",
            "Pick who opens the board when the game starts.",
            ui::select("first_player_policy", first_policy, &first_options),
        ),
        setting_field(
            "✓",
            theme::INFO_SOFT,
            "Chosen member",
            "Only used when ‘Choose a joined member’ is selected.",
            ui::select("chosen_first_user_id", chosen_selected, &members),
        ),
    ]);
    ui::one(container! {
        form hx-post=(action_url) hx-target="#app-page" gap=(theme::SPACE_4) {
            input type=hidden name="action" value="UPDATE";
            div gap=(theme::SPACE_1) {
                h2 margin=0 color=(theme::INK) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XL) font-weight=bold { "Edit lobby settings" }
                span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) { "Tweak anything until you hit start." }
            }
            (basics)
            (visibility)
            (turn_order)
            div direction="row" {
                (ui::submit_button("Save settings", ButtonVariant::Accent, ButtonSize::Medium))
            }
        }
    })
}

/// A muted, non-interactive Start placeholder shown until enough players have joined.
fn start_cue() -> Container {
    let (padding_y, padding_x, font_size, radius) = ui::button_metrics(ButtonSize::Large);
    ui::one(container! {
        span class="start-game-waiting" padding-y=(padding_y) padding-x=(padding_x)
            background=(theme::SURFACE_ALT) color=(theme::INK_SOFT)
            border=((theme::BORDER, theme::EDGE_THIN)) border-bottom=((theme::BORDER, theme::EDGE_THIN))
            border-radius=(radius) font-size=(font_size) font-weight=bold text-align="center"
            white-space="preserve" user-select="none" {
            "Waiting for players…"
        }
    })
}

/// A quiet "Cancel lobby" control at the bottom of the creator's page. Cancelling takes two
/// taps: the first reveals a confirmation, like the resign flow in the game dock.
fn cancel_lobby_zone(action_url: &str) -> Container {
    let ask = ui::action_button(
        "Cancel lobby…",
        ButtonVariant::Ghost,
        ButtonSize::Small,
        ActionType::Multi(vec![
            ActionType::display_by_id("cancel-lobby-confirm"),
            ActionType::no_display_by_id("cancel-lobby-ask"),
        ]),
    );
    let keep = ui::action_button(
        "Keep lobby",
        ButtonVariant::Secondary,
        ButtonSize::Medium,
        ActionType::Multi(vec![
            ActionType::no_display_by_id("cancel-lobby-confirm"),
            ActionType::display_by_id("cancel-lobby-ask"),
        ]),
    );
    ui::one(container! {
        section id="cancel-lobby" width="100%" max-width=(theme::PAGE_MEDIUM) align-items="center" gap=(theme::SPACE_2) {
            div id="cancel-lobby-ask" direction="row" justify-content="center" {
                (ask)
            }
            div id="cancel-lobby-confirm" hidden width="100%" align-items="center" gap=(theme::SPACE_3)
                padding=(theme::SPACE_4) background=(theme::DANGER_SOFT)
                border=((theme::DANGER, theme::EDGE_THIN)) border-radius=(theme::RADIUS_LG) {
                span color=(theme::DANGER_INK) font-weight=bold text-align="center" {
                    "Cancel this lobby for everyone? Joined players will be sent back to their games."
                }
                div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) justify-content="center" gap=(theme::SPACE_2) {
                    form hx-post=(action_url) hx-target="#app-page" {
                        input type=hidden name="action" value="CANCEL";
                        (ui::danger_outline_button("Yes, cancel lobby"))
                    }
                    (keep)
                }
            }
        }
    })
}

fn invitation_section(
    lobby: &GameLobby,
    invitation_url: &str,
    is_creator: bool,
    action_url: &str,
) -> Container {
    ui::one(container! {
        section id="lobby-invitation" gap=(theme::SPACE_3) padding=(theme::SPACE_5)
            background=(theme::PRIMARY_SOFT) border=((theme::PRIMARY, theme::EDGE_THIN))
            border-bottom=((theme::PRIMARY_EDGE, theme::EDGE_DEPTH)) border-radius=(theme::RADIUS_LG) {
            div direction="row" align-items="center" gap=(theme::SPACE_2) {
                span color=(theme::PRIMARY_INK) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold { "✉" }
                span color=(theme::PRIMARY_INK) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold { "Private invitation" }
            }
            span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) {
                @if is_creator {
                    "Share this private link, or invite players by username. Players can join while the lobby is open."
                } @else {
                    "Know someone who'd play? Share this private link. Players can join while the lobby is open."
                }
            }
            input type=text value=(invitation_url)
                width="100%" padding-y=(theme::SPACE_3) padding-x=(theme::SPACE_3)
                background=(theme::WHITE) color=(theme::INK) font-size=(theme::TEXT_SM) font-weight=bold
                border=((theme::PRIMARY_EDGE, theme::EDGE_THIN)) border-radius=(theme::RADIUS_MD);
            anchor href=(invitation_url) overflow-wrap="anywhere" color=(theme::LINK) font-weight=bold font-size=(theme::TEXT_SM) { (invitation_url) }
            @if is_creator {
                form hx-post=(action_url) hx-target="#app-page" gap=(theme::SPACE_2) {
                    input type=hidden name="action" value="INVITE";
                    span font-weight=bold color=(theme::INK) { "Invite by username" }
                    div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_2) align-items="center" {
                        input type=text name="username" placeholder="Exact @username" required=true
                            flex=1 min-width=180 padding-y=(theme::SPACE_3) padding-x=(theme::SPACE_3)
                            background=(theme::WHITE) color=(theme::INK) font-size=(theme::TEXT_MD)
                            border=((theme::INPUT_BORDER, theme::EDGE_THIN)) border-radius=(theme::RADIUS_MD);
                        (ui::submit_button("Send invite", ButtonVariant::Accent, ButtonSize::Medium))
                    }
                }
            }
            @if is_creator && !lobby.invited_usernames.is_empty() {
                div gap=(theme::SPACE_2) {
                    span font-weight=bold color=(theme::INK) font-size=(theme::TEXT_SM) { "Invites sent" }
                    div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_2) {
                        @for username in &lobby.invited_usernames {
                            (ui::pill(&format!("@{username} — awaiting response"), theme::WHITE, theme::INK_MUTED))
                        }
                    }
                }
            }
        }
    })
}

#[allow(clippy::too_many_lines, clippy::large_stack_frames)]
pub fn lobby_page(
    lobby: &GameLobby,
    viewer_user_id: &str,
    public_base_url: &str,
    policy: GameCreationPolicy,
) -> Container {
    let is_creator = lobby.creator_user_id == viewer_user_id;
    let is_open = lobby.status == "OPEN";
    let invitation_url = is_open.then(|| lobby_invitation_url(public_base_url, &lobby.lobby_id));
    let dashboard_channel = format!("dashboard:{viewer_user_id}");
    let lobby_path = format!("/lobbies/{}", lobby.lobby_id);
    let action_url = format!("/lobbies/{}/action", lobby.lobby_id);
    // Page-specific handler elements must disappear on navigation. Idiomorph preserves #app-page,
    // and removing its event attribute alone does not unregister HyperChad's window listener.
    let refresh_lobby = ActionType::Navigate { url: lobby_path };

    let by_seat: BTreeMap<usize, &LobbyMember> = lobby
        .members
        .iter()
        .map(|member| (member.seat, member))
        .collect();
    let slot_count = by_seat
        .keys()
        .next_back()
        .map_or(0, |seat| seat + 1)
        .max(lobby.settings.max_players)
        .max(lobby.members.len());
    let seats: Vec<Container> = (0..slot_count)
        .map(|seat| seat_card(lobby, viewer_user_id, seat, by_seat.get(&seat).copied()))
        .collect();
    let joined = lobby.members.len();
    let total = lobby.settings.max_players.max(joined);
    let status_badge = match lobby.status.as_str() {
        "OPEN" => ui::badge("● Open for players", Tone::Success),
        "STARTED" => ui::badge("★ Game started", Tone::Accent),
        other => ui::badge(other, Tone::Neutral),
    };
    let creator_handle = format!("@{}", display_username(lobby, &lobby.creator_user_id));
    let ready = joined >= 2;
    let start_hint = if ready {
        "Everyone in? Let the tiles fly!".to_string()
    } else {
        "You need at least one more player to start. Share the invite link above.".to_string()
    };
    let seats_full = joined >= lobby.settings.max_players;
    let invitation = invitation_url
        .as_deref()
        .filter(|_| !seats_full)
        .map(|url| invitation_section(lobby, url, is_creator && is_open, &action_url));
    // While seats are empty, inviting is the next step, so the invite card comes first.
    let invitation_first = !ready && invitation.is_some();
    let start_disabled = start_cue();
    let cancel_zone = cancel_lobby_zone(&action_url);
    let settings_form = (is_creator && lobby.status != "STARTED")
        .then(|| lobby_settings_form(lobby, policy, &action_url));

    let body = container! {
        span id="lobby-live-refresh" hidden fx-global-shared-state-event=(refresh_lobby) { }
        main id="multiplayer-lobby" width="100%" max-width=(theme::PAGE_MEDIUM) gap=(theme::SPACE_5) {
            @if let Some(game_id) = lobby.started_game_id {
                span hidden fx-immediate=(ActionType::Navigate { url: format!("/games/{game_id}") }) { }
            }
            div direction="row" align-items="center" justify-content="space-between" gap=(theme::SPACE_2)
                overflow-x=(LayoutOverflow::Wrap { grid: false }) {
                (ui::text_link("/", "← Dashboard"))
                (status_badge)
            }
            (ui::card(theme::PAGE_MEDIUM, vec![
                page_title("LOBBY", "Game lobby", &format!("Hosted by {creator_handle}. Gather your crew, then start the game.")),
                ui::one(container! {
                    div gap=(theme::SPACE_2) {
                        span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold color=(theme::INK) {
                            (joined) " of " (lobby.settings.max_players) " seats joined"
                        }
                        (seat_pips(joined, total))
                    }
                }),
                ui::one(container! {
                    section id="lobby-members" gap=(theme::SPACE_3) {
                        h2 margin=0 color=(theme::INK) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold { "Members" }
                        div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_3) {
                            (seats)
                        }
                    }
                }),
                ui::one(container! {
                    section id="lobby-settings" gap=(theme::SPACE_2) {
                        h2 margin=0 color=(theme::INK) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold { "Game settings" }
                        div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_2) {
                            (settings_chips(lobby))
                        }
                    }
                }),
            ]))
            @if lobby.status == "STARTED" {
                @if let Some(game_id) = lobby.started_game_id {
                    (ui::link_button(&format!("/games/{game_id}"), "Open game →", ButtonVariant::Primary, ButtonSize::Large))
                }
            } @else if is_creator {
                @if invitation_first {
                    @if let Some(invitation) = invitation.clone() {
                        (invitation)
                    }
                }
                (ui::card(theme::PAGE_MEDIUM, vec![ui::one(container! {
                    div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) align-items="center" gap=(theme::SPACE_3) {
                        div flex=1 min-width=200 gap=(theme::SPACE_1) {
                            span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold color=(theme::INK) { "Ready to play?" }
                            span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) { (start_hint) }
                        }
                        @if ready {
                            form hx-post=(action_url) hx-target="#app-page" {
                                input type=hidden name="action" value="START";
                                (ui::submit_button("Start game ▶", ButtonVariant::Primary, ButtonSize::Large))
                            }
                        } @else {
                            (start_disabled)
                        }
                    }
                })]))
            } @else {
                (ui::card(theme::PAGE_MEDIUM, vec![ui::one(container! {
                    div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) align-items="center" gap=(theme::SPACE_3) {
                        div flex=1 min-width=200 gap=(theme::SPACE_1) {
                            span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold color=(theme::INK) { "You're in! ✓" }
                            span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) { "Waiting for " (creator_handle) " to start the game. This page updates live." }
                        }
                        form hx-post=(action_url) hx-target="#app-page" {
                            input type=hidden name="action" value="LEAVE";
                            (ui::danger_outline_button("Leave lobby"))
                        }
                    }
                })]))
            }
            @if !(is_creator && invitation_first) {
                @if let Some(invitation) = invitation {
                    (invitation)
                }
            }
            @if let Some(settings_form) = settings_form {
                (ui::card(theme::PAGE_MEDIUM, vec![settings_form]))
            }
            @if is_creator && lobby.status != "STARTED" {
                (cancel_zone)
            }
        }
    };
    ui::menu_page(body, Some(&dashboard_channel))
}

// ---------------------------------------------------------------------------
// Invite landing page
// ---------------------------------------------------------------------------

/// The page an invited friend lands on from a lobby invite link (`GET /lobbies/join/<id>`).
///
/// `preview` carries the public lobby summary when the lobby could be loaded; otherwise the page
/// falls back to generic copy. The join form keeps its `hx-post` target and the optional hidden
/// legacy `invitation_token` exactly as the join handler expects.
#[must_use]
pub fn lobby_join_page(
    user_id: &str,
    action: &str,
    legacy_token: Option<&str>,
    preview: Option<&LobbyInvitePreview>,
) -> Container {
    let channel = format!("dashboard:{user_id}");
    let subtitle = preview.map_or_else(
        || {
            "A friend saved you a seat. Join the lobby and the game starts when the host is ready."
                .to_string()
        },
        |preview| {
            format!(
                "@{} saved you a seat. Join the lobby and the game starts when they're ready.",
                preview.host_username
            )
        },
    );
    let summary = preview.map(|preview| {
        let first_player = match &preview.settings.first_player {
            FirstPlayerPolicy::Random => "Random",
            FirstPlayerPolicy::Creator => "Host",
            FirstPlayerPolicy::Chosen(_) => "Chosen member",
        };
        let chips = settings_chips_for(&preview.settings, first_player);
        let total = preview.settings.max_players.max(preview.joined);
        let open = total.saturating_sub(preview.joined);
        let seats_line = if open == 1 {
            format!("{} of {} seats taken · 1 seat left", preview.joined, preview.settings.max_players)
        } else {
            format!("{} of {} seats taken · {open} seats left", preview.joined, preview.settings.max_players)
        };
        ui::one(container! {
            div gap=(theme::SPACE_3) {
                div gap=(theme::SPACE_2) {
                    span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold color=(theme::INK) { (seats_line) }
                    (seat_pips(preview.joined, total))
                }
                div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_2) {
                    (chips)
                }
            }
        })
    });
    let join_button = ui::submit_button("Join game ★", ButtonVariant::Primary, ButtonSize::Large);
    let body = container! {
        (ui::brand_logo(36, false))
        (ui::card(theme::PAGE_NARROW + 80, vec![
            page_title("JOIN", "You're invited!", &subtitle),
            ui::one(container! {
                div gap=(theme::SPACE_4) {
                    @if let Some(summary) = summary {
                        (summary)
                    }
                    form hx-post=(action) hx-target="#app-page" gap=(theme::SPACE_3) {
                        @if let Some(token) = legacy_token {
                            input type=hidden name="invitation_token" value=(token);
                        }
                        (join_button)
                    }
                    div direction="row" justify-content="center" {
                        (ui::text_link("/", "Not now · back to my games"))
                    }
                }
            }),
        ]))
    };
    ui::menu_page(body, Some(&channel))
}

// ---------------------------------------------------------------------------
// Lobby creation page
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
pub fn lobby_creation_page(user_id: &str, policy: GameCreationPolicy) -> Container {
    let channel = format!("dashboard:{user_id}");
    let default_players = DEFAULT_PLAYERS.min(policy.max_players).max(2);
    let default_board = if DEFAULT_BOARD_SIZE <= policy.max_board_size {
        DEFAULT_BOARD_SIZE
    } else {
        BOARD_SIZE_CHOICES
            .into_iter()
            .rfind(|size| *size <= policy.max_board_size)
            .unwrap_or(policy.max_board_size)
    };
    let first_options = vec![
        ("CREATOR".to_string(), "I'll go first".to_string()),
        ("RANDOM".to_string(), "Random member starts".to_string()),
    ];
    let basics = setting_row(vec![
        setting_field(
            "☺",
            theme::PREMIUM_DW,
            "Maximum players",
            "Total seats, including you.",
            ui::select(
                "max_players",
                &default_players.to_string(),
                &player_options(policy, default_players),
            ),
        ),
        setting_field(
            "▦",
            theme::PREMIUM_DL,
            "Board size",
            "Width and height of the square board.",
            ui::select(
                "board_size",
                &default_board.to_string(),
                &board_options(policy, default_board),
            ),
        ),
        setting_field(
            "★",
            theme::SUNNY_SOFT,
            "Tile sets",
            "Complete 100-tile bags. Add more for big groups.",
            ui::select("tile_set_count", "1", &tile_set_options(policy, 1)),
        ),
    ]);
    let visibility = setting_row(vec![
        setting_field(
            "#",
            theme::SURFACE_LAVENDER,
            "Remaining tile count",
            "Show how many tiles are left in the bag.",
            ui::select("show_remaining_tile_count", "true", &visibility_options()),
        ),
        setting_field(
            "◇",
            theme::PREMIUM_START,
            "Remaining tile faces",
            "Unordered inventory, never draw order. Showing faces also reveals the total.",
            ui::select("show_remaining_tile_faces", "true", &visibility_options()),
        ),
    ]);
    let turn_order = setting_row(vec![setting_field(
        "→",
        theme::PRIMARY_SOFT,
        "Who takes the first turn?",
        "Go first yourself, or let fate pick a joined member when the game starts.",
        ui::select("first_player_policy", "CREATOR", &first_options),
    )]);
    let body = container! {
        div width="100%" max-width=(theme::PAGE_MEDIUM) {
            (ui::text_link("/", "← Dashboard"))
        }
        (ui::card(theme::PAGE_MEDIUM, vec![
            page_title("NEW GAME", "Create game", "Pick your settings, then invite friends. You can change everything again before the game starts."),
            ui::one(container! {
                form hx-post="/lobbies/create" hx-target="#app-page" gap=(theme::SPACE_5) {
                    input type=hidden name="action" value="CREATE";
                    (group_heading("①", "The basics"))
                    (basics)
                    (group_heading("②", "What everyone can see"))
                    (visibility)
                    (group_heading("③", "Turn order"))
                    (turn_order)
                    (ui::submit_button("Create game ★", ButtonVariant::Primary, ButtonSize::Large))
                }
            }),
        ]))
    };
    ui::menu_page(body, Some(&channel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GameVisibilitySettings, LobbySettings};

    fn policy() -> GameCreationPolicy {
        GameCreationPolicy::new(8, 19, 3).expect("policy")
    }

    fn lobby(first_player: FirstPlayerPolicy) -> GameLobby {
        GameLobby {
            lobby_id: "lobby-1".to_string(),
            creator_user_id: "creator-id".to_string(),
            status: "OPEN".to_string(),
            revision: 1,
            settings: LobbySettings {
                max_players: 4,
                board_size: 17,
                tile_set_count: 2,
                first_player,
                visibility: GameVisibilitySettings {
                    show_remaining_tile_count: true,
                    show_remaining_tile_faces: false,
                },
            },
            members: vec![
                LobbyMember {
                    user_id: "creator-id".to_string(),
                    seat: 0,
                },
                LobbyMember {
                    user_id: "guest-id".to_string(),
                    seat: 1,
                },
            ],
            usernames: BTreeMap::from([
                ("creator-id".to_string(), "ada".to_string()),
                ("guest-id".to_string(), "bram".to_string()),
            ]),
            invited_usernames: vec!["cleo".to_string()],
            started_game_id: None,
        }
    }

    #[test]
    fn creation_page_uses_policy_bounded_selects_with_handler_field_names() {
        let page = lobby_creation_page("creator-id", policy()).to_string();
        for name in [
            "max_players",
            "board_size",
            "tile_set_count",
            "show_remaining_tile_count",
            "show_remaining_tile_faces",
            "first_player_policy",
        ] {
            assert!(page.contains(&format!("name=\"{name}\"")), "{name} missing");
        }
        assert!(page.contains("value=\"CREATE\""));
        assert!(page.contains("Create game"));
        assert!(page.contains("value=\"8\""));
        assert!(!page.contains("value=\"9\" "), "player count above policy");
        assert!(page.contains("19 × 19"));
        assert!(!page.contains("21 × 21"), "board above policy");
        assert!(page.contains("3 sets · 300 tiles"));
        assert!(!page.contains("4 sets"), "tile sets above policy");
        assert!(
            !page.contains("type=\"text\""),
            "no free-text number inputs"
        );
    }

    #[test]
    fn lobby_page_shows_seats_settings_and_member_picker() {
        let page = lobby_page(
            &lobby(FirstPlayerPolicy::Chosen("guest-id".to_string())),
            "creator-id",
            "https://games.example.test/",
            policy(),
        )
        .to_string();
        assert!(page.contains("2 of 4 seats joined"));
        assert!(page.contains("@ada"));
        assert!(page.contains("@bram"));
        assert!(page.contains("Creator"));
        assert_eq!(page.matches("Open seat").count(), 2);
        assert!(page.contains("17 × 17 board"));
        assert!(page.contains("2 complete tile sets"));
        assert!(page.contains("First player: Chosen member: @bram"));
        assert!(page.contains("https://games.example.test/lobbies/join/lobby-1"));
        assert!(page.contains("@cleo — awaiting response"));
        assert!(page.contains("name=\"chosen_first_user_id\""));
        assert!(page.contains("value=\"guest-id\""));
        assert!(page.contains("lobby-settings"));
        assert!(page.contains("lobby-invitation"));
        assert!(page.contains("lobby-members"));
        assert!(page.contains("multiplayer-lobby"));
        assert!(page.contains("lobby-live-refresh"));
        for action in ["INVITE", "UPDATE", "START", "CANCEL"] {
            assert!(
                page.contains(&format!("value=\"{action}\"")),
                "{action} missing"
            );
        }
        assert!(!page.contains("value=\"LEAVE\""));
    }

    #[test]
    fn lobby_page_orders_invites_first_until_ready_and_hides_them_when_full() {
        let mut solo = lobby(FirstPlayerPolicy::Creator);
        solo.members.truncate(1);
        let page =
            lobby_page(&solo, "creator-id", "https://games.example.test", policy()).to_string();
        assert!(
            !page.contains("value=\"START\""),
            "start offered before ready"
        );
        assert!(page.contains("Waiting for players"));
        let invite = page.find("lobby-invitation").expect("invite card");
        let start = page.find("Ready to play?").expect("start card");
        assert!(
            invite < start,
            "invite card should lead while seats are empty"
        );
        assert!(page.contains("cancel-lobby-confirm"));
        assert!(page.contains("value=\"CANCEL\""));

        let mut full = lobby(FirstPlayerPolicy::Creator);
        full.settings.max_players = 2;
        let page =
            lobby_page(&full, "creator-id", "https://games.example.test", policy()).to_string();
        assert!(
            !page.contains("lobby-invitation"),
            "invite card shown when full"
        );
        assert!(page.contains("value=\"START\""));

        let guest = lobby_page(
            &lobby(FirstPlayerPolicy::Creator),
            "guest-id",
            "https://games.example.test",
            policy(),
        )
        .to_string();
        assert!(!guest.contains("invite players by username"));
        assert!(
            !guest.contains("@cleo"),
            "pending invitees leaked to a guest"
        );
    }

    #[test]
    fn invite_landing_page_uses_the_branded_card() {
        let preview = LobbyInvitePreview {
            host_username: "ada".to_string(),
            settings: lobby(FirstPlayerPolicy::Creator).settings,
            joined: 2,
            already_joined: false,
        };
        let page =
            lobby_join_page("guest-id", "/lobbies/join/lobby-1", None, Some(&preview)).to_string();
        assert!(page.contains("id=\"app-page\""));
        assert!(page.contains("hx-post=\"/lobbies/join/lobby-1\""));
        assert!(page.contains("@ada saved you a seat"));
        assert!(page.contains("2 of 4 seats taken"));
        assert!(page.contains("17 × 17 board"));
        assert!(page.contains("Join game"));
        assert!(!page.contains("name=\"invitation_token\""));

        let generic =
            lobby_join_page("guest-id", "/lobbies/join", Some("legacy-token"), None).to_string();
        assert!(generic.contains("A friend saved you a seat"));
        assert!(generic.contains("name=\"invitation_token\""));
        assert!(generic.contains("value=\"legacy-token\""));
    }

    #[test]
    fn lobby_page_for_guest_offers_leave_without_creator_controls() {
        let page = lobby_page(
            &lobby(FirstPlayerPolicy::Creator),
            "guest-id",
            "https://games.example.test",
            policy(),
        )
        .to_string();
        assert!(page.contains("value=\"LEAVE\""));
        assert!(page.contains("Waiting for "));
        for action in ["INVITE", "UPDATE", "START", "CANCEL"] {
            assert!(
                !page.contains(&format!("value=\"{action}\"")),
                "{action} leaked"
            );
        }
        assert!(page.contains("lobby-invitation"));
    }
}
