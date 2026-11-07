//! Visual game scene: board, rack, turn dock, scoreboard, overlays, and definition panels.
//!
//! Everything here is styled with `HyperChad` template attributes and `fx-*` actions only. The
//! board sizes itself with `calc(...)` against the dynamic viewport so the whole board fits the
//! play area by default; zoom levels scale relative to that fit.

use hyperchad::{
    actions::ActionType,
    router::Container,
    template::{JustifyContent, LayoutDirection, LayoutOverflow, container, if_responsive},
    transformer::{Calculation, Number},
};
use wwmtf_game_domain::{Coordinate, PremiumSquare};

use crate::{
    AuthorizedGamePage, move_history_component,
    routes::{
        BoardZoom, DraftFeedback, TurnDraft, TurnMode, draft_feedback, draft_token, rack_action,
    },
    theme,
    ui::{self, ButtonSize, ButtonVariant, Tone},
};

// ---------------------------------------------------------------------------
// Layout constants (page-local; candidates for promotion into `theme`).
// ---------------------------------------------------------------------------

/// Gap between board squares (the frame color shows through as grout).
const BOARD_GAP: f32 = 2.0;
/// Frame border width around the board grid.
const BOARD_FRAME_BORDER: f32 = 5.0;
/// Horizontal space reserved around the board when fitting it to the viewport.
const BOARD_RESERVED_WIDTH: f32 = 16.0;
/// Vertical space reserved for the header and the turn dock when fitting the board.
const BOARD_RESERVED_HEIGHT: f32 = 296.0;
/// Upper bound for the fitted board edge on very large screens.
const BOARD_MAX_EDGE: f32 = 920.0;
/// Largest rack tile edge.
const RACK_TILE_MAX: f32 = 54.0;
/// Horizontal space around the seven rack tiles (dock and tray padding, borders, and gaps).
const RACK_RESERVED_WIDTH: f32 = 100.0;
/// Rack tiles shown per row.
const RACK_SLOTS: f32 = 7.0;
/// Maximum width of the floating turn dock.
const DOCK_MAX_WIDTH: i32 = 560;
/// Width of the floating drawers (menu, bag, definitions).
const DRAWER_WIDTH: i32 = 360;
/// Header avatar size.
const HEADER_AVATAR: i32 = 26;
/// Header control size (back, bag, menu buttons).
const HEADER_CONTROL: i32 = 38;

// ---------------------------------------------------------------------------
// calc() helpers
// ---------------------------------------------------------------------------

fn calc_value(number: Number) -> Calculation {
    Calculation::Number(Box::new(number))
}

fn calc_px(value: f32) -> Calculation {
    calc_value(Number::Real(value))
}

fn calc_group(calculation: Calculation) -> Calculation {
    Calculation::Grouping(Box::new(calculation))
}

fn calc_add(left: Calculation, right: Calculation) -> Calculation {
    calc_group(Calculation::Add(Box::new(left), Box::new(right)))
}

fn calc_sub(left: Calculation, right: Calculation) -> Calculation {
    calc_group(Calculation::Subtract(Box::new(left), Box::new(right)))
}

/// Multiplies a length by a unitless factor.
fn calc_times(left: Calculation, factor: f32) -> Calculation {
    calc_group(Calculation::Multiply(
        Box::new(left),
        Box::new(calc_value(Number::Real(factor))),
    ))
}

/// Divides a length by a unitless divisor.
fn calc_over(left: Calculation, divisor: f32) -> Calculation {
    calc_group(Calculation::Divide(
        Box::new(left),
        Box::new(calc_value(Number::Real(divisor))),
    ))
}

fn calc_min(left: Calculation, right: Calculation) -> Calculation {
    Calculation::Min(Box::new(left), Box::new(right))
}

fn calc_max(left: Calculation, right: Calculation) -> Calculation {
    Calculation::Max(Box::new(left), Box::new(right))
}

const fn calc_number(calculation: Calculation) -> Number {
    Number::Calc(calculation)
}

/// Viewport-relative board geometry for one zoom level.
struct BoardMetrics {
    square: Calculation,
    size: f32,
}

impl BoardMetrics {
    fn new(board_size: u8, zoom: BoardZoom) -> Self {
        let size = f32::from(board_size);
        let available = calc_min(
            calc_min(
                calc_sub(
                    calc_value(Number::RealDvw(100.0)),
                    calc_px(BOARD_RESERVED_WIDTH),
                ),
                calc_sub(
                    calc_value(Number::RealDvh(100.0)),
                    calc_px(BOARD_RESERVED_HEIGHT),
                ),
            ),
            calc_px(BOARD_MAX_EDGE),
        );
        let grid = calc_sub(
            calc_group(available),
            calc_px(BOARD_FRAME_BORDER.mul_add(2.0, (size - 1.0) * BOARD_GAP)),
        );
        let fit = calc_over(grid, size);
        #[allow(clippy::cast_precision_loss)]
        let scale = zoom.fit_percent() as f32 / 100.0;
        let square = if zoom.fit_percent() == 100 {
            fit
        } else {
            calc_times(fit, scale)
        };
        Self { square, size }
    }

    /// Square edge length.
    fn square(&self) -> Number {
        calc_number(self.square.clone())
    }

    /// A fraction of the square edge (font sizes).
    fn scaled(&self, factor: f32) -> Number {
        calc_number(calc_times(self.square.clone(), factor))
    }

    /// Offset of the `index`th square from the grid origin, plus `extra` pixels.
    fn offset(&self, index: u8, extra: f32) -> Calculation {
        calc_add(
            calc_times(
                calc_add(self.square.clone(), calc_px(BOARD_GAP)),
                f32::from(index),
            ),
            calc_px(extra),
        )
    }

    /// Outer edge of the framed board.
    fn frame(&self) -> Number {
        calc_number(calc_add(
            calc_times(self.square.clone(), self.size),
            calc_px(BOARD_FRAME_BORDER.mul_add(2.0, (self.size - 1.0) * BOARD_GAP)),
        ))
    }
}

/// Edge length of one rack tile: as big as fits seven across the dock, capped for desktops.
fn rack_tile_size() -> Calculation {
    calc_min(
        calc_px(RACK_TILE_MAX),
        calc_over(
            calc_sub(
                calc_value(Number::RealDvw(100.0)),
                calc_px(RACK_RESERVED_WIDTH),
            ),
            RACK_SLOTS,
        ),
    )
}

fn active_player_name(game: &AuthorizedGamePage) -> &str {
    game.participants
        .iter()
        .find(|participant| participant.player_id == game.view.active_player)
        .map_or("Player", |participant| participant.display_name.as_str())
}

/// "You vs bob" (or "You vs bob, cleo & dan") from the viewer's point of view.
fn match_title(game: &AuthorizedGamePage) -> String {
    let opponents = game
        .participants
        .iter()
        .filter(|participant| participant.player_id != game.viewer_player)
        .map(|participant| participant.display_name.as_str())
        .collect::<Vec<_>>();
    match opponents.split_last() {
        None => "Your match".to_string(),
        Some((last, [])) => format!("You vs {last}"),
        Some((last, rest)) => format!("You vs {} & {last}", rest.join(", ")),
    }
}

fn participant_seat(
    game: &AuthorizedGamePage,
    player: wwmtf_game_domain::PlayerId,
) -> theme::SeatColor {
    let index = game
        .participants
        .iter()
        .position(|participant| participant.player_id == player)
        .unwrap_or_default();
    theme::seat_color(index)
}

/// `(fill, ink, label)` for an empty board square.
fn square_style(
    coordinate: Coordinate,
    start: Coordinate,
    premium: Option<PremiumSquare>,
) -> (&'static str, &'static str, String) {
    if coordinate == start {
        return (
            theme::PREMIUM_START,
            theme::PREMIUM_START_INK,
            "★".to_string(),
        );
    }
    match premium {
        Some(PremiumSquare::Letter(2)) => {
            (theme::PREMIUM_DL, theme::PREMIUM_DL_INK, "2L".to_string())
        }
        Some(PremiumSquare::Letter(multiplier)) => (
            theme::PREMIUM_TL,
            theme::PREMIUM_TL_INK,
            format!("{multiplier}L"),
        ),
        Some(PremiumSquare::Word(2)) => {
            (theme::PREMIUM_DW, theme::PREMIUM_DW_INK, "2W".to_string())
        }
        Some(PremiumSquare::Word(multiplier)) => (
            theme::PREMIUM_TW,
            theme::PREMIUM_TW_INK,
            format!("{multiplier}W"),
        ),
        None => (theme::SQUARE, theme::SQUARE_INK, String::new()),
    }
}

// ---------------------------------------------------------------------------
// Draft feedback
// ---------------------------------------------------------------------------

pub fn invalid_words_message(words: &[String]) -> String {
    if words.len() == 1 {
        format!("{} is not a valid word", words[0])
    } else {
        format!("{} are not valid words", words.join(", "))
    }
}

