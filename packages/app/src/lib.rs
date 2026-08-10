#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Renderer-neutral Words with More Than Friends routes and page components.

mod accounts;
mod challenges;
mod components;
mod definitions;
mod external_identities;
mod game_service;
mod google_accounts;
mod invitations;
mod journal;
mod migrations;
#[cfg(feature = "metrics")]
mod observability;
mod oidc;
mod oidc_attempts;
mod presentation;
mod profiles;
mod projections;
mod rack_preferences;
mod routes;
mod sessions;
mod shared_state_security;
mod workflows;

pub use accounts::{
    AccountError, authenticate, hash_password, normalize_username, register, verify_password,
};
pub use challenges::{
    ChallengeError, ChallengeStatus, accept_challenge, cancel_challenge, create_challenge,
    create_game_in_transaction, decline_challenge, find_user_by_username,
};
pub use components::{
    GameView, MoveHistoryError, MoveHistoryView, PendingMoveError, PendingMoveView, PlayedWordView,
    PremiumView, board_component, error_component, final_score_adjustments, game_view,
    move_history_component, move_history_view, pending_move_component, premium_square_component,
    rack_component, status_component, tile_component, viewer_turn_component,
};
pub use definitions::{
    DEFAULT_DEFINITION_PROVIDER_BASE_URL, DefinitionError, DefinitionLookup, DefinitionMeaning,
    DefinitionProvider, DefinitionUnavailableReason, FreeDictionaryProvider, WordDefinition,
    lookup_definition,
};
pub use external_identities::{
    ExternalIdentityError, ResolvedExternalAccount, VerifiedExternalIdentity,
    link_external_identity, resolve_or_create_external_account, user_for_external_identity,
};
pub use game_service::{GameServiceError, player_for_user, submit_game_command};
pub use google_accounts::{
    GoogleAccountWorkflowError, complete_legacy_google_migration, google_login_and_create_session,
    prove_legacy_password_account,
};
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
#[cfg(feature = "metrics")]
pub use observability::{AppMetricsSnapshot, app_metrics_snapshot};
pub use oidc::{GOOGLE_ISSUER, GoogleOidcClient, GoogleOidcError};
pub use oidc_attempts::{
    ClaimedOidcAttempt, NewOidcAttempt, OidcAttemptError, OidcAttemptPurpose, claim_oidc_attempt,
    cleanup_oidc_attempts, consume_oidc_attempt, create_oidc_attempt,
};
pub use presentation::{
    AuthenticatedDashboard, AuthorizedGamePage, CSRF_COOKIE_NAME, CSRF_HEADER_NAME,
    OIDC_BINDING_COOKIE_NAME, PresentationError, SESSION_COOKIE_NAME, authenticated_user,
    load_authenticated_dashboard, load_authorized_game_page,
};
pub use profiles::{
    AvatarSource, ProfileError, ProfileFieldSource, ProfileImage, UserProfile,
    can_view_profile_avatar, create_google_profile, download_google_avatar, generate_unique_handle,
    load_profile, load_profile_image, normalize_avatar, normalize_display_name, profile_image_hash,
    remove_custom_avatar, set_custom_avatar, set_custom_display_name, set_google_avatar,
    synchronize_google_profile, use_google_avatar, use_google_display_name,
};
pub use projections::{
    DashboardProjection, GameHistoryEntry, GameSummary, PendingItem, ProjectionError,
    UserScoreTotals, dashboard_projection, game_history, projected_revision,
    rebuild_all_user_score_totals, rebuild_game_projections, user_game_summaries,
    user_score_totals,
};
pub use rack_preferences::{
    RackPreferenceError, load_rack_order, reconcile_rack_order, save_rack_order,
    shuffle_rack_order, swap_rack_tiles,
};
pub use routes::{
    authenticated_session_response, create_product_router, dashboard_page, dashboard_route,
    game_page, game_route, game_view_response, logged_out_response, login_page, logout_page,
    migration_page, register_page, signed_out_page, turn_composer,
};
pub use sessions::{
    SessionError, SessionToken, create_session, resolve_session, revoke_session,
    revoke_user_sessions,
};
pub use shared_state_security::{
    DashboardLiveView, GameSharedStateDispatcher, dashboard_channel, game_channel,
    shared_state_dispatcher,
};
pub use workflows::{
    AccountWorkflowError, ProductWorkflowError, accept_pending_challenge, cancel_pending_challenge,
    challenge_username, create_shareable_invitation, decline_pending_challenge,
    login_and_create_session, logout_session, redeem_shareable_invitation,
    register_and_create_session, revoke_shareable_invitation,
};
