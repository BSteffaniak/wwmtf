use std::{fmt::Write as _, sync::Arc};

use async_trait::async_trait;
use hyperchad::{
    renderer_html_actix::{WebSessionIdentityError, WebSessionIdentityResolver},
    shared_state_models::ParticipantId,
    shared_state_transport::AuthenticatedTransportContext,
};
use sha2::{Digest as _, Sha256};
use switchy_database::Database;
use time::OffsetDateTime;

/// Actix runtime adapter for the renderer-neutral durable session repository.
#[derive(Debug)]
pub struct DurableWebSessionIdentityResolver {
    database: Arc<dyn Database>,
}

impl DurableWebSessionIdentityResolver {
    #[must_use]
    pub fn new(database: Arc<dyn Database>) -> Self {
        Self { database }
    }
}

#[async_trait]
impl WebSessionIdentityResolver for DurableWebSessionIdentityResolver {
    async fn resolve_session(
        &self,
        opaque_session: &str,
    ) -> Result<AuthenticatedTransportContext, WebSessionIdentityError> {
        let user_id = words_with_spouses_app::resolve_session(
            &*self.database,
            opaque_session,
            OffsetDateTime::now_utc(),
        )
        .await
        .map_err(|error| match error {
            words_with_spouses_app::SessionError::Invalid
            | words_with_spouses_app::SessionError::Timestamp => {
                WebSessionIdentityError::Unauthenticated
            }
            words_with_spouses_app::SessionError::Busy => WebSessionIdentityError::Operation(
                "session storage is temporarily busy".to_string(),
            ),
            words_with_spouses_app::SessionError::Database(error) => {
                WebSessionIdentityError::Operation(error.to_string())
            }
        })?;
        Ok(AuthenticatedTransportContext {
            participant_id: ParticipantId::new(user_id),
            identity_binding: format!("session:{}", session_binding(opaque_session)),
        })
    }
}

fn session_binding(opaque_session: &str) -> String {
    Sha256::digest(opaque_session.as_bytes()).iter().fold(
        String::with_capacity(64),
        |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to String is infallible");
            output
        },
    )
}