pub fn draft_feedback_component(
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
    let (background, edge, ink, glyph) = match draft.mode {
        TurnMode::ConfirmResign => (theme::DANGER_SOFT, theme::DANGER, theme::DANGER_INK, "!"),
        TurnMode::Exchange | TurnMode::ConfirmExchange | TurnMode::ConfirmPass => {
            (theme::SUNNY_SOFT, theme::SUNNY, theme::ON_SUNNY, "?")
        }
        TurnMode::Play if candidate_valid => {
            (theme::PRIMARY, theme::PRIMARY_EDGE, theme::ON_PRIMARY, "✓")
        }
        TurnMode::Play if feedback.candidate.is_some() => {
            (theme::DANGER_SOFT, theme::DANGER, theme::DANGER_INK, "×")
        }
        TurnMode::Play => (theme::NIGHT_DEEP, theme::NIGHT_LINE, theme::NIGHT_INK, "✦"),
    };
    ui::one(container! {
        section id="draft-preview" class="dock-message" width="100%" direction="row"
            align-items="center" justify-content="center" gap=(theme::SPACE_2)
            padding-y=6 padding-x=(theme::SPACE_3)
            background=(background) color=(ink) border=((edge, theme::EDGE_THIN))
            border-radius=(theme::RADIUS_LG) overflow-x="hidden" {
            span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_MD) font-weight=bold { (glyph) }
            span min-width=0 font-size=(theme::TEXT_SM) font-weight=bold overflow-wrap="anywhere" text-align="center" {
                (message)
            }
        }
    })
}

pub fn draft_error_page(game: &AuthorizedGamePage, draft: &TurnDraft, message: &str) -> Container {
    visual_game_page(game, draft, Some(message))
}

/// Renders the turn feedback slot. Without a message it is an empty placeholder so a
/// rejected turn's `#turn-feedback` fragment has an element to replace.
pub fn turn_feedback(message: Option<&str>) -> Container {
    ui::one(container! {
        @if let Some(message) = message {
            section id="turn-feedback" width="100%" {
                (ui::with_id(ui::notice(message, Tone::Danger), "game-error"))
            }
        } @else {
            section id="turn-feedback" width="100%" {}
        }
    })
}

// ---------------------------------------------------------------------------
// Definitions
// ---------------------------------------------------------------------------

pub fn definition_content(word: &str, lookup: &crate::DefinitionLookup) -> Container {
    let letters = word
        .chars()
        .take(12)
        .map(|letter| ui::tile_face(&letter.to_uppercase().to_string(), None, 30))
        .collect::<Vec<_>>();
    ui::one(container! {
        div direction="row" gap=(theme::SPACE_1) overflow-x=(LayoutOverflow::Wrap { grid: false }) { (letters) }
        h1 font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XL) color=(theme::INK) { (word) }
        @match lookup {
            crate::DefinitionLookup::Found(definition) => {
                @for meaning in &definition.meanings {
                    section gap=6 background=(theme::SURFACE) border=((theme::BORDER, theme::EDGE_THIN))
                        border-radius=(theme::RADIUS_MD) padding=(theme::SPACE_3) {
                        (ui::eyebrow(&meaning.part_of_speech.to_uppercase(), theme::ACCENT))
                        @for text in &meaning.definitions {
                            span color=(theme::INK) { "• " (text.as_str()) }
                        }
                    }
                }
                span color=(theme::INK_MUTED) font-size=(theme::TEXT_XS) {
                    "Source: " anchor href=(definition.source_url.as_str()) color=(theme::LINK) { (definition.source_url.as_str()) }
                }
                span color=(theme::INK_MUTED) font-size=(theme::TEXT_XS) {
                    "License: " anchor href=(definition.license_url.as_str()) color=(theme::LINK) { (definition.license_name.as_str()) }
                }
            },
            crate::DefinitionLookup::Missing => {
                span color=(theme::INK_MUTED) { "No definition is available from the configured provider." }
            },
            crate::DefinitionLookup::Unavailable(reason) => {
                span color=(theme::INK_MUTED) { (reason.user_message()) }
            },
        }
    })
}

pub fn definition_page(
    game_id: wwmtf_game_domain::GameId,
    word: &str,
    lookup: &crate::DefinitionLookup,
) -> Container {
    let game_href = format!("/games/{game_id}");
    let content = definition_content(word, lookup);
    ui::menu_page(
        container! {
            main id="word-definition" width="100%" max-width=(theme::PAGE_MEDIUM)
                background=(theme::SURFACE) color=(theme::INK)
                border=((theme::BORDER, theme::EDGE_THIN)) border-bottom=((theme::BORDER_STRONG, theme::EDGE_DEPTH))
                border-radius=(theme::RADIUS_XL) padding=(theme::SPACE_6) gap=(theme::SPACE_3) {
                (ui::text_link(&game_href, "← Back to game"))
                (content)
            }
        },
        None,
    )
}

/// Shared chrome for the floating definition panel.
fn definition_panel_shell(children: impl Into<Vec<Container>>) -> Container {
    let children: Vec<Container> = children.into();
    // Swapped in as a whole element, so keep the wrapper (see `definition_page`).
    Container::from(container! {
        aside id="game-definition-layer" class="game-definition-panel" position="fixed"
            top=64 right=8 width=(DRAWER_WIDTH + 80) max-width="94%" max-height="72vh" overflow-y="auto"
            background=(theme::MENU_BG) color=(theme::INK)
            border=((theme::BORDER_STRONG, theme::EDGE_THIN)) border-bottom=((theme::BORDER_STRONG, 6))
            border-radius=(theme::RADIUS_XL) padding=(theme::SPACE_4) gap=(theme::SPACE_3) {
            (children)
        }
    })
}

fn drawer_close_button(target: &str) -> Container {
    ui::action_button(
        "Close",
        ButtonVariant::Secondary,
        ButtonSize::Small,
        ActionType::no_display_by_id(target),
    )
}

pub fn definition_panel(word: &str, lookup: &crate::DefinitionLookup) -> Container {
    let content = definition_content(word, lookup);
    definition_panel_shell(container! {
        div direction="row" justify-content="space-between" align-items="center" gap=(theme::SPACE_3) {
            (ui::eyebrow("WORD DEFINITION", theme::ACCENT))
            (drawer_close_button("game-definition-layer"))
        }
        (content)
    })
}

pub fn definition_panel_error(message: &str) -> Container {
    definition_panel_shell(container! {
        div direction="row" justify-content="space-between" align-items="center" gap=(theme::SPACE_3) {
            h2 font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) { "Definition unavailable" }
            (drawer_close_button("game-definition-layer"))
        }
        span color=(theme::INK_MUTED) { (message) }
    })
}

/// Renders the compact fallback command controls beneath the visual board.
#[must_use]
pub fn turn_composer(game: &AuthorizedGamePage) -> Container {
    let action = format!("/games/{}/turn", game.game_id);
    let command_id = uuid::Uuid::new_v4().to_string();
    let idempotency_key = uuid::Uuid::new_v4().to_string();
    ui::one(container! {
        section id="turn-composer" gap=(theme::SPACE_3) {
            h2 font-family=(theme::FONT_DISPLAY) { "Turn controls" }
            span color=(theme::INK_MUTED) { "Tap a rack tile, then tap an open board square. Use these controls for non-placement turns." }
            div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_2) {
                form hx-post=(action.as_str()) hx-target="#app-page" {
                    input type=hidden name="command" value="PASS";
                    input type=hidden name="command_id" value=(command_id.as_str());
                    input type=hidden name="idempotency_key" value=(idempotency_key.as_str());
                    input type=hidden name="expected_revision" value=(game.view.revision);
                    (ui::submit_button("Pass", ButtonVariant::Secondary, ButtonSize::Medium))
                }
                form hx-post=(action.as_str()) hx-target="#app-page" {
                    input type=hidden name="command" value="RESIGN";
                    input type=hidden name="command_id" value=(command_id.as_str());
                    input type=hidden name="idempotency_key" value=(idempotency_key.as_str());
                    input type=hidden name="expected_revision" value=(game.view.revision);
                    (ui::submit_button("Resign", ButtonVariant::Danger, ButtonSize::Medium))
                }
            }
        }
    })
}

pub fn compose_form_fields(
    game: &AuthorizedGamePage,
    draft: &TurnDraft,
    action: &str,
) -> Container {
    let encoded = draft_token(draft);
    ui::one(container! {
        input type=hidden name="action" value=(action);
        input type=hidden name="expected_revision" value=(game.view.revision);
        input type=hidden name="draft" value=(encoded);
    })
}

// ---------------------------------------------------------------------------
// Board
// ---------------------------------------------------------------------------

/// One round zoom button for the floating control cluster.
fn zoom_button(
    game: &AuthorizedGamePage,
    draft: &TurnDraft,
    action: &str,
    label: &str,
    font_size: i32,
) -> Container {
    let compose_action = format!("/games/{}/compose", game.game_id);
    let style = ui::button_style(ButtonVariant::Night);
    ui::one(container! {
        form hx-post=(compose_action.as_str()) hx-target="#app-page" {
            (compose_form_fields(game, draft, action))
            button type=submit width=(HEADER_CONTROL) height=(HEADER_CONTROL) min-width=(HEADER_CONTROL)
                background=(style.background) color=(style.ink)
                border=((style.edge, theme::EDGE_THIN)) border-bottom=((style.edge, 4))
                border-radius=(theme::RADIUS_PILL) align-items="center" justify-content="center"
                font-family=(theme::FONT_DISPLAY) font-size=(font_size) font-weight=bold cursor=pointer
                fx-hover=(ActionType::set_background_self(style.hover)) {
                (label)
            }
        }
    })
}

