//! Google authentication and legacy password-migration account workflows.

use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::{
    SessionToken, VerifiedExternalIdentity, authenticate, create_session, link_external_identity,
    resolve_or_create_external_account, revoke_user_sessions,
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
    link_external_identity(db, existing_user_id, identity, now).await?;
    let deleted = db
        .delete("password_credentials")
        .where_eq("user_id", existing_user_id)
        .execute(db)
        .await?;
    if deleted.len() != 1 {
        return Err(GoogleAccountWorkflowError::MissingPasswordCredential);
    }
    revoke_user_sessions(db, existing_user_id, now).await?;
    Ok(create_session(db, existing_user_id, now, lifetime).await?)
}

/// Google account workflow failure.
#[derive(Debug, Error)]
pub enum GoogleAccountWorkflowError {
    #[error("legacy password credential is unavailable")]
    MissingPasswordCredential,
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
