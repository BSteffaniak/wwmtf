#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

mod durable_web_session;

use hyperchad::app::AppBuilder;
use hyperchad::renderer::assets::{AssetPathTarget, StaticAssetRoute};
use words_with_spouses_app::{
    CSRF_COOKIE_NAME, CSRF_HEADER_NAME, SESSION_COOKIE_NAME, create_product_router,
};

struct RuntimeConfig {
    address: String,
    port: u16,
    database_path: String,
    public_base_url: String,
    development_mode: bool,
}

impl RuntimeConfig {
    fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let development_mode = std::env::var("WORDS_WITH_SPOUSES_DEV_MODE")
            .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"));
        let address = std::env::var("WORDS_WITH_SPOUSES_BIND_ADDRESS")
            .unwrap_or_else(|_| "0.0.0.0".to_string());
        let port = std::env::var("WORDS_WITH_SPOUSES_PORT")
            .ok()
            .map(|value| value.parse::<u16>())
            .transpose()?
            .unwrap_or(8343);
        let public_base_url =
            std::env::var("WORDS_WITH_SPOUSES_PUBLIC_BASE_URL").unwrap_or_else(|_| {
                let host = if address == "0.0.0.0" {
                    "127.0.0.1"
                } else {
                    address.as_str()
                };
                format!("http://{host}:{port}")
            });
        if !development_mode
            && std::env::var("WORDS_WITH_SPOUSES_PUBLIC_BASE_URL").is_ok()
            && !public_base_url.starts_with("https://")
        {
            return Err(
                "WORDS_WITH_SPOUSES_PUBLIC_BASE_URL must use https outside development mode".into(),
            );
        }
        Ok(Self {
            address,
            port,
            database_path: std::env::var("WORDS_WITH_SPOUSES_DATABASE_PATH")
                .unwrap_or_else(|_| "words-with-spouses.db".to_string()),
            public_base_url,
            development_mode,
        })
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let config = RuntimeConfig::from_env()?;
    let secure_cookies = !config.development_mode;
    log::info!(
        "starting Words with Spouses on {}:{}",
        config.address,
        config.port
    );
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
    futures_lite::future::block_on(words_with_spouses_app::migrate_app(&*database))?;

    let dispatcher = std::sync::Arc::new(words_with_spouses_app::GameSharedStateDispatcher::new(
        database.clone(),
    ));
    let app = {
        use std::sync::Arc;

        use hyperchad::renderer_html::actix::HtmlActixRuntime as _;
        use hyperchad::renderer_html_actix::{CookieCsrfWebSecurity, CookieCsrfWebSecurityConfig};

        let csrf_token = if config.development_mode {
            "words-with-spouses-development-csrf".to_string()
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

        let mut app_builder = AppBuilder::new()
            .with_router(create_product_router(
                database.clone(),
                dispatcher.clone(),
                csrf_token.clone(),
                config.public_base_url,
                secure_cookies,
            ))
            .with_title("Words with Spouses".to_string())
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
        if secure_cookies {
            app.renderer.app.set_html_csrf_cookie_name(CSRF_COOKIE_NAME);
        }
        app.renderer
            .app
            .set_shared_state_transport_dispatcher(dispatcher, web_security);
        app
    };

    app.run()?;

    Ok(())
}
