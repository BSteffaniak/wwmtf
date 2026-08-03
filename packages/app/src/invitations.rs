//! Hashed, expiring, single-use, and revocable shareable invitations.

use std::fmt::Write as _;

use sha2::{Digest as _, Sha256};
use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// Raw invitation token returned only to its creator for sharing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvitationToken(String);

impl InvitationToken {
    /// Returns the secret token for link transport.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// Creates an invitation while persisting only its token hash.
///
/// Token generation retries on the extraordinarily unlikely event of a hash collision.
///
/// # Errors
///
/// * Returns [`InvitationError::Collision`] when collision-safe generation exhausts its attempts.
/// * Returns [`InvitationError::Timestamp`] for unsupported timestamps.
/// * Returns [`InvitationError::Database`] when storage fails.
pub async fn create_invitation(
    db: &dyn Database,
    creator_user_id: &str,
    now: OffsetDateTime,
    lifetime: Duration,
) -> Result<(String, InvitationToken), InvitationError> {
    let expires = now
        .checked_add(lifetime)
        .ok_or(InvitationError::Timestamp)?;
    for _ in 0..4 {
        let token = InvitationToken(format!("{}{}", Uuid::new_v4(), Uuid::new_v4()));
        let token_hash = token_hash(token.expose());
        if !db
            .select("invitations")
            .where_eq("token_hash", token_hash.clone())
            .execute(db)
            .await?
            .is_empty()
        {
            continue;
        }
        let invitation_id = Uuid::new_v4().to_string();
        db.insert("invitations")
            .value("invitation_id", invitation_id.clone())
            .value("creator_user_id", creator_user_id)
            .value("token_hash", token_hash)
            .value("status", "ACTIVE")
            .value("expires_at_ms", timestamp_ms(expires)?)
            .value("redeemed_by_user_id", Option::<String>::None)
            .value("created_at_ms", timestamp_ms(now)?)
            .execute(db)
            .await?;
        return Ok((invitation_id, token));
    }
    Err(InvitationError::Collision)
}

/// Redeems one active invitation exactly once for an authenticated user.
///
/// # Errors
///
/// * Returns [`InvitationError::Invalid`] for unknown, expired, revoked, consumed, or self-issued
///   invitations.
/// * Returns [`InvitationError::Database`] when storage fails.
pub async fn redeem_invitation(
    db: &dyn Database,
    token: &str,
    redeemer_user_id: &str,
    now: OffsetDateTime,
) -> Result<String, InvitationError> {
    let token_hash = token_hash(token);
    let tx = db.begin_transaction().await?;
    let rows = tx
        .select("invitations")
        .where_eq("token_hash", token_hash.clone())
        .where_eq("status", "ACTIVE")
        .execute(&*tx)
        .await?;
    let row = rows.first().ok_or(InvitationError::Invalid)?;
    let invitation_id = string_column(row, "invitation_id")?;
    let creator_user_id = string_column(row, "creator_user_id")?;
    let expires = row
        .get("expires_at_ms")
        .and_then(|value| value.as_i64())
        .ok_or(InvitationError::Invalid)?;
    if creator_user_id == redeemer_user_id || expires <= timestamp_ms(now)? {
        tx.rollback().await?;
        return Err(InvitationError::Invalid);
    }
    let updated = tx
        .update("invitations")
        .value("status", "REDEEMED")
        .value("redeemed_by_user_id", redeemer_user_id)
        .where_eq("invitation_id", invitation_id.clone())
        .where_eq("status", "ACTIVE")
        .execute(&*tx)
        .await?;
    if updated.len() != 1 {
        tx.rollback().await?;
        return Err(InvitationError::Invalid);
    }
    tx.commit().await?;
    Ok(invitation_id)
}

/// Redeems one active invitation and starts its game exactly once.
///
/// # Errors
///
/// * Returns [`InvitationError::Invalid`] for an invalid invitation.
/// * Returns [`InvitationError::GameCreation`] when pinned game initialization fails.
pub async fn redeem_invitation_and_start_game(
    db: &dyn Database,
    token: &str,
    redeemer_user_id: &str,
    now: OffsetDateTime,
    shuffle_seed: u64,
) -> Result<wwmtf_game_domain::GameId, InvitationError> {
    let token_hash = token_hash(token);
    let tx = db.begin_transaction().await?;
    let rows = tx
        .select("invitations")
        .where_eq("token_hash", token_hash)
        .where_eq("status", "ACTIVE")
        .execute(&*tx)
        .await?;
    let row = rows.first().ok_or(InvitationError::Invalid)?;
    let invitation_id = string_column(row, "invitation_id")?;
    let creator_user_id = string_column(row, "creator_user_id")?;
    let expires = row
        .get("expires_at_ms")
        .and_then(|value| value.as_i64())
        .ok_or(InvitationError::Invalid)?;
    if creator_user_id == redeemer_user_id || expires <= timestamp_ms(now)? {
        tx.rollback().await?;
        return Err(InvitationError::Invalid);
    }
    let updated = tx
        .update("invitations")
        .value("status", "REDEEMED")
        .value("redeemed_by_user_id", redeemer_user_id)
        .where_eq("invitation_id", invitation_id.clone())
        .where_eq("status", "ACTIVE")
        .execute(&*tx)
        .await?;
    if updated.len() != 1 {
        tx.rollback().await?;
        return Err(InvitationError::Invalid);
    }
    let game_id = crate::create_game_in_transaction(
        &*tx,
        &creator_user_id,
        redeemer_user_id,
        now,
        shuffle_seed,
        &format!("redeem:{invitation_id}"),
    )
    .await?;
    tx.commit().await?;
    Ok(game_id)
}

/// Revokes an active invitation owned by the authenticated creator.
///
/// # Errors
///
/// * Returns [`InvitationError::Invalid`] if the invitation is not active or is not owned by the
///   caller.
/// * Returns [`InvitationError::Database`] when storage fails.
pub async fn revoke_invitation(
    db: &dyn Database,
    invitation_id: &str,
    creator_user_id: &str,
) -> Result<(), InvitationError> {
    let updated = db
        .update("invitations")
        .value("status", "REVOKED")
        .where_eq("invitation_id", invitation_id)
        .where_eq("creator_user_id", creator_user_id)
        .where_eq("status", "ACTIVE")
        .execute(db)
        .await?;
    if updated.len() != 1 {
        return Err(InvitationError::Invalid);
    }
    Ok(())
}

fn string_column(row: &switchy_database::Row, name: &str) -> Result<String, InvitationError> {
    row.get(name)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(InvitationError::Invalid)
}

fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to String is infallible");
            output
        })
}

