#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

mod durable_web_session;

use hyperchad::app::AppBuilder;
use words_with_spouses_app::{
    CSRF_COOKIE_NAME, CSRF_HEADER_NAME, SESSION_COOKIE_NAME, create_product_router,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let address = std::env::var("WORDS_WITH_SPOUSES_BIND_ADDRESS")
        .unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("WORDS_WITH_SPOUSES_PORT")
        .ok()
        .map(|value| value.parse::<u16>())
        .transpose()?
        .unwrap_or(8343);

    log::info!("starting Words with Spouses on {address}:{port}");

    if let Ok(public_base_url) = std::env::var("WORDS_WITH_SPOUSES_PUBLIC_BASE_URL") {
        if !public_base_url.starts_with("https://") {
            return Err("WORDS_WITH_SPOUSES_PUBLIC_BASE_URL must use https in deployment".into());
        }
        log::info!("public HTTPS origin configured");
    }

    let database_path = std::env::var("WORDS_WITH_SPOUSES_DATABASE_PATH")
        .unwrap_or_else(|_| "words-with-spouses.db".to_string());
    let database: std::sync::Arc<dyn switchy_database::Database> =
        std::sync::Arc::from(futures_lite::future::block_on(
            switchy_database_connection::builder()
                .turso()
                .with_path(database_path)
                .with_busy_timeout(std::time::Duration::from_secs(5))
                .build(),
        )?);
    futures_lite::future::block_on(words_with_spouses_app::migrate_app(&*database))?;

    let app = {
        use std::sync::Arc;

        use hyperchad::renderer_html_actix::{CookieCsrfWebSecurity, CookieCsrfWebSecurityConfig};

        let dispatcher = words_with_spouses_app::shared_state_dispatcher(database.clone());
        let csrf_token = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
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

        let mut app = AppBuilder::new()
            .with_router(create_product_router(database.clone(), csrf_token.clone()))
            .with_title("Words with Spouses".to_string())
            .with_description("Private asynchronous word-tile games".to_string())
            .with_actix_bind_address(address)
            .with_actix_port(port)
            .build_default_html_actix()?;
        app.renderer.app.set_shared_state_csrf_token(csrf_token);
        app.renderer
            .app
            .set_shared_state_transport_dispatcher(dispatcher, web_security);
        app
    };

    app.run()?;

    Ok(())
}
