//! Google authentication and legacy password-migration account workflows.

use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::external_identities::link_external_identity_in_database;
use crate::{
    SessionToken, VerifiedExternalIdentity, authenticate, create_session,
    resolve_or_create_external_account,
};

/// Resolves a verified Google identity and issues the ordinary WWMTF session.
///
/// # Errors
///
/// * Returns identity, profile, timestamp, or session persistence failures.
pub async fn google_login_and_create_session(
    db: &dyn Database,
    identity: &VerifiedExternalIdentity,
    now: OffsetDateTime,
    lifetime: Duration,
) -> Result<(String, SessionToken), GoogleAccountWorkflowError> {
    let account = resolve_or_create_external_account(db, identity, now).await?;
    let session = create_session(db, &account.user_id, now, lifetime).await?;
    Ok((account.user_id, session))
}

/// Proves legacy credentials without creating a password-authenticated session.
///
/// # Errors
///
/// * Returns uniform account credential failures.
pub async fn prove_legacy_password_account(
    db: &dyn Database,
    username: &str,
    password: &str,
) -> Result<String, GoogleAccountWorkflowError> {
    Ok(authenticate(db, username, password).await?)
}

/// Links fresh Google authentication to a password-proven account and completes migration.
///
/// Identity linking/profile initialization happen before deleting the password credential. If the
/// credential transition fails, the durable Google link remains a valid future authentication
/// method rather than stranding the account. Existing sessions are revoked before issuing one new
/// session.
///
/// # Errors
///
/// * Returns identity conflict, profile, session, timestamp, or database failures.
pub async fn complete_legacy_google_migration(
    db: &dyn Database,
    existing_user_id: &str,
    identity: &VerifiedExternalIdentity,
    now: OffsetDateTime,
    lifetime: Duration,
) -> Result<SessionToken, GoogleAccountWorkflowError> {
    let tx = db.begin_transaction().await?;
    let transition = async {
        link_external_identity_in_database(&*tx, existing_user_id, identity, now).await?;
        let deleted = tx
            .delete("password_credentials")
            .where_eq("user_id", existing_user_id)
            .execute(&*tx)
            .await?;
        if deleted.len() != 1 {
            return Err(GoogleAccountWorkflowError::MissingPasswordCredential);
        }
        tx.update("auth_sessions")
            .value("revoked_at_ms", timestamp_ms(now)?)
            .where_eq("user_id", existing_user_id)
            .execute(&*tx)
            .await?;
        let session = create_session(&*tx, existing_user_id, now, lifetime).await?;
        Ok(session)
    }
    .await;
    match transition {
        Ok(session) => {
            tx.commit().await?;
            Ok(session)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}

fn timestamp_ms(timestamp: OffsetDateTime) -> Result<i64, GoogleAccountWorkflowError> {
    i64::try_from(timestamp.unix_timestamp_nanos() / 1_000_000)
        .map_err(|_| GoogleAccountWorkflowError::Timestamp)
}

/// Google account workflow failure.
#[derive(Debug, Error)]
pub enum GoogleAccountWorkflowError {
    #[error("legacy password credential is unavailable")]
    MissingPasswordCredential,
    #[error("legacy migration timestamp is outside the supported range")]
    Timestamp,
    #[error(transparent)]
    Account(#[from] crate::AccountError),
    #[error(transparent)]
    Identity(#[from] crate::ExternalIdentityError),
    #[error(transparent)]
    Session(#[from] crate::SessionError),
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;

    use super::*;
    use crate::{migrate_app, register, resolve_session};

    #[test]
    fn google_login_uses_existing_application_sessions() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .unwrap();
            migrate_app(&*db).await.unwrap();
            let now = OffsetDateTime::UNIX_EPOCH;
            let identity = VerifiedExternalIdentity::google(
                "https://accounts.google.com",
                "new-google-user",
                "Grace Hopper",
                None,
            )
            .unwrap();
            let (user, session) =
                google_login_and_create_session(&*db, &identity, now, Duration::days(30))
                    .await
                    .unwrap();
            assert_eq!(
                resolve_session(&*db, session.expose(), now).await.unwrap(),
                user
            );
        });
    }

    #[test]
    fn migration_identity_conflict_preserves_password_and_existing_sessions() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .unwrap();
            migrate_app(&*db).await.unwrap();
            let now = OffsetDateTime::UNIX_EPOCH;
            let first = register(&*db, "first-user", "correct horse battery staple", now)
                .await
                .unwrap();
            let second = register(&*db, "second-user", "correct horse battery staple", now)
                .await
                .unwrap();
            let session = create_session(&*db, &second, now, Duration::days(30))
                .await
                .unwrap();
            let identity = VerifiedExternalIdentity::google(
                "https://accounts.google.com",
                "shared-google-subject",
                "First User",
                None,
            )
            .unwrap();
            complete_legacy_google_migration(&*db, &first, &identity, now, Duration::days(30))
                .await
                .unwrap();

            let conflict =
                complete_legacy_google_migration(&*db, &second, &identity, now, Duration::days(30))
                    .await;
            assert!(matches!(
                conflict,
                Err(GoogleAccountWorkflowError::Identity(
                    crate::ExternalIdentityError::Conflict
                ))
            ));
            assert_eq!(
                authenticate(&*db, "second-user", "correct horse battery staple")
                    .await
                    .unwrap(),
                second
            );
            assert_eq!(
                resolve_session(&*db, session.expose(), now).await.unwrap(),
                second
            );
        });
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn migration_preserves_games_history_scores_preferences_and_account_identity() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .unwrap();
            migrate_app(&*db).await.unwrap();
            let now = OffsetDateTime::UNIX_EPOCH;
            let user = register(&*db, "legacy-state", "correct horse battery staple", now)
                .await
                .unwrap();
            let opponent = register(&*db, "opponent", "another correct horse battery", now)
                .await
                .unwrap();
            let challenge = crate::create_challenge(&*db, &user, &opponent, now)
                .await
                .unwrap();
            let game_id = crate::accept_challenge(&*db, &challenge, &opponent, now, 19)
                .await
                .unwrap();
            let preferred = crate::load_rack_order(&*db, game_id, &user)
                .await
                .unwrap()
                .into_iter()
                .rev()
                .collect::<Vec<_>>();
            crate::save_rack_order(&*db, game_id, &user, &preferred, 1)
                .await
                .unwrap();
            let summaries_before = crate::user_game_summaries(&*db, &user).await.unwrap();
            let game_ids_before = summaries_before
                .iter()
                .map(|summary| summary.game_id.clone())
                .collect::<Vec<_>>();
            let history_before = crate::game_history(&*db, game_id).await.unwrap();
            let totals_before = crate::user_score_totals(&*db, &user).await.unwrap();
            let old_session = create_session(&*db, &user, now, Duration::days(30))
                .await
                .unwrap();
            let identity = VerifiedExternalIdentity::google(
                "https://accounts.google.com",
                "legacy-state-subject",
                "Legacy State",
                None,
            )
            .unwrap();

            let new_session = complete_legacy_google_migration(
                &*db,
                &user,
                &identity,
                now + Duration::minutes(1),
                Duration::days(30),
            )
            .await
            .unwrap();

            assert!(
                resolve_session(&*db, old_session.expose(), now)
                    .await
                    .is_err()
            );
            assert_eq!(
                resolve_session(&*db, new_session.expose(), now + Duration::minutes(1))
                    .await
                    .unwrap(),
                user
            );
            assert!(
                authenticate(&*db, "legacy-state", "correct horse battery staple")
                    .await
                    .is_err()
            );
            assert_eq!(
                crate::user_for_external_identity(
                    &*db,
                    "https://accounts.google.com",
                    "legacy-state-subject"
                )
                .await
                .unwrap(),
                Some(user.clone())
            );
            let summaries_after = crate::user_game_summaries(&*db, &user).await.unwrap();
            assert_eq!(
                summaries_after
                    .iter()
                    .map(|summary| summary.game_id.clone())
                    .collect::<Vec<_>>(),
                game_ids_before
            );
            assert_eq!(
                summaries_after[0].canonical_revision,
                summaries_before[0].canonical_revision
            );
            assert_eq!(
                crate::game_history(&*db, game_id).await.unwrap(),
                history_before
            );
            assert_eq!(
                crate::user_score_totals(&*db, &user).await.unwrap(),
                totals_before
            );
            assert_eq!(
                crate::load_rack_order(&*db, game_id, &user).await.unwrap(),
                preferred
            );
        });
    }

    #[test]
    fn migration_without_password_rolls_back_identity_and_profile() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .unwrap();
            migrate_app(&*db).await.unwrap();
            let now = OffsetDateTime::UNIX_EPOCH;
            let user = register(&*db, "rollback-user", "correct horse battery staple", now)
                .await
                .unwrap();
            db.delete("password_credentials")
                .where_eq("user_id", user.clone())
                .execute(&*db)
                .await
                .unwrap();
            let identity = VerifiedExternalIdentity::google(
                "https://accounts.google.com",
                "rollback-subject",
                "Rollback User",
                None,
            )
            .unwrap();

            assert!(matches!(
                complete_legacy_google_migration(&*db, &user, &identity, now, Duration::days(30))
                    .await,
                Err(GoogleAccountWorkflowError::MissingPasswordCredential)
            ));
            assert_eq!(
                crate::user_for_external_identity(
                    &*db,
                    "https://accounts.google.com",
                    "rollback-subject"
                )
                .await
                .unwrap(),
                None
            );
            assert!(crate::load_profile(&*db, &user).await.unwrap().is_none());
        });
    }

    #[test]
    fn migration_preserves_user_removes_password_and_revokes_old_sessions() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .unwrap();
            migrate_app(&*db).await.unwrap();
            let now = OffsetDateTime::UNIX_EPOCH;
            let user = register(&*db, "grace", "correct horse battery staple", now)
                .await
                .unwrap();
            let old_session = create_session(&*db, &user, now, Duration::days(30))
                .await
                .unwrap();
            assert_eq!(
                prove_legacy_password_account(&*db, "grace", "correct horse battery staple")
                    .await
                    .unwrap(),
                user
            );
            let identity = VerifiedExternalIdentity::google(
                "https://accounts.google.com",
                "grace-google",
                "Grace Hopper",
                None,
            )
            .unwrap();
            let new_session =
                complete_legacy_google_migration(&*db, &user, &identity, now, Duration::days(30))
                    .await
                    .unwrap();
            assert!(
                resolve_session(&*db, old_session.expose(), now)
                    .await
                    .is_err()
            );
            assert_eq!(
                resolve_session(&*db, new_session.expose(), now)
                    .await
                    .unwrap(),
                user
            );
            assert!(
                authenticate(&*db, "grace", "correct horse battery staple")
                    .await
                    .is_err()
            );
        });
    }
}
