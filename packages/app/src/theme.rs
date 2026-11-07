//! Renderer-neutral design tokens for the "game night" look.
//!
//! Every value here is consumed through `HyperChad` template attributes only (no stylesheets).
//! Colors are flat hex strings so they can be spliced into `container!` attributes, for example
//! `background=(theme::SURFACE)` or `border-bottom=((theme::TILE_EDGE, theme::EDGE_DEPTH))`.
//!
//! Palette roles:
//! - **Menu pages** (login, dashboard, lobby): warm sunny cream ground (`MENU_BG`), white cards
//!   (`SURFACE`), grape ink (`INK`).
//! - **Game scene**: deep grape night (`NIGHT*`) with light ink (`NIGHT_INK`).
//! - **Letter tiles**: sunny yellow faces (`TILE*`) with a darker amber bottom edge.
//! - **Premium squares**: sky (2L), blue (3L), pink (2W), rose (3W), lavender start star.
//! - **Actions**: mint primary (`PRIMARY*`, dark-green ink), grape accent (`ACCENT*`),
//!   coral danger (`DANGER*`), white secondary (`SECONDARY*`).
//! - **Seats**: one playful accent per player seat (`seat_color`).

// ---------------------------------------------------------------------------
// Typography
// ---------------------------------------------------------------------------

/// Rounded, friendly UI font stack used on every `#app-page` root.
pub const FONT_UI: &str = "ui-rounded, SF Pro Rounded, Nunito, Varela Round, Quicksand, system-ui, -apple-system, Segoe UI, Roboto, sans-serif";

/// Heavy display stack for tile letters, scores, and big headings.
pub const FONT_DISPLAY: &str = "Baloo 2, Fredoka, ui-rounded, SF Pro Rounded, Arial Rounded MT Bold, Nunito, system-ui, sans-serif";

/// Font sizes in pixels.
pub const TEXT_XS: i32 = 11;
/// Small supporting text.
pub const TEXT_SM: i32 = 13;
/// Body text.
pub const TEXT_MD: i32 = 15;
/// Emphasized body / card titles.
pub const TEXT_LG: i32 = 18;
/// Section headings.
pub const TEXT_XL: i32 = 24;
/// Page headings.
pub const TEXT_XXL: i32 = 32;
/// Hero / brand display.
pub const TEXT_DISPLAY: i32 = 44;

// ---------------------------------------------------------------------------
// Spacing, radii, and depth (pixels)
// ---------------------------------------------------------------------------

/// 4px spacing step.
pub const SPACE_1: i32 = 4;
/// 8px spacing step.
pub const SPACE_2: i32 = 8;
/// 12px spacing step.
pub const SPACE_3: i32 = 12;
/// 16px spacing step.
pub const SPACE_4: i32 = 16;
/// 20px spacing step.
pub const SPACE_5: i32 = 20;
/// 24px spacing step.
pub const SPACE_6: i32 = 24;
/// 32px spacing step.
pub const SPACE_8: i32 = 32;
/// 48px spacing step.
pub const SPACE_12: i32 = 48;

/// Small radius (chips, small tiles).
pub const RADIUS_SM: i32 = 8;
/// Medium radius (buttons, inputs).
pub const RADIUS_MD: i32 = 12;
/// Large radius (cards, panels).
pub const RADIUS_LG: i32 = 18;
/// Extra-large radius (hero cards, sheets).
pub const RADIUS_XL: i32 = 26;
/// Fully rounded pills and avatars.
pub const RADIUS_PILL: i32 = 999;

/// Thin side border width for chunky elements.
pub const EDGE_THIN: i32 = 2;
/// Bottom "depth" border width for chunky 3D buttons, tiles, and cards.
pub const EDGE_DEPTH: i32 = 5;
/// Reduced depth for pressed/selected chunky elements (pair with `translate-y=PRESS_OFFSET`).
pub const EDGE_PRESSED: i32 = 2;
/// Downward translate applied to pressed/selected chunky elements (`EDGE_DEPTH - EDGE_PRESSED`).
pub const PRESS_OFFSET: i32 = 3;
/// Upward lift applied to a picked/selected tile.
pub const LIFT_OFFSET: i32 = -6;

