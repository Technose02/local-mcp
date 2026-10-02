//! Thin `reqwest` wrapper: shared client, browser user agent, timeouts and
//! error mapping into the domain's `ProviderError`.

use std::time::Duration;

use reqwest::{Client, RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;

use crate::domain::error::ProviderError;

/// Maximum number of body characters embedded in an error message.
const ERROR_BODY_LIMIT: usize = 200;

#[derive(Clone)]
pub struct HttpClient {
    client: Client,
}

impl HttpClient {
    pub fn new(timeout_ms: u64, user_agent: String) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .user_agent(user_agent.clone())
            .timeout(Duration::from_millis(timeout_ms.max(1)))
            .build()
            .map_err(|error| {
                ProviderError::http("http", format!("failed to build HTTP client: {error}"))
            })?;
        Ok(Self { client })
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Send a request and return `(status, body)` without treating non-2xx as an error.
    pub async fn text(
        &self,
        request: RequestBuilder,
        provider: &str,
    ) -> Result<(StatusCode, String), ProviderError> {
        let response = request
            .send()
            .await
            .map_err(|error| ProviderError::http(provider, format!("request failed: {error}")))?;
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
        let response = request
            .send()
            .await
            .map_err(|error| ProviderError::http(provider, format!("request failed: {error}")))?;
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

    /// Send a request and deserialize a JSON body, mapping HTTP/JSON failures.
    pub async fn json<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        provider: &str,
    ) -> Result<T, ProviderError> {
        let response = request
            .send()
            .await
            .map_err(|error| ProviderError::http(provider, format!("request failed: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::http(
                provider,
                format!("HTTP {status}: {}", truncate(&body, ERROR_BODY_LIMIT)),
            ));
        }
        response.json::<T>().await.map_err(|error| {
            ProviderError::decode(provider, format!("invalid JSON response: {error}"))
        })
    }
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
