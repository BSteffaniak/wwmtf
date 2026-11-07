//! Renderer-neutral building blocks for the "game night" look.
//!
//! Every helper returns a single `HyperChad` [`Container`] element styled only with template
//! attributes and `fx-*` actions (no stylesheets, no application JavaScript). Helpers never wrap
//! their output in an extra `div`, so they can be spliced directly into flex rows:
//!
//! ```ignore
//! container! {
//!     form hx-post="/dashboard/action" hx-target="#app-page" {
//!         input type=hidden name="action" value="ACCEPT_CHALLENGE";
//!         (ui::submit_button("Accept", ButtonVariant::Primary, ButtonSize::Medium))
//!     }
//! }
//! ```
//!
//! Chunky-3D recipe: side borders `EDGE_THIN`, bottom border `EDGE_DEPTH` in a darker edge
//! color. Pressed/selected elements use `EDGE_PRESSED` bottom and `translate-y=PRESS_OFFSET`.

use hyperchad::{actions::ActionType, router::Container, template::container};

use crate::theme::{self, SeatColor};

/// Unwraps a single-element `container!` result so helpers do not add wrapper `div`s.
fn single(mut items: Vec<Container>) -> Container {
    if items.len() == 1 {
        items.remove(0)
    } else {
        Container::from(items)
    }
}

/// Adds an element id to a helper's output (for test hooks or `fx-*` targets).
#[must_use]
pub fn with_id(mut container: Container, id: impl Into<String>) -> Container {
    container.str_id = Some(id.into());
    container
}

/// Adds a class to a helper's output (for test hooks such as `primary-turn-action`).
#[must_use]
pub fn with_class(mut container: Container, class: impl Into<String>) -> Container {
    container.classes.push(class.into());
    container
}

/// Adds a `data-*` attribute (key without the `data-` prefix) to a helper's output.
#[must_use]
pub fn with_data(
    mut container: Container,
    key: impl Into<String>,
    value: impl Into<String>,
) -> Container {
    container.data.insert(key.into(), value.into());
    container
}

// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

/// Visual role of a button or link-button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonVariant {
    /// Mint primary action ("Play", "Create game", "Accept").
    Primary,
    /// White secondary action on light or night surfaces ("Shuffle", "Cancel").
    Secondary,
    /// Grape accent action ("Invite", feature callouts).
    Accent,
    /// Sunny highlight action.
    Sunny,
    /// Coral destructive action ("Resign", "Revoke", "Leave").
    Danger,
    /// Raised night pill for controls on the game scene ("Menu", "Bag", zoom).
    Night,
    /// Borderless text-like action on light surfaces.
    Ghost,
}

/// Button padding/font scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonSize {
    /// Compact controls (zoom, close, chips).
    Small,
    /// Default size.
    Medium,
    /// Hero call-to-action.
    Large,
}

/// Resolved colors for a button variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonStyle {
    /// Resting background.
    pub background: &'static str,
    /// Background applied by `fx-hover`.
    pub hover: &'static str,
    /// Chunky edge (side and bottom border) color.
    pub edge: &'static str,
    /// Label color.
    pub ink: &'static str,
}

/// Returns the palette for a button variant.
#[must_use]
pub const fn button_style(variant: ButtonVariant) -> ButtonStyle {
    match variant {
        ButtonVariant::Primary => ButtonStyle {
            background: theme::PRIMARY,
            hover: theme::PRIMARY_HI,
            edge: theme::PRIMARY_EDGE,
            ink: theme::ON_PRIMARY,
        },
        ButtonVariant::Secondary => ButtonStyle {
            background: theme::SECONDARY,
            hover: theme::SECONDARY_HI,
            edge: theme::SECONDARY_EDGE,
            ink: theme::ON_SECONDARY,
        },
        ButtonVariant::Accent => ButtonStyle {
            background: theme::ACCENT,
            hover: theme::ACCENT_HI,
            edge: theme::ACCENT_EDGE,
            ink: theme::ON_ACCENT,
        },
        ButtonVariant::Sunny => ButtonStyle {
            background: theme::SUNNY,
            hover: theme::TILE_HI,
            edge: theme::SUNNY_EDGE,
            ink: theme::ON_SUNNY,
        },
        ButtonVariant::Danger => ButtonStyle {
            background: theme::DANGER,
            hover: theme::DANGER_HI,
            edge: theme::DANGER_EDGE,
            ink: theme::ON_DANGER,
        },
        ButtonVariant::Night => ButtonStyle {
            background: theme::NIGHT_RAISED,
            hover: theme::NIGHT_RAISED_HI,
            edge: theme::NIGHT_BORDER,
            ink: theme::NIGHT_INK,
        },
        ButtonVariant::Ghost => ButtonStyle {
            background: theme::SURFACE,
            hover: theme::SURFACE_LAVENDER,
            edge: theme::SURFACE,
            ink: theme::LINK,
        },
    }
}