/// Max content width for menu pages (dashboard).
pub const PAGE_WIDE: i32 = 1080;
/// Max content width for medium menu pages (lobby, definition).
pub const PAGE_MEDIUM: i32 = 760;
/// Max content width for narrow menu pages (login, sign out).
pub const PAGE_NARROW: i32 = 480;

/// Name of the responsive trigger registered at runtime (`MaxWidth(MOBILE_MAX_WIDTH)`).
/// Use as `if_responsive(theme::MOBILE)` on an element with an explicit `id`.
pub const MOBILE: &str = "mobile";
/// Viewport width (px) at or below which the `mobile` trigger applies.
pub const MOBILE_MAX_WIDTH: i32 = 600;

/// Every named responsive trigger the page modules reference through `if_responsive(..)`.
///
/// Renderer wiring must register each `(name, trigger)` pair (for example with
/// `Renderer::add_responsive_trigger`) so any renderer, not only HTML, applies the same
/// breakpoint overrides.
#[must_use]
pub fn responsive_triggers() -> Vec<(String, hyperchad::transformer::ResponsiveTrigger)> {
    vec![(
        MOBILE.to_string(),
        hyperchad::transformer::ResponsiveTrigger::MaxWidth(
            hyperchad::transformer::Number::Integer(i64::from(MOBILE_MAX_WIDTH)),
        ),
    )]
}

// ---------------------------------------------------------------------------
// Neutrals and menu pages (light)
// ---------------------------------------------------------------------------

/// Pure white.
pub const WHITE: &str = "#ffffff";
/// Warm sunny cream page ground for menu pages.
pub const MENU_BG: &str = "#fff4dc";
/// Slightly deeper cream for bands and inset areas on menu pages.
pub const MENU_BG_DEEP: &str = "#ffe9bd";
/// Card / panel surface.
pub const SURFACE: &str = "#ffffff";
/// Alternate soft surface (list rows, inset wells).
pub const SURFACE_ALT: &str = "#fff9ee";
/// Soft lavender surface for informational wells.
pub const SURFACE_LAVENDER: &str = "#f1ecff";
/// Hairline border on light surfaces.
pub const BORDER: &str = "#efdcbc";
/// Stronger border / chunky card bottom edge on light surfaces.
pub const BORDER_STRONG: &str = "#e2c48e";
/// Input border.
pub const INPUT_BORDER: &str = "#d9c7f5";
/// Primary text on light surfaces (deep grape).
pub const INK: &str = "#2b1d4e";
/// Secondary text on light surfaces.
pub const INK_MUTED: &str = "#5f5180";
/// Tertiary / hint text on light surfaces.
pub const INK_SOFT: &str = "#8a7ea8";
/// Link / interactive text on light surfaces.
pub const LINK: &str = "#6a3fe0";

// ---------------------------------------------------------------------------
// Game scene (night)
// ---------------------------------------------------------------------------

/// Game scene ground (deep grape night).
pub const NIGHT: &str = "#251a4f";
/// Deepest night (header bars, wells).
pub const NIGHT_DEEP: &str = "#1a1239";
/// Raised night surface (dock, pills).
pub const NIGHT_RAISED: &str = "#35287a";
/// Higher raised night surface (hovered pills).
pub const NIGHT_RAISED_HI: &str = "#4a3a9e";
/// Border / bottom edge on night surfaces.
pub const NIGHT_BORDER: &str = "#140d2e";
/// Hairline on night surfaces.
pub const NIGHT_LINE: &str = "#4c3d93";
/// Primary text on night surfaces.
pub const NIGHT_INK: &str = "#f7f2ff";
/// Secondary text on night surfaces.
pub const NIGHT_INK_MUTED: &str = "#c3b8ec";
/// Translucent scrim behind modal sheets (8-digit hex with alpha).
pub const SCRIM: &str = "#120a2acc";

