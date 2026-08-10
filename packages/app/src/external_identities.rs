//! External identity resolution and transactional Google account creation.

use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

/// Verified identity and profile claims produced by an OIDC provider boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedExternalIdentity {
    pub provider: String,
    pub issuer: String,
    pub subject: String,
    pub display_name: String,
    pub picture_url: Option<String>,
}

impl VerifiedExternalIdentity {
    /// Creates verified Google identity input after protocol validation.
    ///
    /// # Errors
    ///
    /// * Returns [`ExternalIdentityError::Invalid`] for empty identity keys or a non-Google
    ///   provider name.
    pub fn google(
        issuer: impl Into<String>,
        subject: impl Into<String>,
        display_name: impl Into<String>,
        picture_url: Option<String>,
    ) -> Result<Self, ExternalIdentityError> {
        let issuer = issuer.into();
        let subject = subject.into();
        let display_name = display_name.into();
        if issuer.trim().is_empty()
            || subject.trim().is_empty()
            || issuer.chars().any(char::is_control)
            || subject.chars().any(char::is_control)
        {
            return Err(ExternalIdentityError::Invalid);
        }
        Ok(Self {
            provider: "GOOGLE".to_string(),
            issuer,
            subject,
            display_name,
            picture_url,
        })
    }
}

/// Result of resolving a verified external identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedExternalAccount {
    pub user_id: String,
    pub created: bool,
}

/// Resolves an external identity, creating its WWMTF user/profile transactionally when absent.
///
/// Existing identities retain their stable user and challenge handle while Google-owned profile
/// fields synchronize. New identities receive an automatically generated unique handle.
///
/// # Errors
///
/// * Returns validation, profile, timestamp, or persistence failures.
pub async fn resolve_or_create_external_account(
    db: &dyn Database,
    identity: &VerifiedExternalIdentity,
    now: OffsetDateTime,
) -> Result<ResolvedExternalAccount, ExternalIdentityError> {
    validate_identity(identity)?;
    if let Some(user_id) =
        user_for_external_identity(db, &identity.issuer, &identity.subject).await?
    {
        crate::synchronize_google_profile(
            db,
            &user_id,
            &identity.display_name,
            identity.picture_url.as_deref(),
            now,
        )
        .await?;
        db.update("external_identities")
            .value("provider_display_name", identity.display_name.clone())
            .value("provider_picture_url", identity.picture_url.as_deref())
            .value("last_authenticated_at_ms", timestamp_ms(now)?)
            .where_eq("issuer", identity.issuer.clone())
            .where_eq("subject", identity.subject.clone())
            .execute(db)
            .await?;
        return Ok(ResolvedExternalAccount {
            user_id,
            created: false,
        });
    }

    let handle = crate::generate_unique_handle(db, &identity.display_name).await?;
    let display_name = crate::normalize_display_name(&identity.display_name)?;
    let user_id = Uuid::new_v4().to_string();
    let now_ms = timestamp_ms(now)?;
    let tx = db.begin_transaction().await?;
    let creation = async {
        tx.insert("users")
            .value("user_id", user_id.clone())
            .value("username_normalized", handle.clone())
            .value("username_display", handle)
            .value("created_at_ms", now_ms)
            .execute(&*tx)
            .await?;
        tx.insert("user_profiles")
            .value("user_id", user_id.clone())
            .value("display_name", display_name)
            .value("display_name_source", "GOOGLE")
            .value("avatar_source", "GOOGLE")
            .value("provider_picture_url", identity.picture_url.as_deref())
            .value("provider_picture_checked_at_ms", Option::<i64>::None)
            .value("updated_at_ms", now_ms)
            .execute(&*tx)
            .await?;
        tx.insert("external_identities")
            .value("external_identity_id", Uuid::new_v4().to_string())
            .value("provider", identity.provider.clone())
            .value("issuer", identity.issuer.clone())
            .value("subject", identity.subject.clone())
            .value("user_id", user_id.clone())
            .value("provider_display_name", identity.display_name.clone())
            .value("provider_picture_url", identity.picture_url.as_deref())
            .value("created_at_ms", now_ms)
            .value("last_authenticated_at_ms", now_ms)
            .execute(&*tx)
            .await?;
        Ok::<_, switchy_database::DatabaseError>(())
    }
    .await;
    if let Err(error) = creation {
        drop(tx);
        if let Some(winner) =
            user_for_external_identity(db, &identity.issuer, &identity.subject).await?
        {
            return Ok(ResolvedExternalAccount {
                user_id: winner,
                created: false,
            });
        }
        return Err(ExternalIdentityError::Database(error));
    }
    tx.commit().await?;
    Ok(ResolvedExternalAccount {
        user_id,
        created: true,
    })
}

/// Returns the WWMTF user linked to one issuer/subject pair.
///
/// # Errors
///
/// * Returns malformed-row or database failures.
pub async fn user_for_external_identity(
    db: &dyn Database,
    issuer: &str,
    subject: &str,
) -> Result<Option<String>, ExternalIdentityError> {
    let rows = db
        .select("external_identities")
        .where_eq("issuer", issuer)
        .where_eq("subject", subject)
        .execute(db)
        .await?;
    rows.first()
        .map(|row| {
            row.get("user_id")
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .ok_or(ExternalIdentityError::Malformed)
        })
        .transpose()
}