/// `(padding_y, padding_x, font_size, radius)` for a button size.
#[must_use]
pub const fn button_metrics(size: ButtonSize) -> (i32, i32, i32, i32) {
    match size {
        ButtonSize::Small => (6, 12, theme::TEXT_SM, theme::RADIUS_PILL),
        ButtonSize::Medium => (10, 18, theme::TEXT_MD, theme::RADIUS_MD),
        ButtonSize::Large => (14, 26, theme::TEXT_LG, theme::RADIUS_LG),
    }
}

/// A `type=submit` chunky button. Place it inside a caller-owned `form` that carries the hidden
/// inputs, so handler-consumed field names stay in the page module.
#[must_use]
pub fn submit_button(label: &str, variant: ButtonVariant, size: ButtonSize) -> Container {
    let style = button_style(variant);
    let (padding_y, padding_x, font_size, radius) = button_metrics(size);
    let (edge, depth) = chunky_edges(variant, size);
    single(container! {
        button type=submit
            padding-y=(padding_y) padding-x=(padding_x)
            background=(style.background) color=(style.ink)
            border=((style.edge, edge)) border-bottom=((style.edge, depth))
            border-radius=(radius) font-size=(font_size) font-weight=bold
            font-family=(theme::FONT_UI) cursor=pointer
            align-items="center" justify-content="center" text-align="center"
            fx-hover=(ActionType::set_background_self(style.hover)) {
            (label)
        }
    })
}

/// A `type=button` chunky button that runs a client action (for example
/// `ActionType::toggle_display_by_id("activity-rail")`).
#[must_use]
pub fn action_button(
    label: &str,
    variant: ButtonVariant,
    size: ButtonSize,
    action: ActionType,
) -> Container {
    let style = button_style(variant);
    let (padding_y, padding_x, font_size, radius) = button_metrics(size);
    let (edge, depth) = chunky_edges(variant, size);
    single(container! {
        button type=button fx-click=(action)
            padding-y=(padding_y) padding-x=(padding_x)
            background=(style.background) color=(style.ink)
            border=((style.edge, edge)) border-bottom=((style.edge, depth))
            border-radius=(radius) font-size=(font_size) font-weight=bold
            font-family=(theme::FONT_UI) cursor=pointer
            align-items="center" justify-content="center" text-align="center"
            fx-hover=(ActionType::set_background_self(style.hover)) {
            (label)
        }
    })
}

/// An anchor styled as a chunky button.
#[must_use]
pub fn link_button(href: &str, label: &str, variant: ButtonVariant, size: ButtonSize) -> Container {
    let style = button_style(variant);
    let (padding_y, padding_x, font_size, radius) = button_metrics(size);
    let (edge, depth) = chunky_edges(variant, size);
    single(container! {
        anchor href=(href)
            padding-y=(padding_y) padding-x=(padding_x)
            background=(style.background) color=(style.ink)
            border=((style.edge, edge)) border-bottom=((style.edge, depth))
            border-radius=(radius) font-size=(font_size) font-weight=bold
            text-decoration="none" text-align="center" cursor=pointer
            align-items="center" justify-content="center"
            fx-hover=(ActionType::set_background_self(style.hover)) {
            (label)
        }
    })
}

/// A plain text link on light surfaces (for example "← Dashboard").
#[must_use]
pub fn text_link(href: &str, label: &str) -> Container {
    single(container! {
        anchor href=(href) color=(theme::LINK) font-weight=bold text-decoration="none" { (label) }
    })
}

/// `(side_border, bottom_border)` widths for a chunky button.
const fn chunky_edges(variant: ButtonVariant, size: ButtonSize) -> (i32, i32) {
    match (variant, size) {
        (ButtonVariant::Ghost, _) => (0, 0),
        (_, ButtonSize::Small) => (theme::EDGE_THIN, theme::EDGE_DEPTH - 2),
        _ => (theme::EDGE_THIN, theme::EDGE_DEPTH),
    }
}

