//! Visual game scene: board, rack, turn dock, scoreboard, overlays, and definition panels.

use hyperchad::{
    actions::ActionType,
    router::Container,
    template::{LayoutOverflow, container},
};
use wwmtf_game_domain::{Coordinate, PremiumSquare};

use crate::{
    AuthorizedGamePage, move_history_component,
    routes::{DraftFeedback, TurnDraft, TurnMode, draft_feedback, draft_token, rack_action},
    theme,
};

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
    container! {
        section id="draft-preview" class="dock-message" width="100%" direction="row"
            align-items="center" justify-content="center" padding-y="4px" padding-x="8px"
            background=(if candidate_valid { "#f4c95d" } else if feedback.candidate.is_some() { "#f7d8ae" } else { theme::NIGHT_DEEP })
            color=(if feedback.candidate.is_some() { "#2d2515" } else { theme::NIGHT_INK })
            border=((if candidate_valid { "#ffe29a" } else if feedback.candidate.is_some() { "#d77a59" } else { theme::NIGHT_LINE }, 1))
            border-radius="10px" overflow-x="hidden" {
            span font-size="13px" font-weight=bold text-overflow="ellipsis" white-space="preserve" {
                (message)
            }
        }
    }
    .into()
}

pub fn draft_error_page(game: &AuthorizedGamePage, draft: &TurnDraft, message: &str) -> Container {
    visual_game_page(game, draft, Some(message))
}

pub fn turn_feedback(message: Option<&str>) -> Container {
    container! {
        @if let Some(message) = message {
            section id="turn-feedback" width="100%" {
                div id="game-error" background=(theme::DANGER_SOFT) border=((theme::DANGER, 1))
                    border-radius="12px" padding="14px" {
                    span color=(theme::DANGER_INK) { (message) }
                }
            }
        }
    }
    .into()
}

pub fn definition_content(word: &str, lookup: &crate::DefinitionLookup) -> Container {
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
                span color=(theme::INK_MUTED) font-size="12px" {
                    "Source: " anchor href=(definition.source_url.as_str()) color=(theme::LINK) { (definition.source_url.as_str()) }
                }
                span color=(theme::INK_MUTED) font-size="12px" {
                    "License: " anchor href=(definition.license_url.as_str()) color=(theme::LINK) { (definition.license_name.as_str()) }
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

pub fn definition_page(
    game_id: wwmtf_game_domain::GameId,
    word: &str,
    lookup: &crate::DefinitionLookup,
) -> Container {
    let game_href = format!("/games/{game_id}");
    let content = definition_content(word, lookup);
    container! {
        div id="app-page" font-family=(theme::FONT_UI) direction="column" align-items="center" min-height="100vh"
            background=(theme::MENU_BG) padding-y=32 padding-x=18 {
            main id="word-definition" width="100%" max-width="720px" background=(theme::WHITE)
                border=((theme::BORDER, 1)) border-radius="18px" padding="24px" gap="14px" {
                anchor href=(game_href) color=(theme::LINK) { "← Back to game" }
                (content)
            }
        }
    }
    .into()
}

pub fn definition_panel(word: &str, lookup: &crate::DefinitionLookup) -> Container {
    let content = definition_content(word, lookup);
    container! {
        aside id="game-definition-layer" class="game-definition-panel" position="fixed"
            top=68 right="2%" width="440px" max-width="96%" max-height="70vh" overflow-y="auto"
            background=(theme::MENU_BG) color=(theme::INK) border=((theme::BORDER_STRONG, 3)) border-radius="18px"
            padding="18px" gap="12px" {
            div direction="row" justify-content="space-between" align-items="center" gap="12px" {
                span color=(theme::INK_MUTED) font-size="11px" font-weight=bold { "WORD DEFINITION" }
                button type=button fx-click=(ActionType::no_display_by_id("game-definition-layer"))
                    background=(theme::NIGHT_RAISED) color=(theme::NIGHT_INK) border=((theme::NIGHT_LINE, 1)) border-radius="999px"
                    padding-y="6px" padding-x="10px" cursor=pointer { "Close" }
            }
            (content)
        }
    }
    .into()
}

pub fn definition_panel_error(message: &str) -> Container {
    container! {
        aside id="game-definition-layer" class="game-definition-panel" position="fixed"
            top=68 right="2%" width="440px" max-width="96%" max-height="70vh" overflow-y="auto"
            background=(theme::MENU_BG) color=(theme::INK) border=((theme::BORDER_STRONG, 3)) border-radius="18px"
            padding="18px" gap="12px" {
            div direction="row" justify-content="space-between" align-items="center" gap="12px" {
                h2 { "Definition unavailable" }
                button type=button fx-click=(ActionType::no_display_by_id("game-definition-layer"))
                    background=(theme::NIGHT_RAISED) color=(theme::NIGHT_INK) border=((theme::NIGHT_LINE, 1)) border-radius="999px"
                    padding-y="6px" padding-x="10px" cursor=pointer { "Close" }
            }
            span { (message) }
        }
    }
    .into()
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
            span color=(theme::INK_MUTED) { "Tap a rack tile, then tap an open board square. Use these controls for non-placement turns." }
            div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap="10px" {
                form hx-post=(action.as_str()) hx-target="#app-page" {
                    input type=hidden name="command" value="PASS";
                    input type=hidden name="command_id" value=(command_id.as_str());
                    input type=hidden name="idempotency_key" value=(idempotency_key.as_str());
                    input type=hidden name="expected_revision" value=(game.view.revision);
                    button type=submit padding-y=10 padding-x=14 background=(theme::WHITE) color=(theme::LINK)
                        border=((theme::SECONDARY_EDGE, 1)) border-radius="9px" cursor=pointer { "Pass" }
                }
                form hx-post=(action.as_str()) hx-target="#app-page" {
                    input type=hidden name="command" value="RESIGN";
                    input type=hidden name="command_id" value=(command_id.as_str());
                    input type=hidden name="idempotency_key" value=(idempotency_key.as_str());
                    input type=hidden name="expected_revision" value=(game.view.revision);
                    button type=submit padding-y=10 padding-x=14 background=(theme::WHITE) color=(theme::DANGER_INK)
                        border=((theme::DANGER_EDGE, 1)) border-radius="9px" cursor=pointer { "Resign" }
                }
            }
        }
    }
    .into()
}

