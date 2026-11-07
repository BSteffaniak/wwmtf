//! Account, invitation, sign-in, and generic error pages.
//!
//! Every page here shares one branded layout ([`auth_page`]): the tile logo on the warm cream
//! ground, a chunky white card with a big glyph tile, a kicker, a display heading, and a friendly
//! lead line, followed by page-specific actions and a playful tile footer.

use hyperchad::{
    actions::ActionType,
    router::Container,
    template::{container, if_responsive},
};

use crate::{
    theme,
    ui::{self, ButtonSize, ButtonVariant, TileColors, Tone},
};

/// Glyph tile colors for celebratory or neutral pages.
const GLYPH_SUNNY: TileColors = ui::TILE_SUNNY;
/// Glyph tile colors for success pages.
const GLYPH_MINT: TileColors = TileColors {
    face: theme::PRIMARY,
    edge: theme::PRIMARY_EDGE,
    ink: theme::ON_PRIMARY,
};
/// Glyph tile colors for invitation and account pages.
const GLYPH_GRAPE: TileColors = TileColors {
    face: theme::ACCENT,
    edge: theme::ACCENT_EDGE,
    ink: theme::ON_ACCENT,
};
/// Glyph tile colors for error pages.
const GLYPH_CORAL: TileColors = TileColors {
    face: theme::DANGER,
    edge: theme::DANGER_EDGE,
    ink: theme::ON_DANGER,
};

/// The heading block of an account card.
struct AuthCard<'a> {
    glyph: &'a str,
    glyph_colors: TileColors,
    kicker: &'a str,
    kicker_color: &'static str,
    title: &'a str,
    lead: &'a str,
}

/// Shared branded layout for every account, invitation, and error page.
///
/// Keeps `#app-page` (via [`ui::menu_page`]) and a `main` content element, which responsive
/// browser checks measure.
fn auth_page(card: &AuthCard<'_>, body: &[Container]) -> Container {
    let logo = ui::brand_logo(44, false);
    let glyph = ui::tile_face_colored(card.glyph, None, 64, card.glyph_colors);
    let kicker = ui::eyebrow(card.kicker, card.kicker_color);
    let footer = tile_footer();
    ui::menu_page(
        container! {
            anchor href="/" text-decoration="none" padding-top=(theme::SPACE_2) { (logo) }
            main id="auth-card" width="100%" max-width=(theme::PAGE_NARROW)
                background=(theme::SURFACE) color=(theme::INK)
                border=((theme::BORDER, theme::EDGE_THIN)) border-bottom=((theme::BORDER_STRONG, theme::EDGE_DEPTH))
                border-radius=(theme::RADIUS_XL)
                padding-x=(if_responsive(theme::MOBILE).then::<i32>(theme::SPACE_5).or_else(theme::SPACE_8))
                padding-y=(if_responsive(theme::MOBILE).then::<i32>(theme::SPACE_6).or_else(theme::SPACE_8))
                gap=(theme::SPACE_5) {
                header align-items="center" text-align="center" gap=(theme::SPACE_2) {
                    (glyph)
                    (kicker)
                    h1 margin=0 color=(theme::INK) font-family=(theme::FONT_DISPLAY)
                        font-size=(theme::TEXT_XXL) font-weight=bold { (card.title) }
                    span color=(theme::INK_MUTED) font-size=(theme::TEXT_MD) { (card.lead) }
                }
                (body.to_vec())
            }
            (footer)
        },
        None,
    )
}

/// A playful row of seat-colored tiles under the card.
fn tile_footer() -> Vec<Container> {
    let tiles = ["P", "L", "A", "Y"]
        .into_iter()
        .enumerate()
        .map(|(index, letter)| {
            let seat = theme::seat_color(index);
            let mut tile = ui::tile_face_colored(
                letter,
                None,
                26,
                TileColors {
                    face: seat.fill,
                    edge: seat.edge,
                    ink: seat.ink,
                },
            );
            if index % 2 == 0 {
                tile.translate_y = Some(hyperchad::transformer::Number::Integer(3));
            }
            tile
        })
        .collect::<Vec<_>>();
    container! {
        footer align-items="center" gap=(theme::SPACE_2) padding-bottom=(theme::SPACE_6) {
            div direction="row" gap=(theme::SPACE_1) align-items="start" { (tiles) }
            span color=(theme::INK_SOFT) font-size=(theme::TEXT_SM) font-weight=bold text-align="center" {
                "Grab a friend. Make some words."
            }
        }
    }
}

