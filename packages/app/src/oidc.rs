//! Google `OpenID` Connect authorization-code protocol boundary.

use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointSet, IssuerUrl,
    Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse as _,
    core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata},
};
use thiserror::Error;

use crate::{ClaimedOidcAttempt, NewOidcAttempt, VerifiedExternalIdentity};

pub const GOOGLE_ISSUER: &str = "https://accounts.google.com";
const MAX_ID_TOKEN_AGE_SECS: i64 = 10 * 60;
const MAX_ID_TOKEN_FUTURE_SKEW_SECS: i64 = 5 * 60;
const PROVIDER_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const PROVIDER_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Server-side Google OIDC client configuration and discovered provider metadata.
pub struct GoogleOidcClient {
    client: CoreClient<
        EndpointSet,
        openidconnect::EndpointNotSet,
        openidconnect::EndpointNotSet,
        openidconnect::EndpointNotSet,
        EndpointMaybeSet,
        EndpointMaybeSet,
    >,
    issuer: String,
    client_id: String,
    client_secret: String,
}

impl GoogleOidcClient {
    /// Discovers and validates Google's OIDC metadata and constructs a confidential web client.
    ///
    /// The supplied HTTP client refuses redirects to avoid discovery/token SSRF expansion.
    ///
    /// # Errors
    ///
    /// * Returns configuration, discovery, or HTTP client construction failures.
    pub async fn discover(
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
    ) -> Result<Self, GoogleOidcError> {
        Self::discover_issuer_with_runtime(
            client_id,
            client_secret,
            redirect_uri,
            GOOGLE_ISSUER,
            None,
        )
        .await
    }

    /// Discovers an OIDC issuer while retaining Google's identity namespace.
    ///
    /// This is intended for deterministic local acceptance providers. Production wiring must use
    /// [`Self::discover`].
    ///
    /// # Errors
    ///
    /// * Returns configuration, discovery, or HTTP client construction failures.
    pub async fn discover_issuer(
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
        issuer_url: &str,
    ) -> Result<Self, GoogleOidcError> {
        Self::discover_issuer_with_runtime(client_id, client_secret, redirect_uri, issuer_url, None)
            .await
    }

