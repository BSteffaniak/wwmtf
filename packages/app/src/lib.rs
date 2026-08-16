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
mod move_plans;
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
    AccountError, authenticate, find_or_create_development_user, hash_password, normalize_username,
    register, verify_password,
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
    InvitationError, InvitationToken, active_invitation_id, create_invitation, redeem_invitation,
    redeem_invitation_and_start_game, redeem_invitation_and_start_game_by_id, revoke_invitation,
};
pub use journal::{
    JournalError, PersistedGameEvent, PersistedPayloadCompatibility, append_events,
    append_events_transactionally, load_events, load_latest_snapshot,
    persisted_payload_compatibility, recover_game, store_snapshot,
};
pub use migrations::{app_migrations, migrate_app};
pub use move_plans::{MovePlanError, clear_move_plan, load_move_plan, save_move_plan};
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
    can_view_profile_avatar, create_google_profile, download_google_avatar,
    download_provider_avatar, generate_unique_handle, load_profile, load_profile_image,
    normalize_avatar, normalize_display_name, profile_image_hash, remove_custom_avatar,
    set_custom_avatar, set_custom_display_name, set_google_avatar, synchronize_google_profile,
    use_google_avatar, use_google_display_name,
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
    migration_page, signed_out_page, turn_composer,
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
    challenge_username, create_shareable_invitation, decline_pending_challenge, logout_session,
    redeem_shareable_invitation, redeem_shareable_invitation_by_id, revoke_shareable_invitation,
};

#[cfg(test)]
mod recovery_tests {
    use std::{io::Cursor, path::Path, sync::Arc};

    use futures_lite::future::block_on;
    use image::{ImageFormat, Rgba, RgbaImage};
    use switchy_database::Database;
    use time::{Duration, OffsetDateTime};

    use crate::{
        OidcAttemptPurpose, VerifiedExternalIdentity, claim_oidc_attempt, create_oidc_attempt,
        google_login_and_create_session, load_profile, load_profile_image, migrate_app,
        profile_image_hash, resolve_session, set_google_avatar, user_for_external_identity,
    };

    async fn open_database(path: &Path) -> Arc<Box<dyn Database>> {
        Arc::new(
            switchy_database_connection::builder()
                .turso()
                .with_path(path)
                .build()
                .await
                .expect("file-backed Turso opens"),
        )
    }

    #[test]
    fn restored_database_retains_google_identity_profile_avatar_and_session() {
        block_on(async {
            let directory = std::env::temp_dir()
                .join(format!("wwmtf-google-recovery-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&directory).expect("recovery directory creates");
            let source_path = directory.join("source.db");
            let restored_path = directory.join("restored.db");
            let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);

            let db = open_database(&source_path).await;
            migrate_app(&**db).await.expect("schema migrates");
            let identity = VerifiedExternalIdentity::google(
                "https://accounts.google.com",
                "recovery-subject",
                "Recovery Player",
                Some("https://lh3.googleusercontent.com/recovery".to_string()),
            )
            .expect("identity validates");
            let (user_id, session) =
                google_login_and_create_session(&**db, &identity, now, Duration::days(30))
                    .await
                    .expect("Google session creates");
            let mut source = RgbaImage::new(2, 2);
            for pixel in source.pixels_mut() {
                *pixel = Rgba([20, 40, 60, 255]);
            }
            let mut avatar = Vec::new();
            image::DynamicImage::ImageRgba8(source)
                .write_to(&mut Cursor::new(&mut avatar), ImageFormat::Png)
                .expect("avatar fixture encodes");
            set_google_avatar(&**db, &user_id, &avatar, now)
                .await
                .expect("avatar stores");
            let image_hash = profile_image_hash(&**db, &user_id)
                .await
                .expect("image hash loads")
                .expect("image exists");
            let attempt = create_oidc_attempt(
                &**db,
                OidcAttemptPurpose::Login,
                None,
                None,
                now,
                Duration::minutes(1),
            )
            .await
            .expect("attempt creates");
            db.close().await.expect("source database closes");
            drop(db);
            std::fs::copy(&source_path, &restored_path).expect("database backup restores");

            let restored = open_database(&restored_path).await;
            assert_eq!(
                user_for_external_identity(
                    &**restored,
                    "https://accounts.google.com",
                    "recovery-subject"
                )
                .await
                .expect("identity loads"),
                Some(user_id.clone())
            );
            assert_eq!(
                load_profile(&**restored, &user_id)
                    .await
                    .expect("profile loads")
                    .expect("profile exists")
                    .display_name,
                "Recovery Player"
            );
            assert!(
                load_profile_image(&**restored, &user_id, &image_hash)
                    .await
                    .expect("image loads")
                    .is_some()
            );
            assert_eq!(
                resolve_session(&**restored, session.expose(), now)
                    .await
                    .expect("session survives"),
                user_id
            );
            assert!(
                claim_oidc_attempt(
                    &**restored,
                    &attempt.state,
                    &attempt.browser_binding,
                    now + Duration::minutes(2)
                )
                .await
                .is_err()
            );
            drop(restored);
            std::fs::remove_dir_all(directory).expect("recovery directory removes");
        });
    }
}