/// The hero "Continue with Google" anchor. It must stay an anchor with `target="_top"` so the
/// OIDC redirect leaves any embedding frame.
fn google_button(href: &str) -> Vec<Container> {
    let style = ui::button_style(ButtonVariant::Primary);
    let (padding_y, padding_x, font_size, radius) = ui::button_metrics(ButtonSize::Large);
    container! {
        anchor href=(href) target="_top" direction="row" align-items="center" justify-content="center"
            gap=(theme::SPACE_3) padding-y=(padding_y) padding-x=(padding_x)
            background=(style.background) color=(style.ink)
            border=((style.edge, theme::EDGE_THIN)) border-bottom=((style.edge, theme::EDGE_DEPTH))
            border-radius=(radius) font-size=(font_size) font-weight=bold
            text-decoration="none" text-align="center" cursor=pointer
            fx-hover=(ActionType::set_background_self(style.hover)) {
            span width=28 height=28 min-width=28 border-radius=(theme::RADIUS_PILL)
                background=(theme::WHITE) color=(theme::INFO) border=((style.edge, theme::EDGE_THIN))
                align-items="center" justify-content="center"
                font-family=(theme::FONT_DISPLAY) font-size=(theme::TEXT_MD) font-weight=bold { "G" }
            span { "Continue with Google" }
        }
    }
}

/// Which credential input a [`text_field`] renders.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    /// Username input that grabs focus on page load.
    UsernameFocused,
    /// Plain username input.
    Username,
    /// Password input.
    Password,
}

/// A chunky labelled credential input. Field names stay `username` / `password` as consumed by
/// the login and migration handlers.
fn text_field(field: Field) -> Vec<Container> {
    let label = if field == Field::Password {
        "Password"
    } else {
        "Username"
    };
    container! {
        div gap=(theme::SPACE_1) {
            span color=(theme::INK) font-size=(theme::TEXT_SM) font-weight=bold { (label) }
            @if field == Field::UsernameFocused {
                input type=text name="username" placeholder="Username" autofocus=true
                    padding-y=(theme::SPACE_3) padding-x=(theme::SPACE_4) font-size=(theme::TEXT_MD)
                    font-family=(theme::FONT_UI) color=(theme::INK) background=(theme::SURFACE_ALT)
                    border=((theme::INPUT_BORDER, theme::EDGE_THIN)) border-bottom=((theme::INPUT_BORDER, 4))
                    border-radius=(theme::RADIUS_MD);
            } @else if field == Field::Username {
                input type=text name="username" placeholder="Username"
                    padding-y=(theme::SPACE_3) padding-x=(theme::SPACE_4) font-size=(theme::TEXT_MD)
                    font-family=(theme::FONT_UI) color=(theme::INK) background=(theme::SURFACE_ALT)
                    border=((theme::INPUT_BORDER, theme::EDGE_THIN)) border-bottom=((theme::INPUT_BORDER, 4))
                    border-radius=(theme::RADIUS_MD);
            } @else {
                input type=password name="password" placeholder="Password"
                    padding-y=(theme::SPACE_3) padding-x=(theme::SPACE_4) font-size=(theme::TEXT_MD)
                    font-family=(theme::FONT_UI) color=(theme::INK) background=(theme::SURFACE_ALT)
                    border=((theme::INPUT_BORDER, theme::EDGE_THIN)) border-bottom=((theme::INPUT_BORDER, 4))
                    border-radius=(theme::RADIUS_MD);
            }
        }
    }
}

/// A soft "or" divider between sign-in methods.
fn or_divider() -> Vec<Container> {
    container! {
        div direction="row" align-items="center" gap=(theme::SPACE_3) {
            div flex=1 height=2 background=(theme::BORDER) border-radius=(theme::RADIUS_PILL) {}
            span color=(theme::INK_SOFT) font-size=(theme::TEXT_SM) font-weight=bold { "or" }
            div flex=1 height=2 background=(theme::BORDER) border-radius=(theme::RADIUS_PILL) {}
        }
    }
}

/// A centered row of secondary links or buttons.
fn link_row(children: &[Container]) -> Vec<Container> {
    container! {
        div direction="row" justify-content="center" align-items="center" gap=(theme::SPACE_4) {
            (children.to_vec())
        }
    }
}