pub fn compose_form_fields(
    game: &AuthorizedGamePage,
    draft: &TurnDraft,
    action: &str,
) -> Container {
    let encoded = draft_token(draft);
    container! {
        input type=hidden name="action" value=(action);
        input type=hidden name="expected_revision" value=(game.view.revision);
        input type=hidden name="draft" value=(encoded);
    }
    .into()
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
                        background=(theme::NIGHT_RAISED) color=(theme::WHITE) border=((theme::NIGHT_LINE, 1)) border-radius="999px" cursor=pointer { "−" }
                }
                form hx-post=(action.as_str()) hx-target="#app-page" {
                    (compose_form_fields(game, draft, "ZOOM_RESET"))
                    button type=submit padding-y="6px" padding-x="11px" background=(theme::NIGHT_RAISED) color=(theme::WHITE)
                        border=((theme::NIGHT_LINE, 1)) border-radius="999px" cursor=pointer { "Fit" }
                }
                form hx-post=(action.as_str()) hx-target="#app-page" {
                    (compose_form_fields(game, draft, "ZOOM_IN"))
                    button type=submit padding-y="6px" padding-x="11px"
                        background=(theme::NIGHT_RAISED) color=(theme::WHITE) border=((theme::NIGHT_LINE, 1)) border-radius="999px" cursor=pointer { "+" }
                }
            }
            div class="board-viewport" width="100%" min-height=0 flex=1
                overflow-x="auto" overflow-y="auto" {
                div class="board-scroll-content" width="100%" height="100%" min-width=(board_frame_width)
                    min-height=(board_frame_width) align-items="center" justify-content="center" {
                    div data-board-grid-width=(board_grid_width) data-board-frame-width=(board_frame_width)
                        width=(board_frame_width) height=(board_frame_width) position="relative"
                        flex="0 0 auto" background=(theme::BOARD_FRAME) border=((theme::BOARD_FRAME_EDGE, 6)) border-radius="8px" gap="2px" {
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
                                    (theme::TILE, letter.to_string(), theme::TILE_INK)
                                } else if let Some((tile_id, blank_letter)) = drafted {
                                    let letter = game.view.rack.iter().find(|(id, _, _)| *id == tile_id)
                                        .map(|(_, letter, _)| blank_letter.unwrap_or(*letter)).unwrap_or('?');
                                    (theme::TILE_PENDING, letter.to_string(), theme::TILE_PENDING_INK)
                                } else if coordinate == game.rules.start {
                                    (theme::PREMIUM_START, "★".to_string(), theme::PREMIUM_START_INK)
                                } else {
                                    match premium {
                                        Some(PremiumSquare::Letter(2)) => (theme::PREMIUM_DL, "DL".to_string(), theme::PREMIUM_DL_INK),
                                        Some(PremiumSquare::Letter(_)) => (theme::PREMIUM_TL, "TL".to_string(), theme::PREMIUM_TL_INK),
                                        Some(PremiumSquare::Word(2)) => (theme::PREMIUM_DW, "DW".to_string(), theme::PREMIUM_DW_INK),
                                        Some(PremiumSquare::Word(_)) => (theme::PREMIUM_TW, "TW".to_string(), theme::WHITE),
                                        None => (theme::SQUARE, String::new(), theme::SQUARE_INK),
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
                                            background=(theme::TILE_PENDING) color=(theme::TILE_PENDING_INK) border=((theme::TILE_PENDING_EDGE, 3)) border-radius="4px"
                                            align-items="center" justify-content="center" font-weight=bold position="relative" cursor=pointer {
                                            span font-size=(tile_font_size) { (label) }
                                            @if let Some(points) = draft_points {
                                                span class="board-tile-points" position="absolute" right="4px" bottom="2px" font-size="10px" { (points) }
                                            }
                                        }
                                    }
                                } @else if committed.is_some() {
                                    div class=(if latest { "board-square committed-square latest-move-square" } else if viewer_owned { "board-square committed-square viewer-owned-square" } else { "board-square committed-square" }) width=(square_size) height=(square_size)
                                        background=(background) color=(color) border=((if latest { theme::LATEST_MOVE } else { theme::TILE_EDGE }, if latest { 3 } else { 1 }))
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
                                            border=(if required { ("#b96d2b", 3) } else if eligible { ("#527a4888", 3) } else { (theme::SQUARE_LINE, 1) })
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
                            background=(if invalid_score { "#b4452f" } else { "#2f8a57" }) color=(theme::WHITE)
                            border=(if invalid_score { ("#7a2d20", 2) } else { (theme::PRIMARY_EDGE, 2) }) border-radius="999px"
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
                                background=(if selected { theme::TILE_HI } else if exchange_selected { theme::TILE_EXCHANGE } else if placed { theme::TILE_DIM } else { theme::TILE }) color=(theme::TILE_INK)
                                border=((if selected { theme::WHITE } else if exchange_selected { theme::TILE_EXCHANGE_EDGE } else { theme::TILE_EDGE }, if selected || exchange_selected { 4 } else { 2 })) border-radius="7px" align-items="center" justify-content="center"
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
                            background=(theme::TILE) color=(theme::TILE_INK) border=((theme::TILE_EDGE, 2)) border-radius="7px"
                            align-items="center" justify-content="center" position="relative" font-weight=bold {
                            span class="rack-tile-face" font-size=calc(min(18, (dvw(100) - 80) / 24)) { (face) }
                            span class="rack-tile-points" position="absolute" right="2px" bottom="1px"
                                font-size=calc(min(8, (dvw(100) - 80) / 48)) { (points) }
                        }
                    }
                }
            }
        }
    }
    .into()
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
    container! {
        @if selected_blank_needs_letter {
            section class="blank-letter-layer" position="fixed" top=0 right=0 bottom=0 left=0
                width="100vw" height="100dvh" align-items="center" justify-content="end"
                background=#00000066 padding="2vw" {
                section width="100%" max-height="72dvh" overflow-y="auto"
                    background=(theme::MENU_BG) color=(theme::INK) border=((theme::BORDER_STRONG, 2)) border-radius="12px"
                    padding="10px" gap="7px" {
                    div width="100%" direction="row" justify-content="space-between" align-items="center" gap="8px" {
                        span font-weight=bold { "Choose the blank tile’s letter:" }
                        form hx-post=(action.as_str()) hx-target="#app-page" {
                            (compose_form_fields(game, draft, "CANCEL_TILE_PICK"))
                            button type=submit background=(theme::WHITE) color=(theme::LINK) border=((theme::SECONDARY_EDGE, 1))
                                border-radius="999px" padding-y="5px" padding-x="9px" cursor=pointer { "Cancel" }
                        }
                    }
                    div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) justify-content="center" gap="5px" {
                        @for letter in 'A'..='Z' {
                            form hx-post=(action.as_str()) hx-target="#app-page" {
                                (compose_form_fields(game, draft, "CHOOSE_BLANK_LETTER"))
                                input type=hidden name="letter" value=(letter.to_string());
                                button type=submit data-blank-letter=(letter.to_string()) width="38px" height="38px" border=((theme::SQUARE_LINE, 1))
                                    background=(if draft.selected_blank_letter == Some(letter) { theme::BLANK_LETTER_CHOSEN } else { theme::WHITE })
                                    border-radius="7px" font-weight=bold cursor=pointer { (letter) }
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
    container! {
        section id="turn-actions" class="turn-composer action-hud" max-width="100%" position="relative" padding-y="4px" padding-x="6px"
            background=(if matches!(draft.mode, TurnMode::ConfirmExchange | TurnMode::ConfirmPass | TurnMode::ConfirmResign) { "#f7d8ae" } else { theme::NIGHT_RAISED })
            color=(if matches!(draft.mode, TurnMode::ConfirmExchange | TurnMode::ConfirmPass | TurnMode::ConfirmResign) { "#402c1e" } else { theme::NIGHT_INK })
            border=((if matches!(draft.mode, TurnMode::ConfirmExchange | TurnMode::ConfirmPass | TurnMode::ConfirmResign) { "#f3b66e" } else { theme::NIGHT_LINE }, 1))
            border-radius="10px" {
            @if draft.mode == TurnMode::Exchange {
                div class="primary-action-row" direction="row" overflow-x="auto" overflow-y="hidden"
                    align-items="center" gap="6px" {
                    form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                        (compose_form_fields(game, draft, "REVIEW_EXCHANGE"))
                        button type=submit class="primary-turn-action" padding-y=10 padding-x=14 background=(theme::PRIMARY) color=(theme::ON_PRIMARY)
                            border=((theme::PRIMARY_EDGE, 2)) border-radius="11px" font-weight=bold cursor=pointer { "Review exchange" }
                    }
                    form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                        (compose_form_fields(game, draft, "CANCEL_MODE"))
                        button type=submit padding-y=10 padding-x=14 background=(theme::WHITE) color=(theme::LINK)
                            border=((theme::SECONDARY_EDGE, 1)) border-radius="9px" cursor=pointer { "Cancel" }
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
                        button type=submit class="primary-turn-action" padding-y=10 padding-x=14 background=(theme::PRIMARY) color=(theme::ON_PRIMARY)
                            border=((theme::PRIMARY_EDGE, 2)) border-radius="11px" font-weight=bold cursor=pointer { "Confirm exchange" }
                    }
                    form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                        (compose_form_fields(game, draft, "CANCEL_MODE"))
                        button type=submit padding-y=10 padding-x=14 background=(theme::WHITE) color=(theme::LINK)
                            border=((theme::SECONDARY_EDGE, 1)) border-radius="9px" cursor=pointer { "Cancel" }
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
                            button type=submit padding-y=7 padding-x=10 background=(theme::WHITE) color=(theme::LINK)
                                border=((theme::SECONDARY_EDGE, 1)) border-radius="9px" cursor=pointer { "Shuffle" }
                        }
                    }
                    @if !draft.placements.is_empty() {
                        form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                            (compose_form_fields(game, draft, "CLEAR"))
                            button type=submit padding-y=7 padding-x=10 background=(theme::WHITE) color=(theme::LINK)
                                border=((theme::SECONDARY_EDGE, 1)) border-radius="9px" cursor=pointer { "Recall" }
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
                            button type=submit class="primary-turn-action" padding-y=7 padding-x=12 background=(theme::PRIMARY) color=(theme::ON_PRIMARY)
                                border=((theme::PRIMARY_EDGE, 2)) border-radius="9px" font-weight=bold cursor=pointer {
                                @if let Some(score) = play_score { "Play · " (score) } @else { "Play" }
                            }
                        }
                    }
                    @if !game.completed && viewer_turn {
                        button type=button fx-click=(ActionType::toggle_display_by_id("more-turn-actions-menu"))
                            color=(theme::NIGHT_INK) font-weight=bold padding-y="7px" padding-x="6px"
                            cursor=pointer { "More ···" }
                    }
                }
                @if !game.completed && viewer_turn {
                    div id="more-turn-actions-menu" hidden position="absolute" bottom="100%" right=0
                        background=(theme::WHITE) color=(theme::INK) border=(("#d8d1c1", 1)) border-radius="12px"
                        padding="12px" gap="8px" {
                        @if game.exchange_available {
                            form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                                (compose_form_fields(game, draft, "BEGIN_EXCHANGE"))
                                button type=submit padding-y=8 padding-x=12 background=(theme::WHITE) color=(theme::LINK)
                                    border=((theme::SECONDARY_EDGE, 1)) border-radius="9px" cursor=pointer { "Exchange" }
                            }
                        }
                        form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                            (compose_form_fields(game, draft, "CONFIRM_PASS"))
                            button type=submit padding-y=8 padding-x=12 background=(theme::WHITE) color=(theme::LINK)
                                border=((theme::SECONDARY_EDGE, 1)) border-radius="9px" cursor=pointer { "Pass" }
                        }
                        form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                            (compose_form_fields(game, draft, "CONFIRM_RESIGN"))
                            button type=submit padding-y=8 padding-x=12 background=(theme::WHITE) color=(theme::DANGER_INK)
                                border=((theme::DANGER_EDGE, 1)) border-radius="9px" cursor=pointer { "Resign" }
                        }
                    }
                }
            }
        }
    }
    .into()
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
    container! {
        div class="primary-action-row" direction="row" overflow-x="auto" overflow-y="hidden" gap="6px" {
            form hx-post=(turn_action.as_str()) hx-target="#app-page" {
                input type=hidden name="command" value=(command);
                input type=hidden name="command_id" value=(command_id);
                input type=hidden name="idempotency_key" value=(idempotency_key);
                input type=hidden name="expected_revision" value=(game.view.revision);
                button type=submit class="primary-turn-action" padding-y=10 padding-x=14 background=(theme::PRIMARY) color=(theme::ON_PRIMARY)
                    border=((theme::PRIMARY_EDGE, 2)) border-radius="11px" font-weight=bold cursor=pointer { (label) }
            }
            form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                (compose_form_fields(game, draft, "CANCEL_MODE"))
                button type=submit padding-y=10 padding-x=14 background=(theme::WHITE) color=(theme::LINK)
                    border=((theme::SECONDARY_EDGE, 1)) border-radius="9px" cursor=pointer { "Cancel" }
            }
        }
    }
    .into()
}

