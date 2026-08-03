//! Authenticated product workflows composed from durable repositories.

use switchy_database::Database;
use time::{Duration, OffsetDateTime};

use crate::{
    AccountError, ChallengeError, InvitationError, InvitationToken, SessionError, SessionToken,
    accept_challenge, authenticate, cancel_challenge, create_challenge, create_invitation,
    decline_challenge, find_user_by_username, normalize_username, redeem_invitation_and_start_game,
    register, revoke_invitation, revoke_session,
};

/// Registers a user and immediately creates a durable browser session.
///
/// # Errors
///
/// * Returns account or session persistence/validation failures.
pub async fn register_and_create_session(
    db: &dyn Database,
    username: &str,
    password: &str,
    now: OffsetDateTime,
    lifetime: Duration,
) -> Result<(String, SessionToken), AccountWorkflowError> {
    let user_id = register(db, username, password, now).await?;
    let session = crate::create_session(db, &user_id, now, lifetime).await?;
    Ok((user_id, session))
}

/// Authenticates credentials and creates a durable browser session.
///
/// # Errors
///
/// * Returns uniform invalid-credential or session persistence failures.
pub async fn login_and_create_session(
    db: &dyn Database,
    username: &str,
    password: &str,
    now: OffsetDateTime,
    lifetime: Duration,
) -> Result<(String, SessionToken), AccountWorkflowError> {
    let user_id = authenticate(db, username, password)
        .await
        .map_err(|error| {
            crate::observability::record_authentication_failure("invalid_credentials");
            AccountWorkflowError::Account(error)
        })?;
    let session = crate::create_session(db, &user_id, now, lifetime).await?;
    Ok((user_id, session))
}

/// Revokes the current durable session.
///
/// # Errors
///
/// * Returns session persistence failures.
pub async fn logout_session(
    db: &dyn Database,
    session: &str,
    now: OffsetDateTime,
) -> Result<(), AccountWorkflowError> {
    revoke_session(db, session, now).await?;
    Ok(())
}

/// Creates a private direct challenge by exact normalized username.
///
/// # Errors
///
/// * Returns unknown-user, duplicate, authorization, validation, or persistence failures.
pub async fn challenge_username(
    db: &dyn Database,
    challenger_user_id: &str,
    username: &str,
    now: OffsetDateTime,
) -> Result<String, ProductWorkflowError> {
    let normalized = normalize_username(username).map_err(ProductWorkflowError::Account)?;
    let (opponent_user_id, _) = find_user_by_username(db, &normalized)
        .await?
        .ok_or(ProductWorkflowError::UnknownUser)?;
    Ok(create_challenge(db, challenger_user_id, &opponent_user_id, now).await?)
}

/// Accepts a pending challenge and creates its game exactly once.
///
/// # Errors
///
/// * Returns authorization, lifecycle, compatibility, or persistence failures.
pub async fn accept_pending_challenge(
    db: &dyn Database,
    challenge_id: &str,
    user_id: &str,
    now: OffsetDateTime,
) -> Result<wwmtf_game_domain::GameId, ProductWorkflowError> {
    Ok(accept_challenge(db, challenge_id, user_id, now, random_seed()).await?)
}

/// Declines a pending incoming challenge.
///
/// # Errors
///
/// * Returns authorization, lifecycle, or persistence failures.
pub async fn decline_pending_challenge(
    db: &dyn Database,
    challenge_id: &str,
    user_id: &str,
    now: OffsetDateTime,
) -> Result<(), ProductWorkflowError> {
    decline_challenge(db, challenge_id, user_id, now).await?;
    Ok(())
}

/// Cancels a pending outgoing challenge.
///
/// # Errors
///
/// * Returns authorization, lifecycle, or persistence failures.
pub async fn cancel_pending_challenge(
    db: &dyn Database,
    challenge_id: &str,
    user_id: &str,
    now: OffsetDateTime,
) -> Result<(), ProductWorkflowError> {
    cancel_challenge(db, challenge_id, user_id, now).await?;
    Ok(())
}

/// Creates a shareable private invitation.
///
/// # Errors
///
/// * Returns timestamp, collision, or persistence failures.
pub async fn create_shareable_invitation(
    db: &dyn Database,
    user_id: &str,
    now: OffsetDateTime,
    lifetime: Duration,
) -> Result<(String, InvitationToken), ProductWorkflowError> {
    Ok(create_invitation(db, user_id, now, lifetime).await?)
}

/// Redeems a shareable invitation and creates its game exactly once.
///
/// # Errors
///
/// * Returns invalid/reused/expired invitation, compatibility, or persistence failures.
pub async fn redeem_shareable_invitation(
    db: &dyn Database,
    token: &str,
    user_id: &str,
    now: OffsetDateTime,
) -> Result<wwmtf_game_domain::GameId, ProductWorkflowError> {
    Ok(redeem_invitation_and_start_game(db, token, user_id, now, random_seed()).await?)
}

/// Revokes an owned shareable invitation.
///
/// # Errors
///
/// * Returns authorization, lifecycle, or persistence failures.
pub async fn revoke_shareable_invitation(
    db: &dyn Database,
    invitation_id: &str,
    user_id: &str,
    _now: OffsetDateTime,
) -> Result<(), ProductWorkflowError> {
    revoke_invitation(db, invitation_id, user_id).await?;
    Ok(())
}

fn random_seed() -> u64 {
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    u64::from_le_bytes(
        bytes[..8]
            .try_into()
            .expect("UUID has at least eight bytes"),
    )
}

/// Account/session workflow failure.
#[derive(Debug, thiserror::Error)]
pub enum AccountWorkflowError {
    #[error(transparent)]
    Account(#[from] AccountError),
    #[error(transparent)]
    Session(#[from] SessionError),
}

/// Challenge/invitation product workflow failure.
#[derive(Debug, thiserror::Error)]
pub enum ProductWorkflowError {
    #[error("username was not found")]
    UnknownUser,
    #[error(transparent)]
    Account(AccountError),
    #[error(transparent)]
    Challenge(#[from] ChallengeError),
    #[error(transparent)]
    Invitation(#[from] InvitationError),
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;

    use super::*;
    use crate::{migrate_app, resolve_session};

    #[test]
    fn account_and_multiplayer_workflows_connect_without_trusted_client_ids() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens");
            migrate_app(&*db).await.expect("migrations run");
            let now = OffsetDateTime::UNIX_EPOCH;
            let (alice, alice_session) = register_and_create_session(
                &*db,
                "alice",
                "correct horse battery staple",
                now,
                Duration::days(1),
            )
            .await
            .expect("Alice registers and signs in");
            let bob = register(&*db, "bob", "another correct horse battery", now)
                .await
                .expect("Bob registers");
            let (logged_in_bob, bob_session) = login_and_create_session(
                &*db,
                "bob",
                "another correct horse battery",
                now,
                Duration::days(1),
            )
            .await
            .expect("Bob signs in");
            assert_eq!(logged_in_bob, bob);
            assert_eq!(
                resolve_session(&*db, alice_session.expose(), now)
                    .await
                    .expect("Alice session resolves"),
                alice
            );

            let challenge = challenge_username(&*db, &alice, "Bob", now)
                .await
                .expect("challenge creates");
            let game_id = accept_pending_challenge(&*db, &challenge, &bob, now)
                .await
                .expect("Bob accepts");
            assert!(crate::recover_game(&*db, game_id).await.is_ok());

            logout_session(&*db, bob_session.expose(), now)
                .await
                .expect("Bob logs out");
            assert!(
                resolve_session(&*db, bob_session.expose(), now)
                    .await
                    .is_err()
            );
        });
    }
}
