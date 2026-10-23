//! Web-search use case: models, the `SearchProvider` port and the service.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use url::Url;

use super::error::{ProviderError, ProviderFuture};

/// How recent search results should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Freshness {
    #[default]
    Any,
    Day,
    Week,
    Month,
    Year,
}

impl Freshness {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }
}

/// A search request, normalized by the application layer.
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub text: String,
    pub max_results: usize,
    pub site: Option<String>,
    pub freshness: Freshness,
    pub language: Option<String>,
    /// ISO date (`YYYY-MM-DD`): only keep results not older than this where a
    /// publication date is known; providers with native support get the hint.
    pub since: Option<String>,
    /// Host-suffix exclusions applied to every provider's results (SEO farms).
    pub exclude_domains: Vec<String>,
}

/// A single search result.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub published_date: Option<String>,
}

/// Result of a successful search, including which provider answered.
#[derive(Debug, Clone)]
pub struct SearchOutcome {
    pub provider: String,
    pub results: Vec<SearchHit>,
    pub answer: Option<String>,
}

/// Port implemented by every search backend (keyless or free-account).
pub trait SearchProvider: Send + Sync {
    fn name(&self) -> &'static str;

    fn search<'a>(
        &'a self,
        query: &'a SearchQuery,
    ) -> ProviderFuture<'a, Result<SearchOutcome, ProviderError>>;
}