// ---------------------------------------------------------------------------
// Board and tiles
// ---------------------------------------------------------------------------

/// Board frame color.
pub const BOARD_FRAME: &str = "#3d2e86";
/// Board frame outer edge (darker, used for the chunky bottom).
pub const BOARD_FRAME_EDGE: &str = "#20175a";
/// Empty board square.
pub const SQUARE: &str = "#ece6ff";
/// Label ink on empty squares.
pub const SQUARE_INK: &str = "#8a7fb8";
/// Hairline around empty squares.
pub const SQUARE_LINE: &str = "#cfc4f2";

/// Sunny letter tile face.
pub const TILE: &str = "#ffd166";
/// Highlight variant of the tile face (picked / hovered).
pub const TILE_HI: &str = "#ffe39a";
/// Darker amber edge for the tile's chunky bottom/right border.
pub const TILE_EDGE: &str = "#d99a1e";
/// Letter ink on tiles.
pub const TILE_INK: &str = "#3a2608";
/// Dimmed tile (already placed from rack).
pub const TILE_DIM: &str = "#d8c08a";

/// Pending (drafted, not yet played) tile face.
pub const TILE_PENDING: &str = "#bdf0ff";
/// Pending tile edge.
pub const TILE_PENDING_EDGE: &str = "#2f9ccf";
/// Pending tile ink.
pub const TILE_PENDING_INK: &str = "#0b3a57";

/// Tile marked for exchange.
pub const TILE_EXCHANGE: &str = "#ffc2b8";
/// Tile marked for exchange edge.
pub const TILE_EXCHANGE_EDGE: &str = "#ff6b5e";

/// Highlight ring for the latest move's squares.
pub const LATEST_MOVE: &str = "#3ddc97";

/// Double-letter premium square.
pub const PREMIUM_DL: &str = "#8fe3ff";
/// Double-letter ink.
pub const PREMIUM_DL_INK: &str = "#0b4a66";
/// Triple-letter premium square.
pub const PREMIUM_TL: &str = "#4f8dff";
/// Triple-letter ink.
pub const PREMIUM_TL_INK: &str = "#ffffff";
/// Double-word premium square.
pub const PREMIUM_DW: &str = "#ffb0d6";
/// Double-word ink.
pub const PREMIUM_DW_INK: &str = "#7a1446";
/// Triple-word premium square.
pub const PREMIUM_TW: &str = "#ff4f7b";
/// Triple-word ink.
pub const PREMIUM_TW_INK: &str = "#ffffff";
/// Start (center star) square.
pub const PREMIUM_START: &str = "#c9a8ff";
/// Start square ink.
pub const PREMIUM_START_INK: &str = "#3b1680";

// ---------------------------------------------------------------------------
// Actions and status
// ---------------------------------------------------------------------------

/// Primary action / success (mint).
pub const PRIMARY: &str = "#3ddc97";
/// Primary hovered.
pub const PRIMARY_HI: &str = "#5ee8ab";
/// Primary chunky edge.
pub const PRIMARY_EDGE: &str = "#1f9e68";
/// Text on primary backgrounds.
pub const ON_PRIMARY: &str = "#06371f";
/// Soft success background.
pub const PRIMARY_SOFT: &str = "#d8fbe9";
/// Success text on light surfaces.
pub const PRIMARY_INK: &str = "#137a4c";

/// Accent (grape) action.
pub const ACCENT: &str = "#7c5cff";
/// Accent hovered.
pub const ACCENT_HI: &str = "#9478ff";
/// Accent chunky edge.
pub const ACCENT_EDGE: &str = "#5636d6";
/// Text on accent backgrounds.
pub const ON_ACCENT: &str = "#ffffff";