pub fn player_scoreboard_component(game: &AuthorizedGamePage) -> Container {
    let current_turn_name = game
        .participants
        .iter()
        .find(|participant| participant.player_id == game.view.active_player)
        .map_or("Player", |participant| participant.display_name.as_str());
    let turn = if game.completed {
        "Game complete".to_string()
    } else if game.view.active_player == game.viewer_player {
        "Your move".to_string()
    } else {
        format!("{current_turn_name}'s move")
    };
    container! {
        section id="game-awareness" class="player-hud scoreboard-hud" min-width=0 flex=1
            overflow-x="auto" overflow-y="hidden" background=#f4c95d color=#2d2515
            border=(("#ffe29a", 2)) border-radius="16px" padding-y="6px" padding-x="8px" gap="5px" {
            div min-width=0 align-items="center" {
                span id="named-turn-status" font-size="11px" font-weight=bold white-space="preserve" { (turn) }
                div direction="row" gap="5px" align-items="center" {
                    span id="live-status-connected" hidden fx-global-shared-state-connected=(live_status_action("live-status-connected")) { "● Live" }
                    span id="live-status-subscribed" hidden fx-global-shared-state-subscribed=(live_status_action("live-status-connected")) { }
                    span id="live-status-disconnected" hidden fx-global-shared-state-disconnected=(live_status_action("live-status-disconnected")) { "● Offline" }
                }
            }
            div id="participant-scoreboard" direction="row" gap="6px" min-width=0 overflow-x="auto" overflow-y="hidden" {
                @for participant in &game.participants {
                    @let public = game.view.players.iter().find(|player| player.player_id == participant.player_id);
                    @let score = public.map_or(0, |player| player.score);
                    @let is_viewer = participant.player_id == game.viewer_player;
                    @let active = public.is_some_and(|player| player.active);
                    @let leader = public.is_some_and(|player| player.leader);
                    @let initial = participant.username.chars().next().unwrap_or('?').to_uppercase().to_string();
                    @let state = if game.completed && leader {
                        "Leader"
                    } else if game.completed {
                        "Completed"
                    } else if !active {
                        "Resigned"
                    } else if participant.player_id == game.view.active_player {
                        "Playing"
                    } else {
                        "Waiting"
                    };
                    div id=(format!("player-scoreboard-{}", participant.player_id.as_uuid()))
                        direction="row" align-items="center" gap="5px" flex="0 0 auto"
                        background=(if is_viewer { "#fff1bd" } else { "#e1bd63" })
                        border=((if leader { "#2e7049" } else { "#b08e43" }, 2)) border-radius="999px"
                        padding-y="3px" padding-x="6px" {
                        @if let Some(avatar_url) = participant.avatar_url.as_deref() {
                            image src=(avatar_url) alt=(format!("{} profile avatar", participant.display_name)) width="30" height="30" border-radius="999px";
                        } @else {
                            span width="30px" height="30px" border-radius="999px"
                                background=(if active { "#2e7049" } else { "#776f5d" }) color=(theme::WHITE)
                                align-items="center" justify-content="center" font-size="14px" font-weight=bold { (initial) }
                        }
                        div gap="1px" min-width="70px" {
                            span font-size="12px" font-weight=bold white-space="preserve" text-overflow="ellipsis" {
                                (participant.display_name.as_str()) @if is_viewer { " (you)" }
                            }
                            span font-size="10px" white-space="preserve" { (state) }
                        }
                        span min-width="30px" align-items="center" justify-content="center"
                            background=(theme::WHITE) border-radius="999px" padding-y="2px" padding-x="5px"
                            font-size="17px" font-weight=bold { (score) }
                    }
                }
            }
        }
    }
    .into()
}

