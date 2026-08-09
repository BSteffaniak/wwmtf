//! Played-word definition provider and rebuildable durable cache.

use std::{
    collections::BTreeMap,
    sync::{Arc, LazyLock},
    time::Duration,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;

pub const FREE_DICTIONARY_PROVIDER: &str = "free-dictionary-api";
pub const FREE_DICTIONARY_PROVIDER_VERSION: u32 = 1;
pub const DEFAULT_DEFINITION_PROVIDER_BASE_URL: &str = "https://api.dictionaryapi.dev";
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_MEANINGS: usize = 12;
const MAX_DEFINITIONS_PER_MEANING: usize = 8;
const SUCCESS_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
const MISSING_TTL_MS: i64 = 24 * 60 * 60 * 1_000;

static LOOKUP_LOCKS: LazyLock<std::sync::Mutex<BTreeMap<String, Arc<async_lock::Mutex<()>>>>> =
    LazyLock::new(|| std::sync::Mutex::new(BTreeMap::new()));

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordDefinition {
    pub word: String,
    pub meanings: Vec<DefinitionMeaning>,
    pub source_url: String,
    pub license_name: String,
    pub license_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefinitionMeaning {
    pub part_of_speech: String,
    pub definitions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionUnavailableReason {
    Disabled,
    TimedOut,
    Unreachable,
    RateLimited,
    ProviderUnavailable,
    ProviderRejected,
    InvalidResponse,
    ResponseTooLarge,
    MissingAttribution,
    CacheUnavailable,
}

impl DefinitionUnavailableReason {
    #[must_use]
    pub const fn user_message(self) -> &'static str {
        match self {
            Self::Disabled => "Definitions have been disabled by the server administrator.",
            Self::TimedOut => "The definition provider timed out. Try again.",
            Self::Unreachable => "The definition provider could not be reached. Try again.",
            Self::RateLimited => {
                "The definition provider is temporarily rate limited. Try again later."
            }
            Self::ProviderUnavailable => {
                "The definition provider is temporarily unavailable. Try again later."
            }
            Self::ProviderRejected => "The definition provider configuration was rejected.",
            Self::InvalidResponse => "The provider returned an invalid definition response.",
            Self::ResponseTooLarge => {
                "The provider response exceeded the application's safety limit."
            }
            Self::MissingAttribution => {
                "The provider response omitted required licensing information."
            }
            Self::CacheUnavailable => "The definition cache could not be accessed. Try again.",
        }
    }

    pub(crate) const fn log_reason(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::TimedOut => "timeout",
            Self::Unreachable => "unreachable",
            Self::RateLimited => "rate_limited",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::ProviderRejected => "provider_rejected",
            Self::InvalidResponse => "invalid_response",
            Self::ResponseTooLarge => "response_too_large",
            Self::MissingAttribution => "missing_attribution",
            Self::CacheUnavailable => "cache_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefinitionLookup {
    Found(WordDefinition),
    Missing,
    Unavailable(DefinitionUnavailableReason),
}

#[async_trait]
pub trait DefinitionProvider: Send + Sync {
    async fn lookup(&self, word: &str) -> Result<Option<WordDefinition>, DefinitionError>;
}

#[derive(Debug, Clone)]
pub struct FreeDictionaryProvider {
    client: reqwest::Client,
    base_url: String,
    max_response_bytes: usize,
}

impl FreeDictionaryProvider {
    /// Builds a bounded HTTPS definition provider.
    ///
    /// # Errors
    ///
    /// * Returns an HTTP client construction error when TLS or timeout configuration fails.
    pub fn new(base_url: impl Into<String>, timeout: Duration) -> Result<Self, DefinitionError> {
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(timeout)
                .timeout(timeout)
                .build()?,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            max_response_bytes: MAX_RESPONSE_BYTES,
        })
    }
}

#[async_trait]
impl DefinitionProvider for FreeDictionaryProvider {
    async fn lookup(&self, word: &str) -> Result<Option<WordDefinition>, DefinitionError> {
        let normalized = normalize_played_word(word).ok_or(DefinitionError::InvalidWord)?;
        let mut response = self
            .client
            .get(format!(
                "{}/api/v2/entries/en/{}",
                self.base_url,
                normalized.to_ascii_lowercase()
            ))
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(DefinitionError::ProviderStatus(response.status().as_u16()));
        }
        if let Some(length) = response.content_length()
            && length > u64::try_from(self.max_response_bytes).unwrap_or(u64::MAX)
        {
            return Err(DefinitionError::ResponseTooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len().saturating_add(chunk.len()) > self.max_response_bytes {
                return Err(DefinitionError::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        parse_provider_response(&normalized, &bytes)
    }
}

/// Resolves a normalized played word through the durable cache and optional provider.
///
/// # Errors
///
/// * Returns [`DefinitionError`] for malformed input/cache data or persistence failures.
pub async fn lookup_definition(
    db: &dyn Database,
    provider: Option<&dyn DefinitionProvider>,
    word: &str,
    now_ms: i64,
) -> Result<DefinitionLookup, DefinitionError> {
    let normalized = normalize_played_word(word).ok_or(DefinitionError::InvalidWord)?;
    if let Some(cached) = load_cached(db, &normalized, now_ms).await? {
        return Ok(cached);
    }
    let lookup_lock = {
        let mut locks = LOOKUP_LOCKS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locks
            .entry(cache_id(&normalized))
            .or_insert_with(|| Arc::new(async_lock::Mutex::new(())))
            .clone()
    };
    let _guard = lookup_lock.lock().await;
    if let Some(cached) = load_cached(db, &normalized, now_ms).await? {
        return Ok(cached);
    }
    let Some(provider) = provider else {
        return Ok(DefinitionLookup::Unavailable(
            DefinitionUnavailableReason::Disabled,
        ));
    };
    match provider.lookup(&normalized).await {
        Ok(Some(definition)) => {
            cache_result(
                db,
                &normalized,
                "FOUND",
                Some(&definition),
                now_ms,
                SUCCESS_TTL_MS,
            )
            .await?;
            Ok(DefinitionLookup::Found(definition))
        }
        Ok(None) => {
            cache_result(db, &normalized, "MISSING", None, now_ms, MISSING_TTL_MS).await?;
            Ok(DefinitionLookup::Missing)
        }
        Err(error) => {
            let reason = DefinitionUnavailableReason::from(&error);
            log::warn!(
                target: "wwmtf::definitions",
                "definition_lookup_failed reason={}",
                reason.log_reason()
            );
            Ok(DefinitionLookup::Unavailable(reason))
        }
    }
}

fn normalize_played_word(word: &str) -> Option<String> {
    wwmtf_game_domain::normalize_word(word)
}

async fn load_cached(
    db: &dyn Database,
    word: &str,
    now_ms: i64,
) -> Result<Option<DefinitionLookup>, DefinitionError> {
    let rows = db
        .select("definition_cache")
        .where_eq("definition_cache_id", cache_id(word))
        .execute(db)
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let expires = row
        .get("expires_at_ms")
        .and_then(|value| value.as_i64())
        .ok_or(DefinitionError::MalformedCache)?;
    if expires <= now_ms {
        return Ok(None);
    }
    let status = row
        .get("status")
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(DefinitionError::MalformedCache)?;
    match status.as_str() {
        "FOUND" => {
            let payload = row
                .get("payload")
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .ok_or(DefinitionError::MalformedCache)?;
            Ok(Some(DefinitionLookup::Found(serde_json::from_str(
                &payload,
            )?)))
        }
        "MISSING" => Ok(Some(DefinitionLookup::Missing)),
        _ => Err(DefinitionError::MalformedCache),
    }
}

async fn cache_result(
    db: &dyn Database,
    word: &str,
    status: &str,
    definition: Option<&WordDefinition>,
    now_ms: i64,
    ttl_ms: i64,
) -> Result<(), DefinitionError> {
    let payload = definition.map(serde_json::to_string).transpose()?;
    let mut query = db
        .upsert("definition_cache")
        .where_eq("definition_cache_id", cache_id(word))
        .value("definition_cache_id", cache_id(word))
        .value("provider", FREE_DICTIONARY_PROVIDER)
        .value(
            "provider_version",
            i64::from(FREE_DICTIONARY_PROVIDER_VERSION),
        )
        .value("language", "en")
        .value("word", word)
        .value("status", status)
        .value("fetched_at_ms", now_ms)
        .value("expires_at_ms", now_ms.saturating_add(ttl_ms));
    query = if let Some(payload) = payload {
        query.value("payload", payload)
    } else {
        query.value("payload", switchy_database::DatabaseValue::Null)
    };
    query.execute(db).await?;
    Ok(())
}

fn cache_id(word: &str) -> String {
    format!("{FREE_DICTIONARY_PROVIDER}:{FREE_DICTIONARY_PROVIDER_VERSION}:en:{word}")
}

fn parse_provider_response(
    word: &str,
    bytes: &[u8],
) -> Result<Option<WordDefinition>, DefinitionError> {
    let entries: Vec<ProviderEntry> = serde_json::from_slice(bytes)?;
    let Some(entry) = entries.into_iter().next() else {
        return Ok(None);
    };
    let meanings = entry
        .meanings
        .into_iter()
        .take(MAX_MEANINGS)
        .filter_map(|meaning| {
            let definitions = meaning
                .definitions
                .into_iter()
                .take(MAX_DEFINITIONS_PER_MEANING)
                .map(|definition| definition.definition.trim().to_string())
                .filter(|definition| !definition.is_empty())
                .collect::<Vec<_>>();
            (!definitions.is_empty()).then_some(DefinitionMeaning {
                part_of_speech: meaning.part_of_speech,
                definitions,
            })
        })
        .collect::<Vec<_>>();
    if meanings.is_empty() {
        return Ok(None);
    }
    let source_url = entry
        .source_urls
        .into_iter()
        .find(|url| url.starts_with("https://"))
        .unwrap_or_else(|| {
            format!(
                "https://en.wiktionary.org/wiki/{}",
                word.to_ascii_lowercase()
            )
        });
    let license = entry.license.ok_or(DefinitionError::MissingAttribution)?;
    if !license.url.starts_with("https://") {
        return Err(DefinitionError::MissingAttribution);
    }
    Ok(Some(WordDefinition {
        word: word.to_string(),
        meanings,
        source_url,
        license_name: license.name,
        license_url: license.url,
    }))
}

#[derive(Debug, Deserialize)]
struct ProviderEntry {
    meanings: Vec<ProviderMeaning>,
    #[serde(default, rename = "sourceUrls")]
    source_urls: Vec<String>,
    license: Option<ProviderLicense>,
}

#[derive(Debug, Deserialize)]
struct ProviderMeaning {
    #[serde(rename = "partOfSpeech")]
    part_of_speech: String,
    definitions: Vec<ProviderDefinition>,
}

#[derive(Debug, Deserialize)]
struct ProviderDefinition {
    definition: String,
}

#[derive(Debug, Deserialize)]
struct ProviderLicense {
    name: String,
    url: String,
}

#[derive(Debug, Error)]
pub enum DefinitionError {
    #[error("played word is invalid")]
    InvalidWord,
    #[error("definition provider failed")]
    ProviderFailure,
    #[error("definition provider returned status {0}")]
    ProviderStatus(u16),
    #[error("definition provider response was too large")]
    ResponseTooLarge,
    #[error("definition provider omitted required attribution")]
    MissingAttribution,
    #[error("definition cache row is malformed")]
    MalformedCache,
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl From<&DefinitionError> for DefinitionUnavailableReason {
    fn from(error: &DefinitionError) -> Self {
        match error {
            DefinitionError::Http(error) if error.is_timeout() => Self::TimedOut,
            DefinitionError::Http(_) => Self::Unreachable,
            DefinitionError::ProviderStatus(429) => Self::RateLimited,
            DefinitionError::ProviderStatus(401 | 403) => Self::ProviderRejected,
            DefinitionError::ProviderStatus(500..=599) | DefinitionError::ProviderFailure => {
                Self::ProviderUnavailable
            }
            DefinitionError::ResponseTooLarge => Self::ResponseTooLarge,
            DefinitionError::MissingAttribution => Self::MissingAttribution,
            DefinitionError::Database(_) | DefinitionError::MalformedCache => {
                Self::CacheUnavailable
            }
            DefinitionError::Json(_)
            | DefinitionError::InvalidWord
            | DefinitionError::ProviderStatus(_) => Self::InvalidResponse,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read as _, Write as _},
        net::TcpListener,
        sync::atomic::{AtomicUsize, Ordering},
        thread,
    };

    use super::*;

    #[derive(Debug)]
    struct StubProvider {
        calls: AtomicUsize,
        result: Result<Option<WordDefinition>, DefinitionError>,
    }

    #[async_trait]
    impl DefinitionProvider for StubProvider {
        async fn lookup(&self, _word: &str) -> Result<Option<WordDefinition>, DefinitionError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.result.as_ref().map_or_else(
                |_| Err(DefinitionError::ProviderStatus(503)),
                |result| Ok(result.clone()),
            )
        }
    }

    fn definition() -> WordDefinition {
        WordDefinition {
            word: "WORD".to_string(),
            meanings: vec![DefinitionMeaning {
                part_of_speech: "noun".to_string(),
                definitions: vec!["A unit of language.".to_string()],
            }],
            source_url: "https://en.wiktionary.org/wiki/word".to_string(),
            license_name: "CC BY-SA 3.0".to_string(),
            license_url: "https://creativecommons.org/licenses/by-sa/3.0".to_string(),
        }
    }

    async fn database() -> Box<dyn Database> {
        let db = switchy_database_connection::builder()
            .turso()
            .with_in_memory()
            .build()
            .await
            .expect("database opens");
        crate::migrate_app(&*db).await.expect("migrations run");
        db
    }

    fn serve_once(response: Vec<u8>, delay: Duration) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server address");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("test request arrives");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            thread::sleep(delay);
            stream.write_all(&response).expect("test response writes");
        });
        (format!("http://{address}"), handle)
    }

    fn http_response(status: &str, body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Tokio runtime builds")
    }

    #[test]
    fn http_provider_maps_success_missing_errors_limits_and_timeout() {
        let body = br#"[{"meanings":[{"partOfSpeech":"noun","definitions":[{"definition":"A unit."}]}],"license":{"name":"CC BY-SA 3.0","url":"https://creativecommons.org/licenses/by-sa/3.0"},"sourceUrls":["https://en.wiktionary.org/wiki/word"]}]"#;
        let (base_url, handle) = serve_once(http_response("200 OK", body), Duration::ZERO);
        let provider =
            FreeDictionaryProvider::new(base_url, Duration::from_secs(1)).expect("provider builds");
        assert!(
            runtime()
                .block_on(provider.lookup("WORD"))
                .expect("success resolves")
                .is_some()
        );
        handle.join().expect("server joins");

        let (base_url, handle) = serve_once(http_response("404 Not Found", b"{}"), Duration::ZERO);
        let provider =
            FreeDictionaryProvider::new(base_url, Duration::from_secs(1)).expect("provider builds");
        assert!(
            runtime()
                .block_on(provider.lookup("WORD"))
                .expect("missing resolves")
                .is_none()
        );
        handle.join().expect("server joins");

        for status in ["429 Too Many Requests", "503 Service Unavailable"] {
            let (base_url, handle) = serve_once(http_response(status, b"{}"), Duration::ZERO);
            let provider = FreeDictionaryProvider::new(base_url, Duration::from_secs(1))
                .expect("provider builds");
            assert!(matches!(
                runtime().block_on(provider.lookup("WORD")),
                Err(DefinitionError::ProviderStatus(429 | 503))
            ));
            handle.join().expect("server joins");
        }

        let oversized = vec![b'a'; MAX_RESPONSE_BYTES + 1];
        let (base_url, handle) = serve_once(http_response("200 OK", &oversized), Duration::ZERO);
        let provider =
            FreeDictionaryProvider::new(base_url, Duration::from_secs(1)).expect("provider builds");
        assert!(matches!(
            runtime().block_on(provider.lookup("WORD")),
            Err(DefinitionError::ResponseTooLarge)
        ));
        handle.join().expect("server joins");

        let (base_url, handle) =
            serve_once(http_response("200 OK", body), Duration::from_millis(100));
        let provider = FreeDictionaryProvider::new(base_url, Duration::from_millis(10))
            .expect("provider builds");
        assert!(matches!(
            runtime().block_on(provider.lookup("WORD")),
            Err(DefinitionError::Http(_))
        ));
        handle.join().expect("server joins");
    }

    #[test]
    fn definition_errors_map_to_specific_unavailable_reasons() {
        let timeout_response = br#"[{"meanings":[]}]"#;
        let (base_url, handle) = serve_once(
            http_response("200 OK", timeout_response),
            Duration::from_millis(100),
        );
        let provider = FreeDictionaryProvider::new(base_url, Duration::from_millis(10))
            .expect("provider builds");
        let timeout = runtime()
            .block_on(provider.lookup("WORD"))
            .expect_err("lookup times out");
        assert_eq!(
            DefinitionUnavailableReason::from(&timeout),
            DefinitionUnavailableReason::TimedOut
        );
        handle.join().expect("server joins");

        let unreachable = reqwest::Client::new().get("http://127.0.0.1:0").send();
        let unreachable = runtime()
            .block_on(unreachable)
            .expect_err("connection fails");
        assert_eq!(
            DefinitionUnavailableReason::from(&DefinitionError::Http(unreachable)),
            DefinitionUnavailableReason::Unreachable
        );

        for (error, expected) in [
            (
                DefinitionError::ProviderStatus(429),
                DefinitionUnavailableReason::RateLimited,
            ),
            (
                DefinitionError::ProviderStatus(503),
                DefinitionUnavailableReason::ProviderUnavailable,
            ),
            (
                DefinitionError::ProviderStatus(403),
                DefinitionUnavailableReason::ProviderRejected,
            ),
            (
                DefinitionError::ResponseTooLarge,
                DefinitionUnavailableReason::ResponseTooLarge,
            ),
            (
                DefinitionError::MissingAttribution,
                DefinitionUnavailableReason::MissingAttribution,
            ),
            (
                DefinitionError::MalformedCache,
                DefinitionUnavailableReason::CacheUnavailable,
            ),
            (
                DefinitionError::ProviderStatus(418),
                DefinitionUnavailableReason::InvalidResponse,
            ),
        ] {
            assert_eq!(DefinitionUnavailableReason::from(&error), expected);
        }
    }

    #[test]
    fn concurrent_cache_misses_are_coalesced() {
        futures_lite::future::block_on(async {
            let db = database().await;
            let provider = StubProvider {
                calls: AtomicUsize::new(0),
                result: Ok(Some(definition())),
            };
            let (first, second) = futures_lite::future::zip(
                lookup_definition(&*db, Some(&provider), "WORD", 10),
                lookup_definition(&*db, Some(&provider), "WORD", 10),
            )
            .await;
            assert!(matches!(first, Ok(DefinitionLookup::Found(_))));
            assert!(matches!(second, Ok(DefinitionLookup::Found(_))));
            assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
        });
    }

    #[test]
    fn cache_hits_expire_and_provider_failures_are_not_cached() {
        futures_lite::future::block_on(async {
            let db = database().await;
            let provider = StubProvider {
                calls: AtomicUsize::new(0),
                result: Ok(Some(definition())),
            };
            assert!(matches!(
                lookup_definition(&*db, Some(&provider), "word", 10).await,
                Ok(DefinitionLookup::Found(_))
            ));
            assert!(matches!(
                lookup_definition(&*db, Some(&provider), "WORD", 11).await,
                Ok(DefinitionLookup::Found(_))
            ));
            assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
            assert!(matches!(
                lookup_definition(&*db, Some(&provider), "WORD", 10 + SUCCESS_TTL_MS).await,
                Ok(DefinitionLookup::Found(_))
            ));
            assert_eq!(provider.calls.load(Ordering::Relaxed), 2);

            let failing = StubProvider {
                calls: AtomicUsize::new(0),
                result: Err(DefinitionError::ProviderStatus(503)),
            };
            assert_eq!(
                lookup_definition(&*db, Some(&failing), "OTHER", 20)
                    .await
                    .expect("failure is recoverable"),
                DefinitionLookup::Unavailable(DefinitionUnavailableReason::ProviderUnavailable)
            );
            assert_eq!(
                lookup_definition(&*db, Some(&failing), "OTHER", 21)
                    .await
                    .expect("failure retries"),
                DefinitionLookup::Unavailable(DefinitionUnavailableReason::ProviderUnavailable)
            );
            assert_eq!(failing.calls.load(Ordering::Relaxed), 2);
        });
    }

    #[test]
    fn disabled_provider_has_an_explicit_lookup_state() {
        futures_lite::future::block_on(async {
            let db = database().await;
            assert_eq!(
                lookup_definition(&*db, None, "WORD", 10)
                    .await
                    .expect("disabled lookup resolves"),
                DefinitionLookup::Unavailable(DefinitionUnavailableReason::Disabled)
            );
        });
    }

    #[test]
    fn unavailable_reasons_have_distinct_actionable_messages() {
        let cases = [
            (
                DefinitionUnavailableReason::Disabled,
                "disabled by the server administrator",
            ),
            (DefinitionUnavailableReason::TimedOut, "timed out"),
            (
                DefinitionUnavailableReason::Unreachable,
                "could not be reached",
            ),
            (DefinitionUnavailableReason::RateLimited, "rate limited"),
            (
                DefinitionUnavailableReason::ProviderUnavailable,
                "temporarily unavailable",
            ),
            (
                DefinitionUnavailableReason::ProviderRejected,
                "configuration was rejected",
            ),
            (
                DefinitionUnavailableReason::InvalidResponse,
                "invalid definition response",
            ),
            (
                DefinitionUnavailableReason::ResponseTooLarge,
                "safety limit",
            ),
            (
                DefinitionUnavailableReason::MissingAttribution,
                "licensing information",
            ),
            (
                DefinitionUnavailableReason::CacheUnavailable,
                "cache could not be accessed",
            ),
        ];
        let mut messages = std::collections::BTreeSet::new();
        let mut log_reasons = std::collections::BTreeSet::new();
        for (reason, expected) in cases {
            assert!(reason.user_message().contains(expected));
            assert!(messages.insert(reason.user_message()));
            assert!(log_reasons.insert(reason.log_reason()));
        }
    }

    #[test]
    fn provider_confirmed_misses_are_negatively_cached() {
        futures_lite::future::block_on(async {
            let db = database().await;
            let provider = StubProvider {
                calls: AtomicUsize::new(0),
                result: Ok(None),
            };
            assert_eq!(
                lookup_definition(&*db, Some(&provider), "ZYZZYVA", 10)
                    .await
                    .expect("miss resolves"),
                DefinitionLookup::Missing
            );
            assert_eq!(
                lookup_definition(&*db, Some(&provider), "ZYZZYVA", 11)
                    .await
                    .expect("cached miss resolves"),
                DefinitionLookup::Missing
            );
            assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
        });
    }

    #[test]
    fn malformed_or_unattributed_provider_payloads_fail_closed() {
        assert!(parse_provider_response("WORD", b"not json").is_err());
        assert!(matches!(
            parse_provider_response(
                "WORD",
                br#"[{"meanings":[{"partOfSpeech":"noun","definitions":[{"definition":"text"}]}],"sourceUrls":["javascript:alert(1)"]}]"#,
            ),
            Err(DefinitionError::MissingAttribution)
        ));
    }

    #[test]
    fn provider_response_is_bounded_and_preserves_attribution() {
        let definition = parse_provider_response(
            "WORD",
            br#"[{"meanings":[{"partOfSpeech":"noun","definitions":[{"definition":"A unit of language."}]}],"license":{"name":"CC BY-SA 3.0","url":"https://creativecommons.org/licenses/by-sa/3.0"},"sourceUrls":["https://en.wiktionary.org/wiki/word"]}]"#,
        )
        .expect("response parses")
        .expect("definition exists");
        assert_eq!(definition.word, "WORD");
        assert_eq!(definition.meanings[0].definitions, ["A unit of language."]);
        assert_eq!(definition.license_name, "CC BY-SA 3.0");
    }
}
