//! Durable, browser-bound `OpenID` Connect login attempts.

use std::fmt::Write as _;

use rand_core::{OsRng, RngCore as _};
use sha2::{Digest as _, Sha256};
use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

const TOKEN_BYTES: usize = 32;
const MAX_GENERATION_ATTEMPTS: usize = 4;

/// Purpose attached to an OIDC authorization attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OidcAttemptPurpose {
    /// Authenticate an existing external identity or create a new account.
    Login,
    /// Link a freshly authenticated external identity to a password-proven account.
    MigratePassword,
}

impl OidcAttemptPurpose {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Login => "LOGIN",
            Self::MigratePassword => "MIGRATE_PASSWORD",
        }
    }
}

/// Secrets returned only to the browser/provider flow that created an OIDC attempt.
#[derive(Debug)]
pub struct NewOidcAttempt {
    pub attempt_id: String,
    pub state: String,
    pub browser_binding: String,
    pub nonce: String,
    pub pkce_verifier: String,
}

/// A claimed exactly-once OIDC attempt containing callback validation material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedOidcAttempt {
    pub attempt_id: String,
    pub purpose: OidcAttemptPurpose,
    pub nonce: String,
    pub pkce_verifier: String,
    pub existing_user_id: Option<String>,
    pub continuation_invitation_id: Option<String>,
}

/// Creates one short-lived browser-bound OIDC attempt.
///
/// # Errors
///
/// * Returns [`OidcAttemptError::Invalid`] for invalid purpose-specific arguments or lifetime.
/// * Returns [`OidcAttemptError::Collision`] if collision-safe state generation is exhausted.
/// * Returns [`OidcAttemptError::Timestamp`] for unsupported timestamps.
/// * Returns [`OidcAttemptError::Database`] when persistence fails.
pub async fn create_oidc_attempt(
    db: &dyn Database,
    purpose: OidcAttemptPurpose,
    existing_user_id: Option<&str>,
    continuation_invitation_id: Option<&str>,
    now: OffsetDateTime,
    lifetime: Duration,
) -> Result<NewOidcAttempt, OidcAttemptError> {
    if lifetime <= Duration::ZERO
        || matches!(purpose, OidcAttemptPurpose::Login) && existing_user_id.is_some()
        || matches!(purpose, OidcAttemptPurpose::MigratePassword) && existing_user_id.is_none()
    {
        return Err(OidcAttemptError::Invalid);
    }
    let expires = now
        .checked_add(lifetime)
        .ok_or(OidcAttemptError::Timestamp)?;
    for _ in 0..MAX_GENERATION_ATTEMPTS {
        let state = random_secret();
        let state_hash = secret_hash(&state);
        if !db
            .select("auth_login_attempts")
            .where_eq("state_hash", state_hash.clone())
            .execute(db)
            .await?
            .is_empty()
        {
            continue;
        }
        let attempt = NewOidcAttempt {
            attempt_id: Uuid::new_v4().to_string(),
            state,
            browser_binding: random_secret(),
            nonce: random_secret(),
            pkce_verifier: random_secret(),
        };
        db.insert("auth_login_attempts")
            .value("attempt_id", attempt.attempt_id.clone())
            .value("state_hash", state_hash)
            .value(
                "browser_binding_hash",
                secret_hash(&attempt.browser_binding),
            )
            .value("nonce", attempt.nonce.clone())
            .value("pkce_verifier", attempt.pkce_verifier.clone())
            .value("purpose", purpose.as_str())
            .value("existing_user_id", existing_user_id)
            .value("continuation_invitation_id", continuation_invitation_id)
            .value("status", "PENDING")
            .value("created_at_ms", timestamp_ms(now)?)
            .value("expires_at_ms", timestamp_ms(expires)?)
            .value("claimed_at_ms", Option::<i64>::None)
            .value("consumed_at_ms", Option::<i64>::None)
            .execute(db)
            .await?;
        return Ok(attempt);
    }
    Err(OidcAttemptError::Collision)
}

