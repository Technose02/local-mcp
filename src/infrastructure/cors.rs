//! CORS + Private Network Access middleware for browser-based MCP clients.
//!
//! A browser page served over HTTPS that talks to this local HTTP server performs
//! a cross-origin `POST`. That triggers a CORS preflight (`OPTIONS`), and when the
//! page is on a public origin while the server is on a private address
//! (`127.0.0.1`), Chrome additionally sends a *Private Network Access* preflight.
//! Without the corresponding response headers the browser aborts the request with
//! a generic `TypeError: NetworkError`.
//!
//! Implemented by hand (no extra dependency) so the exact headers can be tailored.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

/// Headers allowed on preflight when the browser does not state its own list.
const DEFAULT_ALLOWED_HEADERS: &str = "content-type, accept, authorization, \
    mcp-protocol-version, mcp-session-id, mcp-method, mcp-name, last-event-id, x-requested-with";

const ALLOWED_METHODS: &str = "GET, POST, DELETE, OPTIONS";

/// Headers a browser client is allowed to read from the response.
const EXPOSED_HEADERS: &str = "mcp-session-id, mcp-protocol-version";

/// How long a browser may cache the preflight result (seconds).
const MAX_AGE: &str = "86400";

const ALLOW_PRIVATE_NETWORK: HeaderName =
    HeaderName::from_static("access-control-allow-private-network");

/// Which origins may call the server from a browser.
#[derive(Debug, Clone)]
pub struct CorsConfig {
    allowed_origins: Vec<String>,
}

impl CorsConfig {
    pub fn new(allowed_origins: Vec<String>) -> Self {
        Self {
            allowed_origins: allowed_origins
                .into_iter()
                .map(|origin| origin.trim().to_string())
                .collect(),
        }
    }

    /// CORS is only active when at least one origin is configured.
    pub fn is_enabled(&self) -> bool {
        !self.allowed_origins.is_empty()
    }

    fn allows_any(&self) -> bool {
        self.allowed_origins.iter().any(|origin| origin == "*")
    }

    fn origin_allowed(&self, origin: &str) -> bool {
        self.allows_any() || self.allowed_origins.iter().any(|allowed| allowed == origin)
    }
}

/// The value to echo in `Access-Control-Allow-Origin`, if the origin is allowed.
fn allow_origin_value(config: &CorsConfig, origin: &HeaderValue) -> Option<HeaderValue> {
    let origin = origin.to_str().ok()?;
    if !config.origin_allowed(origin) {
        return None;
    }
    Some(if config.allows_any() {
        HeaderValue::from_static("*")
    } else {
        origin.to_owned().parse().ok()?
    })
}

fn apply_cors_headers(
    headers: &mut HeaderMap,
    config: &CorsConfig,
    request_origin: Option<&HeaderValue>,
    requested_headers: Option<&HeaderValue>,
    private_network: bool,
) {
    if !config.is_enabled() {
        return;
    }

    headers.append(header::VARY, HeaderValue::from_static("Origin"));
    headers.append(
        header::VARY,
        HeaderValue::from_static("Access-Control-Request-Private-Network"),
    );

    let Some(allow_origin) = request_origin.and_then(|origin| allow_origin_value(config, origin))
    else {
        return;
    };

    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, allow_origin);
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(ALLOWED_METHODS),
    );
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static(EXPOSED_HEADERS),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static(MAX_AGE),
    );

    let allow_headers = requested_headers
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .and_then(|value| HeaderValue::from_str(value).ok())
        .unwrap_or_else(|| HeaderValue::from_static(DEFAULT_ALLOWED_HEADERS));
    headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, allow_headers);

    if private_network {
        headers.insert(ALLOW_PRIVATE_NETWORK, HeaderValue::from_static("true"));
    }
}

/// Axum middleware: answers preflights and decorates responses with CORS headers.
pub async fn cors_middleware(
    State(config): State<Arc<CorsConfig>>,
    request: Request,
    next: Next,
) -> Response {
    if !config.is_enabled() {
        return next.run(request).await;
    }

    let request_origin = request.headers().get(header::ORIGIN).cloned();
    let requested_headers = request
        .headers()
        .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
        .cloned();
    let private_network = request
        .headers()
        .get("access-control-request-private-network")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let is_preflight = request.method() == Method::OPTIONS
        && request
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);

    if is_preflight {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NO_CONTENT;
        apply_cors_headers(
            response.headers_mut(),
            &config,
            request_origin.as_ref(),
            requested_headers.as_ref(),
            private_network,
        );
        return response;
    }

    let mut response = next.run(request).await;
    apply_cors_headers(
        response.headers_mut(),
        &config,
        request_origin.as_ref(),
        None,
        false,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(origins: &[&str]) -> CorsConfig {
        CorsConfig::new(origins.iter().map(|origin| origin.to_string()).collect())
    }

    #[test]
    fn wildcard_allows_any_origin_and_private_network() {
        let mut headers = HeaderMap::new();
        apply_cors_headers(
            &mut headers,
            &config(&["*"]),
            Some(&HeaderValue::from_static("https://app.example")),
            Some(&HeaderValue::from_static("content-type, mcp-method")),
            true,
        );
        assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
        assert_eq!(
            headers[header::ACCESS_CONTROL_ALLOW_HEADERS],
            "content-type, mcp-method"
        );
        assert_eq!(headers[ALLOW_PRIVATE_NETWORK], "true");
    }

    #[test]
    fn specific_origin_is_echoed() {
        let mut headers = HeaderMap::new();
        apply_cors_headers(
            &mut headers,
            &config(&["https://allowed.example"]),
            Some(&HeaderValue::from_static("https://allowed.example")),
            None,
            false,
        );
        assert_eq!(
            headers[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "https://allowed.example"
        );
        assert!(!headers.contains_key(ALLOW_PRIVATE_NETWORK));
    }

    #[test]
    fn disallowed_origin_gets_no_allow_origin() {
        let mut headers = HeaderMap::new();
        apply_cors_headers(
            &mut headers,
            &config(&["https://allowed.example"]),
            Some(&HeaderValue::from_static("https://evil.example")),
            None,
            false,
        );
        assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
    }

    #[test]
    fn empty_config_is_disabled() {
        let mut headers = HeaderMap::new();
        apply_cors_headers(
            &mut headers,
            &config(&[]),
            Some(&HeaderValue::from_static("https://app.example")),
            None,
            true,
        );
        assert!(headers.is_empty());
    }

    #[test]
    fn missing_origin_gets_no_headers() {
        let mut headers = HeaderMap::new();
        apply_cors_headers(&mut headers, &config(&["*"]), None, None, false);
        assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
    }
}
