#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Renderer-neutral Words with Spouses routes and page components.

mod accounts;
mod challenges;
mod components;
mod game_service;
mod invitations;
mod journal;
mod migrations;
mod projections;
mod sessions;
mod shared_state_security;

pub use accounts::{
    AccountError, authenticate, hash_password, normalize_username, register, verify_password,
};
pub use challenges::{
    ChallengeError, ChallengeStatus, accept_challenge, cancel_challenge, create_challenge,
    create_game_in_transaction, decline_challenge, find_user_by_username,
};
pub use components::{
    GameView, MoveHistoryView, PendingMoveView, PremiumView, board_component, error_component,
    game_view, move_history_component, pending_move_component, premium_square_component,
    rack_component, status_component, tile_component,
};
pub use game_service::{GameServiceError, player_for_user, submit_game_command};
pub use invitations::{
    InvitationError, InvitationToken, create_invitation, redeem_invitation,
    redeem_invitation_and_start_game, revoke_invitation,
};
pub use journal::{
    JournalError, PersistedGameEvent, append_events, append_events_transactionally, load_events,
    load_latest_snapshot, recover_game, store_snapshot,
};
pub use migrations::{app_migrations, migrate_app};
pub use projections::{
    DashboardProjection, GameSummary, PendingItem, ProjectionError, dashboard_projection,
    projected_revision, rebuild_game_projections, user_game_summaries,
};
pub use sessions::{SessionError, SessionToken, create_session, resolve_session, revoke_session};
pub use shared_state_security::{game_channel, shared_state_dispatcher};

use hyperchad::{
    router::{Container, RoutePath, RouteRequest, Router},
    template::container,
};

/// Builds the application's renderer-neutral router.
#[must_use]
pub fn create_router() -> Router {
    let router = Router::new();
    router.add_route_result("/", |_request: RouteRequest| async move {
        Ok(home_page()) as Result<Container, Box<dyn std::error::Error>>
    });
    router.add_route_result(
        RoutePath::LiteralPrefix("/games/".to_string()),
        |request: RouteRequest| async move {
            Ok(game_page(&request.path)) as Result<Container, Box<dyn std::error::Error>>
        },
    );
    router
}

/// Renders the initial product shell.
#[must_use]
pub fn home_page() -> Container {
    container! {
        div padding=32 gap=24 {
            header gap=8 {
                h1 { "Words with Spouses" }
                span { "Private asynchronous word-tile games" }
            }
            main gap=16 {
                h2 { "Welcome" }
                span {
                    "Sign-in, challenges, active games, and history will live in this renderer-neutral HyperChad application."
                }
                anchor href="/games/example" { "Open an example stable game route" }
            }
        }
    }
    .into()
}

/// Renders the non-authoritative shell for a stable game route.
#[must_use]
pub fn game_page(path: &str) -> Container {
    let game_id = path.strip_prefix("/games/").unwrap_or_default();
    container! {
        div padding=32 gap=24 {
            header gap=8 {
                anchor href="/" { "Dashboard" }
                h1 { "Game" }
                span { (game_id) }
            }
            main gap=16 {
                span {
                    "Authoritative board and private rack projections will render here after authentication and persistence are connected."
                }
            }
        }
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_exposes_dashboard_and_stable_game_routes() {
        let router = create_router();
        assert!(router.has_route("/"));
        assert!(router.has_route("/games/abc-123"));
    }

    #[test]
    fn game_page_renders_the_route_identity() {
        let rendered = game_page("/games/abc-123")
            .display_to_string(false, false)
            .expect("page renders");
        assert!(rendered.contains("abc-123"));
    }
}