/// Claims a valid pending OIDC attempt exactly once.
///
/// # Errors
///
/// * Returns [`OidcAttemptError::Invalid`] for unknown, expired, mismatched, or reused attempts.
/// * Returns [`OidcAttemptError::Timestamp`] for unsupported timestamps.
/// * Returns [`OidcAttemptError::Database`] when persistence fails.
pub async fn claim_oidc_attempt(
    db: &dyn Database,
    state: &str,
    browser_binding: &str,
    now: OffsetDateTime,
) -> Result<ClaimedOidcAttempt, OidcAttemptError> {
    let state_hash = secret_hash(state);
    let rows = db
        .select("auth_login_attempts")
        .where_eq("state_hash", state_hash.clone())
        .where_eq("browser_binding_hash", secret_hash(browser_binding))
        .where_eq("status", "PENDING")
        .execute(db)
        .await?;
    let row = rows.first().ok_or(OidcAttemptError::Invalid)?;
    let expires_at_ms = integer_column(row, "expires_at_ms")?;
    if expires_at_ms <= timestamp_ms(now)? {
        return Err(OidcAttemptError::Invalid);
    }
    let attempt_id = string_column(row, "attempt_id")?;
    let purpose = match string_column(row, "purpose")?.as_str() {
        "LOGIN" => OidcAttemptPurpose::Login,
        "MIGRATE_PASSWORD" => OidcAttemptPurpose::MigratePassword,
        _ => return Err(OidcAttemptError::Invalid),
    };
    let nonce = string_column(row, "nonce")?;
    let pkce_verifier = string_column(row, "pkce_verifier")?;
    let existing_user_id = optional_string_column(row, "existing_user_id");
    let continuation_invitation_id = optional_string_column(row, "continuation_invitation_id");
    let updated = db
        .update("auth_login_attempts")
        .value("status", "CLAIMED")
        .value("claimed_at_ms", timestamp_ms(now)?)
        .where_eq("attempt_id", attempt_id.clone())
        .where_eq("state_hash", state_hash)
        .where_eq("status", "PENDING")
        .execute(db)
        .await?;
    if updated.len() != 1 {
        return Err(OidcAttemptError::Invalid);
    }
    Ok(ClaimedOidcAttempt {
        attempt_id,
        purpose,
        nonce,
        pkce_verifier,
        existing_user_id,
        continuation_invitation_id,
    })
}

/// Marks a claimed OIDC attempt consumed after successful callback completion.
///
/// # Errors
///
/// * Returns [`OidcAttemptError::Invalid`] unless exactly one claimed attempt is consumed.
/// * Returns [`OidcAttemptError::Timestamp`] for unsupported timestamps.
/// * Returns [`OidcAttemptError::Database`] when persistence fails.
pub async fn consume_oidc_attempt(
    db: &dyn Database,
    attempt_id: &str,
    now: OffsetDateTime,
) -> Result<(), OidcAttemptError> {
    let updated = db
        .update("auth_login_attempts")
        .value("status", "CONSUMED")
        .value("consumed_at_ms", timestamp_ms(now)?)
        .where_eq("attempt_id", attempt_id)
        .where_eq("status", "CLAIMED")
        .execute(db)
        .await?;
    if updated.len() != 1 {
        return Err(OidcAttemptError::Invalid);
    }
    Ok(())
}

/// Deletes terminal and expired OIDC attempts at or before `now`.
///
/// # Errors
///
/// * Returns [`OidcAttemptError::Timestamp`] for unsupported timestamps.
/// * Returns [`OidcAttemptError::Database`] when persistence fails.
pub async fn cleanup_oidc_attempts(
    db: &dyn Database,
    now: OffsetDateTime,
) -> Result<(), OidcAttemptError> {
    let now_ms = timestamp_ms(now)?;
    let rows = db.select("auth_login_attempts").execute(db).await?;
    for row in rows {
        let status = string_column(&row, "status")?;
        let expires_at_ms = integer_column(&row, "expires_at_ms")?;
        if expires_at_ms <= now_ms || matches!(status.as_str(), "CONSUMED" | "FAILED") {
            db.delete("auth_login_attempts")
                .where_eq("attempt_id", string_column(&row, "attempt_id")?)
                .execute(db)
                .await?;
        }
    }
    Ok(())
}