    /// Discovers an issuer and retains the runtime that owns the HTTP client's async resources.
    ///
    /// # Errors
    ///
    /// * Returns configuration, discovery, or HTTP client construction failures.
    pub async fn discover_issuer_with_runtime(
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
        issuer_url: &str,
        runtime: Option<std::sync::Arc<tokio::runtime::Runtime>>,
    ) -> Result<Self, GoogleOidcError> {
        if client_id.trim().is_empty() || client_secret.trim().is_empty() {
            return Err(GoogleOidcError::Configuration);
        }
        let runtime = match runtime {
            Some(runtime) => runtime,
            None => std::sync::Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()
                    .map_err(|_| GoogleOidcError::HttpClient)?,
            ),
        };
        let http_client = {
            let runtime_guard = runtime.enter();
            let client = openidconnect::reqwest::ClientBuilder::new()
                .redirect(openidconnect::reqwest::redirect::Policy::none())
                .connect_timeout(PROVIDER_CONNECT_TIMEOUT)
                .timeout(PROVIDER_REQUEST_TIMEOUT)
                .build()
                .map_err(|_| GoogleOidcError::HttpClient)?;
            drop(runtime_guard);
            client
        };
        let issuer =
            IssuerUrl::new(issuer_url.to_string()).map_err(|_| GoogleOidcError::Configuration)?;
        let provider_metadata = CoreProviderMetadata::discover_async(issuer, &http_client)
            .await
            .map_err(|_| GoogleOidcError::Discovery)?;
        let discovered_issuer = provider_metadata.issuer().as_str().to_string();
        if discovered_issuer != issuer_url {
            return Err(GoogleOidcError::Discovery);
        }
        let client = CoreClient::from_provider_metadata(
            provider_metadata,
            ClientId::new(client_id.to_string()),
            Some(ClientSecret::new(client_secret.to_string())),
        )
        .set_redirect_uri(
            RedirectUrl::new(redirect_uri.to_string())
                .map_err(|_| GoogleOidcError::Configuration)?,
        );
        Ok(Self {
            client,
            issuer: discovered_issuer,
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
        })
    }

    /// Downloads one verified profile picture using the same bounded HTTP policy as production.
    ///
    /// Google uses its fixed HTTPS allowlist. Deterministic development issuers may serve a
    /// picture only from their exact origin; production wiring cannot construct such a client.
    ///
    /// # Errors
    ///
    /// * Returns profile URL, transport, response, or image-bound failures.
    pub async fn download_avatar(
        &self,
        picture_url: &str,
        timeout: std::time::Duration,
    ) -> Result<Vec<u8>, crate::ProfileError> {
        let development_origin = (self.issuer != GOOGLE_ISSUER).then_some(self.issuer.as_str());
        crate::download_provider_avatar(picture_url, timeout, development_origin).await
    }

    fn provider_http_client() -> Result<openidconnect::reqwest::Client, GoogleOidcError> {
        openidconnect::reqwest::ClientBuilder::new()
            .redirect(openidconnect::reqwest::redirect::Policy::none())
            .connect_timeout(PROVIDER_CONNECT_TIMEOUT)
            .timeout(PROVIDER_REQUEST_TIMEOUT)
            .build()
            .map_err(|_| GoogleOidcError::HttpClient)
    }

    async fn refresh_provider_keys(&self) -> Result<Self, GoogleOidcError> {
        let provider_metadata = CoreProviderMetadata::discover_async(
            IssuerUrl::new(self.issuer.clone()).map_err(|_| GoogleOidcError::Configuration)?,
            &Self::provider_http_client()?,
        )
        .await
        .map_err(|_| GoogleOidcError::Discovery)?;
        if provider_metadata.issuer().as_str() != self.issuer {
            return Err(GoogleOidcError::Discovery);
        }
        let client = CoreClient::from_provider_metadata(
            provider_metadata,
            ClientId::new(self.client_id.clone()),
            Some(ClientSecret::new(self.client_secret.clone())),
        )
        .set_redirect_uri(
            self.client
                .redirect_uri()
                .cloned()
                .ok_or(GoogleOidcError::Configuration)?,
        );
        Ok(Self {
            client,
            issuer: self.issuer.clone(),
            client_id: self.client_id.clone(),
            client_secret: self.client_secret.clone(),
        })
    }

    /// Builds the Google authorization URL from a durable attempt's exact secrets.
    #[must_use]
    pub fn authorization_url(&self, attempt: &NewOidcAttempt) -> String {
        let challenge = PkceCodeChallenge::from_code_verifier_sha256(&PkceCodeVerifier::new(
            attempt.pkce_verifier.clone(),
        ));
        let state = attempt.state.clone();
        let nonce = attempt.nonce.clone();
        let (url, _, _) = self
            .client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                move || CsrfToken::new(state),
                move || Nonce::new(nonce),
            )
            .add_scope(Scope::new("profile".to_string()))
            .set_pkce_challenge(challenge)
            .url();
        url.to_string()
    }

    /// Exchanges a callback code and validates the signed ID token and nonce.
    ///
    /// # Errors
    ///
    /// * Returns protocol errors for exchange failure, missing ID token, invalid signature/claims,
    ///   missing subject, or malformed profile claims.
    pub async fn exchange_callback(
        &self,
        code: &str,
        attempt: &ClaimedOidcAttempt,
    ) -> Result<VerifiedExternalIdentity, GoogleOidcError> {
        if code.trim().is_empty() {
            return Err(GoogleOidcError::Callback);
        }
        let token = self
            .client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .map_err(|_| GoogleOidcError::Callback)?
            .set_pkce_verifier(PkceCodeVerifier::new(attempt.pkce_verifier.clone()))
            .request_async(&Self::provider_http_client()?)
            .await
            .map_err(|_| GoogleOidcError::TokenExchange)?;
        let id_token = token.id_token().ok_or(GoogleOidcError::MissingIdToken)?;
        match self.validate_id_token(id_token, attempt) {
            Err(GoogleOidcError::InvalidIdToken) => self
                .refresh_provider_keys()
                .await?
                .validate_id_token(id_token, attempt),
            result => result,
        }
    }

    fn validate_id_token(
        &self,
        id_token: &openidconnect::core::CoreIdToken,
        attempt: &ClaimedOidcAttempt,
    ) -> Result<VerifiedExternalIdentity, GoogleOidcError> {
        let verifier = self
            .client
            .id_token_verifier()
            .set_issue_time_verifier_fn(|issue_time| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| "system clock precedes Unix epoch".to_string())?;
                let now = i64::try_from(now.as_secs())
                    .map_err(|_| "system clock is out of range".to_string())?;
                let issue_time = issue_time.timestamp();
                if issue_time > now.saturating_add(MAX_ID_TOKEN_FUTURE_SKEW_SECS) {
                    return Err("ID token issue time is in the future".to_string());
                }
                if issue_time < now.saturating_sub(MAX_ID_TOKEN_AGE_SECS) {
                    return Err("ID token issue time is too old".to_string());
                }
                Ok(())
            });
        let claims = id_token
            .claims(&verifier, &Nonce::new(attempt.nonce.clone()))
            .map_err(|_| GoogleOidcError::InvalidIdToken)?;
        let audiences = claims.audiences();
        let requires_authorized_party = audiences.len() > 1;
        let authorized_party_matches = claims
            .authorized_party()
            .is_none_or(|authorized_party| authorized_party.as_str() == self.client_id);
        if (requires_authorized_party && claims.authorized_party().is_none())
            || !authorized_party_matches
        {
            return Err(GoogleOidcError::InvalidIdToken);
        }
        let subject = claims.subject().as_str();
        if subject.trim().is_empty() {
            return Err(GoogleOidcError::InvalidIdToken);
        }
        let display_name = claims
            .name()
            .and_then(|localized| localized.get(None))
            .map_or_else(|| "Player".to_string(), |name| (**name).clone());
        let picture_url = claims
            .picture()
            .and_then(|localized| localized.get(None))
            .map(|url| (**url).clone());
        VerifiedExternalIdentity::google(
            self.issuer.clone(),
            subject.to_string(),
            display_name,
            picture_url,
        )
        .map_err(|_| GoogleOidcError::InvalidIdToken)
    }
}

/// Google OIDC protocol/configuration failure with no secret-bearing values.
#[derive(Debug, Error)]
pub enum GoogleOidcError {
    #[error("Google OIDC configuration is invalid")]
    Configuration,
    #[error("Google OIDC HTTP client could not be constructed")]
    HttpClient,
    #[error("Google OIDC discovery failed")]
    Discovery,
    #[error("Google OIDC callback is invalid")]
    Callback,
    #[error("Google OIDC token exchange failed")]
    TokenExchange,
    #[error("Google OIDC response did not contain an ID token")]
    MissingIdToken,
    #[error("Google OIDC ID token is invalid")]
    InvalidIdToken,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_configuration_fails_before_network_access() {
        futures_lite::future::block_on(async {
            assert!(
                GoogleOidcClient::discover("", "secret", "https://example.com/callback")
                    .await
                    .is_err()
            );
            assert!(
                GoogleOidcClient::discover("client", "", "https://example.com/callback")
                    .await
                    .is_err()
            );
        });
    }
}
