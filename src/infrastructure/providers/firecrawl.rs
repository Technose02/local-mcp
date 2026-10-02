//! Firecrawl adapter (search + scrape).
//!
//! Keyless mode covers `/v1/search` and `/v1/scrape` with a monthly free credit
//! budget. A free API key enables more endpoints (e.g. `/v1/map`) and higher
//! limits; set `providers.firecrawl.api_key` or `FIRECRAWL_API_KEY`.

use reqwest::RequestBuilder;
use serde::{Deserialize, Serialize};

use crate::domain::error::{ProviderError, ProviderFuture};
use crate::domain::fetch::{ContentFormat, ExtractProvider, ExtractedDocument};
use crate::domain::search::{SearchHit, SearchOutcome, SearchProvider, SearchQuery};
use crate::infrastructure::html::normalize_whitespace;
use crate::infrastructure::http::HttpClient;

const SEARCH_URL: &str = "https://api.firecrawl.dev/v1/search";
const SCRAPE_URL: &str = "https://api.firecrawl.dev/v1/scrape";

fn apply_auth(request: RequestBuilder, api_key: &Option<String>) -> RequestBuilder {
    match api_key {
        Some(key) => request.bearer_auth(key),
        None => request,
    }
}

pub struct FirecrawlSearchProvider {
    http: HttpClient,
    api_key: Option<String>,
}

impl FirecrawlSearchProvider {
    pub fn new(http: HttpClient, api_key: Option<String>) -> Self {
        Self { http, api_key }
    }
}

impl SearchProvider for FirecrawlSearchProvider {
    fn name(&self) -> &'static str {
        "firecrawl"
    }

    fn search<'a>(
        &'a self,
        query: &'a SearchQuery,
    ) -> ProviderFuture<'a, Result<SearchOutcome, ProviderError>> {
        Box::pin(async move {
            #[derive(Serialize)]
            struct Request<'a> {
                query: &'a str,
                limit: usize,
                #[serde(skip_serializing_if = "Option::is_none")]
                location: Option<String>,
            }

            #[derive(Deserialize)]
            struct Response {
                #[serde(default)]
                success: bool,
                #[serde(default)]
                data: Vec<ResponseItem>,
            }

            #[derive(Deserialize)]
            struct ResponseItem {
                #[serde(default)]
                title: Option<String>,
                url: String,
                #[serde(default)]
                description: Option<String>,
            }

            let body = Request {
                query: &query.text,
                limit: query.max_results,
                location: None,
            };
            let request = apply_auth(
                self.http.client().post(SEARCH_URL).json(&body),
                &self.api_key,
            );
            let response: Response = self.http.json(request, "firecrawl").await?;
            if !response.success && response.data.is_empty() {
                return Err(ProviderError::unavailable(
                    "firecrawl",
                    "search returned no data",
                ));
            }

            let results = response
                .data
                .into_iter()
                .map(|item| SearchHit {
                    title: item.title.unwrap_or_else(|| item.url.clone()),
                    url: item.url,
                    snippet: normalize_whitespace(&item.description.unwrap_or_default()),
                    published_date: None,
                })
                .collect();

            Ok(SearchOutcome {
                provider: "firecrawl".to_string(),
                results,
                answer: None,
            })
        })
    }
}

pub struct FirecrawlScrapeProvider {
    http: HttpClient,
    api_key: Option<String>,
}

impl FirecrawlScrapeProvider {
    pub fn new(http: HttpClient, api_key: Option<String>) -> Self {
        Self { http, api_key }
    }
}

impl ExtractProvider for FirecrawlScrapeProvider {
    fn name(&self) -> &'static str {
        "firecrawl"
    }

    fn extract<'a>(
        &'a self,
        url: &'a str,
    ) -> ProviderFuture<'a, Result<ExtractedDocument, ProviderError>> {
        Box::pin(async move {
            #[derive(Serialize)]
            struct Request<'a> {
                url: &'a str,
                formats: Vec<&'static str>,
            }

            #[derive(Deserialize)]
            struct Response {
                #[serde(default)]
                data: Option<ResponseData>,
            }

            #[derive(Deserialize)]
            struct ResponseData {
                #[serde(default)]
                markdown: Option<String>,
                #[serde(default)]
                metadata: Option<Metadata>,
            }

            #[derive(Deserialize)]
            struct Metadata {
                #[serde(default)]
                title: Option<String>,
                #[serde(default, alias = "sourceURL", alias = "source_url", alias = "url")]
                source_url: Option<String>,
            }

            let body = Request {
                url,
                formats: vec!["markdown"],
            };
            let request = apply_auth(
                self.http.client().post(SCRAPE_URL).json(&body),
                &self.api_key,
            );
            let response: Response = self.http.json(request, "firecrawl").await?;
            let data = response.data.ok_or_else(|| {
                ProviderError::unavailable("firecrawl", "scrape returned no data")
            })?;
            let metadata = data.metadata.unwrap_or(Metadata {
                title: None,
                source_url: None,
            });

            Ok(ExtractedDocument {
                final_url: metadata.source_url.unwrap_or_else(|| url.to_string()),
                title: metadata.title,
                content: data.markdown.unwrap_or_default(),
                format: ContentFormat::Markdown,
            })
        })
    }
}
