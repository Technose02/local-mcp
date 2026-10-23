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
    /// Return the (capped) sitemap URL list; off by default because the dump
    /// costs thousands of tokens and is rarely the right tool.
    pub include_sitemap: bool,
}

/// Everything discovered around a destination.
#[derive(Debug, Clone)]
pub struct ExploreResult {
    pub domain: String,
    pub base_url: String,
    pub search: Option<SearchOutcome>,
    pub pages: Vec<Link>,
    pub sitemap_urls: Vec<String>,
    /// Total sitemap URLs discovered before capping (0 when not requested).
    pub sitemap_total: usize,
    /// Guidance for the model when the exploration was skipped or narrowed.
    pub note: Option<String>,
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

/// Path markers of single-item pages (files, PRs, issues) where site mapping
/// returns only site chrome.
const ITEM_PATH_MARKERS: &[&str] = &["/blob/", "/pull/", "/issues/", "/releases/download/"];

/// When the target looks like a single file or item page, `explore_site`
/// returns this guidance instead of scraping chrome around the item.
pub fn file_like_note(url: &str) -> Option<&'static str> {
    let lower = url.to_ascii_lowercase();
    if ITEM_PATH_MARKERS.iter().any(|marker| lower.contains(marker)) {
        return Some(FILE_NOTE);
    }
    let path = lower
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(&lower);
    let path = path.split(['?', '#']).next().unwrap_or(path);
    // Strip the host: only the path may indicate a file (`docs.rs` is a
    // domain, not a Rust file).
    let path = path.split_once('/').map(|(_, path)| path).unwrap_or("");
    let last = path.rsplit('/').next().unwrap_or("");
    if let Some((stem, extension)) = last.rsplit_once('.')
        && !stem.is_empty()
        && extension.len() >= 2
        && extension.len() <= 5
        && extension.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Some(FILE_NOTE);
    }
    None
}

const FILE_NOTE: &str = "target looks like a single file or item page, so site mapping was skipped (it only returns site chrome). Use fetch_url to read this page (pattern=/outline= search inside it), or web_search with `site` to find related pages.";

/// Composes search, link discovery and sitemap discovery.
pub struct ExploreService {
    search: Arc<WebSearchService>,
    links: Arc<dyn LinkSource>,
    sitemap: Arc<dyn SiteMapSource>,
}

/// Maximum number of sitemap URLs returned even when requested.
const SITEMAP_RETURN_CAP: usize = 25;

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

        if let Some(note) = file_like_note(&base_url) {
            tracing::debug!(target = %base_url, "explore_site target looks like a file; skipping site mapping");
            return Ok(ExploreResult {
                domain,
                base_url,
                search: None,
                pages: Vec::new(),
                sitemap_urls: Vec::new(),
                sitemap_total: 0,
                note: Some(note.to_string()),
            });
        }

        let search = match &request.query {
            Some(query) if !query.trim().is_empty() => {
                let query = SearchQuery {
                    text: query.trim().to_string(),
                    max_results: request.max_results,
                    site: Some(domain.clone()),
                    freshness: request.freshness,
                    language: request.language.clone(),
                    since: None,
                    exclude_domains: Vec::new(),
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

        let mut sitemap_urls = Vec::new();
        let mut sitemap_total = 0usize;
        if request.include_sitemap {
            match self.sitemap.discover(&base_url).await {
                Ok(urls) => {
                    sitemap_total = urls.len();
                    sitemap_urls = urls.into_iter().take(SITEMAP_RETURN_CAP).collect();
                }
                Err(error) => {
                    tracing::warn!(error = %error, "sitemap discovery failed");
                }
            }
        }

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
            sitemap_total,
            note: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_like_targets_are_detected() {
        assert!(file_like_note("https://github.com/a/b/blob/main/README.md").is_some());
        assert!(file_like_note("https://github.com/a/b/pull/28092").is_some());
        assert!(file_like_note("https://example.com/guide.html").is_some());
        assert!(file_like_note("https://example.com/blog/post-title").is_none());
        assert!(file_like_note("https://docs.rs").is_none());
        assert!(file_like_note("https://docs.rs/tokio").is_none());
    }
}