pub fn invitation_joined_page(game_id: wwmtf_game_domain::GameId) -> Container {
    let game_href = format!("/games/{game_id}");
    let open = ui::link_button(
        &game_href,
        "Open game →",
        ButtonVariant::Primary,
        ButtonSize::Large,
    );
    let back = ui::text_link("/", "Back to games");
    auth_page(
        &AuthCard {
            glyph: "✓",
            glyph_colors: GLYPH_MINT,
            kicker: "INVITATION ACCEPTED",
            kicker_color: theme::PRIMARY_INK,
            title: "Your game is ready",
            lead: "The invitation was redeemed and the game was created. Time to play your first word!",
        },
        &container! {
            (open)
            (link_row(&[back]))
        },
    )
}

pub fn invitation_page(invitation_token: &str, signed_in: bool) -> Container {
    let login_href = format!("/login?invite={invitation_token}");
    let rules = ui::notice(
        "This invitation creates a private two-player game. It can be used once.",
        Tone::Accent,
    );
    let body = if signed_in {
        let accept = ui::submit_button(
            "Accept invitation",
            ButtonVariant::Primary,
            ButtonSize::Large,
        );
        let back = ui::text_link("/", "Back to dashboard");
        container! {
            (rules)
            form hx-post="/join" hx-target="#app-page" gap=(theme::SPACE_3) {
                input type=hidden name="action" value="REDEEM_INVITATION";
                input type=hidden name="invitation_token" value=(invitation_token);
                (accept)
            }
            (link_row(&[back]))
        }
    } else {
        let continue_button = ui::link_button(
            &login_href,
            "Continue with Google",
            ButtonVariant::Primary,
            ButtonSize::Large,
        );
        container! {
            (rules)
            span color=(theme::INK_MUTED) text-align="center" { "Continue with Google to accept this invitation." }
            (continue_button)
        }
    };
    auth_page(
        &AuthCard {
            glyph: "★",
            glyph_colors: GLYPH_GRAPE,
            kicker: "PRIVATE GAME INVITATION",
            kicker_color: theme::ACCENT,
            title: "You’ve been invited to play",
            lead: "A friend saved you a seat at the board.",
        },
        &body,
    )
}

/// Renders the renderer-neutral login form.
#[must_use]
pub fn login_page(error: Option<&str>) -> Container {
    login_page_with_invitation(error, "", false, true)
}

pub fn login_page_with_invitation(
    error: Option<&str>,
    invitation_token: &str,
    development_login: bool,
    google_login: bool,
) -> Container {
    let message = error.unwrap_or_default();
    let google_href = if invitation_token.is_empty() {
        "/auth/google/start".to_string()
    } else {
        format!("/auth/google/start?invite={invitation_token}")
    };
    let result = ui::with_id(ui::notice(message, Tone::Danger), "account-result");
    let dev_badge = ui::badge("DEV MODE", Tone::Warning);
    let dev_continue = ui::submit_button("Continue", ButtonVariant::Primary, ButtonSize::Large);
    let migrate = ui::text_link("/account/migrate", "Migrate it to Google");
    let home = ui::text_link("/", "← Home");
    let lead = if invitation_token.is_empty() {
        "Sign in to jump back into your games."
    } else {
        "Sign in to accept your invitation and start playing."
    };
    auth_page(
        &AuthCard {
            glyph: "Hi",
            glyph_colors: GLYPH_SUNNY,
            kicker: "WELCOME BACK",
            kicker_color: theme::ACCENT,
            title: "Ready for a rematch?",
            lead,
        },
        &container! {
            @if !message.is_empty() {
                (result)
            }
            @if development_login {
                section background=(theme::SUNNY_SOFT) border=((theme::SUNNY, theme::EDGE_THIN))
                    border-bottom=((theme::SUNNY_EDGE, 4)) border-radius=(theme::RADIUS_LG)
                    padding=(theme::SPACE_4) gap=(theme::SPACE_3) {
                    div direction="row" align-items="center" gap=(theme::SPACE_2) {
                        (dev_badge)
                        span color=(theme::ON_SUNNY) font-weight=bold { "Local development login" }
                    }
                    span color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) {
                        "Enter any username. It will be created automatically if needed."
                    }
                    form method="post" gap=(theme::SPACE_3) {
                        input type=hidden name="invitation_token" value=(invitation_token);
                        (text_field(Field::UsernameFocused))
                        (dev_continue)
                    }
                }
                @if google_login {
                    (or_divider())
                }
            }
            @if google_login {
                (google_button(&google_href))
                div direction="row" justify-content="center" align-items="center" gap=(theme::SPACE_1)
                    overflow-x=(hyperchad::template::LayoutOverflow::Wrap { grid: false })
                    color=(theme::INK_MUTED) font-size=(theme::TEXT_SM) text-align="center" {
                    span { "Have a username/password account?" }
                    (migrate)
                }
            }
            @if !development_login && !google_login {
                (ui::notice("No login method is configured for this server.", Tone::Neutral))
            }
            (link_row(&[home]))
        },
    )
}