pub fn completed_game_summary(game: &AuthorizedGamePage) -> Container {
    let mut standings = game
        .participants
        .iter()
        .filter_map(|participant| {
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
                    )
                })
        })
        .collect::<Vec<_>>();
    standings.sort_by_key(|standing| std::cmp::Reverse(standing.1));
    let leaders = standings
        .iter()
        .filter(|standing| standing.3)
        .map(|standing| standing.0.display_name.as_str())
        .collect::<Vec<_>>();
    let outcome = if leaders.len() > 1 {
        format!("Co-leaders: {}", leaders.join(", "))
    } else if let Some(leader) = leaders.first() {
        format!("{leader} won")
    } else {
        "Game complete".to_string()
    };
    container! {
        section id="completed-game-summary" width="100%" background=(theme::WHITE) color=(theme::INK) border=(("#c8b88f", 2))
            border-radius="12px" padding-y="8px" padding-x="10px" gap="6px" {
            div width="100%" direction="row" justify-content="space-between" align-items="center" gap="8px" {
                span font-size="18px" font-weight=bold { (outcome) }
                button type=button fx-click=(ActionType::no_display_by_id("completed-game-summary"))
                    background=(theme::WHITE) color=(theme::LINK) border=((theme::SECONDARY_EDGE, 1)) border-radius="999px"
                    padding-y="4px" padding-x="8px" cursor=pointer { "Close" }
            }
            div id="final-standings" width="100%" overflow-x="auto" overflow-y="hidden" gap="4px" {
                @for (rank, (participant, score, adjustment, leader)) in standings.iter().enumerate() {
                    @let is_viewer = participant.player_id == game.viewer_player;
                    div direction="row" justify-content="space-between" align-items="center" gap="8px"
                        border-bottom=((theme::BORDER, 1)) padding-y="4px" padding-x="2px" {
                        span {
                            @if *leader { span font-weight=bold { (rank + 1) ". " (participant.display_name.as_str()) } }
                            @else { span { (rank + 1) ". " (participant.display_name.as_str()) } }
                            @if is_viewer { " (you)" }
                            @if *leader { " · leader" }
                        }
                        span white-space="preserve" {
                            (score) @if *adjustment != 0 { " (" (format!("{adjustment:+}")) ")" }
                        }
                    }
                }
            }
            @if let Some(reason) = &game.completion_reason {
                span color=(theme::INK_MUTED) { "Completed by: " (reason.as_str()) }
            }
        }
    }
    .into()
}

