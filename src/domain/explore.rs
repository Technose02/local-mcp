//! "Explore around a destination" use case.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use super::fetch::{Link, LinkSource, SiteMapSource};
use super::search::{Freshness, SearchError, SearchOutcome, SearchQuery, WebSearchService};

/// A normalized exploration request.
#[derive(Debug, Clone)]
pub struct ExploreRequest {
    pub target: String,
    pub query: Option<String>,
    pub max_results: usize,
    pub include_external: bool,
    pub freshness: Freshness,
    pub language: Option<String>,
}

/// Everything discovered around a destination.
#[derive(Debug, Clone)]
pub struct ExploreResult {
    pub domain: String,
    pub base_url: String,
    pub search: Option<SearchOutcome>,
    pub pages: Vec<Link>,
    pub sitemap_urls: Vec<String>,
}

/// Errors of the explore use case.
#[derive(Debug)]
pub enum ExploreError {
    InvalidTarget(String),
    Search(SearchError),
}

impl fmt::Display for ExploreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTarget(target) => write!(f, "invalid target: '{target}'"),
            Self::Search(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ExploreError {}

/// Turns a bare domain into an absolute base URL.
pub fn normalize_base_url(target: &str) -> Result<String, ExploreError> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return Err(ExploreError::InvalidTarget(target.to_string()));
    }
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    if host_of(&with_scheme).is_empty() {
        return Err(ExploreError::InvalidTarget(target.to_string()));
    }
    Ok(with_scheme)
}

/// Extract the host (without port) from an absolute URL or bare domain.
pub fn host_of(target: &str) -> String {
    let without_scheme = target
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(target);
    let authority = without_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    authority.split(':').next().unwrap_or("").to_lowercase()
}

/// Composes search, link discovery and sitemap discovery.
pub struct ExploreService {
    search: Arc<WebSearchService>,
    links: Arc<dyn LinkSource>,
    sitemap: Arc<dyn SiteMapSource>,
}

impl ExploreService {
    pub fn new(
        search: Arc<WebSearchService>,
        links: Arc<dyn LinkSource>,
        sitemap: Arc<dyn SiteMapSource>,
    ) -> Self {
        Self {
            search,
            links,
            sitemap,
        }
    }

    pub async fn explore(&self, request: ExploreRequest) -> Result<ExploreResult, ExploreError> {
        let base_url = normalize_base_url(&request.target)?;
        let domain = host_of(&base_url);
        if domain.is_empty() {
            return Err(ExploreError::InvalidTarget(request.target.clone()));
        }

        let search = match &request.query {
            Some(query) if !query.trim().is_empty() => {
                let query = SearchQuery {
                    text: query.trim().to_string(),
                    max_results: request.max_results,
                    site: Some(domain.clone()),
                    freshness: request.freshness,
                    language: request.language.clone(),
                };
                Some(
                    self.search
                        .search(query)
                        .await
                        .map_err(ExploreError::Search)?,
                )
            }
            _ => None,
        };

        let discovered = match self.links.links(&base_url, request.include_external).await {
            Ok(links) => links,
            Err(error) => {
                tracing::warn!(error = %error, "link discovery failed");
                Vec::new()
            }
        };

        let sitemap_urls = match self.sitemap.discover(&base_url).await {
            Ok(urls) => urls,
            Err(error) => {
                tracing::warn!(error = %error, "sitemap discovery failed");
                Vec::new()
            }
        };

        let mut seen = HashSet::new();
        let pages = discovered
            .into_iter()
            .filter(|link| seen.insert(link.url.clone()))
            .take(request.max_results)
            .collect();

        Ok(ExploreResult {
            domain,
            base_url,
            search,
            pages,
            sitemap_urls,
        })
    }
}