/// Renders the explicit legacy-account migration form.
#[must_use]
pub fn migration_page(error: Option<&str>) -> Container {
    let message = error.unwrap_or_default();
    let result = ui::with_id(ui::notice(message, Tone::Danger), "account-result");
    let submit = ui::submit_button(
        "Continue with Google",
        ButtonVariant::Primary,
        ButtonSize::Large,
    );
    let back = ui::text_link("/login", "← Back to sign in");
    auth_page(
        &AuthCard {
            glyph: "→",
            glyph_colors: GLYPH_GRAPE,
            kicker: "MOVE TO GOOGLE SIGN-IN",
            kicker_color: theme::ACCENT,
            title: "Migrate your existing account",
            lead: "Confirm your existing credentials, then connect Google without losing your games.",
        },
        &container! {
            @if !message.is_empty() {
                (result)
            }
            form method="post" gap=(theme::SPACE_4) {
                (text_field(Field::Username))
                (text_field(Field::Password))
                (submit)
            }
            (link_row(&[back]))
        },
    )
}

/// Renders logout confirmation. Session revocation is performed by the authenticated workflow.
#[must_use]
pub fn logout_page() -> Container {
    let sign_out = ui::submit_button("Sign out", ButtonVariant::Danger, ButtonSize::Large);
    let stay = ui::link_button(
        "/",
        "Stay and keep playing",
        ButtonVariant::Secondary,
        ButtonSize::Large,
    );
    auth_page(
        &AuthCard {
            glyph: "↩",
            glyph_colors: GLYPH_SUNNY,
            kicker: "SIGN OUT",
            kicker_color: theme::DANGER_INK,
            title: "Leaving so soon?",
            lead: "Signing out revokes the current durable session. Your games will be waiting.",
        },
        &container! {
            form hx-post="/logout" hx-target="#app-page" gap=(theme::SPACE_3) {
                (sign_out)
            }
            (stay)
            section id="account-result" {}
        },
    )
}

/// Renders signed-out navigation and account entry points without exposing state.
#[must_use]
pub fn signed_out_page() -> Container {
    signed_out_page_for(
        "HOLD UP",
        "A valid secure session is required to view games. Sign in to pick up where you left off.",
        None,
    )
}

/// Renders the signed-out page with context-specific copy, for example after opening an invite
/// link or tapping Rematch without a session. `hint` adds a friendly next-step note.
#[must_use]
pub fn signed_out_page_for(kicker: &str, lead: &str, hint: Option<&str>) -> Container {
    let sign_in = ui::link_button(
        "/login",
        "Sign in",
        ButtonVariant::Primary,
        ButtonSize::Large,
    );
    let hint = hint.map(|hint| ui::notice(hint, Tone::Accent));
    auth_page(
        &AuthCard {
            glyph: "?",
            glyph_colors: GLYPH_GRAPE,
            kicker,
            kicker_color: theme::ACCENT,
            title: "Sign in required",
            lead,
        },
        &container! {
            (sign_in)
            @if let Some(hint) = hint {
                (hint)
            }
        },
    )
}

/// Capitalizes the first letter so lowercase domain error strings read as sentences.
fn sentence_case(message: &str) -> String {
    let mut characters = message.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(characters).collect()
    })
}