pub fn rules_component(game: &AuthorizedGamePage) -> Container {
    let exchange_requirement = game.rules.minimum_tiles_for_exchange;
    let rack_size = game.rules.rack_size;
    let full_rack_bonus = game.rules.full_rack_bonus;
    let scoreless_turn_limit = game.rules.scoreless_turn_limit;
    container! {
        details id="game-rules" width="100%" {
            summary cursor="pointer" font-weight=bold { "Rules and board key" }
            div padding-top="12px" gap="9px" color=(theme::INK_MUTED) {
                span { "Place tiles in one row or column to form connected words. The opening play must cover the center star. All formed words must be accepted by this game’s pinned dictionary." }
                span { "A tile scores its printed value. DL and TL multiply a newly placed tile; DW and TW multiply the whole word. Premium squares apply only when first covered." }
                span { "Playing all " (rack_size) " rack tiles adds a " (full_rack_bonus) "-point full-rack bonus." }
                span { "Exchange replaces selected tiles and ends your turn. It is available only while at least " (exchange_requirement) " tiles remain in the reserve; exchanged tile identities stay private." }
                span { "Passing ends your turn without playing. Three uninterrupted complete rounds of passes by all active players end the game. For the classic two-player profile this is " (scoreless_turn_limit) " consecutive passes; the threshold scales with active membership." }
                span { "Resigning removes that player from turn rotation. Their rack stays locked and hidden while play continues; resignation ends the game only when fewer than two active players remain." }
                span { "When the game ends, remaining rack values are applied according to the completion reason. Final adjusted scores determine the ordered standings, and equal highest scores produce co-leaders." }
            }
        }
    }
    .into()
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
    container! {
        section id="remaining-tiles" gap="8px" {
            h3 { "Tiles remaining" }
            @if let Some(count) = game.view.remaining_tile_count {
                span { (count) " tile" (if count == 1 { "" } else { "s" }) " in the bag" }
            }
            @if let Some(faces) = faces {
                div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap="6px" {
                    @for face in faces {
                        span background=(theme::TILE) color=(theme::INK) border=((theme::TILE_EDGE, 1))
                            border-radius="7px" padding-y="5px" padding-x="8px" {
                            (face.letter.map_or_else(|| "Blank".to_string(), |letter| letter.to_string()))
                            " × " (face.count)
                        }
                    }
                }
            }
        }
    }
    .into()
}