// ---------------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------------

/// Page root for menu pages: `#app-page` on the warm cream ground with the UI font stack and a
/// centered column. Pass the page body (usually one or more cards).
///
/// Prefer this over hand-writing the root so every menu page keeps the same id, font, and ground.
#[must_use]
pub fn menu_page(children: impl Into<Vec<Container>>) -> Container {
    let children: Vec<Container> = children.into();
    single(container! {
        div id="app-page" direction="column" align-items="center" min-height="100vh"
            background=(theme::MENU_BG) color=(theme::INK) font-family=(theme::FONT_UI)
            padding-y=(theme::SPACE_8) padding-x=(theme::SPACE_4) gap=(theme::SPACE_6) {
            (children)
        }
    })
}

/// A white chunky card for menu pages. `max_width` is in pixels (`theme::PAGE_*`).
#[must_use]
pub fn card(max_width: i32, children: impl Into<Vec<Container>>) -> Container {
    let children: Vec<Container> = children.into();
    single(container! {
        section width="100%" max-width=(max_width)
            background=(theme::SURFACE) color=(theme::INK)
            border=((theme::BORDER, theme::EDGE_THIN)) border-bottom=((theme::BORDER_STRONG, theme::EDGE_DEPTH))
            border-radius=(theme::RADIUS_XL) padding=(theme::SPACE_6) gap=(theme::SPACE_4) {
            (children)
        }
    })
}

/// A raised night panel for the game scene (sheets, dock sections).
#[must_use]
pub fn night_card(children: impl Into<Vec<Container>>) -> Container {
    let children: Vec<Container> = children.into();
    single(container! {
        section background=(theme::NIGHT_RAISED) color=(theme::NIGHT_INK)
            border=((theme::NIGHT_BORDER, theme::EDGE_THIN)) border-bottom=((theme::NIGHT_BORDER, theme::EDGE_DEPTH))
            border-radius=(theme::RADIUS_LG) padding=(theme::SPACE_3) gap=(theme::SPACE_2) {
            (children)
        }
    })
}

/// Semantic tone for badges and notices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Mint success.
    Success,
    /// Coral danger / error.
    Danger,
    /// Sunny warning / your-turn highlight.
    Warning,
    /// Sky info.
    Info,
    /// Grape accent.
    Accent,
    /// Quiet neutral.
    Neutral,
}

/// `(background, border, ink)` for a tone on light surfaces.
#[must_use]
pub const fn tone_colors(tone: Tone) -> (&'static str, &'static str, &'static str) {
    match tone {
        Tone::Success => (theme::PRIMARY_SOFT, theme::PRIMARY, theme::PRIMARY_INK),
        Tone::Danger => (theme::DANGER_SOFT, theme::DANGER, theme::DANGER_INK),
        Tone::Warning => (theme::SUNNY_SOFT, theme::SUNNY, theme::ON_SUNNY),
        Tone::Info => (theme::INFO_SOFT, theme::INFO, theme::INK),
        Tone::Accent => (theme::SURFACE_LAVENDER, theme::ACCENT, theme::ACCENT_EDGE),
        Tone::Neutral => (theme::SURFACE_ALT, theme::BORDER, theme::INK_MUTED),
    }
}

/// A rounded notice banner (errors, confirmations). Keep caller-owned ids such as
/// `account-result` or `game-error` by wrapping with [`with_id`].
#[must_use]
pub fn notice(message: &str, tone: Tone) -> Container {
    let (background, border, ink) = tone_colors(tone);
    single(container! {
        section background=(background) color=(ink)
            border=((border, theme::EDGE_THIN)) border-radius=(theme::RADIUS_MD)
            padding-y=(theme::SPACE_3) padding-x=(theme::SPACE_4) font-weight=bold {
            (message)
        }
    })
}

/// A small pill with arbitrary fill/ink (for example seat-colored score chips).
#[must_use]
pub fn pill(label: &str, fill: &str, ink: &str) -> Container {
    single(container! {
        span background=(fill) color=(ink) border-radius=(theme::RADIUS_PILL)
            padding-y=3 padding-x=10 font-size=(theme::TEXT_SM) font-weight=bold
            white-space="preserve" align-items="center" justify-content="center" {
            (label)
        }
    })
}