fn random_secret() -> String {
    let mut bytes = [0_u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    hex(&bytes)
}

fn secret_hash(value: &str) -> String {
    hex(&Sha256::digest(value.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to String is infallible");
            output
        },
    )
}

fn string_column(row: &switchy_database::Row, name: &str) -> Result<String, OidcAttemptError> {
    row.get(name)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(OidcAttemptError::Invalid)
}

fn optional_string_column(row: &switchy_database::Row, name: &str) -> Option<String> {
    row.get(name)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
}

fn integer_column(row: &switchy_database::Row, name: &str) -> Result<i64, OidcAttemptError> {
    row.get(name)
        .and_then(|value| value.as_i64())
        .ok_or(OidcAttemptError::Invalid)
}

fn timestamp_ms(timestamp: OffsetDateTime) -> Result<i64, OidcAttemptError> {
    i64::try_from(timestamp.unix_timestamp_nanos() / 1_000_000)
        .map_err(|_| OidcAttemptError::Timestamp)
}

/// Durable OIDC attempt lifecycle failure.
#[derive(Debug, Error)]
pub enum OidcAttemptError {
    #[error("OIDC attempt is invalid, expired, mismatched, or already used")]
    Invalid,
    #[error("could not generate a collision-free OIDC state")]
    Collision,
    #[error("OIDC attempt timestamp is outside the supported range")]
    Timestamp,
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;

    use super::*;
    use crate::migrate_app;

    #[test]
    fn attempts_are_browser_bound_expiring_and_exactly_once() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens");
            migrate_app(&*db).await.expect("migrations run");
            let now = OffsetDateTime::UNIX_EPOCH;
            let attempt = create_oidc_attempt(
                &*db,
                OidcAttemptPurpose::Login,
                None,
                Some("invitation-id"),
                now,
                Duration::minutes(10),
            )
            .await
            .expect("attempt creates");
            let stored = db
                .select("auth_login_attempts")
                .execute(&*db)
                .await
                .expect("attempt loads");
            assert_ne!(
                stored[0]
                    .get("state_hash")
                    .and_then(|value| value.as_str().map(ToOwned::to_owned))
                    .as_deref(),
                Some(attempt.state.as_str())
            );
            assert!(
                claim_oidc_attempt(&*db, &attempt.state, "wrong-binding", now)
                    .await
                    .is_err()
            );
            let claimed = claim_oidc_attempt(&*db, &attempt.state, &attempt.browser_binding, now)
                .await
                .expect("attempt claims");
            assert_eq!(claimed.nonce, attempt.nonce);
            assert_eq!(
                claimed.continuation_invitation_id.as_deref(),
                Some("invitation-id")
            );
            assert!(
                claim_oidc_attempt(&*db, &attempt.state, &attempt.browser_binding, now)
                    .await
                    .is_err()
            );
            consume_oidc_attempt(&*db, &claimed.attempt_id, now)
                .await
                .expect("claim consumes");
            cleanup_oidc_attempts(&*db, now)
                .await
                .expect("terminal attempt cleans");
            assert!(
                db.select("auth_login_attempts")
                    .execute(&*db)
                    .await
                    .expect("attempts load")
                    .is_empty()
            );
        });
    }

    #[test]
    fn expired_and_invalid_purpose_attempts_fail_closed() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens");
            migrate_app(&*db).await.expect("migrations run");
            let now = OffsetDateTime::UNIX_EPOCH;
            assert!(
                create_oidc_attempt(
                    &*db,
                    OidcAttemptPurpose::MigratePassword,
                    None,
                    None,
                    now,
                    Duration::minutes(10)
                )
                .await
                .is_err()
            );
            let attempt = create_oidc_attempt(
                &*db,
                OidcAttemptPurpose::MigratePassword,
                Some("existing-user"),
                None,
                now,
                Duration::minutes(1),
            )
            .await
            .expect("migration attempt creates");
            assert!(
                claim_oidc_attempt(
                    &*db,
                    &attempt.state,
                    &attempt.browser_binding,
                    now + Duration::minutes(2)
                )
                .await
                .is_err()
            );
            cleanup_oidc_attempts(&*db, now + Duration::minutes(2))
                .await
                .expect("expired attempt cleans");
        });
    }
}