#[allow(clippy::too_many_lines)]
pub fn visual_game_page(
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
    container! {
        div id="app-page" font-family=(theme::FONT_UI) class="game-scene" data-shared-state-channel=(game_channel.as_str())
            direction="column" width=vw100 height=dvh100 min-height=dvh100
            position="fixed" top=0 right=0 bottom=0 left=0
            overflow-x="hidden" overflow-y="hidden"
            background=(theme::NIGHT) color=(theme::NIGHT_INK) gap="6px" {
            span id="game-live-refresh" hidden fx-global-shared-state-event=(refresh_game) { }
            header id="scene-controls" width="100%" min-width=0 direction="row"
                align-items="center" gap="6px" padding-y="8px" padding-x="6px" {
                div class="header-action-slot header-action-left" min-width=0 flex=1 direction="row" justify-content="start" {
                    anchor href="/" flex="0 0 auto" background=(theme::NIGHT_RAISED) color=(theme::NIGHT_INK) border=((theme::NIGHT_LINE, 1))
                        border-radius="999px" padding-y="7px" padding-x="12px" font-weight=bold { "← Leave" }
                }
                (scoreboard)
                div class="header-action-slot header-action-right" min-width=0 flex=1 direction="row" justify-content="end" gap="6px" {
                    @if game.view.remaining_tile_count.is_some() || game.view.remaining_tile_faces.is_some() {
                        button id="remaining-tiles-button" type=button
                            fx-click=(ActionType::toggle_display_by_id("remaining-tiles-panel"))
                            background=(theme::NIGHT_RAISED) color=(theme::NIGHT_INK) border=((theme::NIGHT_LINE, 1))
                            border-radius="999px" padding-y="7px" padding-x="12px" font-weight=bold cursor=pointer {
                            "Bag"
                            @if let Some(count) = game.view.remaining_tile_count {
                                " · " (count)
                            }
                        }
                    }
                    button type=button fx-click=(ActionType::toggle_display_by_id("activity-rail"))
                        background=(theme::NIGHT_RAISED) color=(theme::NIGHT_INK) border=((theme::NIGHT_LINE, 1))
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
                    background=(theme::NIGHT_RAISED) border=((theme::NIGHT_BORDER, 3)) border-radius="16px"
                    padding-y="6px" padding-x="8px" gap="5px" {
                    @if let Some(completed_summary) = completed_summary {
                        (completed_summary)
                    } @else if error.is_some() {
                        (turn_feedback_view)
                    } @else if game.view.active_player == game.viewer_player {
                        (draft_preview)
                    } @else {
                        section class="dock-message" width="100%" direction="row" justify-content="center"
                            background=(theme::NIGHT_DEEP) border=((theme::NIGHT_LINE, 1)) border-radius="10px" padding-y="4px" padding-x="8px" {
                            span font-size="13px" font-weight=bold white-space="preserve" text-overflow="ellipsis" {
                                (game.participants.iter().find(|participant| participant.player_id == game.view.active_player).map_or("Player", |participant| participant.display_name.as_str())) " is playing · rearrange your rack while you wait"
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
                                button type=submit padding-y=7 padding-x=12 background=(theme::WHITE) color=(theme::LINK)
                                    border=((theme::SECONDARY_EDGE, 1)) border-radius="9px" cursor=pointer { "Shuffle" }
                            }
                            @if !draft.placements.is_empty() {
                                form hx-post=(compose_action.as_str()) hx-target="#app-page" {
                                    (compose_form_fields(game, draft, "CLEAR"))
                                    button type=submit padding-y=7 padding-x=12 background=(theme::WHITE) color=(theme::LINK)
                                        border=((theme::SECONDARY_EDGE, 1)) border-radius="9px" cursor=pointer { "Recall" }
                                }
                            }
                        }
                    }
                }
            }
            @if game.view.remaining_tile_count.is_some() || game.view.remaining_tile_faces.is_some() {
                aside id="remaining-tiles-panel" hidden position="fixed" top=62 right=6
                    width="340px" max-width="92vw" max-height="72vh" overflow-y="auto"
                    background=(theme::SURFACE_ALT) color=(theme::INK) border=((theme::BORDER_STRONG, 3)) border-radius="18px"
                    padding-y="16px" padding-x="16px" gap="12px" {
                    div direction="row" justify-content="space-between" align-items="start" gap="8px" {
                        h2 { "Bag" }
                        button type=button fx-click=(ActionType::no_display_by_id("remaining-tiles-panel"))
                            background=(theme::WHITE) color=(theme::LINK) border=((theme::SECONDARY_EDGE, 1)) border-radius="999px"
                            padding-y="5px" padding-x="9px" cursor=pointer { "Close" }
                    }
                    (remaining_tiles_component(game))
                }
            }
            aside id="activity-rail" hidden position="fixed" top=62 right=6 width="340px" max-width="92vw"
                max-height="78vh" overflow-y="auto" background=(theme::SURFACE_ALT) color=(theme::INK)
                border=((theme::BORDER_STRONG, 3)) border-radius="18px" padding-y="16px" padding-x="16px" gap="14px" {
                div direction="row" justify-content="space-between" align-items="start" gap="8px" {
                    div gap="2px" {
                        span color=(theme::INK_MUTED) font-size="11px" font-weight=bold { "MATCH " (short_game_id) }
                        h2 { (game.opponent_display_name.as_str()) " vs " (game.viewer_display_name.as_str()) }
                    }
                    button type=button fx-click=(ActionType::no_display_by_id("activity-rail"))
                        background=(theme::WHITE) color=(theme::LINK) border=((theme::SECONDARY_EDGE, 1)) border-radius="999px"
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
                background=(theme::MENU_BG) color=(theme::INK) border=((theme::BORDER_STRONG, 3)) border-radius="18px"
                padding="18px" gap="12px" {
                span color=(theme::INK_MUTED) font-size="11px" font-weight=bold { "WORD DEFINITION" }
                span { "Loading definition…" }
            }
            (blank_letter_layer)
        }
    }
    .into()
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
}