/// A toned status badge ("Your turn", "You won", "Waiting").
#[must_use]
pub fn badge(label: &str, tone: Tone) -> Container {
    let (background, border, ink) = tone_colors(tone);
    single(container! {
        span background=(background) color=(ink) border=((border, theme::EDGE_THIN))
            border-radius=(theme::RADIUS_PILL) padding-y=2 padding-x=10
            font-size=(theme::TEXT_XS) font-weight=bold white-space="preserve" {
            (label)
        }
    })
}

/// A circular avatar: the profile image when available, otherwise the display name's initial on
/// the seat color. `size` is in pixels.
#[must_use]
pub fn avatar(
    display_name: &str,
    avatar_url: Option<&str>,
    seat: SeatColor,
    size: i32,
) -> Container {
    let initial = display_name
        .chars()
        .find(|character| character.is_alphanumeric())
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    let alt = format!("{display_name} profile avatar");
    let font_size = (size * 9) / 20;
    single(container! {
        @if let Some(avatar_url) = avatar_url {
            image src=(avatar_url) alt=(alt) width=(size) height=(size)
                border-radius=(theme::RADIUS_PILL) border=((seat.edge, theme::EDGE_THIN));
        } @else {
            span width=(size) height=(size) min-width=(size) border-radius=(theme::RADIUS_PILL)
                background=(seat.fill) color=(seat.ink) border=((seat.edge, theme::EDGE_THIN))
                align-items="center" justify-content="center"
                font-family=(theme::FONT_DISPLAY) font-size=(font_size) font-weight=bold {
                (initial)
            }
        }
    })
}

/// A small uppercase kicker label above headings (write the text in uppercase yourself).
#[must_use]
pub fn eyebrow(text: &str, color: &str) -> Container {
    single(container! {
        span color=(color) font-size=(theme::TEXT_XS) font-weight=bold { (text) }
    })
}

/// A section heading with an optional supporting line, for light surfaces.
#[must_use]
pub fn section_header(title: &str, subtitle: Option<&str>) -> Container {
    single(container! {
        div gap=(theme::SPACE_1) {
            h2 color=(theme::INK) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XL) font-weight=bold { (title) }
            @if let Some(subtitle) = subtitle {
                span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) { (subtitle) }
            }
        }
    })
}

/// A compact stat block (big value over a small label), on a soft tinted well.
#[must_use]
pub fn stat_tile(label: &str, value: &str, fill: &str, ink: &str) -> Container {
    single(container! {
        div flex=1 min-width=96 align-items="center" gap=2
            background=(fill) color=(ink) border-radius=(theme::RADIUS_LG)
            padding-y=(theme::SPACE_3) padding-x=(theme::SPACE_3) {
            span font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_XL) font-weight=bold { (value) }
            span font-size=(theme::TEXT_XS) font-weight=bold { (label) }
        }
    })
}

/// A friendly empty-state block: a big glyph tile, a title, and a supporting line.
#[must_use]
pub fn empty_state(glyph: &str, title: &str, body: &str) -> Container {
    let glyph_tile = tile_face(glyph, None, 52);
    single(container! {
        div align-items="center" gap=(theme::SPACE_2) padding=(theme::SPACE_6)
            background=(theme::SURFACE_ALT) border=((theme::BORDER, theme::EDGE_THIN))
            border-radius=(theme::RADIUS_LG) text-align="center" {
            (glyph_tile)
            span color=(theme::INK) font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_LG) font-weight=bold { (title) }
            span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) { (body) }
        }
    })
}

// ---------------------------------------------------------------------------
// Tiles and brand
// ---------------------------------------------------------------------------

/// Colors for a tile face.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileColors {
    /// Face fill.
    pub face: &'static str,
    /// Chunky edge.
    pub edge: &'static str,
    /// Letter ink.
    pub ink: &'static str,
}

/// Standard sunny letter tile.
pub const TILE_SUNNY: TileColors = TileColors {
    face: theme::TILE,
    edge: theme::TILE_EDGE,
    ink: theme::TILE_INK,
};

/// Drafted (pending) tile.
pub const TILE_PENDING: TileColors = TileColors {
    face: theme::TILE_PENDING,
    edge: theme::TILE_PENDING_EDGE,
    ink: theme::TILE_PENDING_INK,
};