fn timestamp_ms(timestamp: OffsetDateTime) -> Result<i64, InvitationError> {
    i64::try_from(timestamp.unix_timestamp_nanos() / 1_000_000)
        .map_err(|_| InvitationError::Timestamp)
}

/// Invitation lifecycle failure.
#[derive(Debug, Error)]
pub enum InvitationError {
    #[error("invitation is unknown, expired, revoked, consumed, or unauthorized")]
    Invalid,
    #[error("could not generate a collision-free invitation token")]
    Collision,
    #[error("invitation timestamp is outside the supported range")]
    Timestamp,
    #[error(transparent)]
    GameCreation(#[from] crate::ChallengeError),
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;

    use super::*;
    use crate::{migrate_app, register};

    async fn users(db: &dyn Database) -> (String, String) {
        let creator = register(
            db,
            "alice",
            "correct horse battery staple",
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .expect("creator registers");
        let redeemer = register(
            db,
            "bob",
            "another correct horse battery",
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .expect("redeemer registers");
        (creator, redeemer)
    }

    #[test]
    fn invitation_tokens_are_hashed_single_use_and_revocable() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens");
            migrate_app(&*db).await.expect("migrations run");
            let (creator, redeemer) = users(&*db).await;
            let (invitation_id, token) = create_invitation(
                &*db,
                &creator,
                OffsetDateTime::UNIX_EPOCH,
                Duration::days(1),
            )
            .await
            .expect("invitation creates");
            let rows = db
                .select("invitations")
                .execute(&*db)
                .await
                .expect("query succeeds");
            let stored = string_column(&rows[0], "token_hash").expect("hash exists");
            assert_ne!(stored, token.expose());
            assert_eq!(
                redeem_invitation(&*db, token.expose(), &redeemer, OffsetDateTime::UNIX_EPOCH)
                    .await
                    .expect("invitation redeems"),
                invitation_id
            );
            assert!(matches!(
                redeem_invitation(&*db, token.expose(), &redeemer, OffsetDateTime::UNIX_EPOCH)
                    .await,
                Err(InvitationError::Invalid)
            ));

            let (invitation_id, token) = create_invitation(
                &*db,
                &creator,
                OffsetDateTime::UNIX_EPOCH,
                Duration::days(1),
            )
            .await
            .expect("invitation creates");
            revoke_invitation(&*db, &invitation_id, &creator)
                .await
                .expect("invitation revokes");
            assert!(matches!(
                redeem_invitation(&*db, token.expose(), &redeemer, OffsetDateTime::UNIX_EPOCH)
                    .await,
                Err(InvitationError::Invalid)
            ));
        });
    }

    #[test]
    fn invitation_redemption_starts_exactly_one_game() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens");
            migrate_app(&*db).await.expect("migrations run");
            let (creator, redeemer) = users(&*db).await;
            let (_, token) = create_invitation(
                &*db,
                &creator,
                OffsetDateTime::UNIX_EPOCH,
                Duration::days(1),
            )
            .await
            .expect("invitation creates");
            let game = redeem_invitation_and_start_game(
                &*db,
                token.expose(),
                &redeemer,
                OffsetDateTime::UNIX_EPOCH,
                7,
            )
            .await
            .expect("invitation starts game");
            assert_eq!(
                db.select("game_players")
                    .where_eq("game_id", game.to_string())
                    .execute(&*db)
                    .await
                    .expect("players load")
                    .len(),
                2
            );
            assert!(matches!(
                redeem_invitation_and_start_game(
                    &*db,
                    token.expose(),
                    &redeemer,
                    OffsetDateTime::UNIX_EPOCH,
                    7,
                )
                .await,
                Err(InvitationError::Invalid)
            ));
        });
    }

    #[test]
    fn invitation_expiry_and_self_redemption_are_rejected() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens");
            migrate_app(&*db).await.expect("migrations run");
            let (creator, redeemer) = users(&*db).await;
            let (_, token) = create_invitation(
                &*db,
                &creator,
                OffsetDateTime::UNIX_EPOCH,
                Duration::hours(1),
            )
            .await
            .expect("invitation creates");
            assert!(matches!(
                redeem_invitation(&*db, token.expose(), &creator, OffsetDateTime::UNIX_EPOCH).await,
                Err(InvitationError::Invalid)
            ));
            assert!(matches!(
                redeem_invitation(
                    &*db,
                    token.expose(),
                    &redeemer,
                    OffsetDateTime::UNIX_EPOCH + Duration::hours(2),
                )
                .await,
                Err(InvitationError::Invalid)
            ));
        });
    }
}
