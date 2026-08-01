//! Opaque, hashed, expiring, and revocable authentication sessions.

use std::fmt::Write as _;

use sha2::{Digest as _, Sha256};
use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// Raw opaque session token returned only to the authenticated client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionToken(String);

impl SessionToken {
    /// Returns the secret token for cookie transport.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// Creates and stores a session while persisting only its token hash.
///
/// # Errors
///
/// * Returns [`SessionError::Database`] when storage fails.
/// * Returns [`SessionError::Timestamp`] for unsupported timestamps.
pub async fn create_session(
    db: &dyn Database,
    user_id: &str,
    now: OffsetDateTime,
    lifetime: Duration,
) -> Result<SessionToken, SessionError> {
    let token = SessionToken(format!("{}{}", Uuid::new_v4(), Uuid::new_v4()));
    let expires = now.checked_add(lifetime).ok_or(SessionError::Timestamp)?;
    db.insert("auth_sessions")
        .value("session_hash", token_hash(token.expose()))
        .value("user_id", user_id)
        .value("expires_at_ms", timestamp_ms(expires)?)
        .value("revoked_at_ms", Option::<i64>::None)
        .value("created_at_ms", timestamp_ms(now)?)
        .execute(db)
        .await?;
    Ok(token)
}

/// Resolves an active session to its user identity.
///
/// # Errors
///
/// * Returns [`SessionError::Invalid`] for unknown, expired, or revoked tokens.
/// * Returns [`SessionError::Database`] when storage fails.
pub async fn resolve_session(
    db: &dyn Database,
    token: &str,
    now: OffsetDateTime,
) -> Result<String, SessionError> {
    let rows = db
        .select("auth_sessions")
        .where_eq("session_hash", token_hash(token))
        .execute(db)
        .await?;
    let row = rows.first().ok_or(SessionError::Invalid)?;
    if !matches!(
        row.get("revoked_at_ms"),
        None | Some(switchy_database::DatabaseValue::Null)
    ) {
        return Err(SessionError::Invalid);
    }
    let expires = row
        .get("expires_at_ms")
        .and_then(|value| value.as_i64())
        .ok_or(SessionError::Invalid)?;
    if expires <= timestamp_ms(now)? {
        return Err(SessionError::Invalid);
    }
    row.get("user_id")
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(SessionError::Invalid)
}

/// Revokes a session token immediately.
///
/// # Errors
///
/// * Returns [`SessionError::Database`] when storage fails.
pub async fn revoke_session(
    db: &dyn Database,
    token: &str,
    now: OffsetDateTime,
) -> Result<(), SessionError> {
    db.update("auth_sessions")
        .value("revoked_at_ms", timestamp_ms(now)?)
        .where_eq("session_hash", token_hash(token))
        .execute(db)
        .await?;
    Ok(())
}

fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to String is infallible");
            output
        })
}

fn timestamp_ms(timestamp: OffsetDateTime) -> Result<i64, SessionError> {
    i64::try_from(timestamp.unix_timestamp_nanos() / 1_000_000).map_err(|_| SessionError::Timestamp)
}

/// Session lifecycle failure.
#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session is unknown, expired, or revoked")]
    Invalid,
    #[error("session timestamp is outside the supported range")]
    Timestamp,
    #[error("session storage is temporarily busy")]
    Busy,
    #[error(transparent)]
    Database(switchy_database::DatabaseError),
}

impl From<switchy_database::DatabaseError> for SessionError {
    fn from(error: switchy_database::DatabaseError) -> Self {
        if error.to_string().contains("concurrent use forbidden") {
            Self::Busy
        } else {
            Self::Database(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;

    use super::*;
    use crate::{migrate_app, register};

    #[test]
    fn sessions_store_hashes_expire_and_revoke() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens");
            migrate_app(&*db).await.expect("migrations run");
            let user_id = register(
                &*db,
                "alice",
                "correct horse battery staple",
                OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .expect("registration succeeds");
            let token = create_session(
                &*db,
                &user_id,
                OffsetDateTime::UNIX_EPOCH,
                Duration::hours(1),
            )
            .await
            .expect("session is created");
            let rows = db
                .select("auth_sessions")
                .execute(&*db)
                .await
                .expect("query succeeds");
            let stored = rows[0]
                .get("session_hash")
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .expect("hash exists");
            assert_ne!(stored, token.expose());
            assert_eq!(
                resolve_session(&*db, token.expose(), OffsetDateTime::UNIX_EPOCH)
                    .await
                    .expect("session resolves"),
                user_id
            );
            assert!(matches!(
                resolve_session(
                    &*db,
                    token.expose(),
                    OffsetDateTime::UNIX_EPOCH + Duration::hours(2)
                )
                .await,
                Err(SessionError::Invalid)
            ));
            revoke_session(&*db, token.expose(), OffsetDateTime::UNIX_EPOCH)
                .await
                .expect("session revokes");
            assert!(matches!(
                resolve_session(&*db, token.expose(), OffsetDateTime::UNIX_EPOCH).await,
                Err(SessionError::Invalid)
            ));
        });
    }
}
