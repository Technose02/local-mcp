//! Wikipedia adapter (keyless).
//!
//! A reliable, fully free fallback for encyclopedic queries. Attribution
//! requirements apply (CC BY-SA); see README.md.

use serde::Deserialize;
use url::Url;

use crate::domain::error::{ProviderError, ProviderFuture};
use crate::domain::search::{SearchHit, SearchOutcome, SearchProvider, SearchQuery};
use crate::infrastructure::html::strip_tags;
use crate::infrastructure::http::HttpClient;

const API_ENDPOINT: &str = "https://en.wikipedia.org/w/api.php";

pub struct WikipediaSearchProvider {
    http: HttpClient,
}

impl WikipediaSearchProvider {
    pub fn new(http: HttpClient) -> Self {
        Self { http }
    }
}

impl SearchProvider for WikipediaSearchProvider {
    fn name(&self) -> &'static str {
        "wikipedia"
    }

    fn search<'a>(
        &'a self,
        query: &'a SearchQuery,
    ) -> ProviderFuture<'a, Result<SearchOutcome, ProviderError>> {
        Box::pin(async move {
            let mut url = Url::parse(API_ENDPOINT)
                .map_err(|error| ProviderError::http("wikipedia", error.to_string()))?;
            {
                let mut pairs = url.query_pairs_mut();
                pairs.append_pair("action", "query");
                pairs.append_pair("list", "search");
                pairs.append_pair("format", "json");
                pairs.append_pair("utf8", "1");
                pairs.append_pair("srprop", "snippet");
                pairs.append_pair("srlimit", &query.max_results.to_string());
                pairs.append_pair("srsearch", &query.text);
            }

            let request = self
                .http
                .client()
                .get(url)
                .header("Accept", "application/json");
            let response: Response = self.http.json(request, "wikipedia").await?;

            let items = response.query.map(|query| query.search).unwrap_or_default();
            let results = items
                .into_iter()
                .filter_map(|item| {
                    let url = wikipedia_url(&item.title)?;
                    Some(SearchHit {
                        title: item.title,
                        url,
                        snippet: strip_tags(&item.snippet),
                        published_date: None,
                    })
                })
                .collect();

            Ok(SearchOutcome {
                provider: "wikipedia".to_string(),
                results,
                answer: None,
            })
        })
    }
}

fn wikipedia_url(title: &str) -> Option<String> {
    let mut url = Url::parse("https://en.wikipedia.org/").ok()?;
    url.path_segments_mut()
        .ok()?
        .push("wiki")
        .push(&title.replace(' ', "_"));
    Some(url.to_string())
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    query: Option<QueryPart>,
}

#[derive(Deserialize)]
struct QueryPart {
    #[serde(default)]
    search: Vec<ResponseItem>,
}

#[derive(Deserialize)]
struct ResponseItem {
    title: String,
    #[serde(default)]
    snippet: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wikipedia_url_encodes_title() {
        let url = wikipedia_url("Rust (programming language)").expect("url");
        assert_eq!(
            url,
            "https://en.wikipedia.org/wiki/Rust_(programming_language)"
        );
    }
}
