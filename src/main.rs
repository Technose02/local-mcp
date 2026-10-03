//! Composition root: load configuration, wire providers and services, then serve
//! the MCP Streamable HTTP endpoint.

mod application;
mod config;
mod domain;
mod error;
mod infrastructure;

use std::sync::Arc;

use clap::Parser;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::application::server::LocalToolsServer;
use crate::config::{Cli, ServerSettings, load_config};
use crate::domain::clock::TimeService;
use crate::domain::explore::ExploreService;
use crate::domain::fetch::{FetchService, LinkSource, SiteMapSource};
use crate::domain::search::WebSearchService;
use crate::error::AppError;
use crate::infrastructure::cors::{CorsConfig, cors_middleware};
use crate::infrastructure::http::HttpClient;
use crate::infrastructure::providers::{
    DirectFetcher, build_extract_providers, build_search_providers,
};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("fatal: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), AppError> {
    let cli = Cli::parse();
    if cli.print_example_config {
        print!("{}", crate::config::EXAMPLE_CONFIG);
        return Ok(());
    }

    init_tracing(&cli)?;
    let config = load_config(&cli)?;
    tracing::debug!(?config, "configuration loaded");

    let http = HttpClient::new(config.http.timeout_ms, config.http.user_agent.clone())?;

    let direct = Arc::new(DirectFetcher::new(http.clone()));

    let search_providers = build_search_providers(&config, &http);
    if search_providers.is_empty() {
        tracing::warn!("no search provider enabled: web_search and explore_site will fail");
    } else {
        tracing::info!(
            providers = ?search_providers.iter().map(|provider| provider.name()).collect::<Vec<_>>(),
            "search provider chain ready"
        );
    }

    let extract_providers = build_extract_providers(&config, &http, &direct);
    tracing::info!(
        providers = ?extract_providers.iter().map(|provider| provider.name()).collect::<Vec<_>>(),
        "content extraction chain ready"
    );

    let search_service = Arc::new(WebSearchService::new(search_providers));
    let links: Arc<dyn LinkSource> = direct.clone();
    let sitemap: Arc<dyn SiteMapSource> = direct.clone();
    let fetch_service = Arc::new(FetchService::new(extract_providers, links.clone()));
    let explore_service = Arc::new(ExploreService::new(search_service.clone(), links, sitemap));
    let time_service = Arc::new(TimeService::new(config.time.default_timezone.clone()));

    let disabled_tools = config.tools.disabled.clone();
    let factory = {
        let search = search_service.clone();
        let fetch = fetch_service.clone();
        let explore = explore_service.clone();
        let time = time_service.clone();
        let disabled = disabled_tools.clone();
        move || {
            Ok(LocalToolsServer::new(
                search.clone(),
                fetch.clone(),
                explore.clone(),
                time.clone(),
                disabled.clone(),
            ))
        }
    };

    let cancellation = CancellationToken::new();
    let service = StreamableHttpService::new(
        factory,
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_json_response(config.server.json_response)
            .with_legacy_session_mode(config.server.legacy_session_mode)
            .with_cancellation_token(cancellation.child_token()),
    );

    let router = axum::Router::new()
        .nest_service(config.server.path.as_str(), service)
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(CorsConfig::new(config.server.cors_origins.clone())),
            cors_middleware,
        ));

    if config.server.tls.enabled {
        serve_tls(router, &config.server, cancellation).await
    } else {
        serve_http(router, &config.server, cancellation).await
    }
}

/// Serve plain HTTP (the default, and the right choice when TLS terminates
/// elsewhere, e.g. a reverse proxy or the Docker host).
async fn serve_http(
    router: axum::Router,
    server: &ServerSettings,
    cancellation: CancellationToken,
) -> Result<(), AppError> {
    let listener = tokio::net::TcpListener::bind(&server.bind).await?;
    tracing::info!(
        bind = %server.bind,
        path = %server.path,
        scheme = "http",
        "MCP server listening over Streamable HTTP"
    );

    let shutdown = cancellation.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            wait_for_shutdown().await;
            shutdown.cancel();
        })
        .await?;
    Ok(())
}

/// Serve HTTPS directly. Requires a build with the `tls` Cargo feature and
/// `[server.tls] enabled = true` with a PEM certificate/key.
#[cfg(feature = "tls")]
async fn serve_tls(
    router: axum::Router,
    server: &ServerSettings,
    cancellation: CancellationToken,
) -> Result<(), AppError> {
    use std::time::Duration;

    use axum_server::tls_rustls::RustlsConfig;

    let cert_path = server.tls.cert_path.as_ref().ok_or_else(|| {
        AppError::Server("[server.tls] enabled = true but cert_path is missing".to_string())
    })?;
    let key_path = server.tls.key_path.as_ref().ok_or_else(|| {
        AppError::Server("[server.tls] enabled = true but key_path is missing".to_string())
    })?;
    let tls = RustlsConfig::from_pem_file(cert_path, key_path)
        .await
        .map_err(|error| {
            AppError::Server(format!("failed to load TLS certificate/key: {error}"))
        })?;
    let address = server
        .bind
        .parse::<std::net::SocketAddr>()
        .map_err(|error| {
            AppError::Server(format!("invalid bind address '{}': {error}", server.bind))
        })?;

    // Bind eagerly so port conflicts surface immediately and the log line is accurate.
    // The socket must be non-blocking before tokio takes it over.
    let listener = std::net::TcpListener::bind(address)
        .map_err(|error| AppError::Server(format!("failed to bind '{}': {error}", server.bind)))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| AppError::Server(format!("failed to configure listener: {error}")))?;

    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    let shutdown_token = cancellation.clone();
    tokio::spawn(async move {
        wait_for_shutdown().await;
        shutdown_token.cancel();
        shutdown_handle.graceful_shutdown(Some(Duration::from_secs(5)));
    });

    tracing::info!(
        bind = %server.bind,
        path = %server.path,
        scheme = "https",
        "MCP server listening over Streamable HTTP"
    );
    axum_server::from_tcp_rustls(listener, tls)
        .map_err(|error| AppError::Server(format!("HTTPS server error: {error}")))?
        .handle(handle)
        .serve(router.into_make_service())
        .await
        .map_err(|error| AppError::Server(format!("HTTPS server error: {error}")))?;
    Ok(())
}

#[cfg(not(feature = "tls"))]
async fn serve_tls(
    _router: axum::Router,
    server: &ServerSettings,
    _cancellation: CancellationToken,
) -> Result<(), AppError> {
    Err(AppError::Server(format!(
        "[server.tls] enabled = true for '{}', but this binary was built without the `tls` feature; \
         rebuild with `cargo build --features tls` or set [server.tls] enabled = false",
        server.bind
    )))
}

async fn wait_for_shutdown() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(error = %error, "failed to listen for shutdown signal");
    }
    tracing::info!("shutdown requested");
}

fn init_tracing(cli: &Cli) -> Result<(), AppError> {
    let default_level = cli.log_level.clone().unwrap_or_else(|| "info".to_string());
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .try_init()
        .map_err(|error| AppError::Server(format!("failed to initialize tracing: {error}")))
}
