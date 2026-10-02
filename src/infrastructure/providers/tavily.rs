//! Tavily adapter (search + extract).
//!
//! Works without an account via the keyless mode
//! (`X-Tavily-Access-Mode: keyless`). A free API key (no credit card) raises the
//! rate limits; set `providers.tavily.api_key` or `TAVILY_API_KEY`.

use reqwest::RequestBuilder;
use serde::{Deserialize, Serialize};

use crate::domain::error::{ProviderError, ProviderFuture};
use crate::domain::fetch::{ContentFormat, ExtractProvider, ExtractedDocument};
use crate::domain::search::{Freshness, SearchHit, SearchOutcome, SearchProvider, SearchQuery};
use crate::infrastructure::html::normalize_whitespace;
use crate::infrastructure::http::HttpClient;

const SEARCH_URL: &str = "https://api.tavily.com/search";
const EXTRACT_URL: &str = "https://api.tavily.com/extract";

fn apply_auth(request: RequestBuilder, api_key: &Option<String>) -> RequestBuilder {
    match api_key {
        Some(key) => request.bearer_auth(key),
        None => request.header("X-Tavily-Access-Mode", "keyless"),
    }
}

pub struct TavilySearchProvider {
    http: HttpClient,
    api_key: Option<String>,
}

impl TavilySearchProvider {
    pub fn new(http: HttpClient, api_key: Option<String>) -> Self {
        Self { http, api_key }
    }
}

impl SearchProvider for TavilySearchProvider {
    fn name(&self) -> &'static str {
        "tavily"
    }

    fn search<'a>(
        &'a self,
        query: &'a SearchQuery,
    ) -> ProviderFuture<'a, Result<SearchOutcome, ProviderError>> {
        Box::pin(async move {
            #[derive(Serialize)]
            struct Request<'a> {
                query: &'a str,
                max_results: usize,
                include_answer: bool,
                search_depth: &'static str,
                #[serde(skip_serializing_if = "Option::is_none")]
                topic: Option<&'static str>,
                #[serde(skip_serializing_if = "Option::is_none")]
                days: Option<u32>,
                #[serde(skip_serializing_if = "Option::is_none")]
                include_domains: Option<Vec<String>>,
            }

            #[derive(Deserialize)]
            struct Response {
                #[serde(default)]
                answer: Option<String>,
                #[serde(default)]
                results: Vec<ResponseItem>,
            }

            #[derive(Deserialize)]
            struct ResponseItem {
                title: String,
                url: String,
                #[serde(default)]
                content: String,
                #[serde(default)]
                published_date: Option<String>,
            }

            let (topic, days) = match query.freshness {
                Freshness::Any => (None, None),
                Freshness::Day => (Some("news"), Some(1)),
                Freshness::Week => (Some("news"), Some(7)),
                Freshness::Month => (Some("news"), Some(30)),
                Freshness::Year => (Some("news"), Some(365)),
            };

            let body = Request {
                query: &query.text,
                max_results: query.max_results,
                include_answer: true,
                search_depth: "basic",
                topic,
                days,
                include_domains: query.site.as_ref().map(|site| vec![site.clone()]),
            };

            let request = apply_auth(
                self.http.client().post(SEARCH_URL).json(&body),
                &self.api_key,
            );
            let response: Response = self.http.json(request, "tavily").await?;

            let results = response
                .results
                .into_iter()
                .map(|item| SearchHit {
                    title: item.title,
                    url: item.url,
                    snippet: normalize_whitespace(&item.content),
                    published_date: item.published_date,
                })
                .collect();

            Ok(SearchOutcome {
                provider: "tavily".to_string(),
                results,
                answer: response.answer,
            })
        })
    }
}

pub struct TavilyExtractProvider {
    http: HttpClient,
    api_key: Option<String>,
}

impl TavilyExtractProvider {
    pub fn new(http: HttpClient, api_key: Option<String>) -> Self {
        Self { http, api_key }
    }
}

impl ExtractProvider for TavilyExtractProvider {
    fn name(&self) -> &'static str {
        "tavily"
    }

    fn extract<'a>(
        &'a self,
        url: &'a str,
    ) -> ProviderFuture<'a, Result<ExtractedDocument, ProviderError>> {
        Box::pin(async move {
            #[derive(Serialize)]
            struct Request<'a> {
                urls: Vec<&'a str>,
            }

            #[derive(Deserialize)]
            struct Response {
                #[serde(default)]
                results: Vec<ResponseItem>,
            }

            #[derive(Deserialize)]
            struct ResponseItem {
                url: String,
                #[serde(default)]
                title: Option<String>,
                #[serde(default)]
                raw_content: Option<String>,
            }

            let request = apply_auth(
                self.http
                    .client()
                    .post(EXTRACT_URL)
                    .json(&Request { urls: vec![url] }),
                &self.api_key,
            );
            let response: Response = self.http.json(request, "tavily").await?;
            let item = response
                .results
                .into_iter()
                .next()
                .ok_or_else(|| ProviderError::unavailable("tavily", "no extraction result"))?;

            Ok(ExtractedDocument {
                final_url: item.url,
                title: item.title,
                content: item.raw_content.unwrap_or_default(),
                format: ContentFormat::Markdown,
            })
        })
    }
}