/// Links a verified identity to an existing password-proven user.
///
/// This function rejects identities linked to any user and users already linked for the issuer.
/// Profile initialization and identity linking occur in one transaction. Password credential and
/// session transition is owned by the higher-level migration workflow.
///
/// # Errors
///
/// * Returns [`ExternalIdentityError::Conflict`] for an existing identity/user link.
/// * Returns validation, profile, timestamp, or database failures.
pub async fn link_external_identity(
    db: &dyn Database,
    user_id: &str,
    identity: &VerifiedExternalIdentity,
    now: OffsetDateTime,
) -> Result<(), ExternalIdentityError> {
    validate_identity(identity)?;
    if user_for_external_identity(db, &identity.issuer, &identity.subject)
        .await?
        .is_some()
        || !db
            .select("external_identities")
            .where_eq("user_id", user_id)
            .where_eq("issuer", identity.issuer.clone())
            .execute(db)
            .await?
            .is_empty()
    {
        return Err(ExternalIdentityError::Conflict);
    }
    let display_name = crate::normalize_display_name(&identity.display_name)?;
    let now_ms = timestamp_ms(now)?;
    let tx = db.begin_transaction().await?;
    tx.insert("user_profiles")
        .value("user_id", user_id)
        .value("display_name", display_name)
        .value("display_name_source", "GOOGLE")
        .value("avatar_source", "GOOGLE")
        .value("provider_picture_url", identity.picture_url.as_deref())
        .value("provider_picture_checked_at_ms", Option::<i64>::None)
        .value("updated_at_ms", now_ms)
        .execute(&*tx)
        .await?;
    tx.insert("external_identities")
        .value("external_identity_id", Uuid::new_v4().to_string())
        .value("provider", identity.provider.clone())
        .value("issuer", identity.issuer.clone())
        .value("subject", identity.subject.clone())
        .value("user_id", user_id)
        .value("provider_display_name", identity.display_name.clone())
        .value("provider_picture_url", identity.picture_url.as_deref())
        .value("created_at_ms", now_ms)
        .value("last_authenticated_at_ms", now_ms)
        .execute(&*tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

fn validate_identity(identity: &VerifiedExternalIdentity) -> Result<(), ExternalIdentityError> {
    if identity.provider != "GOOGLE"
        || identity.issuer.trim().is_empty()
        || identity.subject.trim().is_empty()
        || identity.issuer.chars().any(char::is_control)
        || identity.subject.chars().any(char::is_control)
    {
        return Err(ExternalIdentityError::Invalid);
    }
    Ok(())
}

fn timestamp_ms(timestamp: OffsetDateTime) -> Result<i64, ExternalIdentityError> {
    i64::try_from(timestamp.unix_timestamp_nanos() / 1_000_000)
        .map_err(|_| ExternalIdentityError::Timestamp)
}

/// External identity resolution failure.
#[derive(Debug, Error)]
pub enum ExternalIdentityError {
    #[error("external identity is invalid")]
    Invalid,
    #[error("external identity conflicts with an existing account")]
    Conflict,
    #[error("external identity row is malformed")]
    Malformed,
    #[error("external identity timestamp is outside the supported range")]
    Timestamp,
    #[error(transparent)]
    Profile(#[from] crate::ProfileError),
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;

    use super::*;
    use crate::{load_profile, migrate_app, register};

    #[test]
    fn google_identity_creates_and_resolves_one_stable_account() {
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
                "google-subject",
                "Ada Lovelace",
                Some("https://lh3.googleusercontent.com/avatar".to_string()),
            )
            .unwrap();
            let created = resolve_or_create_external_account(&*db, &identity, now)
                .await
                .unwrap();
            assert!(created.created);
            let resolved = resolve_or_create_external_account(&*db, &identity, now)
                .await
                .unwrap();
            assert!(!resolved.created);
            assert_eq!(resolved.user_id, created.user_id);
            assert_eq!(
                load_profile(&*db, &created.user_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .display_name,
                "Ada Lovelace"
            );
            assert!(
                db.select("password_credentials")
                    .where_eq("user_id", created.user_id)
                    .execute(&*db)
                    .await
                    .unwrap()
                    .is_empty()
            );
        });
    }

    #[test]
    fn explicit_link_preserves_existing_user_and_rejects_conflicts() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .unwrap();
            migrate_app(&*db).await.unwrap();
            let now = OffsetDateTime::UNIX_EPOCH;
            let user = register(&*db, "ada", "correct horse battery staple", now)
                .await
                .unwrap();
            let identity = VerifiedExternalIdentity::google(
                "https://accounts.google.com",
                "subject",
                "Ada",
                None,
            )
            .unwrap();
            link_external_identity(&*db, &user, &identity, now)
                .await
                .unwrap();
            assert_eq!(
                user_for_external_identity(&*db, &identity.issuer, &identity.subject)
                    .await
                    .unwrap(),
                Some(user.clone())
            );
            assert!(
                link_external_identity(&*db, &user, &identity, now)
                    .await
                    .is_err()
            );
        });
    }
}