pub fn product_error_page(title: &str, message: &str) -> Container {
    let message = sentence_case(message);
    let error = crate::error_component(&message);
    let back = ui::link_button(
        "/",
        "← Back to games",
        ButtonVariant::Primary,
        ButtonSize::Large,
    );
    auth_page(
        &AuthCard {
            glyph: "!",
            glyph_colors: GLYPH_CORAL,
            kicker: "WHOOPS",
            kicker_color: theme::DANGER_INK,
            title,
            lead: "Something went sideways, but your games are safe.",
        },
        &container! {
            (error)
            (back)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_pages_expose_renderer_neutral_forms() {
        let login = login_page(None)
            .display_to_string(false, false)
            .expect("login page renders");
        assert!(login.contains("href=\"/auth/google/start\""));
        assert!(login.contains("target=\"_top\""));
        assert!(login.contains("Continue with Google"));
        assert!(login.contains("href=\"/account/migrate\""));
        assert!(!login.contains("script"));

        let logout = logout_page()
            .display_to_string(false, false)
            .expect("logout page renders");
        assert!(logout.contains("hx-post=\"/logout\""));
        assert!(!logout.contains("script"));
        let migration = migration_page(None)
            .display_to_string(false, false)
            .expect("migration page renders");
        assert!(migration.contains("method=\"post\""));
        assert!(!migration.contains("action="));
        assert!(!migration.contains("hx-post"));
        assert!(!migration.contains("script"));
    }

    #[test]
    fn account_pages_do_not_serialize_authentication_secrets() {
        let secrets = [
            "secret-subject",
            "https://lh3.googleusercontent.com/secret-picture",
            "secret-state",
            "secret-nonce",
            "secret-pkce-verifier",
            "secret-code",
            "secret-token",
            "secret-password",
            "secret-session",
            "secret-invitation",
        ];
        for page in [
            login_page(Some("Google sign-in could not be verified.")),
            migration_page(Some("Existing credentials are incorrect.")),
            signed_out_page(),
            logout_page(),
        ] {
            let rendered = page
                .display_to_string(false, false)
                .expect("account page renders");
            for secret in secrets {
                assert!(!rendered.contains(secret));
            }
        }
    }

    #[test]
    fn account_pages_share_the_branded_card_layout() {
        let game_id: wwmtf_game_domain::GameId = "00000000-0000-0000-0000-000000000001"
            .parse()
            .expect("game id parses");
        for page in [
            login_page_with_invitation(Some("Enter a username."), "", true, true),
            migration_page(Some("Existing credentials are incorrect.")),
            logout_page(),
            signed_out_page(),
            invitation_page("token", true),
            invitation_page("token", false),
            invitation_joined_page(game_id),
            product_error_page("Lobby unavailable", "The lobby is full."),
        ] {
            let rendered = page
                .display_to_string(false, false)
                .expect("account page renders");
            assert!(rendered.contains("id=\"app-page\""), "{rendered}");
            assert!(rendered.contains("brand-logo"), "{rendered}");
            assert!(rendered.contains("<main"), "{rendered}");
            assert!(!rendered.contains("<style"));
            assert!(!rendered.contains("<script"));
        }
    }

    #[test]
    fn login_page_offers_chunky_entry_points_and_keeps_form_fields() {
        let login =
            login_page_with_invitation(Some("Enter a username."), "invite-token", true, true)
                .display_to_string(false, false)
                .expect("login renders");
        assert!(login.contains("id=\"account-result\""));
        assert!(login.contains("Enter a username."));
        assert!(login.contains("name=\"username\""));
        assert!(login.contains("name=\"invitation_token\""));
        assert!(login.contains("value=\"invite-token\""));
        assert!(login.contains("href=\"/auth/google/start?invite=invite-token\""));
        assert!(login.contains("<button"));

        let migration = migration_page(None)
            .display_to_string(false, false)
            .expect("migration renders");
        assert!(migration.contains("name=\"username\""));
        assert!(migration.contains("name=\"password\""));
        assert!(migration.contains("type=\"password\""));
        assert!(migration.contains("Continue with Google"));
    }

    #[test]
    fn error_and_invitation_pages_offer_a_clear_next_step() {
        let error = product_error_page("Lobby unavailable", "The lobby is full.")
            .display_to_string(false, false)
            .expect("error renders");
        assert!(error.contains("Lobby unavailable"));
        assert!(error.contains("The lobby is full."));
        assert!(error.contains("id=\"game-error\""));
        assert!(error.contains("Back to games"));
        assert!(error.contains("href=\"/\""));

        let invitation = invitation_page("invite-token", true)
            .display_to_string(false, false)
            .expect("invitation renders");
        assert!(invitation.contains("hx-post=\"/join\""));
        assert!(invitation.contains("value=\"REDEEM_INVITATION\""));
        assert!(invitation.contains("name=\"invitation_token\""));
        assert!(invitation.contains("Accept invitation"));

        let signed_out = signed_out_page()
            .display_to_string(false, false)
            .expect("signed-out renders");
        assert!(signed_out.contains("Sign in required"));
        assert!(signed_out.contains("href=\"/login\""));
    }
}
