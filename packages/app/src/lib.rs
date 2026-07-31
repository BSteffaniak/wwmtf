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
mod presentation;
mod projections;
mod routes;
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
    final_score_adjustments, game_view, move_history_component, move_history_view,
    pending_move_component, premium_square_component, rack_component, status_component,
    tile_component,
};
pub use game_service::{GameServiceError, player_for_user, submit_game_command};
pub use invitations::{
    InvitationError, InvitationToken, create_invitation, redeem_invitation,
    redeem_invitation_and_start_game, revoke_invitation,
};
pub use journal::{
    JournalError, PersistedGameEvent, PersistedPayloadCompatibility, append_events,
    append_events_transactionally, load_events, load_latest_snapshot,
    persisted_payload_compatibility, recover_game, store_snapshot,
};
pub use migrations::{app_migrations, migrate_app};
pub use presentation::{
    AuthenticatedDashboard, AuthorizedGamePage, CSRF_COOKIE_NAME, CSRF_HEADER_NAME,
    PresentationError, SESSION_COOKIE_NAME, authenticated_user, load_authenticated_dashboard,
    load_authorized_game_page,
};
pub use projections::{
    DashboardProjection, GameHistoryEntry, GameSummary, PendingItem, ProjectionError,
    UserScoreTotals, dashboard_projection, game_history, projected_revision,
    rebuild_all_user_score_totals, rebuild_game_projections, user_game_summaries,
    user_score_totals,
};
pub use routes::{
    create_product_router, dashboard_page, dashboard_route, game_page, game_route,
    game_view_response, signed_out_page,
};
pub use sessions::{SessionError, SessionToken, create_session, resolve_session, revoke_session};
pub use shared_state_security::{game_channel, shared_state_dispatcher};
