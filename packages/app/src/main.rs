#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

mod durable_web_session;

use hyperchad::app::AppBuilder;
use hyperchad::renderer::assets::{AssetPathTarget, StaticAssetRoute};
use wwmtf_app::{CSRF_COOKIE_NAME, CSRF_HEADER_NAME, SESSION_COOKIE_NAME, create_product_router};

struct RuntimeConfig {
    address: String,
    port: u16,
    database_path: String,
    public_base_url: String,
    google_client_id: String,
    google_client_secret: String,
    google_callback_url: String,
    production_mode: bool,
    development_mode: bool,
}

fn google_runtime_config(
    client_id: Option<String>,
    client_secret: Option<String>,
    public_base_url: &str,
    production_mode: bool,
) -> Result<(String, String, String), Box<dyn std::error::Error>> {
    let (Some(client_id), Some(client_secret)) = (client_id, client_secret) else {
        return Err("WWMTF_GOOGLE_CLIENT_ID and WWMTF_GOOGLE_CLIENT_SECRET are required".into());
    };
    if client_id.trim().is_empty() || client_secret.trim().is_empty() {
        return Err(
            "WWMTF_GOOGLE_CLIENT_ID and WWMTF_GOOGLE_CLIENT_SECRET must not be empty".into(),
        );
    }
    let base_url = reqwest::Url::parse(public_base_url)
        .map_err(|_| "WWMTF_PUBLIC_BASE_URL must be an absolute HTTP(S) origin")?;
    if !matches!(base_url.scheme(), "http" | "https")
        || base_url.host_str().is_none()
        || !base_url.username().is_empty()
        || base_url.password().is_some()
        || base_url.query().is_some()
        || base_url.fragment().is_some()
        || base_url.path() != "/"
    {
        return Err("WWMTF_PUBLIC_BASE_URL must be an absolute HTTP(S) origin".into());
    }
    if production_mode && base_url.scheme() != "https" {
        return Err("WWMTF_PUBLIC_BASE_URL must use https in production mode".into());
    }
    let callback = base_url
        .join("auth/google/callback")
        .map_err(|_| "Google callback URL could not be constructed")?;
    Ok((client_id, client_secret, callback.to_string()))
}

fn definitions_enabled(value: Option<&str>) -> Result<bool, &'static str> {
    let Some(value) = value else {
        return Ok(true);
    };
    if value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes") {
        Ok(true)
    } else if value == "0"
        || value.eq_ignore_ascii_case("false")
        || value.eq_ignore_ascii_case("no")
    {
        Ok(false)
    } else {
        Err("WWMTF_DEFINITIONS_ENABLED must be true/false, yes/no, or 1/0 when specified")
    }
}

impl RuntimeConfig {
    fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let production_mode = std::env::var("WWMTF_PRODUCTION_MODE")
            .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"));
        let development_mode = std::env::var("WWMTF_DEV_MODE")
            .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"));
        let address = std::env::var("WWMTF_BIND_ADDRESS").unwrap_or_else(|_| "0.0.0.0".to_string());
        let port = std::env::var("WWMTF_PORT")
            .ok()
            .map(|value| value.parse::<u16>())
            .transpose()?
            .unwrap_or(8343);
        let public_base_url = std::env::var("WWMTF_PUBLIC_BASE_URL")
            .unwrap_or_else(|_| format!("http://{address}:{port}"));
        if production_mode && cfg!(feature = "insecure") {
            return Err("the insecure feature may not run in production mode".into());
        }
        if production_mode && development_mode {
            return Err("production mode and development mode are mutually exclusive".into());
        }
        if development_mode && !cfg!(feature = "insecure") {
            return Err("WWMTF_DEV_MODE requires building with --features insecure".into());
        }
        if !development_mode && cfg!(feature = "insecure") {
            return Err("the insecure feature may only run with WWMTF_DEV_MODE=true".into());
        }
        if production_mode {
            if std::env::var("WWMTF_PUBLIC_BASE_URL").is_err() {
                return Err("WWMTF_PUBLIC_BASE_URL is required in production mode".into());
            }
            if !public_base_url.starts_with("https://") {
                return Err("WWMTF_PUBLIC_BASE_URL must use https in production mode".into());
            }
        }
        let database_path = match std::env::var("WWMTF_DATABASE_PATH") {
            Ok(path) if !path.trim().is_empty() => path,
            Ok(_) => return Err("WWMTF_DATABASE_PATH must not be empty".into()),
            Err(_) if !production_mode => "wwmtf.db".to_string(),
            Err(_) => {
                return Err("WWMTF_DATABASE_PATH is required in production mode".into());
            }
        };
        let (google_client_id, google_client_secret, google_callback_url) = google_runtime_config(
            std::env::var("WWMTF_GOOGLE_CLIENT_ID").ok(),
            std::env::var("WWMTF_GOOGLE_CLIENT_SECRET").ok(),
            &public_base_url,
            production_mode,
        )?;
        Ok(Self {
            address,
            port,
            database_path,
            public_base_url,
            google_client_id,
            google_client_secret,
            google_callback_url,
            production_mode,
            development_mode,
        })
    }
}