#[allow(clippy::too_many_lines)]
pub fn visual_board(
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
    let metrics = BoardMetrics::new(game.rules.board_size, draft.board_zoom);
    let square = metrics.square();
    let tile_font = metrics.scaled(0.5);
    let points_font = metrics.scaled(0.26);
    let premium_font = metrics.scaled(0.34);
    let star_font = metrics.scaled(0.58);
    let frame = metrics.frame();
    let score_bubble_position = score_anchor.map(|coordinate| {
        (
            calc_number(calc_add(
                metrics.offset(coordinate.x, -8.0),
                metrics.square.clone(),
            )),
            calc_number(calc_max(metrics.offset(coordinate.y, -14.0), calc_px(-4.0))),
        )
    });
    let picking_tile = draft.selected_tile.is_some() && draft.mode == TurnMode::Play;
    let zoom_in = zoom_button(game, draft, "ZOOM_IN", "+", theme::TEXT_LG);
    let zoom_reset = zoom_button(game, draft, "ZOOM_RESET", "Fit", theme::TEXT_XS);
    let zoom_out = zoom_button(game, draft, "ZOOM_OUT", "−", theme::TEXT_LG);
    ui::one(container! {
        section id="game-board" data-revision=(game.view.revision) data-board-zoom=(format!("{:?}", draft.board_zoom))
            position="absolute" top=0 right=0 bottom=0 left=0 direction="column" {
            div class="board-viewport" width="100%" min-height=0 flex=1
                overflow-x="auto" overflow-y="auto" {
                div id="board-scroll-content" class="board-scroll-content" width="100%" height="100%" min-width=(frame.clone())
                    min-height=(frame.clone()) align-items="center"
                    justify-content=(if_responsive(theme::MOBILE).then::<JustifyContent>(JustifyContent::Start).or_else(JustifyContent::Center))
                    padding-top=(if_responsive(theme::MOBILE).then::<i32>(theme::SPACE_1).or_else(0)) {
                    div class="board-frame" data-board-size=(game.rules.board_size)
                        data-board-fit-percent=(draft.board_zoom.fit_percent())
                        width=(frame.clone()) height=(frame.clone()) min-width=(frame.clone()) position="relative"
                        background=(theme::BOARD_FRAME) border=((theme::BOARD_FRAME_EDGE, 5))
                        border-radius=(theme::RADIUS_MD) gap=(BOARD_GAP) {
                    @for y in 0..game.rules.board_size {
                        div direction="row" gap=(BOARD_GAP) {
                            @for x in 0..game.rules.board_size {
                                @let coordinate = Coordinate::new(x, y);
                                @let committed = occupied.get(&coordinate).copied();
                                @let drafted = pending.get(&coordinate).copied();
                                @let premium = game.rules.premiums.get(&coordinate).copied();
                                @let required = feedback.guidance.required.contains(&coordinate);
                                @let eligible = feedback.guidance.eligible.contains(&coordinate);
                                @let latest = game.latest_play_coordinates.contains(&coordinate);
                                @let viewer_owned = game.viewer_play_coordinates.contains(&coordinate);
                                @if let Some((tile_id, blank_letter)) = drafted {
                                    @let rack_tile = game.view.rack.iter().find(|(id, _, _)| *id == tile_id);
                                    @let label = rack_tile.map_or('?', |(_, letter, _)| blank_letter.unwrap_or(*letter));
                                    @let draft_points = rack_tile.map(|(_, _, points)| *points);
                                    form hx-post=(action.as_str()) hx-target="#app-page" {
                                        (compose_form_fields(game, draft, "REMOVE_TILE"))
                                        input type=hidden name="tile_id" value=(tile_id);
                                        button type=submit class="board-square pending-square"
                                            width=(square.clone()) height=(square.clone())
                                            background=(theme::TILE_PENDING) color=(theme::TILE_PENDING_INK)
                                            border=((theme::TILE_PENDING_EDGE, 1)) border-bottom=((theme::TILE_PENDING_EDGE, 4))
                                            border-radius=5 align-items="center" justify-content="center"
                                            font-family=(theme::FONT_DISPLAY) font-weight=bold position="relative" cursor=pointer
                                            fx-hover=(ActionType::set_background_self(theme::WHITE))
                                            fx-click=(ActionType::set_background_self(theme::SQUARE)) {
                                            span font-size=(tile_font.clone()) { (label) }
                                            @if let Some(points) = draft_points {
                                                span class="board-tile-points" position="absolute" right=2 bottom=0 font-size=(points_font.clone()) { (points) }
                                            }
                                        }
                                    }
                                } @else if let Some((letter, points)) = committed {
                                    div class=(if latest { "board-square committed-square latest-move-square" } else if viewer_owned { "board-square committed-square viewer-owned-square" } else { "board-square committed-square" })
                                        width=(square.clone()) height=(square.clone())
                                        background=(if latest { theme::TILE_HI } else { theme::TILE }) color=(theme::TILE_INK)
                                        border=((if latest { theme::LATEST_MOVE } else { theme::TILE_EDGE }, if latest { 2 } else { 1 }))
                                        border-bottom=((if latest { theme::PRIMARY_EDGE } else { theme::TILE_EDGE }, 4))
                                        border-radius=5 align-items="center" justify-content="center"
                                        font-family=(theme::FONT_DISPLAY) font-weight=bold position="relative" user-select="none" {
                                        @if viewer_owned {
                                            span class="viewer-tile-marker" position="absolute" top=2 left=2
                                                width=4 height=4 background=(theme::TILE_EDGE) opacity=0.7 border-radius=(theme::RADIUS_PILL) { }
                                        }
                                        span font-size=(tile_font.clone()) { (letter) }
                                        span class="board-tile-points" position="absolute" right=2 bottom=0 font-size=(points_font.clone()) { (points) }
                                    }
                                } @else {
                                    @let (background, ink, label) = square_style(coordinate, game.rules.start, premium);
                                    form hx-post=(action.as_str()) hx-target="#app-page" {
                                        (compose_form_fields(game, draft, "PLACE_TILE"))
                                        input type=hidden name="x" value=(x);
                                        input type=hidden name="y" value=(y);
                                        button type=submit class="board-square open-square" data-x=(x) data-y=(y)
                                            width=(square.clone()) height=(square.clone()) background=(background) color=(ink)
                                            border=(if required { (theme::SUNNY, 2) } else { (background, 0) })
                                            border-radius=4 position="relative" align-items="center" justify-content="center"
                                            font-family=(theme::FONT_DISPLAY) font-weight=bold cursor=pointer
                                            fx-hover=(ActionType::set_background_self(if picking_tile { theme::TILE_PENDING } else { theme::WHITE }))
                                            fx-click=(if picking_tile { ActionType::set_background_self(theme::TILE_PENDING) } else { ActionType::NoOp }) {
                                            @if required {
                                                span class="required-square-highlight" position="absolute" top=0 left=0
                                                    width="100%" height="100%" background=(theme::SUNNY) opacity=0.45 border-radius=3 { }
                                            } @else if eligible {
                                                span class="eligible-square-highlight" position="absolute" top=0 left=0
                                                    width="100%" height="100%" background=(theme::PRIMARY) opacity=0.3 border-radius=3 { }
                                            }
                                            @if !label.is_empty() {
                                                span position="relative" font-size=(if label == "★" { star_font.clone() } else { premium_font.clone() }) { (label) }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    @if let (Some(score), Some((left, top))) = (score, score_bubble_position) {
                        span class=(if invalid_score { "draft-score-bubble invalid-draft-score" } else { "draft-score-bubble" })
                            position="absolute" left=(left) top=(top) min-width=30 height=26 padding-x=7
                            background=(if invalid_score { theme::DANGER } else { theme::SUNNY })
                            color=(if invalid_score { theme::ON_DANGER } else { theme::ON_SUNNY })
                            border=((theme::WHITE, 2)) border-bottom=((if invalid_score { theme::DANGER_EDGE } else { theme::SUNNY_EDGE }, 4))
                            border-radius=(theme::RADIUS_PILL) align-items="center" justify-content="center"
                            font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_SM) font-weight=bold user-select="none" {
                            @if invalid_score { "✕" } @else { (score) }
                        }
                    }
                }
            }
            }
            div id="board-zoom-controls" class="board-zoom-controls" position="absolute" right=10 bottom=10
                direction=(if_responsive(theme::MOBILE).then::<LayoutDirection>(LayoutDirection::Row).or_else(LayoutDirection::Column))
                align-items="center" gap=6 padding=4 background=(theme::NIGHT_DEEP)
                border=((theme::NIGHT_LINE, 1)) border-radius=(theme::RADIUS_PILL) opacity=0.94 {
                (zoom_in)
                (zoom_reset)
                (zoom_out)
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Rack
// ---------------------------------------------------------------------------

pub fn visual_rack(game: &AuthorizedGamePage, draft: &TurnDraft) -> Container {
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
    let tile = rack_tile_size();
    let tile_size = calc_number(tile.clone());
    let face_font = calc_number(calc_times(tile.clone(), 0.4));
    let points_font = calc_number(calc_times(tile, 0.19));
    ui::one(container! {
        section id="player-rack" width="100%" max-width="100%" position="relative" {
            div class="rack-tray" width="100%" max-width="100%" direction="row" overflow-x="hidden" overflow-y="hidden"
                justify-content="center" align-items="end" gap=5
                background=(theme::NIGHT_DEEP) border=((theme::NIGHT_BORDER, theme::EDGE_THIN))
                border-radius=(theme::RADIUS_LG) padding-top=12 padding-bottom=8 padding-x=6 {
                @for (tile_id, letter, points) in rack {
                    @let placed = draft.placements.iter().any(|placement| placement.tile_id == *tile_id);
                    @let selected = draft.selected_tile == Some(*tile_id) || draft.rack_tile == Some(*tile_id);
                    @let exchange_selected = draft.exchange_tiles.contains(tile_id);
                    @let face = if *letter == ' ' { "?".to_string() } else { letter.to_string() };
                    @let face_color = if selected { theme::TILE_HI } else if exchange_selected { theme::TILE_EXCHANGE } else if placed { theme::NIGHT_RAISED } else { theme::TILE };
                    @let edge_color = if selected { theme::WHITE } else if exchange_selected { theme::TILE_EXCHANGE_EDGE } else if placed { theme::NIGHT_LINE } else { theme::TILE_EDGE };
                    @let ink_color = if placed && !selected { theme::NIGHT_INK_MUTED } else { theme::TILE_INK };
                    @let lift = if selected { theme::LIFT_OFFSET } else if exchange_selected { theme::PRESS_OFFSET } else { 0 };
                    @if can_compose || draft.mode == TurnMode::Play {
                        @let rack_action = rack_action(draft);
                        form hx-post=(action.as_str()) hx-target="#app-page"
                            width=(tile_size.clone()) height=(tile_size.clone()) min-width=0 {
                            (compose_form_fields(game, draft, rack_action))
                            input type=hidden name="tile_id" value=(tile_id);
                            button type=submit class=(if selected || exchange_selected { "rack-tile rack-tile-selected" } else { "rack-tile" }) data-tile-id=(tile_id)
                                width=(tile_size.clone()) height=(tile_size.clone())
                                min-width=0 overflow-x="hidden" overflow-y="hidden"
                                background=(face_color) color=(ink_color)
                                border=((edge_color, theme::EDGE_THIN))
                                border-bottom=((if selected { theme::TILE_EDGE } else { edge_color }, if exchange_selected { theme::EDGE_PRESSED } else { theme::EDGE_DEPTH }))
                                border-radius=(theme::RADIUS_SM + 2) align-items="center" justify-content="center"
                                translate-y=(lift)
                                position="relative" font-family=(theme::FONT_DISPLAY) font-weight=bold
                                opacity=(if placed { 0.55 } else { 1.0 }) cursor=pointer user-select="none"
                                fx-hover=(ActionType::set_background_self(theme::TILE_HI))
                                fx-click=(ActionType::set_background_self(theme::TILE_PENDING)) {
                                span class="rack-tile-face" font-size=(face_font.clone()) { (face) }
                                span class="rack-tile-points" position="absolute" right=3 bottom=1
                                    font-size=(points_font.clone()) { (points) }
                            }
                        }
                    } @else {
                        div class="rack-tile" data-tile-id=(tile_id)
                            width=(tile_size.clone()) height=(tile_size.clone())
                            min-width=0 overflow-x="hidden" overflow-y="hidden"
                            background=(theme::TILE) color=(theme::TILE_INK)
                            border=((theme::TILE_EDGE, theme::EDGE_THIN)) border-bottom=((theme::TILE_EDGE, theme::EDGE_DEPTH))
                            border-radius=(theme::RADIUS_SM + 2) align-items="center" justify-content="center"
                            position="relative" font-family=(theme::FONT_DISPLAY) font-weight=bold user-select="none" {
                            span class="rack-tile-face" font-size=(face_font.clone()) { (face) }
                            span class="rack-tile-points" position="absolute" right=3 bottom=1
                                font-size=(points_font.clone()) { (points) }
                        }
                    }
                }
            }
        }
    })
}

pub fn blank_letter_picker_open(draft: &TurnDraft, rack: &[(u16, char, u8)]) -> bool {
    draft.selected_blank_letter.is_none()
        && draft.selected_tile.is_some_and(|tile_id| {
            rack.iter()
                .any(|(id, letter, _)| *id == tile_id && *letter == ' ')
        })
}

pub fn visual_blank_letter_layer(game: &AuthorizedGamePage, draft: &TurnDraft) -> Container {
    let action = format!("/games/{}/compose", game.game_id);
    let selected_blank_needs_letter = blank_letter_picker_open(draft, &game.view.rack);
    let blank_tile = ui::tile_face("?", None, 36);
    ui::one(container! {
        @if selected_blank_needs_letter {
            section class="blank-letter-layer" position="fixed" top=0 right=0 bottom=0 left=0
                width="100vw" height="100dvh" align-items="center" justify-content="end"
                background=(theme::SCRIM) padding=(theme::SPACE_2) {
                section width="100%" max-width=(theme::PAGE_NARROW + 40) max-height="78dvh" overflow-y="auto"
                    background=(theme::MENU_BG) color=(theme::INK)
                    border=((theme::BORDER_STRONG, theme::EDGE_THIN)) border-bottom=((theme::BORDER_STRONG, 6))
                    border-radius=(theme::RADIUS_XL) padding=(theme::SPACE_4) gap=(theme::SPACE_3) {
                    div width="100%" direction="row" justify-content="space-between" align-items="center" gap=(theme::SPACE_2) {
                        div direction="row" align-items="center" gap=(theme::SPACE_3) min-width=0 {
                            (blank_tile)
                            div gap=2 min-width=0 {
                                span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold { "Choose the blank tile’s letter:" }
                                span color=(theme::INK_MUTED) font-size=(theme::TEXT_XS) { "Blanks score 0 points but can be any letter." }
                            }
                        }
                        form hx-post=(action.as_str()) hx-target="#app-page" {
                            (compose_form_fields(game, draft, "CANCEL_TILE_PICK"))
                            (ui::submit_button("Cancel", ButtonVariant::Secondary, ButtonSize::Small))
                        }
                    }
                    div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) justify-content="center" gap=6 {
                        @for letter in 'A'..='Z' {
                            @let chosen = draft.selected_blank_letter == Some(letter);
                            form hx-post=(action.as_str()) hx-target="#app-page" {
                                (compose_form_fields(game, draft, "CHOOSE_BLANK_LETTER"))
                                input type=hidden name="letter" value=(letter.to_string());
                                button type=submit data-blank-letter=(letter.to_string()) width=44 height=44 min-width=44
                                    background=(if chosen { theme::BLANK_LETTER_CHOSEN } else { theme::TILE })
                                    color=(theme::TILE_INK)
                                    border=((theme::TILE_EDGE, 1)) border-bottom=((theme::TILE_EDGE, 4))
                                    border-radius=(theme::RADIUS_SM + 1) align-items="center" justify-content="center"
                                    font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG + 2) font-weight=bold cursor=pointer
                                    fx-hover=(ActionType::set_background_self(theme::TILE_HI))
                                    fx-click=(ActionType::set_background_self(theme::BLANK_LETTER_CHOSEN)) {
                                    (letter)
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Turn actions
// ---------------------------------------------------------------------------

/// A compact icon+label secondary button for the dock (Shuffle, Recall, Cancel).
fn dock_submit_button(label: &str) -> Container {
    ui::submit_button(label, ButtonVariant::Secondary, ButtonSize::Small)
}

/// The dominant call to action in the dock ("Play · 12", "Confirm exchange").
fn hero_submit_button(label: &str, variant: ButtonVariant) -> Container {
    let style = ui::button_style(variant);
    let label = label.to_string();
    ui::one(container! {
        button type=submit class="primary-turn-action" width="100%"
            padding-y=(theme::SPACE_3) padding-x=(theme::SPACE_4)
            background=(style.background) color=(style.ink)
            border=((style.edge, theme::EDGE_THIN)) border-bottom=((style.edge, 6))
            border-radius=(theme::RADIUS_LG) align-items="center" justify-content="center" text-align="center"
            font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG + 2) font-weight=bold cursor=pointer
            white-space="preserve"
            fx-hover=(ActionType::set_background_self(style.hover)) {
            (label)
        }
    })
}

/// A full-width menu row button inside the "More" sheet.
fn sheet_submit_button(label: &str, variant: ButtonVariant) -> Container {
    let mut button = ui::submit_button(label, variant, ButtonSize::Medium);
    button.width = Some(Number::IntegerPercent(100));
    button
}

#[allow(clippy::too_many_lines)]
pub fn visual_turn_actions(game: &AuthorizedGamePage, draft: &TurnDraft) -> Container {
    let turn_action = format!("/games/{}/turn", game.game_id);
    let compose_action = format!("/games/{}/compose", game.game_id);
    let command_id = uuid::Uuid::new_v4().to_string();
    let idempotency_key = uuid::Uuid::new_v4().to_string();
    let viewer_turn = !game.completed && game.view.active_player == game.viewer_player;
    let play_score = draft_feedback(game, draft)
        .candidate
        .filter(wwmtf_game_domain::CandidatePlayAnalysis::is_valid)
        .map(|candidate| candidate.play.score);
    let play_label =
        play_score.map_or_else(|| "Play".to_string(), |score| format!("Play · {score}"));
    let more_style = ui::button_style(ButtonVariant::Night);
    ui::one(container! {
        section id="turn-actions" class="turn-composer action-hud" width="100%" max-width="100%" position="relative" {
            @if draft.mode == TurnMode::Exchange {
                div class="primary-action-row" direction="row" align-items="center" gap=(theme::SPACE_2) {
                    form hx-post=(compose_action.as_str()) hx-target="#app-page" flex=1 min-width=0 {
                        (compose_form_fields(game, draft, "REVIEW_EXCHANGE"))
                        (hero_submit_button("Review exchange", ButtonVariant::Sunny))
                    }
                    form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                        (compose_form_fields(game, draft, "CANCEL_MODE"))
                        (dock_submit_button("Cancel"))
                    }
                }
            } @else if draft.mode == TurnMode::ConfirmExchange {
                div class="primary-action-row" direction="row" align-items="center" gap=(theme::SPACE_2) {
                    form hx-post=(turn_action.as_str()) hx-target="#app-page" flex=1 min-width=0 {
                        input type=hidden name="command" value="EXCHANGE";
                        input type=hidden name="command_id" value=(command_id.as_str());
                        input type=hidden name="idempotency_key" value=(idempotency_key.as_str());
                        input type=hidden name="expected_revision" value=(game.view.revision);
                        @for (index, tile_id) in draft.exchange_tiles.iter().enumerate() {
                            input type=hidden name=(format!("tile_{index}")) value=(tile_id);
                        }
                        (hero_submit_button("Confirm exchange", ButtonVariant::Sunny))
                    }
                    form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                        (compose_form_fields(game, draft, "CANCEL_MODE"))
                        (dock_submit_button("Cancel"))
                    }
                }
            } @else if draft.mode == TurnMode::ConfirmPass {
                (confirmed_command_forms(game, draft, "PASS", "Confirm pass", &command_id, &idempotency_key))
            } @else if draft.mode == TurnMode::ConfirmResign {
                (confirmed_command_forms(game, draft, "RESIGN", "Confirm resignation", &command_id, &idempotency_key))
            } @else {
                div class="primary-action-row" direction="row" align-items="center" gap=(theme::SPACE_2) {
                    @if !game.completed {
                        form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                            (compose_form_fields(game, draft, "SHUFFLE_RACK"))
                            (dock_submit_button("⇄ Shuffle"))
                        }
                    }
                    @if !draft.placements.is_empty() {
                        form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                            (compose_form_fields(game, draft, "CLEAR"))
                            (dock_submit_button("↺ Recall"))
                        }
                    }
                    @if !draft.placements.is_empty() && viewer_turn {
                        form hx-post=(turn_action.as_str()) hx-target="#app-page" flex=1 min-width=0 {
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
                            (hero_submit_button(&play_label, ButtonVariant::Primary))
                        }
                    } @else {
                        div class="play-placeholder" flex=1 min-width=0 align-items="center" justify-content="center"
                            padding-y=(theme::SPACE_3) border=((theme::NIGHT_LINE, theme::EDGE_THIN))
                            border-radius=(theme::RADIUS_LG) color=(theme::NIGHT_INK_MUTED)
                            font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_MD) font-weight=bold
                            white-space="preserve" text-align="center" {
                            "Place tiles to play"
                        }
                    }
                    @if !game.completed && viewer_turn {
                        button type=button fx-click=(ActionType::toggle_display_by_id("more-turn-actions-menu"))
                            padding-y=6 padding-x=(theme::SPACE_3)
                            background=(more_style.background) color=(more_style.ink)
                            border=((theme::NIGHT_LINE, theme::EDGE_THIN)) border-bottom=((more_style.edge, 3))
                            border-radius=(theme::RADIUS_PILL) font-size=(theme::TEXT_SM) font-weight=bold
                            white-space="preserve" cursor=pointer
                            fx-hover=(ActionType::set_background_self(more_style.hover)) { "More ···" }
                    }
                }
                @if !game.completed && viewer_turn {
                    div id="more-turn-actions-menu" hidden position="absolute" bottom="100%" right=0
                        width=240 max-width="100%" margin-bottom=(theme::SPACE_2)
                        background=(theme::MENU_BG) color=(theme::INK)
                        border=((theme::BORDER_STRONG, theme::EDGE_THIN)) border-bottom=((theme::BORDER_STRONG, 6))
                        border-radius=(theme::RADIUS_LG) padding=(theme::SPACE_3) gap=(theme::SPACE_2) {
                        div direction="row" justify-content="space-between" align-items="center" {
                            (ui::eyebrow("MORE MOVES", theme::ACCENT))
                            button type=button fx-click=(ActionType::no_display_by_id("more-turn-actions-menu"))
                                color=(theme::INK_MUTED) font-size=(theme::TEXT_LG) font-weight=bold cursor=pointer
                                padding-x=6 { "×" }
                        }
                        @if game.exchange_available {
                            form hx-post=(compose_action.as_str()) hx-target="#app-page" width="100%" {
                                (compose_form_fields(game, draft, "BEGIN_EXCHANGE"))
                                (sheet_submit_button("⇅ Exchange", ButtonVariant::Secondary))
                            }
                        }
                        form hx-post=(compose_action.as_str()) hx-target="#app-page" width="100%" {
                            (compose_form_fields(game, draft, "CONFIRM_PASS"))
                            (sheet_submit_button("→ Pass", ButtonVariant::Secondary))
                        }
                        form hx-post=(compose_action.as_str()) hx-target="#app-page" width="100%" {
                            (compose_form_fields(game, draft, "CONFIRM_RESIGN"))
                            (sheet_submit_button("⚑ Resign", ButtonVariant::Danger))
                        }
                    }
                }
            }
        }
    })
}

pub fn confirmed_command_forms(
    game: &AuthorizedGamePage,
    draft: &TurnDraft,
    command: &str,
    label: &str,
    command_id: &str,
    idempotency_key: &str,
) -> Container {
    let turn_action = format!("/games/{}/turn", game.game_id);
    let compose_action = format!("/games/{}/compose", game.game_id);
    let variant = if command == "RESIGN" {
        ButtonVariant::Danger
    } else {
        ButtonVariant::Sunny
    };
    ui::one(container! {
        div class="primary-action-row" direction="row" align-items="center" gap=(theme::SPACE_2) {
            form hx-post=(turn_action.as_str()) hx-target="#app-page" flex=1 min-width=0 {
                input type=hidden name="command" value=(command);
                input type=hidden name="command_id" value=(command_id);
                input type=hidden name="idempotency_key" value=(idempotency_key);
                input type=hidden name="expected_revision" value=(game.view.revision);
                (hero_submit_button(label, variant))
            }
            form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                (compose_form_fields(game, draft, "CANCEL_MODE"))
                (dock_submit_button("Cancel"))
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Scoreboard and results
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
pub fn player_scoreboard_component(game: &AuthorizedGamePage) -> Container {
    let viewer_turn = !game.completed && game.view.active_player == game.viewer_player;
    let turn = if game.completed {
        "Game complete".to_string()
    } else if viewer_turn {
        "Your move".to_string()
    } else {
        format!("{}'s move", active_player_name(game))
    };
    let pills = game
        .participants
        .iter()
        .enumerate()
        .map(|(index, participant)| {
            let public = game
                .view
                .players
                .iter()
                .find(|player| player.player_id == participant.player_id);
            let score = public.map_or(0, |player| player.score);
            let is_viewer = participant.player_id == game.viewer_player;
            let active = public.is_some_and(|player| player.active);
            let leader = public.is_some_and(|player| player.leader);
            let playing = !game.completed && participant.player_id == game.view.active_player;
            let seat = theme::seat_color(index);
            let uuid = participant.player_id.as_uuid();
            let avatar = ui::avatar(
                &participant.display_name,
                participant.avatar_url.as_deref(),
                seat,
                HEADER_AVATAR,
            );
            let name = if is_viewer {
                format!("{} (you)", participant.display_name)
            } else {
                participant.display_name.clone()
            };
            // On phones the viewer's own pill shows just avatar and score to leave room for
            // opponents' short names.
            let short_name = if is_viewer {
                String::new()
            } else {
                let mut letters = participant.display_name.chars();
                let short = letters.by_ref().take(6).collect::<String>();
                if letters.next().is_some() {
                    format!("{short}…")
                } else {
                    short
                }
            };
            let (fill, ink, edge) = if playing {
                (seat.fill, seat.ink, seat.edge)
            } else {
                (theme::NIGHT_RAISED, theme::NIGHT_INK, theme::NIGHT_BORDER)
            };
            ui::one(container! {
                div id=(format!("player-scoreboard-{uuid}"))
                    direction="row" align-items="center" gap=6
                    background=(fill) color=(ink)
                    border=((if playing { theme::WHITE } else { theme::NIGHT_LINE }, theme::EDGE_THIN))
                    border-bottom=((edge, 4))
                    border-radius=(theme::RADIUS_PILL) padding-y=2 padding-left=2 padding-right=10
                    opacity=(if active || game.completed { 1.0 } else { 0.5 }) {
                    (avatar)
                    @if !short_name.is_empty() {
                        span id=(format!("player-scoreboard-short-{uuid}"))
                            font-size=(theme::TEXT_XS) font-weight=bold white-space="preserve"
                            hidden=(if_responsive(theme::MOBILE).then::<bool>(false).or_else(true)) {
                            (short_name)
                        }
                    }
                    span id=(format!("player-scoreboard-name-{uuid}"))
                        font-size=(theme::TEXT_XS) font-weight=bold white-space="preserve"
                        text-overflow="ellipsis" overflow-x="hidden" max-width=110
                        hidden=(if_responsive(theme::MOBILE).then::<bool>(true).or_else(false)) {
                        (name)
                    }
                    span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold white-space="preserve" {
                        @if game.completed && leader { "★ " }
                        (score)
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    ui::one(container! {
        section id="game-awareness" class="player-hud scoreboard-hud" min-width=0 flex=1
            align-items="center" gap=3 overflow-x="hidden" overflow-y="hidden" {
            div id="participant-scoreboard" direction="row" gap=6 max-width="100%"
                justify-content=(if_responsive(theme::MOBILE).then::<JustifyContent>(JustifyContent::Start).or_else(JustifyContent::Center))
                min-width=0 overflow-x="auto" overflow-y="hidden" padding-bottom=1 {
                (pills)
            }
            div direction="row" align-items="center" justify-content="center" gap=6 max-width="100%" {
                span id="live-status-connected" hidden color=(theme::LATEST_MOVE) font-size=(theme::TEXT_XS) font-weight=bold
                    fx-global-shared-state-connected=(live_status_action("live-status-connected")) { "●" }
                span id="live-status-subscribed" hidden fx-global-shared-state-subscribed=(live_status_action("live-status-connected")) { }
                span id="live-status-disconnected" hidden color=(theme::DANGER) font-size=(theme::TEXT_XS) font-weight=bold
                    fx-global-shared-state-disconnected=(live_status_action("live-status-disconnected")) { "● Offline" }
                span id="named-turn-status" font-size=(theme::TEXT_XS) font-weight=bold white-space="preserve"
                    text-overflow="ellipsis" overflow-x="hidden"
                    color=(if viewer_turn { theme::PRIMARY } else { theme::NIGHT_INK_MUTED }) { (turn) }
            }
        }
    })
}

fn completed_outcome(game: &AuthorizedGamePage) -> (String, bool) {
    let leaders = game
        .participants
        .iter()
        .filter(|participant| {
            game.view
                .players
                .iter()
                .any(|player| player.player_id == participant.player_id && player.leader)
        })
        .collect::<Vec<_>>();
    let viewer_won = leaders.len() == 1 && leaders[0].player_id == game.viewer_player;
    let outcome = if leaders.len() > 1 {
        format!(
            "Co-leaders: {}",
            leaders
                .iter()
                .map(|leader| leader.display_name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else if let Some(leader) = leaders.first() {
        format!("{} won", leader.display_name)
    } else {
        "Game complete".to_string()
    };
    (outcome, viewer_won)
}

#[allow(clippy::too_many_lines)]
pub fn completed_game_summary(game: &AuthorizedGamePage) -> Container {
    let mut standings = game
        .participants
        .iter()
        .enumerate()
        .filter_map(|(seat, participant)| {
            game.view
                .players
                .iter()
                .find(|player| player.player_id == participant.player_id)
                .map(|player| {
                    (
                        participant,
                        player.score,
                        game.final_score_adjustments
                            .get(&participant.player_id)
                            .copied()
                            .unwrap_or_default(),
                        player.leader,
                        theme::seat_color(seat),
                    )
                })
        })
        .collect::<Vec<_>>();
    standings.sort_by_key(|standing| std::cmp::Reverse(standing.1));
    // Competition ranking: equal scores share a rank ("1, 1, 3").
    let ranks = standings
        .iter()
        .enumerate()
        .scan((0_usize, None), |(rank, previous), (index, standing)| {
            if *previous != Some(standing.1) {
                *rank = index + 1;
                *previous = Some(standing.1);
            }
            Some(*rank)
        })
        .collect::<Vec<_>>();
    let (outcome, viewer_won) = completed_outcome(game);
    let banner_word = if viewer_won { "WIN" } else { "GG" };
    let banner = banner_word
        .chars()
        .enumerate()
        .map(|(index, letter)| {
            let mut tile = ui::tile_face(&letter.to_string(), None, 44);
            if index % 2 == 1 {
                tile.translate_y = Some(Number::Integer(5));
            }
            tile
        })
        .collect::<Vec<_>>();
    let rematch_action = format!("/games/{}/rematch", game.game_id);
    let rematch_style = ui::button_style(ButtonVariant::Primary);
    ui::one(container! {
        section id="completed-game-summary" position="fixed" top=0 right=0 bottom=0 left=0
            width="100vw" height="100dvh" align-items="center" justify-content="center"
            background=(theme::SCRIM) padding=(theme::SPACE_4) {
            section class="results-card" width="100%" max-width=(theme::PAGE_NARROW - 40) max-height="88dvh" overflow-y="auto"
                background=(theme::MENU_BG) color=(theme::INK)
                border=((theme::BORDER_STRONG, theme::EDGE_THIN)) border-bottom=((theme::BORDER_STRONG, 7))
                border-radius=(theme::RADIUS_XL) padding=(theme::SPACE_5) gap=(theme::SPACE_4) align-items="center" {
                div width="100%" direction="row" justify-content="end" {
                    (ui::action_button("Close", ButtonVariant::Secondary, ButtonSize::Medium, ActionType::no_display_by_id("completed-game-summary")))
                }
                div direction="row" gap=6 align-items="start" { (banner) }
                div align-items="center" gap=2 text-align="center" {
                    (ui::eyebrow("FINAL SCORES", theme::ACCENT))
                    span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XXL) font-weight=bold text-align="center" { (outcome) }
                    @if let Some(reason) = &game.completion_reason {
                        span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) { (reason.as_str()) }
                    }
                }
                div id="final-standings" width="100%" overflow-x="hidden" overflow-y="hidden" gap=(theme::SPACE_2) {
                    @for (rank, (participant, score, adjustment, leader, seat)) in ranks.iter().zip(standings.iter()) {
                        @let is_viewer = participant.player_id == game.viewer_player;
                        @let avatar = ui::avatar(&participant.display_name, participant.avatar_url.as_deref(), *seat, 36);
                        div direction="row" align-items="center" gap=(theme::SPACE_3)
                            background=(if *leader { theme::SUNNY_SOFT } else { theme::SURFACE })
                            border=((if *leader { theme::SUNNY } else { theme::BORDER }, theme::EDGE_THIN))
                            border-bottom=((if *leader { theme::SUNNY_EDGE } else { theme::BORDER_STRONG }, 4))
                            border-radius=(theme::RADIUS_LG) padding-y=(theme::SPACE_2) padding-x=(theme::SPACE_3) {
                            span width=30 min-width=30 height=30 border-radius=(theme::RADIUS_PILL)
                                align-items="center" justify-content="center"
                                background=(if *leader { theme::SUNNY } else { theme::SURFACE_LAVENDER })
                                color=(if *leader { theme::ON_SUNNY } else { theme::INK_MUTED })
                                font-family=(theme::FONT_DISPLAY) font-weight=bold {
                                @if *leader { "★" } @else { (rank) }
                            }
                            (avatar)
                            div flex=1 min-width=0 gap=1 {
                                span font-weight=bold white-space="preserve" text-overflow="ellipsis" overflow-x="hidden" {
                                    (participant.display_name.as_str())
                                    @if is_viewer { " (you)" }
                                    @if *leader { " · leader" }
                                }
                                @if *adjustment != 0 {
                                    span font-size=(theme::TEXT_XS) font-weight=bold
                                        color=(if *adjustment > 0 { theme::PRIMARY_INK } else { theme::DANGER_INK }) {
                                        "Rack adjustment " (format!("{adjustment:+}"))
                                    }
                                }
                            }
                            span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XL) font-weight=bold white-space="preserve" {
                                (score)
                            }
                        }
                    }
                }
                @if let Some(invite) = &game.rematch_invite {
                    div id="rematch-offer" width="100%" align-items="center" gap=(theme::SPACE_2)
                        background=(theme::PRIMARY_SOFT) color=(theme::PRIMARY_INK)
                        border=((theme::PRIMARY, theme::EDGE_THIN)) border-radius=(theme::RADIUS_LG)
                        padding=(theme::SPACE_3) text-align="center" {
                        span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold {
                            (invite.inviter_display_name.as_str()) " wants a rematch!"
                        }
                    }
                }
                div width="100%" direction="row" gap=(theme::SPACE_2) align-items="center" {
                    @if let Some(invite) = &game.rematch_invite {
                        form hx-post=(format!("/lobbies/{}/action", invite.lobby_id)) hx-target="#app-page" flex=1 min-width=0 {
                            input type=hidden name="action" value="ACCEPT_INVITE";
                            button type=submit width="100%" padding-y=(theme::SPACE_3) padding-x=(theme::SPACE_4)
                                background=(rematch_style.background) color=(rematch_style.ink)
                                border=((rematch_style.edge, theme::EDGE_THIN)) border-bottom=((rematch_style.edge, 6))
                                border-radius=(theme::RADIUS_LG) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG)
                                font-weight=bold cursor=pointer align-items="center" justify-content="center"
                                fx-hover=(ActionType::set_background_self(rematch_style.hover)) { "Join rematch" }
                        }
                    } @else {
                    form hx-post=(rematch_action) hx-target="#app-page" flex=1 min-width=0 {
                        button type=submit width="100%" padding-y=(theme::SPACE_3) padding-x=(theme::SPACE_4)
                            background=(rematch_style.background) color=(rematch_style.ink)
                            border=((rematch_style.edge, theme::EDGE_THIN)) border-bottom=((rematch_style.edge, 6))
                            border-radius=(theme::RADIUS_LG) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG)
                            font-weight=bold cursor=pointer align-items="center" justify-content="center"
                            fx-hover=(ActionType::set_background_self(rematch_style.hover)) { "Rematch" }
                    }
                    }
                    (ui::link_button("/", "Back to games", ButtonVariant::Secondary, ButtonSize::Medium))
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Drawers: rules, bag, menu
// ---------------------------------------------------------------------------

pub fn rules_component(game: &AuthorizedGamePage) -> Container {
    let exchange_requirement = game.rules.minimum_tiles_for_exchange;
    let rack_size = game.rules.rack_size;
    let full_rack_bonus = game.rules.full_rack_bonus;
    let scoreless_turn_limit = game.rules.scoreless_turn_limit;
    ui::one(container! {
        details id="game-rules" width="100%" background=(theme::SURFACE) border=((theme::BORDER, theme::EDGE_THIN))
            border-radius=(theme::RADIUS_MD) padding=(theme::SPACE_3) {
            summary cursor="pointer" font-weight=bold color=(theme::INK) { "How to play" }
            div padding-top=(theme::SPACE_3) gap=(theme::SPACE_2) color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) {
                span { "Place tiles in one row or column to form connected words. The opening play must cover the center star. All formed words must be accepted by this game’s pinned dictionary." }
                span { "A tile scores its printed value. 2L and 3L multiply a newly placed tile; 2W and 3W multiply the whole word. Premium squares apply only when first covered." }
                span { "Playing all " (rack_size) " rack tiles adds a " (full_rack_bonus) "-point full-rack bonus." }
                span { "Exchange replaces selected tiles and ends your turn. It is available only while at least " (exchange_requirement) " tiles remain in the reserve; exchanged tile identities stay private." }
                span { "Passing ends your turn without playing. Three uninterrupted complete rounds of passes by all active players end the game. For the classic two-player profile this is " (scoreless_turn_limit) " consecutive passes; the threshold scales with active membership." }
                span { "Resigning removes that player from turn rotation. Their rack stays locked and hidden while play continues; resignation ends the game only when fewer than two active players remain." }
                span { "When the game ends, remaining rack values are applied according to the completion reason. Final adjusted scores determine the ordered standings, and equal highest scores produce co-leaders." }
            }
        }
    })
}

/// One swatch in the board key legend.
fn legend_item(
    fill: &'static str,
    ink: &'static str,
    edge: &'static str,
    label: &str,
    meaning: &str,
) -> Container {
    let label = label.to_string();
    let meaning = meaning.to_string();
    ui::one(container! {
        div direction="row" align-items="center" gap=(theme::SPACE_2) width="47%" min-width=130 {
            span width=28 height=28 min-width=28 border-radius=5 background=(fill) color=(ink)
                border=((edge, 1)) border-bottom=((edge, 3))
                align-items="center" justify-content="center"
                font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XS) font-weight=bold { (label) }
            span font-size=(theme::TEXT_XS) font-weight=bold color=(theme::INK_MUTED) { (meaning) }
        }
    })
}

fn board_key_component() -> Container {
    ui::one(container! {
        section id="board-key" gap=(theme::SPACE_2) {
            h3 font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_MD) { "Board key" }
            div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_2) {
                (legend_item(theme::PREMIUM_DL, theme::PREMIUM_DL_INK, theme::PREMIUM_DL, "2L", "Double letter"))
                (legend_item(theme::PREMIUM_TL, theme::PREMIUM_TL_INK, theme::PREMIUM_TL, "3L", "Triple letter"))
                (legend_item(theme::PREMIUM_DW, theme::PREMIUM_DW_INK, theme::PREMIUM_DW, "2W", "Double word"))
                (legend_item(theme::PREMIUM_TW, theme::PREMIUM_TW_INK, theme::PREMIUM_TW, "3W", "Triple word"))
                (legend_item(theme::PREMIUM_START, theme::PREMIUM_START_INK, theme::PREMIUM_START, "★", "Start square"))
                (legend_item(theme::TILE_PENDING, theme::TILE_PENDING_INK, theme::TILE_PENDING_EDGE, "A", "Your draft"))
                (legend_item(theme::TILE_HI, theme::TILE_INK, theme::LATEST_MOVE, "A", "Latest move"))
                (legend_item(theme::SQUARE, theme::PRIMARY_EDGE, theme::PRIMARY, "", "Playable spot"))
            }
        }
    })
}

pub fn live_status_action(visible_id: &str) -> ActionType {
    ActionType::Multi(vec![
        ActionType::no_display_by_id("live-status-connecting"),
        ActionType::no_display_by_id("live-status-connected"),
        ActionType::no_display_by_id("live-status-reconnecting"),
        ActionType::no_display_by_id("live-status-disconnected"),
        ActionType::display_by_id(visible_id),
    ])
}

pub fn remaining_tiles_component(game: &AuthorizedGamePage) -> Container {
    let faces = game.view.remaining_tile_faces.as_deref();
    ui::one(container! {
        section id="remaining-tiles" gap=(theme::SPACE_3) {
            @if let Some(count) = game.view.remaining_tile_count {
                div direction="row" align-items="center" gap=(theme::SPACE_3)
                    background=(theme::SUNNY_SOFT) border=((theme::SUNNY, theme::EDGE_THIN))
                    border-radius=(theme::RADIUS_LG) padding=(theme::SPACE_3) {
                    span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XXL) font-weight=bold color=(theme::ON_SUNNY) { (count) }
                    span font-weight=bold color=(theme::ON_SUNNY) { "tile" (if count == 1 { "" } else { "s" }) " in the bag" }
                }
            }
            @if let Some(faces) = faces {
                h3 font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_MD) { "Tiles remaining" }
                div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=(theme::SPACE_2) {
                    @for face in faces {
                        @let letter = face.letter.map_or_else(|| "?".to_string(), |letter| letter.to_string());
                        @let tile = ui::tile_face(&letter, None, 32);
                        div align-items="center" gap=2 width=40 {
                            (tile)
                            span font-size=(theme::TEXT_XS) font-weight=bold color=(theme::INK_MUTED) { "×" (face.count) }
                        }
                    }
                }
            }
        }
    })
}

/// Shared chrome for the bag and menu drawers.
fn drawer_shell(id: &str, max_height: &str, children: impl Into<Vec<Container>>) -> Container {
    let children: Vec<Container> = children.into();
    let id = id.to_string();
    let max_height = max_height.to_string();
    ui::one(container! {
        aside id=(id) hidden position="fixed" top=64 right=8 width=(DRAWER_WIDTH) max-width="92vw"
            max-height=(max_height) overflow-y="auto" background=(theme::MENU_BG) color=(theme::INK)
            border=((theme::BORDER_STRONG, theme::EDGE_THIN)) border-bottom=((theme::BORDER_STRONG, 6))
            border-radius=(theme::RADIUS_XL) padding=(theme::SPACE_4) gap=(theme::SPACE_4) {
            (children)
        }
    })
}

/// A round header control (back, bag, menu) on the night scene.
const fn header_control_style() -> ui::ButtonStyle {
    ui::button_style(ButtonVariant::Night)
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
pub fn visual_game_page(
    game: &AuthorizedGamePage,
    draft: &TurnDraft,
    error: Option<&str>,
) -> Container {
    let game_id = game.game_id.to_string();
    let match_title = match_title(game);
    let feedback = draft_feedback(game, draft);
    let board = visual_board(game, draft, &feedback);
    let draft_preview = draft_feedback_component(game, &feedback, draft);
    let scoreboard = player_scoreboard_component(game);
    let completed_summary = game.completed.then(|| completed_game_summary(game));
    let rack = visual_rack(game, draft);
    let actions = visual_turn_actions(game, draft);
    let blank_letter_layer = visual_blank_letter_layer(game, draft);
    let history = move_history_component(game.game_id, &game.history);
    let game_channel = format!("game:{}", game.game_id);
    let game_path = if draft.has_composed_turn_input() {
        format!("/games/{game_id}?draft_revision={}", game.view.revision)
    } else {
        format!("/games/{game_id}")
    };
    let refresh_game = ActionType::Navigate { url: game_path };
    let turn_feedback_view = turn_feedback(error);
    let control = header_control_style();
    let active_name = active_player_name(game).to_string();
    let active_seat = participant_seat(game, game.view.active_player);
    let active_avatar = game
        .participants
        .iter()
        .find(|participant| participant.player_id == game.view.active_player)
        .map(|participant| {
            ui::avatar(
                &participant.display_name,
                participant.avatar_url.as_deref(),
                active_seat,
                24,
            )
        });
    let (outcome, _) = completed_outcome(game);
    let has_bag =
        game.view.remaining_tile_count.is_some() || game.view.remaining_tile_faces.is_some();
    // Keep the wrapper so `#app-page` is rendered (see `definition_page`).
    Container::from(container! {
        div id="app-page" font-family=(theme::FONT_UI) class="game-scene" data-shared-state-channel=(game_channel.as_str())
            direction="column" width=vw100 height=dvh100 min-height=dvh100
            position="fixed" top=0 right=0 bottom=0 left=0
            overflow-x="hidden" overflow-y="hidden"
            background=(theme::NIGHT) color=(theme::NIGHT_INK) gap=4 {
            span id="game-live-refresh" hidden fx-global-shared-state-event=(refresh_game) { }
            header id="scene-controls" width="100%" min-width=0 direction="row"
                align-items="center" gap=(theme::SPACE_2) padding-top=(theme::SPACE_2) padding-x=(theme::SPACE_2) {
                div class="header-action-slot header-action-left" direction="row" justify-content="start" {
                    anchor href="/" id="leave-game" width=(HEADER_CONTROL) height=(HEADER_CONTROL) min-width=(HEADER_CONTROL)
                        background=(control.background) color=(control.ink)
                        border=((theme::NIGHT_LINE, theme::EDGE_THIN)) border-bottom=((control.edge, 4))
                        border-radius=(theme::RADIUS_PILL) align-items="center" justify-content="center"
                        font-size=(theme::TEXT_LG) font-weight=bold text-decoration="none"
                        fx-hover=(ActionType::set_background_self(control.hover)) { "←" }
                }
                (scoreboard)
                div class="header-action-slot header-action-right" direction="row" justify-content="end" gap=6 {
                    @if has_bag {
                        button id="remaining-tiles-button" type=button
                            fx-click=(ActionType::toggle_display_by_id("remaining-tiles-panel"))
                            height=(HEADER_CONTROL) direction="row" align-items="center" gap=5
                            background=(control.background) color=(control.ink)
                            border=((theme::NIGHT_LINE, theme::EDGE_THIN)) border-bottom=((control.edge, 4))
                            border-radius=(theme::RADIUS_PILL) padding-left=5 padding-right=10
                            font-weight=bold cursor=pointer white-space="preserve"
                            fx-hover=(ActionType::set_background_self(control.hover)) {
                            span width=24 height=24 min-width=24 background=(theme::TILE) color=(theme::TILE_INK)
                                border-bottom=((theme::TILE_EDGE, 3)) border-radius=6
                                align-items="center" justify-content="center"
                                font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XS) font-weight=bold {
                                @if let Some(count) = game.view.remaining_tile_count { (count) } @else { "?" }
                            }
                            span id="remaining-tiles-label" font-size=(theme::TEXT_SM) { "Bag" }
                        }
                    }
                    button type=button fx-click=(ActionType::toggle_display_by_id("activity-rail"))
                        height=(HEADER_CONTROL) padding-x=(theme::SPACE_3)
                        background=(control.background) color=(control.ink)
                        border=((theme::NIGHT_LINE, theme::EDGE_THIN)) border-bottom=((control.edge, 4))
                        border-radius=(theme::RADIUS_PILL) font-size=(theme::TEXT_SM) font-weight=bold
                        white-space="preserve" cursor=pointer
                        fx-hover=(ActionType::set_background_self(control.hover)) { "Menu ···" }
                }
            }
            main id="game-layout" class="game-arena" width="100%" min-height=0
                flex=1 position="relative" overflow-x="hidden" overflow-y="hidden" {
                section id="board-region" position="absolute" top=0 right=0 bottom=0 left=0
                    overflow-x="hidden" overflow-y="hidden" {
                    (board)
                }
            }
            section id="turn-dock-layer" width="100%" flex="0 0 auto" align-items="center"
                padding-x=(theme::SPACE_2) padding-bottom=(theme::SPACE_2) {
                section id="play-console" class="game-console turn-dock" width="100%" max-width=(DOCK_MAX_WIDTH)
                    align-items="center" position="relative"
                    background=(theme::NIGHT_RAISED)
                    border=((theme::NIGHT_LINE, theme::EDGE_THIN)) border-bottom=((theme::NIGHT_BORDER, 6))
                    border-radius=(theme::RADIUS_XL) padding=(theme::SPACE_2) gap=(theme::SPACE_2) {
                    @if game.completed {
                        (turn_feedback_view)
                        section class="dock-message" width="100%" direction="row" align-items="center"
                            justify-content="space-between" gap=(theme::SPACE_2)
                            background=(theme::SUNNY) color=(theme::ON_SUNNY)
                            border=((theme::SUNNY_EDGE, theme::EDGE_THIN)) border-radius=(theme::RADIUS_PILL)
                            padding-y=4 padding-left=(theme::SPACE_3) padding-right=4 {
                            span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_MD) font-weight=bold
                                white-space="preserve" text-overflow="ellipsis" overflow-x="hidden" {
                                "★ " (outcome)
                            }
                            (ui::action_button("Results", ButtonVariant::Secondary, ButtonSize::Small, ActionType::display_by_id("completed-game-summary")))
                        }
                    } @else if error.is_some() {
                        (turn_feedback_view)
                    } @else if game.view.active_player == game.viewer_player {
                        (turn_feedback_view)
                        (draft_preview)
                    } @else {
                        (turn_feedback_view)
                        section class="dock-message waiting-banner" width="100%" direction="row" align-items="center"
                            justify-content="center" gap=(theme::SPACE_2)
                            background=(active_seat.fill) color=(active_seat.ink)
                            border=((active_seat.edge, theme::EDGE_THIN)) border-radius=(theme::RADIUS_LG)
                            padding-y=4 padding-x=(theme::SPACE_3) {
                            @if let Some(active_avatar) = active_avatar { (active_avatar) }
                            span min-width=0 font-size=(theme::TEXT_SM) font-weight=bold overflow-wrap="anywhere" {
                                span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_MD) { (active_name) " is playing" }
                                " · plan your next word"
                            }
                        }
                    }
                    (rack)
                    @if !game.completed && game.view.active_player == game.viewer_player {
                        (actions)
                    } @else if !game.completed {
                        @let compose_action = format!("/games/{}/compose", game.game_id);
                        section id="turn-actions" class="primary-action-row" width="100%" max-width="100%" direction="row"
                            justify-content="center" gap=(theme::SPACE_2) {
                            form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                                (compose_form_fields(game, draft, "SHUFFLE_RACK"))
                                (dock_submit_button("⇄ Shuffle"))
                            }
                            @if !draft.placements.is_empty() {
                                form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                                    (compose_form_fields(game, draft, "CLEAR"))
                                    (dock_submit_button("↺ Recall"))
                                }
                            }
                        }
                    }
                }
            }
            @if has_bag {
                (drawer_shell("remaining-tiles-panel", "72vh", container! {
                    div direction="row" justify-content="space-between" align-items="center" gap=(theme::SPACE_2) {
                        div gap=2 {
                            (ui::eyebrow("TILE BAG", theme::ACCENT))
                            h2 font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XL) { "Bag" }
                        }
                        (drawer_close_button("remaining-tiles-panel"))
                    }
                    (remaining_tiles_component(game))
                }))
            }
            (drawer_shell("activity-rail", "78vh", container! {
                div direction="row" justify-content="space-between" align-items="start" gap=(theme::SPACE_2) {
                    div gap=2 min-width=0 {
                        (ui::eyebrow("THIS MATCH", theme::ACCENT))
                        h2 font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XL) {
                            (match_title)
                        }
                    }
                    (drawer_close_button("activity-rail"))
                }
                @if let Some(latest) = &game.latest_action {
                    section id="latest-game-action" background=(theme::SUNNY_SOFT)
                        border=((theme::SUNNY, theme::EDGE_THIN)) border-bottom=((theme::SUNNY_EDGE, 4))
                        border-radius=(theme::RADIUS_LG) padding=(theme::SPACE_3) gap=3 {
                        (ui::eyebrow("LATEST", theme::ON_SUNNY))
                        span font-weight=bold color=(theme::ON_SUNNY) { (latest.as_str()) }
                    }
                }
                section id="recent-activity" gap=(theme::SPACE_2) {
                    h3 font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_MD) { "Move history" }
                    (history)
                }
                (board_key_component())
                section id="game-reference" width="100%" gap=(theme::SPACE_2) {
                    (rules_component(game))
                }
            }))
            aside id="game-definition-layer" hidden position="fixed" top=64 right=8
                width=(DRAWER_WIDTH + 80) max-width="94%" max-height="72vh" overflow-y="auto"
                background=(theme::MENU_BG) color=(theme::INK)
                border=((theme::BORDER_STRONG, theme::EDGE_THIN)) border-bottom=((theme::BORDER_STRONG, 6))
                border-radius=(theme::RADIUS_XL) padding=(theme::SPACE_4) gap=(theme::SPACE_3) {
                (ui::eyebrow("WORD DEFINITION", theme::ACCENT))
                span { "Loading definition…" }
            }
            @if let Some(completed_summary) = completed_summary {
                (completed_summary)
            }
            (blank_letter_layer)
        }
    })
}

/// Renders public board/status/history and only the authorized viewer's rack.
#[must_use]
pub fn game_page(game: &AuthorizedGamePage) -> Container {
    visual_game_page(game, &TurnDraft::default(), None)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures_lite::future::block_on;
    use switchy_database::Database;
    use time::{Duration, OffsetDateTime};

    use super::*;
    use crate::{
        SESSION_COOKIE_NAME, accept_challenge, create_challenge, create_session,
        load_authorized_game_page, migrate_app, register,
    };

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
    #[allow(clippy::too_many_lines)]
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

            let page = visual_game_page(&game, &TurnDraft::default(), None)
                .display_to_string(false, false)
                .expect("game page renders");
            assert!(page.contains("id=\"app-page\""));
            assert!(page.contains("id=\"player-rack\""));
            assert!(page.contains("id=\"game-board\""));
            assert_eq!(page.matches("data-tile-id=").count(), game.view.rack.len());
            assert!(page.contains("Your move"));
            assert!(page.contains("Menu ···"));
            assert!(page.contains("board-zoom-controls"));
            assert!(page.contains("Place tiles to play"));
            assert!(page.contains("100dvh") || page.contains("dvh"));
            assert!(!page.contains("<style"));
            assert!(!page.contains("<script"));

            let summary = completed_game_summary(&game)
                .display_to_string(false, false)
                .expect("results render");
            assert!(summary.contains("id=\"completed-game-summary\""));
            assert!(summary.contains("id=\"final-standings\""));
            assert!(summary.contains(&format!("hx-post=\"/games/{game_id}/rematch\"")));
            assert!(summary.contains("Rematch"));
            assert!(summary.contains("Back to games"));
            assert!(summary.contains("href=\"/\""));
            assert!(summary.contains("Close"));
        });
    }
}
