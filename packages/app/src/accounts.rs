//! Durable username/password accounts and revocable opaque sessions.

use argon2::{
    Algorithm, Argon2, Params, PasswordHash, PasswordHasher as _, PasswordVerifier as _, Version,
    password_hash::{SaltString, rand_core::OsRng},
};
use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

const PASSWORD_MEMORY_KIB: u32 = 19_456;
const PASSWORD_ITERATIONS: u32 = 2;
const PASSWORD_PARALLELISM: u32 = 1;

fn password_hasher() -> Result<Argon2<'static>, AccountError> {
    let params = Params::new(
        PASSWORD_MEMORY_KIB,
        PASSWORD_ITERATIONS,
        PASSWORD_PARALLELISM,
        None,
    )
    .map_err(|_| AccountError::PasswordHash)?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

///
/// # Errors
///
/// * Returns [`AccountError::InvalidUsername`] unless the trimmed username is 3–32 ASCII
///   alphanumeric, underscore, or hyphen characters.
pub fn normalize_username(username: &str) -> Result<String, AccountError> {
    let username = username.trim();
    if !(3..=32).contains(&username.len())
        || !username
            .bytes()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, b'_' | b'-'))
    {
        return Err(AccountError::InvalidUsername);
    }
    Ok(username.to_ascii_lowercase())
}

/// Creates an Argon2id password hash with a fresh salt.
///
/// # Errors
///
/// * Returns [`AccountError::PasswordHash`] if the password cannot be hashed.
pub fn hash_password(password: &str) -> Result<String, AccountError> {
    if password.len() < 12 {
        return Err(AccountError::WeakPassword);
    }
    let salt = SaltString::generate(&mut OsRng);
    password_hasher()?
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| AccountError::PasswordHash)
}

/// Verifies a password against its stored PHC string.
///
/// # Errors
///
/// * Returns [`AccountError::PasswordHash`] when the stored hash is malformed.
pub fn verify_password(password: &str, stored_hash: &str) -> Result<bool, AccountError> {
    let parsed = PasswordHash::new(stored_hash).map_err(|_| AccountError::PasswordHash)?;
    Ok(password_hasher()?
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

fn password_needs_upgrade(stored_hash: &str) -> bool {
    PasswordHash::new(stored_hash).map_or(true, |hash| {
        hash.algorithm.as_str() != "argon2id"
            || hash.version != Some(19)
            || hash.params.get_decimal("m") != Some(PASSWORD_MEMORY_KIB)
            || hash.params.get_decimal("t") != Some(PASSWORD_ITERATIONS)
            || hash.params.get_decimal("p") != Some(PASSWORD_PARALLELISM)
    })
}

/// Registers a unique account through portable `switchy` builders.
///
/// # Errors
///
/// * Returns username/password validation errors.
/// * Returns [`AccountError::UsernameTaken`] when normalized identity already exists.
/// * Returns [`AccountError::Database`] on database failure.
pub async fn register(
    db: &dyn Database,
    username: &str,
    password: &str,
    now: OffsetDateTime,
) -> Result<String, AccountError> {
    let normalized = normalize_username(username)?;
    if !db
        .select("users")
        .where_eq("username_normalized", normalized.clone())
        .execute(db)
        .await?
        .is_empty()
    {
        return Err(AccountError::UsernameTaken);
    }
    let user_id = Uuid::new_v4().to_string();
    let password_hash = hash_password(password)?;
    let now = now.unix_timestamp_nanos() / 1_000_000;
    let now = i64::try_from(now).map_err(|_| AccountError::InvalidTimestamp)?;
    let tx = db.begin_transaction().await?;
    tx.insert("users")
        .value("user_id", user_id.clone())
        .value("username_normalized", normalized)
        .value("username_display", username.trim())
        .value("created_at_ms", now)
        .execute(&*tx)
        .await?;
    tx.insert("password_credentials")
        .value("user_id", user_id.clone())
        .value("password_hash", password_hash)
        .value("updated_at_ms", now)
        .execute(&*tx)
        .await?;
    tx.commit().await?;
    Ok(user_id)
}

/// Authenticates a username/password pair.
///
/// # Errors
///
/// * Returns [`AccountError::InvalidCredentials`] without revealing whether username or password
///   was incorrect.
/// * Returns [`AccountError::Database`] on database failure.
pub async fn authenticate(
    db: &dyn Database,
    username: &str,
    password: &str,
) -> Result<String, AccountError> {
    let normalized = normalize_username(username).map_err(|_| AccountError::InvalidCredentials)?;
    let users = db
        .select("users")
        .where_eq("username_normalized", normalized)
        .execute(db)
        .await?;
    let user_id = users
        .first()
        .and_then(|row| row.get("user_id"))
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(AccountError::InvalidCredentials)?;
    let credentials = db
        .select("password_credentials")
        .where_eq("user_id", user_id.clone())
        .execute(db)
        .await?;
    let stored_hash = credentials
        .first()
        .and_then(|row| row.get("password_hash"))
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(AccountError::InvalidCredentials)?;
    if !verify_password(password, &stored_hash)? {
        return Err(AccountError::InvalidCredentials);
    }
    if password_needs_upgrade(&stored_hash) {
        let upgraded = hash_password(password)?;
        db.update("password_credentials")
            .value("password_hash", upgraded)
            .where_eq("user_id", user_id.clone())
            .execute(db)
            .await?;
    }
    Ok(user_id)
}

/// Account and credential failure.
#[derive(Debug, Error)]
pub enum AccountError {
    #[error("username must contain 3 to 32 ASCII letters, numbers, underscores, or hyphens")]
    InvalidUsername,
    #[error("password must contain at least 12 bytes")]
    WeakPassword,
    #[error("password hashing failed")]
    PasswordHash,
    #[error("username is already registered")]
    UsernameTaken,
    /// Current password hash does not use the configured Argon2id profile.
    #[error("password hash requires a parameter upgrade")]
    PasswordUpgradeRequired,
    #[error("invalid username or password")]
    InvalidCredentials,
    #[error("timestamp is outside the supported range")]
    InvalidTimestamp,
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;

    use super::*;
    use crate::migrate_app;

    #[test]
    fn normalization_and_password_hashes_are_safe() {
        assert_eq!(
            normalize_username(" Alice_1 ").expect("username normalizes"),
            "alice_1"
        );
        assert!(normalize_username("a!").is_err());
        let first = hash_password("correct horse battery staple").expect("password hashes");
        let second = hash_password("correct horse battery staple").expect("password hashes");
        assert_ne!(first, second);
        assert!(verify_password("correct horse battery staple", &first).expect("hash parses"));
        assert!(!verify_password("incorrect password", &first).expect("hash parses"));
    }

    #[test]
    fn registration_and_authentication_use_turso_storage() {
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
                "Alice",
                "correct horse battery staple",
                OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .expect("registration succeeds");
            assert_eq!(
                authenticate(&*db, "ALICE", "correct horse battery staple")
                    .await
                    .expect("authentication succeeds"),
                user_id
            );
            assert!(matches!(
                authenticate(&*db, "alice", "wrong password").await,
                Err(AccountError::InvalidCredentials)
            ));
        });
    }
}