/// A sunny chunky letter tile of `size` pixels with an optional point value in the corner.
#[must_use]
pub fn tile_face(letter: &str, points: Option<u8>, size: i32) -> Container {
    tile_face_colored(letter, points, size, TILE_SUNNY)
}

/// A chunky letter tile with explicit colors (pending tiles, seat-colored logos).
///
/// The letter is rendered in the display stack at ~45% of `size`; points at ~22%. The bottom
/// border is the darker edge to fake depth. Callers that need test hooks (`rack-tile-face`,
/// `rack-tile-points`) should build the tile inline using the same recipe, or attach classes to
/// the returned container with [`with_class`].
#[must_use]
pub fn tile_face_colored(
    letter: &str,
    points: Option<u8>,
    size: i32,
    colors: TileColors,
) -> Container {
    let letter_size = (size * 9) / 20;
    let points_size = ((size * 22) / 100).max(8);
    let depth = (size / 10).clamp(2, theme::EDGE_DEPTH);
    let radius = (size / 5).clamp(4, theme::RADIUS_MD);
    single(container! {
        span width=(size) height=(size) min-width=(size) position="relative"
            background=(colors.face) color=(colors.ink)
            border=((colors.edge, 1)) border-bottom=((colors.edge, depth))
            border-radius=(radius) align-items="center" justify-content="center"
            font-family=(theme::FONT_DISPLAY) font-weight=bold user-select="none" {
            span font-size=(letter_size) { (letter) }
            @if let Some(points) = points {
                span position="absolute" right=(size / 12) bottom=(size / 20) font-size=(points_size) { (points) }
            }
        }
    })
}

