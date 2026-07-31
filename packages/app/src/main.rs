#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use hyperchad::app::AppBuilder;
use words_with_spouses_app::create_router;

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

    let app = {
        use std::sync::Arc;

        use hyperchad::renderer_html_actix::{
            CookieCsrfWebSecurity, CookieCsrfWebSecurityConfig, RejectWebSessionIdentityResolver,
        };

        let dispatcher = words_with_spouses_app::shared_state_dispatcher();
        let web_security = Arc::new(CookieCsrfWebSecurity::new(
            CookieCsrfWebSecurityConfig::new(
                "words-with-spouses-session",
                "words-with-spouses-csrf",
                "x-csrf-token",
            ),
            Arc::new(RejectWebSessionIdentityResolver),
        ));

        let mut app = AppBuilder::new()
            .with_router(create_router())
            .with_title("Words with Spouses".to_string())
            .with_description("Private asynchronous word-tile games".to_string())
            .with_actix_bind_address(address)
            .with_actix_port(port)
            .build_default_html_actix()?;
        app.renderer
            .app
            .set_shared_state_transport_dispatcher(dispatcher, web_security);
        app
    };

    app.run()?;

    Ok(())
}
