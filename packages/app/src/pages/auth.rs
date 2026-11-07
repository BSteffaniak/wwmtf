//! Account, invitation, sign-in, and generic error pages.

use hyperchad::{
    router::Container,
    template::{LayoutOverflow, container},
};

use crate::{error_component, theme};

pub fn invitation_joined_page(game_id: wwmtf_game_domain::GameId) -> Container {
    let game_href = format!("/games/{game_id}");
    container! {
        div id="app-page" font-family=(theme::FONT_UI) direction="column" align-items="center" min-height="100vh" background=(theme::MENU_BG) padding-y=48 padding-x=24 {
            main width="100%" max-width="560px"
                background=(theme::WHITE) border=((theme::BORDER, 1)) border-radius="18px" padding="32px" gap="16px" {
                span color=(theme::PRIMARY_INK) font-weight=bold { "INVITATION ACCEPTED" }
                h1 { "Your game is ready" }
                span color=(theme::INK_MUTED) { "The invitation was redeemed and the game was created." }
                anchor href=(game_href) color=(theme::ON_PRIMARY) background=(theme::PRIMARY) border=((theme::PRIMARY_EDGE, 1))
                    border-radius="10px" padding-y=13 padding-x=18 { "Open game" }
            }
        }
    }
    .into()
}

pub fn invitation_page(invitation_token: &str, signed_in: bool) -> Container {
    let login_href = format!("/login?invite={invitation_token}");
    container! {
        div id="app-page" font-family=(theme::FONT_UI) direction="column" align-items="center" min-height="100vh" background=(theme::MENU_BG) padding-y=48 padding-x=24 {
            main width="100%" max-width="560px"
                background=(theme::WHITE) border=((theme::BORDER, 1)) border-radius="18px" padding="32px" gap="18px" {
                span color=(theme::ACCENT) font-weight=bold { "PRIVATE GAME INVITATION" }
                h1 { "You’ve been invited to play" }
                span color=(theme::INK_MUTED) { "This invitation creates a private two-player game. It can be used once." }
                @if signed_in {
                    form hx-post="/join" hx-target="#app-page" gap="10px" {
                        input type=hidden name="action" value="REDEEM_INVITATION";
                        input type=hidden name="invitation_token" value=(invitation_token);
                        button type=submit padding-y=13 padding-x=18 background=(theme::PRIMARY) color=(theme::ON_PRIMARY)
                            border=((theme::PRIMARY_EDGE, 1)) border-radius="10px" cursor=pointer { "Accept invitation" }
                    }
                    anchor href="/" color=(theme::LINK) { "Back to dashboard" }
                } @else {
                    span { "Continue with Google to accept this invitation." }
                    div direction="row" gap="10px" {
                        anchor href=(login_href) color=(theme::ON_PRIMARY) background=(theme::PRIMARY) border=((theme::PRIMARY_EDGE, 1))
                            border-radius="10px" padding-y=13 padding-x=18 { "Continue with Google" }
                    }
                }
            }
        }
    }
    .into()
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
    container! {
        div id="app-page" font-family=(theme::FONT_UI) direction="column" align-items="center" min-height="100vh" background=(theme::MENU_BG) padding-y=48 padding-x=24 {
            main width="100%" max-width="480px"
                background=(theme::WHITE) border=((theme::BORDER, 1)) border-radius="18px" padding="32px" gap="20px" {
                anchor href="/" color=(theme::LINK) { "← Home" }
                div gap="6px" {
                    span color=(theme::ACCENT) font-weight=bold { "WORDS WITH MORE THAN FRIENDS" }
                    h1 { "Welcome back" }
                    span color=(theme::INK_MUTED) { "Sign in to continue your private games." }
                }
                @if development_login {
                    section background=(theme::PRIMARY_SOFT) border=((theme::PRIMARY, 1)) border-radius="10px" padding="14px" gap="12px" {
                        span color=(theme::PRIMARY_INK) font-weight=bold { "Local development login" }
                        span color=(theme::INK_MUTED) { "Enter any username. It will be created automatically if needed." }
                        form method="post" gap="12px" {
                            input type=hidden name="invitation_token" value=(invitation_token);
                            input type=text name="username" placeholder="Username" autofocus=true padding-y=13 padding-x=14
                                border=((theme::INPUT_BORDER, 1)) border-radius="10px";
                            button type=submit padding-y=13 padding-x=18 background=(theme::PRIMARY) color=(theme::ON_PRIMARY)
                                border=((theme::PRIMARY_EDGE, 1)) border-radius="10px" cursor=pointer { "Continue" }
                        }
                    }
                }
                @if google_login {
                    anchor href=(google_href) target="_top" color=(theme::ON_PRIMARY) background=(theme::PRIMARY) border=((theme::PRIMARY_EDGE, 1))
                        border-radius="10px" padding-y=13 padding-x=18 text-align="center" { "Continue with Google" }
                    span color=(theme::INK_MUTED) { "Already have a username/password account? "
                        anchor href="/account/migrate" color=(theme::LINK) { "Migrate it to Google" }
                    }
                }
                @if !development_login && !google_login {
                    span color=(theme::INK_MUTED) { "No login method is configured for this server." }
                }
                @if !message.is_empty() {
                    section id="account-result" background=(theme::DANGER_SOFT) border=((theme::DANGER, 1))
                        border-radius="10px" padding="12px" { span color=(theme::DANGER_INK) { (message) } }
                }
            }
        }
    }
    .into()
}

