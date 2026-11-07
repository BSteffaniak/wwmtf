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
    FirstPlayerPolicy, GameCreationPolicy, GameLobby, LobbyMember, theme,
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

/// A chunky select bound to a handler field name, with `selected` preselecting the current value.
fn select_control(name: &str, selected: &str, options: &[(String, String)]) -> Container {
    single(container! {
        select name=(name) selected=(selected)
            width="100%" padding-y=(theme::SPACE_3) padding-x=(theme::SPACE_3)
            background=(theme::WHITE) color=(theme::INK)
            border=((theme::INPUT_BORDER, theme::EDGE_THIN)) border-bottom=((theme::INPUT_BORDER, 4))
            border-radius=(theme::RADIUS_MD) font-family=(theme::FONT_UI)
            font-size=(theme::TEXT_MD) font-weight=bold cursor=pointer {
            @for (value, label) in options {
                option value=(value) { (label) }
            }
        }
    })
}

/// A labeled setting tile: glyph badge, label, one-line hint, and the control underneath.
fn setting_field(
    glyph: &str,
    tint: &str,
    label: &str,
    hint: &str,
    control: Container,
) -> Container {
    let control = vec![control];
    single(container! {
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
    single(container! {
        div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_3) {
            (children)
        }
    })
}

/// A small group heading inside a form.
fn group_heading(glyph: &str, title: &str) -> Container {
    single(container! {
        div direction="row" align-items="center" gap=(theme::SPACE_2) {
            span color=(theme::ACCENT) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold { (glyph) }
            h2 margin=0 color=(theme::INK) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold { (title) }
        }
    })
}

/// A white button with coral ink for reversible-but-serious exits (cancel, leave).
fn danger_secondary_button(label: &str) -> Container {
    let (padding_y, padding_x, font_size, radius) = ui::button_metrics(ButtonSize::Medium);
    single(container! {
        button type=submit
            padding-y=(padding_y) padding-x=(padding_x)
            background=(theme::SECONDARY) color=(theme::DANGER_INK)
            border=((theme::DANGER, theme::EDGE_THIN)) border-bottom=((theme::DANGER_EDGE, theme::EDGE_DEPTH))
            border-radius=(radius) font-size=(font_size) font-weight=bold
            font-family=(theme::FONT_UI) cursor=pointer
            align-items="center" justify-content="center" text-align="center"
            fx-hover=(ActionType::set_background_self(theme::DANGER_SOFT)) {
            (label)
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
    single(container! {
        div gap=(theme::SPACE_2) {
            div direction="row" gap=(theme::SPACE_1) padding-y=(theme::SPACE_1) { (tiles) }
            h1 margin=0 color=(theme::INK) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XXL) font-weight=bold { (title) }
            span color=(theme::INK_MUTED) font-size=(theme::TEXT_MD) { (subtitle) }
        }
    })
}

/// Unwraps a single-element `container!` result so helpers do not add wrapper `div`s.
fn single(mut items: Vec<Container>) -> Container {
    if items.len() == 1 {
        items.remove(0)
    } else {
        Container::from(items)
    }
}

/// Wraps the `#app-page` element in an anonymous root. Renderers treat a route's top-level
/// container as the document root and emit only its children, so without this wrapper the
/// `#app-page` element (id, ground, font, centering) would be dropped.
fn page_root(app_page: Container) -> Container {
    Container::from(vec![app_page])
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
        return single(container! {
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
    single(container! {
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
            single(container! {
                span width=18 height=18 min-width=18 border-radius=(theme::RADIUS_SM / 2)
                    background=(fill) border=((edge, theme::EDGE_THIN)) { }
            })
        })
        .collect();
    single(container! {
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
    let settings = &lobby.settings;
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
            &format!("→ First player: {}", first_player_label(lobby)),
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
            select_control(
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
            select_control(
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
            select_control(
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
            select_control(
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
            select_control(
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
            select_control("first_player_policy", first_policy, &first_options),
        ),
        setting_field(
            "✓",
            theme::INFO_SOFT,
            "Chosen member",
            "Only used when ‘Choose a joined member’ is selected.",
            select_control("chosen_first_user_id", chosen_selected, &members),
        ),
    ]);
    single(container! {
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

fn invitation_section(
    lobby: &GameLobby,
    invitation_url: &str,
    is_creator: bool,
    action_url: &str,
) -> Container {
    single(container! {
        section id="lobby-invitation" gap=(theme::SPACE_3) padding=(theme::SPACE_5)
            background=(theme::PRIMARY_SOFT) border=((theme::PRIMARY, theme::EDGE_THIN))
            border-bottom=((theme::PRIMARY_EDGE, theme::EDGE_DEPTH)) border-radius=(theme::RADIUS_LG) {
            div direction="row" align-items="center" gap=(theme::SPACE_2) {
                span color=(theme::PRIMARY_INK) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold { "✉" }
                span color=(theme::PRIMARY_INK) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold { "Private invitation" }
            }
            span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) { "Share this private link, or invite players by username. Players can join while the lobby is open." }
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
            @if !lobby.invited_usernames.is_empty() {
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
        "You need at least one more player to start.".to_string()
    };
    let invitation = invitation_url
        .as_deref()
        .map(|url| invitation_section(lobby, url, is_creator && is_open, &action_url));
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
                single(container! {
                    div gap=(theme::SPACE_2) {
                        span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold color=(theme::INK) {
                            (joined) " of " (lobby.settings.max_players) " seats joined"
                        }
                        (seat_pips(joined, total))
                    }
                }),
                single(container! {
                    section id="lobby-members" gap=(theme::SPACE_3) {
                        h2 margin=0 color=(theme::INK) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold { "Members" }
                        div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_3) {
                            (seats)
                        }
                    }
                }),
                single(container! {
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
                (ui::card(theme::PAGE_MEDIUM, vec![single(container! {
                    div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) align-items="center" gap=(theme::SPACE_3) {
                        div flex=1 min-width=200 gap=(theme::SPACE_1) {
                            span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold color=(theme::INK) { "Ready to play?" }
                            span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) { (start_hint) }
                        }
                        form hx-post=(action_url) hx-target="#app-page" {
                            input type=hidden name="action" value="START";
                            (ui::submit_button("Start game ▶", ButtonVariant::Primary, ButtonSize::Large))
                        }
                        form hx-post=(action_url) hx-target="#app-page" {
                            input type=hidden name="action" value="CANCEL";
                            (danger_secondary_button("Cancel lobby"))
                        }
                    }
                })]))
            } @else {
                (ui::card(theme::PAGE_MEDIUM, vec![single(container! {
                    div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) align-items="center" gap=(theme::SPACE_3) {
                        div flex=1 min-width=200 gap=(theme::SPACE_1) {
                            span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold color=(theme::INK) { "You're in! ✓" }
                            span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) { "Waiting for " (creator_handle) " to start the game. This page updates live." }
                        }
                        form hx-post=(action_url) hx-target="#app-page" {
                            input type=hidden name="action" value="LEAVE";
                            (danger_secondary_button("Leave lobby"))
                        }
                    }
                })]))
            }
            @if let Some(invitation) = invitation {
                (invitation)
            }
            @if let Some(settings_form) = settings_form {
                (ui::card(theme::PAGE_MEDIUM, vec![settings_form]))
            }
        }
    };
    page_root(ui::with_data(
        ui::menu_page(body),
        "shared-state-channel",
        dashboard_channel,
    ))
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
            select_control(
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
            select_control(
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
            select_control("tile_set_count", "1", &tile_set_options(policy, 1)),
        ),
    ]);
    let visibility = setting_row(vec![
        setting_field(
            "#",
            theme::SURFACE_LAVENDER,
            "Remaining tile count",
            "Show how many tiles are left in the bag.",
            select_control("show_remaining_tile_count", "true", &visibility_options()),
        ),
        setting_field(
            "◇",
            theme::PREMIUM_START,
            "Remaining tile faces",
            "Unordered inventory, never draw order. Showing faces also reveals the total.",
            select_control("show_remaining_tile_faces", "true", &visibility_options()),
        ),
    ]);
    let turn_order = setting_row(vec![setting_field(
        "→",
        theme::PRIMARY_SOFT,
        "Who takes the first turn?",
        "Go first yourself, or let fate pick a joined member when the game starts.",
        select_control("first_player_policy", "CREATOR", &first_options),
    )]);
    let body = container! {
        div width="100%" max-width=(theme::PAGE_MEDIUM) {
            (ui::text_link("/", "← Dashboard"))
        }
        (ui::card(theme::PAGE_MEDIUM, vec![
            page_title("NEW GAME", "Create game", "Pick your settings, then invite friends. You can change everything again before the game starts."),
            single(container! {
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
    page_root(ui::with_data(
        ui::menu_page(body),
        "shared-state-channel",
        channel,
    ))
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
