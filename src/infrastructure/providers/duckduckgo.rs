//! DuckDuckGo HTML adapter (keyless).
//!
//! DuckDuckGo answers `GET` requests from non-browser clients with an HTTP 202
//! bot-detection page. Sending a `POST` form request with a browser `User-Agent`
//! and a `Referer` header returns real results instead.

use reqwest::StatusCode;

use crate::domain::error::{ProviderError, ProviderFuture};
use crate::domain::search::{SearchOutcome, SearchProvider, SearchQuery};
use crate::infrastructure::html::parse_ddg_results;
use crate::infrastructure::http::HttpClient;

const HTML_ENDPOINT: &str = "https://html.duckduckgo.com/html/";

pub struct DuckDuckGoSearchProvider {
    http: HttpClient,
}

impl DuckDuckGoSearchProvider {
    pub fn new(http: HttpClient) -> Self {
        Self { http }
    }
}

impl SearchProvider for DuckDuckGoSearchProvider {
    fn name(&self) -> &'static str {
        "duckduckgo"
    }

    fn search<'a>(
        &'a self,
        query: &'a SearchQuery,
    ) -> ProviderFuture<'a, Result<SearchOutcome, ProviderError>> {
        Box::pin(async move {
            let mut text = query.text.clone();
            if let Some(site) = &query.site {
                text = format!("site:{site} {text}");
            }
            let language = query
                .language
                .clone()
                .unwrap_or_else(|| "en-US,en;q=0.9".to_string());

            let request = self
                .http
                .client()
                .post(HTML_ENDPOINT)
                .header("Referer", "https://html.duckduckgo.com/")
                .header("Origin", "https://html.duckduckgo.com")
                .header("Accept-Language", language)
                .header(
                    "Accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                )
                .form(&[("q", text.as_str()), ("b", "")]);

            let (status, body) = self.http.text(request, "duckduckgo").await?;
            if status == StatusCode::ACCEPTED {
                return Err(ProviderError::blocked(
                    "duckduckgo",
                    "bot-detection challenge (HTTP 202)",
                ));
            }
            if !status.is_success() {
                return Err(ProviderError::http("duckduckgo", format!("HTTP {status}")));
            }

            let mut hits = parse_ddg_results(&body)?;
            hits.truncate(query.max_results);
            Ok(SearchOutcome {
                provider: "duckduckgo".to_string(),
                results: hits,
                answer: None,
            })
        })
    }
}