/// Secondary action background.
pub const SECONDARY: &str = "#ffffff";
/// Secondary hovered.
pub const SECONDARY_HI: &str = "#f6f1ff";
/// Secondary chunky edge.
pub const SECONDARY_EDGE: &str = "#d6c9f2";
/// Text on secondary backgrounds.
pub const ON_SECONDARY: &str = "#3b2a6b";

/// Danger action (coral).
pub const DANGER: &str = "#ff6b5e";
/// Danger hovered.
pub const DANGER_HI: &str = "#ff8579";
/// Danger chunky edge.
pub const DANGER_EDGE: &str = "#cf3f33";
/// Text on danger backgrounds.
pub const ON_DANGER: &str = "#ffffff";
/// Soft danger background (error notices).
pub const DANGER_SOFT: &str = "#ffe4df";
/// Danger text on light surfaces.
pub const DANGER_INK: &str = "#b3261e";

/// Sunny warning / highlight.
pub const SUNNY: &str = "#ffd166";
/// Sunny chunky edge.
pub const SUNNY_EDGE: &str = "#d99a1e";
/// Text on sunny backgrounds.
pub const ON_SUNNY: &str = "#3a2608";
/// Soft warning background.
pub const SUNNY_SOFT: &str = "#fff1c7";

/// Info / sky.
pub const INFO: &str = "#4f8dff";
/// Soft info background.
pub const INFO_SOFT: &str = "#e2edff";

/// Selected blank-letter choice background. Browser acceptance asserts this exact value.
pub const BLANK_LETTER_CHOSEN: &str = "#e8f1e3";

// ---------------------------------------------------------------------------
// Player seats
// ---------------------------------------------------------------------------

/// Colors for one player seat: background fill, chunky edge, and readable ink on the fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeatColor {
    /// Fill color (avatar fallback, score pill).
    pub fill: &'static str,
    /// Darker edge for chunky borders.
    pub edge: &'static str,
    /// Readable text color on `fill`.
    pub ink: &'static str,
}

/// Playful per-seat accents, in seat order.
pub const SEAT_COLORS: [SeatColor; 8] = [
    SeatColor {
        fill: "#ff8fb8",
        edge: "#d9568a",
        ink: "#4a0b26",
    },
    SeatColor {
        fill: "#6fb1ff",
        edge: "#2f78d6",
        ink: "#0a2a55",
    },
    SeatColor {
        fill: "#ffb347",
        edge: "#d9861a",
        ink: "#4a2a00",
    },
    SeatColor {
        fill: "#3ddc97",
        edge: "#1f9e68",
        ink: "#06371f",
    },
    SeatColor {
        fill: "#b892ff",
        edge: "#7c55e0",
        ink: "#2a1061",
    },
    SeatColor {
        fill: "#3fd0e0",
        edge: "#1a98a8",
        ink: "#063b42",
    },
    SeatColor {
        fill: "#ffe066",
        edge: "#d9b31a",
        ink: "#3f3200",
    },
    SeatColor {
        fill: "#ff8a7a",
        edge: "#d65a4a",
        ink: "#4a0f08",
    },
];

/// Returns the accent for a zero-based seat index, cycling past the palette length.
#[must_use]
pub const fn seat_color(seat: usize) -> SeatColor {
    SEAT_COLORS[seat % SEAT_COLORS.len()]
}

/// Picks a stable seat accent from arbitrary text (for example a user id) when no seat index is
/// known. Deterministic across renders.
#[must_use]
pub fn seat_color_for(key: &str) -> SeatColor {
    let hash = key.bytes().fold(0_usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(usize::from(byte))
    });
    seat_color(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seat_colors_cycle_and_hash_deterministically() {
        assert_eq!(seat_color(0), seat_color(SEAT_COLORS.len()));
        assert_eq!(seat_color_for("alice"), seat_color_for("alice"));
    }

    #[test]
    fn blank_letter_selection_matches_browser_acceptance() {
        assert_eq!(BLANK_LETTER_CHOSEN, "#e8f1e3");
    }
}