fn bootstrap_legacy_account(
    database: &dyn switchy_database::Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let username = std::env::var("WWMTF_DEV_BOOTSTRAP_USERNAME").ok();
    let password = std::env::var("WWMTF_DEV_BOOTSTRAP_PASSWORD").ok();
    let (Some(username), Some(password)) = (username, password) else {
        if std::env::var("WWMTF_DEV_BOOTSTRAP_USERNAME").is_ok()
            || std::env::var("WWMTF_DEV_BOOTSTRAP_PASSWORD").is_ok()
        {
            return Err(
                "development bootstrap username and password must be supplied together".into(),
            );
        }
        return Ok(());
    };
    if !std::env::var("WWMTF_DEV_MODE")
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
    {
        return Err("development account bootstrap is allowed only in development mode".into());
    }
    match futures_lite::future::block_on(wwmtf_app::register(
        database,
        &username,
        &password,
        time::OffsetDateTime::now_utc(),
    )) {
        Ok(_) | Err(wwmtf_app::AccountError::UsernameTaken) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let config = RuntimeConfig::from_env()?;
    let release = option_env!("WWMTF_RELEASE").unwrap_or("development");
    let machine = std::env::var("FLY_MACHINE_ID").unwrap_or_else(|_| "local".to_string());
    let secure_cookies = !config.development_mode;
    log::info!(
        "starting Words with More Than Friends on {}:{} release={} machine={}",
        config.address,
        config.port,
        release,
        machine
    );
    if !config.development_mode && !config.public_base_url.starts_with("https://") {
        log::warn!("public base URL is not HTTPS; intended only for local development");
    }
    if config.production_mode {
        log::info!("production mode enabled");
    }
    if config.development_mode {
        log::warn!("development mode enabled: HTTP and non-secure cookies are allowed");
    } else if config.public_base_url.starts_with("https://") {
        log::info!("public HTTPS origin configured");
    }

    let database_path = config.database_path;
    let open_database = || {
        futures_lite::future::block_on(
            switchy_database_connection::builder()
                .turso()
                .with_path(&database_path)
                .with_busy_timeout(std::time::Duration::from_secs(5))
                .build(),
        )
        .map(std::sync::Arc::<dyn switchy_database::Database>::from)
    };
    let database = open_database()?;
    futures_lite::future::block_on(wwmtf_app::migrate_app(&*database))?;
    bootstrap_legacy_account(&*database)?;

    let dispatcher =
        std::sync::Arc::new(wwmtf_app::GameSharedStateDispatcher::new(database.clone()));
    let oidc_runtime = std::sync::Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?,
    );
    let app = {
        use std::sync::Arc;

        use hyperchad::renderer_html::actix::HtmlActixRuntime as _;
        use hyperchad::renderer_html_actix::{CookieCsrfWebSecurity, CookieCsrfWebSecurityConfig};

        let csrf_token = if config.development_mode {
            "wwmtf-development-csrf".to_string()
        } else {
            format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4())
        };
        let web_security = Arc::new(CookieCsrfWebSecurity::new(
            CookieCsrfWebSecurityConfig::new(
                SESSION_COOKIE_NAME,
                CSRF_COOKIE_NAME,
                CSRF_HEADER_NAME,
            ),
            Arc::new(durable_web_session::DurableWebSessionIdentityResolver::new(
                database.clone(),
            )),
        ));

        let definitions_setting = std::env::var("WWMTF_DEFINITIONS_ENABLED").ok();
        let definition_provider: Option<Arc<dyn wwmtf_app::DefinitionProvider>> =
            if definitions_enabled(definitions_setting.as_deref())? {
                let base_url =
                    std::env::var("WWMTF_DEFINITION_PROVIDER_BASE_URL").unwrap_or_else(|_| {
                        wwmtf_app::DEFAULT_DEFINITION_PROVIDER_BASE_URL.to_string()
                    });
                let timeout_ms = std::env::var("WWMTF_DEFINITION_TIMEOUT_MS")
                    .ok()
                    .map(|value| value.parse::<u64>())
                    .transpose()?
                    .unwrap_or(3_000);
                Some(Arc::new(wwmtf_app::FreeDictionaryProvider::new(
                    base_url,
                    std::time::Duration::from_millis(timeout_ms),
                )?))
            } else {
                None
            };

        let google_oidc = {
            let google_issuer = std::env::var("WWMTF_DEVELOPMENT_OIDC_ISSUER").ok();
            if google_issuer.is_some() && !config.development_mode {
                return Err(
                    "WWMTF_DEVELOPMENT_OIDC_ISSUER is only allowed in development mode".into(),
                );
            }
            Some(Arc::new(oidc_runtime.block_on(async {
                if let Some(issuer) = google_issuer {
                    wwmtf_app::GoogleOidcClient::discover_issuer_with_runtime(
                        &config.google_client_id,
                        &config.google_client_secret,
                        &config.google_callback_url,
                        &issuer,
                        Some(oidc_runtime.clone()),
                    )
                    .await
                } else {
                    wwmtf_app::GoogleOidcClient::discover_issuer_with_runtime(
                        &config.google_client_id,
                        &config.google_client_secret,
                        &config.google_callback_url,
                        wwmtf_app::GOOGLE_ISSUER,
                        Some(oidc_runtime.clone()),
                    )
                    .await
                }
            })?))
        };

        let mut app_builder = AppBuilder::new()
            .with_router(create_product_router(
                database.clone(),
                dispatcher.clone(),
                definition_provider,
                google_oidc,
                csrf_token.clone(),
                config.public_base_url,
                secure_cookies,
            ))
            .with_title("Words with More Than Friends".to_string())
            .with_description("Private asynchronous word-tile games".to_string())
            .with_viewport("width=device-width, initial-scale=1".to_string())
            .with_actix_bind_address(config.address)
            .with_actix_port(config.port);
        app_builder.static_asset_route_result(StaticAssetRoute {
            route: format!(
                "js/{}",
                hyperchad::renderer_vanilla_js::SCRIPT_NAME_HASHED.as_str()
            ),
            target: AssetPathTarget::FileContents(
                hyperchad::renderer_vanilla_js::SCRIPT.as_bytes().into(),
            ),
            not_found_behavior: None,
        })?;
        let mut app = app_builder.build_default_html_vanilla_js_actix()?;
        app.renderer
            .app
            .set_shared_state_csrf_token(csrf_token.clone());
        app.renderer.app.set_html_csrf_token(csrf_token);
        app.renderer
            .app
            .set_html_csrf_cookie(CSRF_COOKIE_NAME, secure_cookies);
        app.renderer
            .app
            .set_shared_state_transport_dispatcher(dispatcher, web_security);
        app
    };

    app.run()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{definitions_enabled, google_runtime_config};

    #[test]
    fn google_configuration_is_complete_bounded_and_origin_based() {
        let (client_id, client_secret, callback) = google_runtime_config(
            Some("client".to_string()),
            Some("secret".to_string()),
            "https://games.example.test",
            true,
        )
        .expect("valid production Google configuration parses");
        assert_eq!(client_id, "client");
        assert_eq!(client_secret, "secret");
        assert_eq!(callback, "https://games.example.test/auth/google/callback");

        for (client_id, client_secret) in [
            (None, None),
            (Some("client".to_string()), None),
            (None, Some("secret".to_string())),
            (Some(String::new()), Some("secret".to_string())),
            (Some("client".to_string()), Some("  ".to_string())),
        ] {
            assert!(
                google_runtime_config(client_id, client_secret, "https://games.example.test", true)
                    .is_err()
            );
        }
        for invalid_origin in [
            "not-a-url",
            "ftp://games.example.test",
            "https://user@games.example.test",
            "https://games.example.test/path",
            "https://games.example.test?query=1",
            "https://games.example.test#fragment",
        ] {
            assert!(
                google_runtime_config(
                    Some("client".to_string()),
                    Some("secret".to_string()),
                    invalid_origin,
                    true
                )
                .is_err(),
                "{invalid_origin}"
            );
        }
        assert!(
            google_runtime_config(
                Some("client".to_string()),
                Some("secret".to_string()),
                "http://games.example.test",
                true
            )
            .is_err()
        );
        assert!(
            google_runtime_config(
                Some("client".to_string()),
                Some("secret".to_string()),
                "http://127.0.0.1:8343",
                false
            )
            .is_ok()
        );
    }

    #[test]
    fn definitions_are_enabled_by_default_and_accept_explicit_boolean_values() {
        assert_eq!(definitions_enabled(None), Ok(true));
        assert_eq!(definitions_enabled(Some("TrUe")), Ok(true));
        assert_eq!(definitions_enabled(Some("YES")), Ok(true));
        assert_eq!(definitions_enabled(Some("0")), Ok(false));
        assert_eq!(definitions_enabled(Some("FALSE")), Ok(false));
        assert!(definitions_enabled(Some("sometimes")).is_err());
    }
}