/// The brand mark: "WORDS" spelled in sunny tiles with the tagline underneath.
///
/// `tile_size` is in pixels (around 40 for a hero, 24 for a header). `on_night` switches the
/// tagline ink for the dark game scene.
#[must_use]
pub fn brand_logo(tile_size: i32, on_night: bool) -> Container {
    let tiles = [("W", 4), ("O", 1), ("R", 1), ("D", 2), ("S", 1)]
        .into_iter()
        .enumerate()
        .map(|(index, (letter, points))| {
            let mut tile = tile_face(letter, Some(points), tile_size);
            // Alternate a small vertical bob for a playful hand-placed feel.
            if index % 2 == 1 {
                tile.translate_y = Some(hyperchad::transformer::Number::Integer(i64::from(
                    tile_size / 10,
                )));
            }
            tile
        })
        .collect::<Vec<_>>();
    let tagline_color = if on_night {
        theme::NIGHT_INK_MUTED
    } else {
        theme::ACCENT
    };
    single(container! {
        div class="brand-logo" align-items="center" gap=(theme::SPACE_2) {
            div direction="row" gap=(tile_size / 8) align-items="start" { (tiles) }
            span color=(tagline_color) font-family=(theme::FONT_DISPLAY)
                font-size=((tile_size * 2) / 5) font-weight=bold white-space="preserve" {
                "with more than friends"
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(container: &Container) -> String {
        container
            .display_to_string(false, false)
            .expect("helper renders")
    }

    #[test]
    fn submit_button_is_a_single_submit_element_with_hover() {
        let button = submit_button("Play", ButtonVariant::Primary, ButtonSize::Medium);
        let html = render(&button);
        assert!(html.starts_with("<button"), "{html}");
        assert!(html.contains("type=\"submit\""));
        assert!(html.contains("Play"));
        assert!(
            html.contains("fx-hover") || html.contains("v-onmouseover"),
            "{html}"
        );
    }

    #[test]
    fn link_button_and_ids_compose() {
        let link = with_id(
            link_button(
                "/lobbies/new",
                "Create game",
                ButtonVariant::Primary,
                ButtonSize::Large,
            ),
            "create-game",
        );
        let html = render(&link);
        assert!(html.starts_with("<a"), "{html}");
        assert!(html.contains("href=\"/lobbies/new\""));
        assert!(html.contains("id=\"create-game\""));
    }

    #[test]
    fn avatar_falls_back_to_seat_colored_initial() {
        let html = render(&avatar("zoe", None, theme::seat_color(0), 32));
        assert!(html.contains(">Z<"), "{html}");
        let html = render(&avatar(
            "zoe",
            Some("/avatar.png"),
            theme::seat_color(0),
            32,
        ));
        assert!(html.contains("src=\"/avatar.png\""));
        assert!(html.contains("zoe profile avatar"));
    }

    #[test]
    fn brand_logo_spells_words_in_tiles() {
        let html = render(&brand_logo(32, false));
        for letter in ["W", "O", "R", "D", "S"] {
            assert!(html.contains(&format!(">{letter}<")), "{html}");
        }
        assert!(html.contains("with more than friends"));
    }

    #[test]
    fn mobile_responsive_overrides_are_available_to_page_modules() {
        use hyperchad::template::if_responsive;

        let containers = container! {
            div id="responsive-probe"
                padding-x=(if_responsive(theme::MOBILE).then::<i32>(theme::SPACE_2).or_else(theme::SPACE_6)) {
                "probe"
            }
        };
        assert_eq!(containers.len(), 1);
        assert!(!containers[0].overrides.is_empty());
    }

    #[test]
    fn helpers_never_add_stylesheets_or_scripts() {
        for container in [
            card(theme::PAGE_NARROW, Vec::new()),
            notice("Oops", Tone::Danger),
            empty_state("?", "Nothing here", "Start a game"),
            stat_tile("WINS", "3", theme::PRIMARY_SOFT, theme::PRIMARY_INK),
            badge("Your turn", Tone::Warning),
            pill("12", theme::seat_color(1).fill, theme::seat_color(1).ink),
            section_header("Games", Some("Your active matches")),
            menu_page(Vec::new()),
        ] {
            let html = render(&container);
            assert!(!html.contains("<style"));
            assert!(!html.contains("<script"));
        }
    }
}

/// Visual reference for the helpers. Run with
/// `SHOWCASE_OUT=/abs/path/showcase.html cargo nextest run -p wwmtf_app --run-ignored only ui_showcase`
/// and screenshot the file; it embeds `HyperChad`'s own root HTML so layout matches the app.
#[cfg(test)]
mod showcase {
    use super::*;

    #[test]
    #[ignore = "writes a visual reference page to SHOWCASE_OUT"]
    fn ui_showcase() {
        let page = menu_page(container! {
            (brand_logo(44, false))
            (card(theme::PAGE_NARROW, container! {
                (section_header("Your games", Some("Pick up where you left off")))
                div direction="row" gap=8 {
                    (submit_button("Play", ButtonVariant::Primary, ButtonSize::Medium))
                    (submit_button("Shuffle", ButtonVariant::Secondary, ButtonSize::Medium))
                    (submit_button("Resign", ButtonVariant::Danger, ButtonSize::Medium))
                }
                div direction="row" gap=8 align-items="center" {
                    (link_button("/", "Create game", ButtonVariant::Accent, ButtonSize::Large))
                    (submit_button("Close", ButtonVariant::Night, ButtonSize::Small))
                    (badge("Your turn", Tone::Warning))
                    (pill("42", theme::seat_color(1).fill, theme::seat_color(1).ink))
                }
                div direction="row" gap=8 align-items="center" {
                    (avatar("alice", None, theme::seat_color(0), 40))
                    (avatar("bob", None, theme::seat_color(1), 40))
                    (tile_face("Q", Some(10), 44))
                    (tile_face_colored("Z", Some(10), 44, TILE_PENDING))
                }
                div direction="row" gap=8 {
                    (stat_tile("WINS", "3", theme::PRIMARY_SOFT, theme::PRIMARY_INK))
                    (stat_tile("POINTS", "812", theme::SUNNY_SOFT, theme::ON_SUNNY))
                }
                (notice("That word is not in the dictionary.", Tone::Danger))
                (empty_state("?", "No games yet", "Start one and invite a friend."))
            }))
        });
        let root = Container::from(vec![page]);
        let html = hyperchad::renderer_html::html::container_element_to_html_response(
            &std::collections::BTreeMap::new(),
            &root,
            None,
            None,
            None,
            None,
            &hyperchad::renderer_html::DefaultHtmlTagRenderer::default(),
            &[],
            &[],
            &[],
        )
        .expect("showcase renders");
        let out = std::env::var("SHOWCASE_OUT").expect("SHOWCASE_OUT names an output file");
        std::fs::write(out, html).expect("showcase writes");
    }
}