/// Errors of the search use case.
#[derive(Debug)]
pub enum SearchError {
    EmptyQuery,
    NoProviders,
    AllProvidersFailed(Vec<ProviderError>),
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyQuery => write!(f, "search query must not be empty"),
            Self::NoProviders => write!(f, "no search provider is enabled"),
            Self::AllProvidersFailed(errors) => {
                write!(f, "all search providers failed: ")?;
                for (index, error) in errors.iter().enumerate() {
                    if index > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{error}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for SearchError {}

/// Business logic for searching: validates the query and walks the provider
/// fallback chain in order.
pub struct WebSearchService {
    providers: Vec<Arc<dyn SearchProvider>>,
}

impl WebSearchService {
    pub fn new(providers: Vec<Arc<dyn SearchProvider>>) -> Self {
        Self { providers }
    }

    pub async fn search(&self, query: SearchQuery) -> Result<SearchOutcome, SearchError> {
        if query.text.trim().is_empty() {
            return Err(SearchError::EmptyQuery);
        }
        if self.providers.is_empty() {
            return Err(SearchError::NoProviders);
        }

        let mut errors = Vec::with_capacity(self.providers.len());
        for provider in &self.providers {
            match provider.search(&query).await {
                Ok(outcome) if !outcome.results.is_empty() => {
                    let outcome = post_process(outcome, &query);
                    if outcome.results.is_empty() {
                        errors.push(ProviderError::unavailable(
                            provider.name(),
                            "results removed by filters",
                        ));
                        continue;
                    }
                    return Ok(outcome);
                }
                Ok(outcome) => {
                    tracing::debug!(provider = provider.name(), "provider returned no results");
                    errors.push(ProviderError::unavailable(
                        provider.name(),
                        format!("no results for '{}'", query.text),
                    ));
                    let _ = outcome.answer;
                }
                Err(error) => {
                    tracing::warn!(provider = provider.name(), error = %error, "search provider failed");
                    errors.push(error);
                }
            }
        }

        Err(SearchError::AllProvidersFailed(errors))
    }
}

/// Snippets longer than this are trimmed at a sentence boundary.
const SNIPPET_LIMIT: usize = 400;

fn host_of_url(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_lowercase))
        .unwrap_or_default()
}

fn is_excluded(url: &str, exclude_domains: &[String]) -> bool {
    if exclude_domains.is_empty() {
        return false;
    }
    let host = host_of_url(url);
    if host.is_empty() {
        return false;
    }
    exclude_domains.iter().any(|domain| {
        let domain = domain
            .trim()
            .trim_start_matches('.')
            .trim_end_matches('/')
            .to_ascii_lowercase();
        !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
    })
}

fn is_too_old(published_date: &Option<String>, since: &str) -> bool {
    // Results without a usable date are kept; ISO and RFC-2822 dates are
    // compared by calendar day.
    let Some(date) = published_date else {
        return false;
    };
    let iso = date.trim();
    if iso.len() >= 10 {
        let head = &iso[..10];
        let bytes = head.as_bytes();
        let looks_iso = bytes[4] == b'-' && bytes[7] == b'-'
            && bytes.iter().enumerate().all(|(i, b)| matches!(i, 4 | 7) || b.is_ascii_digit());
        if looks_iso {
            return head < since;
        }
    }
    match jiff::fmt::rfc2822::parse(date.trim()) {
        Ok(zoned) => zoned.date().to_string().as_str() < since,
        Err(_) => false,
    }
}

/// Trim an over-long snippet at the last sentence boundary in its tail.
pub fn trim_snippet(snippet: &str, max_chars: usize) -> String {
    let chars: Vec<char> = snippet.chars().collect();
    if chars.len() <= max_chars {
        return snippet.to_string();
    }
    let cut: String = chars[..max_chars].iter().collect();
    let tail_start = (max_chars * 3 / 5).min(max_chars - 1);
    let tail = &cut[tail_start..];
    let sentence_end = tail
        .char_indices()
        .rev()
        .find(|(_, c)| matches!(c, '.' | '!' | '?' | '。'))
        .map(|(index, _)| tail_start + index + 1);
    if let Some(end) = sentence_end {
        return format!("{} …", cut[..end].trim_end());
    }
    match cut.rfind(' ') {
        Some(space) if space > tail_start => format!("{} …", cut[..space].trim_end()),
        _ => format!("{cut} …"),
    }
}

/// Provider-independent result hygiene: exclusions, `since`, dedup, snippet
/// trimming and the final `max_results` cut.
fn post_process(outcome: SearchOutcome, query: &SearchQuery) -> SearchOutcome {
    let since = query
        .since
        .as_deref()
        .filter(|since| since.len() >= 10)
        .map(str::to_string);
    let mut seen = HashSet::new();
    let mut results: Vec<SearchHit> = outcome
        .results
        .into_iter()
        .filter(|hit| !is_excluded(&hit.url, &query.exclude_domains))
        .filter(|hit| !since.as_deref().is_some_and(|since| is_too_old(&hit.published_date, since)))
        .filter(|hit| seen.insert(hit.url.trim_end_matches('/').to_ascii_lowercase()))
        .map(|mut hit| {
            hit.snippet = trim_snippet(&hit.snippet, SNIPPET_LIMIT);
            hit
        })
        .collect();
    results.truncate(query.max_results.max(1));
    SearchOutcome {
        provider: outcome.provider,
        answer: outcome.answer,
        results,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(exclude: &[&str], since: Option<&str>) -> SearchQuery {
        SearchQuery {
            text: "test".to_string(),
            max_results: 8,
            site: None,
            freshness: Freshness::Any,
            language: None,
            since: since.map(str::to_string),
            exclude_domains: exclude.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn hit(url: &str, date: Option<&str>) -> SearchHit {
        SearchHit {
            title: "t".to_string(),
            url: url.to_string(),
            snippet: "s".to_string(),
            published_date: date.map(str::to_string),
        }
    }

    #[test]
    fn post_process_filters_dedups_and_keeps_undated() {
        let outcome = SearchOutcome {
            provider: "p".to_string(),
            results: vec![
                hit("https://whatsnew.fyi/x", None),
                hit("https://docs.rs/axum", None),
                hit("https://docs.rs/axum/", None),
                hit("https://blog.example/post", Some("2024-01-01")),
                hit("https://blog.example/fresh", Some("2026-07-01")),
            ],
            answer: None,
        };
        let processed = post_process(
            outcome,
            &query(&["whatsnew.fyi"], Some("2026-01-01")),
        );
        let urls: Vec<&str> = processed
            .results
            .iter()
            .map(|hit| hit.url.as_str())
            .collect();
        assert_eq!(urls, vec!["https://docs.rs/axum", "https://blog.example/fresh"]);
    }

    #[test]
    fn since_understands_rfc2822_dates() {
        assert!(is_too_old(&Some("Mon, 29 Jun 2026 09:04:03 GMT".to_string()), "2026-07-01"));
        assert!(!is_too_old(&Some("Mon, 29 Jun 2026 09:04:03 GMT".to_string()), "2026-06-01"));
        assert!(!is_too_old(&Some("no date at all".to_string()), "2026-06-01"));
    }

    #[test]
    fn trim_snippet_prefers_sentence_boundary() {
        let text = "Word. ".repeat(100);
        let trimmed = trim_snippet(&text, 60);
        assert!(trimmed.chars().count() <= 62);
        assert!(trimmed.ends_with(". …"), "{trimmed}");
        assert_eq!(trim_snippet("short", 60), "short");
    }
}
