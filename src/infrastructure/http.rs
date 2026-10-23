//! Thin `reqwest` wrapper: shared client, browser user agent, timeouts,
//! retry/backoff, SSRF redirect policy and error mapping into the domain's
//! `ProviderError`.
//!
//! Security rules for this file:
//! * Upstream response bodies are **never** embedded in `ProviderError`
//!   reasons (they can carry signed URLs / credentials). They are logged at
//!   `debug` level only.
//! * Transport error strings (which embed the request URL, possibly with a
//!   query string) are redacted through `infrastructure::sanitize`.

use std::time::Duration;

use reqwest::redirect::{Attempt, Policy};
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;

use crate::domain::error::{ProviderError, ProviderErrorKind};
use crate::domain::fetch::url_host_blocked;
use crate::infrastructure::sanitize::redact;

/// Maximum number of body characters embedded in `debug` logs.
const DEBUG_BODY_LIMIT: usize = 2000;
/// Total attempts per request (1 initial + 1 transparent retry).
const MAX_ATTEMPTS: u8 = 2;
/// Never sleep longer than this waiting for a `Retry-After` before retrying.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(5);
/// Base delay before a blind retry (jitter is added on top).
const BASE_RETRY_DELAY: Duration = Duration::from_millis(400);

#[derive(Clone)]
pub struct HttpClient {
    client: Client,
}

impl HttpClient {
    /// `allow_private_network` disables the SSRF redirect guard; see
    /// `[fetch]` in the configuration.
    pub fn new(
        timeout_ms: u64,
        user_agent: String,
        allow_private_network: bool,
    ) -> Result<Self, ProviderError> {
        let redirect_policy = if allow_private_network {
            Policy::default()
        } else {
            Policy::custom(|attempt: Attempt| {
                if url_host_blocked(attempt.url().as_str(), false) {
                    tracing::warn!(url = %attempt.url(), "blocked redirect to private network host");
                    return attempt.error("redirect blocked by fetch URL policy");
                }
                attempt.follow()
            })
        };
        let client = Client::builder()
            .user_agent(user_agent.clone())
            .timeout(Duration::from_millis(timeout_ms.max(1)))
            .redirect(redirect_policy)
            .build()
            .map_err(|error| {
                ProviderError::http("http", format!("failed to build HTTP client: {error}"))
            })?;
        Ok(Self { client })
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Send a request with one transparent retry on 429/5xx, honouring
    /// `Retry-After` up to [`MAX_RETRY_WAIT`].
    async fn send(
        &self,
        request: RequestBuilder,
        provider: &str,
    ) -> Result<reqwest::Response, ProviderError> {
        let mut attempt: u8 = 0;
        loop {
            let response = request
                .try_clone()
                .ok_or_else(|| {
                    ProviderError::http(provider, "request cannot be retried (consumed body)")
                })?
                .send()
                .await
                .map_err(|error| {
                    ProviderError::http(provider, redact(&format!("request failed: {error}")))
                })?;
            let status = response.status();
            let retry_after = parse_retry_after(&response);

            if attempt < MAX_ATTEMPTS - 1 && is_retryable(status) {
                attempt += 1;
                let wait = retry_after
                    .map(|secs| Duration::from_secs(secs).min(MAX_RETRY_WAIT))
                    .unwrap_or(BASE_RETRY_DELAY);
                let jitter = {
                    let nanos = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.subsec_nanos())
                        .unwrap_or(0);
                    Duration::from_millis(u64::from(nanos % 250))
                };
                tracing::warn!(
                    provider,
                    attempt,
                    status = %status,
                    wait_ms = wait.as_millis() as u64,
                    "retryable upstream status; backing off"
                );
                tokio::time::sleep(wait + jitter).await;
                continue;
            }

            return Ok(response);
        }
    }

    /// Send a request and return `(status, body)` without treating non-2xx as an error.
    pub async fn text(
        &self,
        request: RequestBuilder,
        provider: &str,
    ) -> Result<(StatusCode, String), ProviderError> {
        let response = self.send(request, provider).await?;
        let status = response.status();
        let body = response.text().await.map_err(|error| {
            ProviderError::http(provider, format!("failed to read body: {error}"))
        })?;
        Ok((status, body))
    }

    /// Like [`Self::text`], but also returns the `Content-Type` header.
    pub async fn text_with_content_type(
        &self,
        request: RequestBuilder,
        provider: &str,
    ) -> Result<(StatusCode, Option<String>, String), ProviderError> {
        let response = self.send(request, provider).await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = response.text().await.map_err(|error| {
            ProviderError::http(provider, format!("failed to read body: {error}"))
        })?;
        Ok((status, content_type, body))
    }

    /// Send a request and deserialize a JSON body. Non-success responses map
    /// to a structured `ProviderError` **without** echoing the body (which is
    /// logged at `debug` level only).
    pub async fn json<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        provider: &str,
    ) -> Result<T, ProviderError> {
        let response = self.send(request, provider).await?;
        let status = response.status();
        if !status.is_success() {
            let retry_after = parse_retry_after(&response);
            let body = response.text().await.unwrap_or_default();
            tracing::debug!(
                provider,
                %status,
                retry_after_s = retry_after,
                body = %truncate(&body, DEBUG_BODY_LIMIT),
                "upstream failure body (debug only, not returned to client)"
            );
            return Err(status_error(provider, status, retry_after));
        }
        response.json::<T>().await.map_err(|error| {
            ProviderError::decode(provider, format!("invalid JSON response: {error}"))
        })
    }
}

fn is_retryable(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

/// Map a failure status to a structured error: 429/401/403 => Blocked,
/// everything else => Http. The body is never included.
pub fn status_error(
    provider: &str,
    status: StatusCode,
    retry_after_s: Option<u64>,
) -> ProviderError {
    let kind = if matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS | StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        ProviderErrorKind::Blocked
    } else {
        ProviderErrorKind::Http
    };
    let reason = match status {
        StatusCode::TOO_MANY_REQUESTS => "rate limited upstream; retry after the shown delay",
        StatusCode::UNAUTHORIZED => "upstream rejected credentials",
        StatusCode::FORBIDDEN => "upstream denied access (bot protection or keyless limit)",
        StatusCode::NOT_FOUND => "URL not found upstream",
        _ if status.is_server_error() => "upstream server error",
        _ => "upstream returned an error status",
    };
    ProviderError::with_status(
        kind,
        provider,
        reason,
        Some(status.as_u16()),
        retry_after_s,
    )
}

/// Parse the `Retry-After` header (delta-seconds form) from a response.
fn parse_retry_after(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// Truncate a string to at most `max` characters (never splits a UTF-8 char).
pub fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut truncated: String = value.chars().take(max).collect();
    truncated.push('…');
    truncated
}
