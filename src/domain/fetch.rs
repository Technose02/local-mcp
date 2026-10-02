//! Page-fetching use case: content extraction, paging and link discovery.

use std::fmt;
use std::sync::Arc;

use super::error::{ProviderError, ProviderFuture};

/// Format of extracted page content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentFormat {
    Markdown,
    Text,
}

impl ContentFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Text => "text",
        }
    }
}

/// Content extracted from a URL by an extractor adapter.
#[derive(Debug, Clone)]
pub struct ExtractedDocument {
    pub final_url: String,
    pub title: Option<String>,
    pub content: String,
    pub format: ContentFormat,
}

/// A hyperlink discovered on a page.
#[derive(Debug, Clone)]
pub struct Link {
    pub text: String,
    pub url: String,
}

/// A normalized fetch request.
#[derive(Debug, Clone)]
pub struct FetchRequest {
    pub url: String,
    pub max_chars: usize,
    pub start_char: usize,
    pub include_links: bool,
}

/// A paged, ready-to-return page.
#[derive(Debug, Clone)]
pub struct FetchedPage {
    pub url: String,
    pub final_url: String,
    pub title: Option<String>,
    pub format: ContentFormat,
    pub content: String,
    pub truncated: bool,
    pub next_start_char: Option<usize>,
    pub links: Vec<Link>,
}

/// Port for content extractors (API-based or direct HTTP).
pub trait ExtractProvider: Send + Sync {
    fn name(&self) -> &'static str;

    fn extract<'a>(
        &'a self,
        url: &'a str,
    ) -> ProviderFuture<'a, Result<ExtractedDocument, ProviderError>>;
}

/// Port for discovering the links on a page.
pub trait LinkSource: Send + Sync {
    fn links<'a>(
        &'a self,
        url: &'a str,
        include_external: bool,
    ) -> ProviderFuture<'a, Result<Vec<Link>, ProviderError>>;
}

/// Port for discovering a site's URLs via `robots.txt` / `sitemap.xml`.
pub trait SiteMapSource: Send + Sync {
    fn discover<'a>(
        &'a self,
        base_url: &'a str,
    ) -> ProviderFuture<'a, Result<Vec<String>, ProviderError>>;
}

/// Errors of the fetch use case.
#[derive(Debug)]
pub enum FetchError {
    InvalidUrl(String),
    UnsupportedScheme(String),
    NoExtractors,
    AllExtractorsFailed(Vec<ProviderError>),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(url) => write!(f, "invalid URL: '{url}'"),
            Self::UnsupportedScheme(url) => {
                write!(f, "unsupported URL scheme (expected http/https): '{url}'")
            }
            Self::NoExtractors => write!(f, "no content extractor is enabled"),
            Self::AllExtractorsFailed(errors) => {
                write!(f, "all content extractors failed: ")?;
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

impl std::error::Error for FetchError {}

/// Validate that a URL is an absolute http(s) URL.
pub fn validate_url(url: &str) -> Result<(), FetchError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(FetchError::InvalidUrl(url.to_string()));
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(FetchError::UnsupportedScheme(url.to_string()));
    }
    if trimmed.len() <= "https://".len() {
        return Err(FetchError::InvalidUrl(url.to_string()));
    }
    Ok(())
}

/// Business logic for reading a page: tries the extractor chain, then pages the
/// result and (optionally) discovers links.
pub struct FetchService {
    extractors: Vec<Arc<dyn ExtractProvider>>,
    links: Arc<dyn LinkSource>,
}

impl FetchService {
    pub fn new(extractors: Vec<Arc<dyn ExtractProvider>>, links: Arc<dyn LinkSource>) -> Self {
        Self { extractors, links }
    }

    pub async fn fetch(&self, request: FetchRequest) -> Result<FetchedPage, FetchError> {
        validate_url(&request.url)?;

        let mut errors = Vec::with_capacity(self.extractors.len());
        let mut document = None;
        for extractor in &self.extractors {
            match extractor.extract(&request.url).await {
                Ok(candidate) if !candidate.content.trim().is_empty() => {
                    document = Some(candidate);
                    break;
                }
                Ok(_) => {
                    tracing::debug!(
                        provider = extractor.name(),
                        "extractor returned empty content"
                    );
                    errors.push(ProviderError::unavailable(
                        extractor.name(),
                        "empty content",
                    ));
                }
                Err(error) => {
                    tracing::warn!(provider = extractor.name(), error = %error, "extractor failed");
                    errors.push(error);
                }
            }
        }

        let document = match document {
            Some(document) => document,
            None if self.extractors.is_empty() => return Err(FetchError::NoExtractors),
            None => return Err(FetchError::AllExtractorsFailed(errors)),
        };

        let links = if request.include_links {
            match self.links.links(&request.url, false).await {
                Ok(links) => links,
                Err(error) => {
                    tracing::warn!(error = %error, "link discovery failed");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        Ok(page_from(document, request, links))
    }
}

fn page_from(document: ExtractedDocument, request: FetchRequest, links: Vec<Link>) -> FetchedPage {
    let chars: Vec<char> = document.content.chars().collect();
    let total = chars.len();
    let start = request.start_char.min(total);
    let end = start.saturating_add(request.max_chars).min(total);
    let content: String = chars[start..end].iter().collect();
    let truncated = end < total;

    FetchedPage {
        url: request.url,
        final_url: document.final_url,
        title: document.title,
        format: document.format,
        content,
        truncated,
        next_start_char: if truncated { Some(end) } else { None },
        links,
    }
}
