//! Web-search use case: models, the `SearchProvider` port and the service.

use std::fmt;
use std::sync::Arc;

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
                Ok(outcome) if !outcome.results.is_empty() => return Ok(outcome),
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