/// Renders the explicit legacy-account migration form.
#[must_use]
pub fn migration_page(error: Option<&str>) -> Container {
    let message = error.unwrap_or_default();
    container! {
        div id="app-page" font-family=(theme::FONT_UI) direction="column" align-items="center" min-height="100vh" background=(theme::MENU_BG) padding-y=48 padding-x=24 {
            main width="100%" max-width="480px" background=(theme::WHITE) border=((theme::BORDER, 1)) border-radius="18px" padding="32px" gap="20px" {
                anchor href="/login" color=(theme::LINK) { "← Back to sign in" }
                h1 { "Migrate your existing account" }
                span color=(theme::INK_MUTED) { "Confirm your existing credentials, then connect Google without losing your games." }
                form method="post" gap="12px" {
                    span font-weight=bold { "Username" }
                    input type=text name="username" placeholder="Username" padding-y=13 padding-x=14 border=((theme::INPUT_BORDER, 1)) border-radius="10px";
                    span font-weight=bold { "Password" }
                    input type=password name="password" placeholder="Password" padding-y=13 padding-x=14 border=((theme::INPUT_BORDER, 1)) border-radius="10px";
                    button type=submit padding-y=13 padding-x=18 background=(theme::PRIMARY) color=(theme::ON_PRIMARY) border=((theme::PRIMARY_EDGE, 1)) border-radius="10px" cursor=pointer { "Continue with Google" }
                }
                @if !message.is_empty() {
                    section background=(theme::DANGER_SOFT) border=((theme::DANGER, 1)) border-radius="10px" padding="12px" { (message) }
                }
            }
        }
    }
    .into()
}

/// Renders logout confirmation. Session revocation is performed by the authenticated workflow.
#[must_use]
pub fn logout_page() -> Container {
    container! {
        div id="app-page" font-family=(theme::FONT_UI) direction="column" align-items="center" min-height="100vh"
            background=(theme::MENU_BG) padding=24 {
            main width="100%" max-width="480px" background=(theme::WHITE) border=((theme::BORDER, 1))
                border-radius=18 padding=32 gap=16 {
                h1 { "Sign out" }
                span color=(theme::INK_MUTED) { "Signing out revokes the current durable session." }
                form hx-post="/logout" hx-target="#app-page" {
                    button type=submit padding-y=12 padding-x=16 background=(theme::PRIMARY) color=(theme::ON_PRIMARY)
                        border=((theme::PRIMARY_EDGE, 1)) border-radius=10 cursor="pointer" { "Sign out" }
                }
                section id="account-result" {}
            }
        }
    }
    .into()
}

/// Renders signed-out navigation and account entry points without exposing state.
#[must_use]
pub fn signed_out_page() -> Container {
    container! {
        div id="app-page" font-family=(theme::FONT_UI) direction="column" align-items="center" min-height="100vh"
            background=(theme::MENU_BG) padding=24 {
            main width="100%" max-width="560px" background=(theme::WHITE) border=((theme::BORDER, 1))
                border-radius=18 padding=32 gap=18 {
                span color=(theme::ACCENT) font-weight=bold { "WORDS WITH MORE THAN FRIENDS" }
                h1 { "Sign in required" }
                span color=(theme::INK_MUTED) { "A valid secure session is required to view games." }
                div direction="row" overflow-x=(LayoutOverflow::Wrap { grid: false }) gap=10 {
                    anchor href="/login" color=(theme::ON_PRIMARY) background=(theme::PRIMARY) border=((theme::PRIMARY_EDGE, 1))
                        border-radius=10 padding-y=12 padding-x=16 { "Continue with Google" }
                }
            }
        }
    }
    .into()
}

pub fn product_error_page(title: &str, message: &str) -> Container {
    let error = error_component(message);
    container! {
        div id="app-page" font-family=(theme::FONT_UI) direction="column" align-items="center" min-height="100vh"
            background=(theme::MENU_BG) padding=24 {
            main width="100%" max-width="560px" background=(theme::WHITE) border=((theme::BORDER, 1))
                border-radius=18 padding=32 gap=16 {
                anchor href="/" color=(theme::LINK) { "← Dashboard" }
                h1 { (title) }
                (error)
            }
        }
    }
    .into()
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
}
