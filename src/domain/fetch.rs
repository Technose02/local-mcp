//! Page-fetching use case: URL policy, canonicalization, extraction with a
//! small cache, in-document search (pattern/outline), paging and link
//! discovery.

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use url::Url;

use super::error::{ProviderError, ProviderFuture};
use super::textsearch::{self, OutlineEntry, TextMatch};

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
    /// Name of the adapter that produced this document (provenance).
    pub provider: String,
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
    /// Case-insensitive regex to grep inside the extracted document.
    pub pattern: Option<String>,
    /// Context characters around each pattern match.
    pub context_chars: usize,
    /// Return the markdown heading outline instead of body content.
    pub outline: bool,
    /// Download up to this many content images for multimodal delivery (0 = off).
    pub max_images: usize,
}

/// GitHub pull-request metadata attached to `github.com/<owner>/<repo>/pull/N`
/// fetches (so "is it merged?" costs zero extra calls).
#[derive(Debug, Clone)]
pub struct PullRequestInfo {
    pub number: u64,
    pub title: String,
    /// "open" or "closed" (GitHub's state; combine with `merged`).
    pub state: String,
    pub draft: bool,
    pub merged: bool,
    pub merged_at: Option<String>,
    pub closed_at: Option<String>,
    pub comments: u64,
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
    /// Extractor that produced the document (provenance).
    pub extractor: String,
    /// Effective page size limit of this call.
    pub max_chars: usize,
    pub returned_chars: usize,
    /// Total length of the extracted document.
    pub total_chars: usize,
    /// Served from the in-memory extraction cache.
    pub from_cache: bool,
    /// Age of the cache entry in whole seconds.
    pub cached_age_s: Option<u64>,
    /// Cache entry was past its TTL and every live extractor failed.
    pub stale: bool,
    /// Windows matched by `pattern` (empty when no pattern was given).
    pub matches: Vec<TextMatch>,
    /// Total number of pattern matches in the document.
    pub match_count: usize,
    /// Heading outline (empty when not requested).
    pub outline: Vec<OutlineEntry>,
    /// GitHub pull-request state, when the URL points at one.
    pub github: Option<PullRequestInfo>,
    /// Model-ready content images (multimodal delivery; empty unless asked).
    pub images: Vec<PageImage>,
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

/// Port for GitHub pull-request metadata (`api.github.com`).
pub trait PullRequestSource: Send + Sync {
    fn pull<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
    ) -> ProviderFuture<'a, Result<PullRequestInfo, ProviderError>>;
}

/// A downloaded, model-ready image (already decoded/resized/re-encoded).
#[derive(Debug, Clone)]
pub struct PageImage {
    pub url: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// Port for image fetching/preparation used by the multimodal pipeline.
pub trait ImageSource: Send + Sync {
    fn image<'a>(
        &'a self,
        url: &'a str,
    ) -> ProviderFuture<'a, Result<PageImage, ProviderError>>;
}

/// Errors of the fetch use case.
#[derive(Debug)]
pub enum FetchError {
    InvalidUrl(String),
    UnsupportedScheme(String),
    UrlRejected(&'static str),
    BlockedHost(String),
    NoExtractors,
    AllExtractorsFailed(Vec<ProviderError>),
    BadPattern(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(url) => write!(f, "invalid URL: '{url}'"),
            Self::UnsupportedScheme(url) => {
                write!(f, "unsupported URL scheme (expected http/https): '{url}'")
            }
            Self::UrlRejected(reason) => write!(f, "URL rejected: {reason}"),
            Self::BlockedHost(host) => write!(
                f,
                "blocked by fetch URL policy: '{host}' is a private/internal network host \
                 (set [fetch] allow_private_network = true to override)"
            ),
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
            Self::BadPattern(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Network policy applied to every outgoing fetch.
#[derive(Debug, Clone, Copy, Default)]
pub struct UrlPolicy {
    /// Allow loopback / private / link-local hosts (SSRF opt-in).
    pub allow_private_network: bool,
}

const PRIVATE_DOMAIN_SUFFIXES: &[&str] = &[
    ".localhost",
    ".local",
    ".internal",
    ".localdomain",
    ".home.arpa",
];

fn is_private_domain(domain: &str) -> bool {
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    domain == "localhost"
        || PRIVATE_DOMAIN_SUFFIXES
            .iter()
            .any(|suffix| domain.ends_with(suffix))
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || octets[0] == 0 // 0.0.0.0/8
                || (octets[0] == 100 && octets[1] & 0b1100_0000 == 0b0100_0000) // CGNAT 100.64/10
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0) // TEST-NET-1 / 192.0.0.0/24
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return true;
            }
            let segments = v6.segments();
            // fe80::/10 link-local
            if segments[0] & 0xffc0 == 0xfe80 {
                return true;
            }
            // IPv4-mapped (::ffff:a.b.c.d): classify by the embedded v4 address.
            if segments[..4].iter().all(|s| *s == 0) && segments[4] == 0xffff {
                let inner =
                    IpAddr::from([segments[4] as u8, (segments[4] & 0xff) as u8, 0, 0]);
                let _ = inner;
                let octets = [
                    (segments[4] >> 8) as u8,
                    (segments[4] & 0xff) as u8,
                    (segments[5] >> 8) as u8,
                    (segments[5] & 0xff) as u8,
                ];
                return is_private_ip(IpAddr::from(octets));
            }
            // fc00::/7 unique-local
            segments[0] & 0xfe00 == 0xfc00
        }
    }
}

/// Whether the host of `url` must be blocked under the fetch policy.
/// Unparseable URLs are not blocked here (validated elsewhere).
pub fn url_host_blocked(url: &str, allow_private_network: bool) -> bool {
    if allow_private_network {
        return false;
    }
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    match parsed.host() {
        None => true,
        Some(url::Host::Domain(domain)) => is_private_domain(domain),
        Some(url::Host::Ipv4(ip)) => is_private_ip(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => is_private_ip(IpAddr::V6(ip)),
    }
}

/// Validate a fetch target: absolute http(s), no credentials, policy-compliant
/// host.
pub fn validate_fetch_url(url: &str, policy: &UrlPolicy) -> Result<(), FetchError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(FetchError::InvalidUrl(url.to_string()));
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(FetchError::UnsupportedScheme(url.to_string()));
    }
    let parsed = Url::parse(trimmed).map_err(|_| FetchError::InvalidUrl(url.to_string()))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(FetchError::UrlRejected(
            "credentials in the URL are not allowed",
        ));
    }
    if parsed.host_str().unwrap_or("").is_empty() {
        return Err(FetchError::InvalidUrl(url.to_string()));
    }
    if url_host_blocked(trimmed, policy.allow_private_network) {
        let host = parsed
            .host_str()
            .filter(|host| !host.is_empty())
            .unwrap_or(trimmed)
            .to_string();
        return Err(FetchError::BlockedHost(host));
    }
    Ok(())
}

/// Extensions considered safe to rewrite from GitHub's HTML view to raw.
const TEXTY_EXTENSIONS: &[&str] = &[
    "md", "markdown", "txt", "rs", "py", "js", "ts", "tsx", "jsx", "json", "yaml", "yml",
    "toml", "csv", "tsv", "sh", "bash", "zsh", "c", "h", "cpp", "hpp", "cc", "go", "java",
    "kt", "swift", "rb", "php", "css", "html", "htm", "xml", "sql", "lua", "r", "pl", "ini",
    "conf", "cfg", "edn", "clj", "dart", "ex", "exs", "erl", "hs", "ml", "nim", "zig", "lock",
];

const TEXTY_NAMES: &[&str] = &[
    "readme",
    "license",
    "licence",
    "changelog",
    "notice",
    "copying",
    "authors",
    "contributing",
    "security",
    "code_of_conduct",
    "makefile",
    "dockerfile",
];

fn is_texty_path(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    let name = file.to_ascii_lowercase();
    if let Some((stem, extension)) = name.split_once('.')
        && !extension.is_empty()
        && !stem.is_empty()
    {
        return TEXTY_EXTENSIONS.contains(&extension) || TEXTY_NAMES.contains(&stem);
    }
    // Extension-less well-known files: README, LICENSE, Makefile, ...
    TEXTY_NAMES.contains(&name.as_str())
}

/// Rewrite URLs to their canonical (cheapest, chrome-free) equivalent.
/// Currently: GitHub `…/blob/…` HTML views of text files become
/// `raw.githubusercontent.com` fetches.
pub fn canonicalize_url(url: &str) -> String {
    let Ok(parsed) = Url::parse(url) else {
        return url.to_string();
    };
    if parsed.host_str() != Some("github.com") {
        return url.to_string();
    }
    let segments: Vec<&str> = parsed.path_segments().map(Iterator::collect).unwrap_or_default();
    if segments.len() >= 5 && segments[2] == "blob" {
        let (owner, repo, rest) = (segments[0], segments[1], &segments[3..]);
        let file_path = rest[1..].join("/");
        if is_texty_path(&file_path) {
            return format!(
                "https://raw.githubusercontent.com/{}/{}/{}/{}",
                owner,
                repo,
                rest[0],
                file_path
            );
        }
    }
    url.to_string()
}

/// Parse `github.com/<owner>/<repo>/pull/<number>` into its parts.
pub fn parse_github_pull(url: &str) -> Option<(String, String, u64)> {
    let parsed = Url::parse(url).ok()?;
    matches!(parsed.host_str(), Some("github.com") | Some("www.github.com")).then(|| ())?;
    let segments: Vec<&str> = parsed.path_segments()?.collect();
    if segments.len() >= 4 && segments[2] == "pull" {
        let number = segments[3].parse::<u64>().ok()?;
        return Some((segments[0].to_string(), segments[1].to_string(), number));
    }
    None
}

/// Normalize a URL for cache keying (absolute, fragment removed).
fn cache_key(url: &str) -> String {
    Url::parse(url)
        .map(|mut url| {
            url.set_fragment(None);
            url.to_string()
        })
        .unwrap_or_else(|_| url.to_string())
}

/// Hosts/paths that are never article content (badges, avatars, icons).
const IMAGE_DENY_MARKERS: &[&str] = &[
    "shields.io",
    "badge.rs",
    "img.shields",
    "badgen.net",
    "gravatar.com",
    "favicon",
    "/avatars/",
    "/badge",
    "badge-",
    "/icon",
    "-icon",
    "1x1.",
    "pixel.",
    "tracker",
];
const IMAGE_DENY_EXTENSIONS: &[&str] = &["svg", "ico"];

fn image_is_content(url: &Url) -> bool {
    let href = url.as_str().to_ascii_lowercase();
    if IMAGE_DENY_MARKERS.iter().any(|marker| href.contains(marker)) {
        return false;
    }
    let path = url.path().to_ascii_lowercase();
    !IMAGE_DENY_EXTENSIONS
        .iter()
        .any(|extension| path.ends_with(&format!(".{extension}")))
}

/// Extract content-image candidate URLs from a markdown document (resolved
/// against `base_url`, deduplicated, chrome/badges/icons filtered).
pub fn markdown_images(markdown: &str, base_url: &str, max: usize) -> Vec<String> {
    static IMAGE_LINK: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let regex = IMAGE_LINK.get_or_init(|| {
        regex::Regex::new(r"!\[[^\]]*\]\(\s*<?([^()\s>]+)>?").expect("valid regex")
    });
    let base = Url::parse(base_url).ok();
    let mut seen = std::collections::HashSet::new();
    let mut urls = Vec::new();
    for capture in regex.captures_iter(markdown) {
        let Some(target) = capture.get(1).map(|m| m.as_str()) else {
            continue;
        };
        if target.starts_with("data:") {
            continue;
        }
        let Some(resolved) = base
            .as_ref()
            .and_then(|base| base.join(target).ok())
            .filter(|url| (url.scheme() == "http" || url.scheme() == "https") && image_is_content(url))
        else {
            continue;
        };
        if seen.insert(resolved.to_string()) {
            urls.push(resolved.to_string());
            if urls.len() >= max {
                break;
            }
        }
    }
    urls
}

struct CacheEntry {
    fetched_at: Instant,
    sequence: u64,
    document: Arc<ExtractedDocument>,
}

/// Business logic for reading a page: policy check, canonicalization, extractor
/// chain with a small LRU/TTL cache (stale-on-error included), GitHub PR
/// enrichment, in-document search and paging.
pub struct FetchService {
    extractors: Vec<Arc<dyn ExtractProvider>>,
    links: Arc<dyn LinkSource>,
    policy: UrlPolicy,
    cache: Mutex<HashMap<String, CacheEntry>>,
    sequence: AtomicU64,
    cache_capacity: usize,
    cache_ttl: Duration,
    pull_requests: Option<Arc<dyn PullRequestSource>>,
    images: Option<Arc<dyn ImageSource>>,
}

impl FetchService {
    pub fn new(
        extractors: Vec<Arc<dyn ExtractProvider>>,
        links: Arc<dyn LinkSource>,
        policy: UrlPolicy,
        cache_capacity: usize,
        cache_ttl: Duration,
    ) -> Self {
        Self {
            extractors,
            links,
            policy,
            cache: Mutex::new(HashMap::new()),
            sequence: AtomicU64::new(0),
            cache_capacity: cache_capacity.max(1),
            cache_ttl,
            pull_requests: None,
            images: None,
        }
    }

    pub fn with_pull_requests(mut self, source: Arc<dyn PullRequestSource>) -> Self {
        self.pull_requests = Some(source);
        self
    }

    pub fn with_images(mut self, source: Arc<dyn ImageSource>) -> Self {
        self.images = Some(source);
        self
    }

    fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }

    /// Fresh cache entry (within TTL). Bumps the entry's LRU sequence.
    fn cached_fresh(&self, key: &str) -> Option<(Arc<ExtractedDocument>, u64)> {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = cache.get_mut(key)?;
        let age = entry.fetched_at.elapsed().as_secs();
        if entry.fetched_at.elapsed() >= self.cache_ttl {
            return None;
        }
        entry.sequence = self.next_sequence();
        Some((Arc::clone(&entry.document), age))
    }

    /// Any cache entry for the key, even expired (stale-if-error).
    fn cached_any(&self, key: &str) -> Option<(Arc<ExtractedDocument>, u64)> {
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = cache.get(key)?;
        Some((Arc::clone(&entry.document), entry.fetched_at.elapsed().as_secs()))
    }

    fn cache_insert(&self, key: String, document: Arc<ExtractedDocument>) {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let sequence = self.next_sequence();
        cache.insert(
            key,
            CacheEntry {
                fetched_at: Instant::now(),
                sequence,
                document,
            },
        );
        while cache.len() > self.cache_capacity {
            let oldest = cache
                .iter()
                .min_by_key(|(_, entry)| entry.sequence)
                .map(|(key, _)| key.clone());
            match oldest {
                Some(key) => {
                    cache.remove(&key);
                }
                None => break,
            }
        }
    }

    pub async fn fetch(&self, request: FetchRequest) -> Result<FetchedPage, FetchError> {
        validate_fetch_url(&request.url, &self.policy)?;
        let canonical = canonicalize_url(request.url.trim());
        let key = cache_key(&canonical);

        let mut stale = false;
        let (document, from_cache, cached_age_s) = match self.cached_fresh(&key) {
            Some((document, age)) => (document, true, Some(age)),
            None => {
                let mut errors = Vec::with_capacity(self.extractors.len());
                let mut fresh: Option<Arc<ExtractedDocument>> = None;
                for extractor in &self.extractors {
                    match extractor.extract(&canonical).await {
                        Ok(candidate) if !candidate.content.trim().is_empty() => {
                            fresh = Some(Arc::new(candidate));
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

                match fresh {
                    Some(document) => {
                        self.cache_insert(key, Arc::clone(&document));
                        (document, false, None)
                    }
                    None if self.extractors.is_empty() => return Err(FetchError::NoExtractors),
                    // Stale-if-error: an expired cache entry beats a hard failure.
                    None => match self.cached_any(&key) {
                        Some((document, age)) => {
                            tracing::warn!(age_s = age, "serving stale cached extraction after upstream failures");
                            stale = true;
                            (document, true, Some(age))
                        }
                        None => return Err(FetchError::AllExtractorsFailed(errors)),
                    },
                }
            }
        };

        let links = if request.include_links {
            match self.links.links(&canonical, false).await {
                Ok(links) => links,
                Err(error) => {
                    tracing::warn!(error = %error, "link discovery failed");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        let github = match (&self.pull_requests, parse_github_pull(&canonical)) {
            (Some(source), Some((owner, repo, number))) => {
                match source.pull(&owner, &repo, number).await {
                    Ok(info) => Some(info),
                    Err(error) => {
                        tracing::warn!(error = %error, "github pull request enrichment failed");
                        None
                    }
                }
            }
            _ => None,
        };

        // Multimodal delivery: download the first N content images referenced
        // by the extracted markdown (icons/badges/trackers filtered out).
        let mut images = Vec::new();
        if request.max_images > 0
            && let Some(source) = &self.images
        {
            let candidates =
                markdown_images(&document.content, &canonical, request.max_images * 3);
            for candidate in candidates {
                if images.len() >= request.max_images {
                    break;
                }
                match source.image(&candidate).await {
                    Ok(image) => images.push(image),
                    Err(error) => {
                        tracing::debug!(url = %candidate, error = %error, "image skipped");
                    }
                }
            }
        }

        let outline_entries = if request.outline {
            textsearch::outline(&document.content)
        } else {
            Vec::new()
        };
        let (matches, match_count) = match &request.pattern {
            Some(pattern) => textsearch::find_windows(
                &document.content,
                pattern,
                request.context_chars,
                20,
            )
            .map_err(FetchError::BadPattern)?,
            None => (Vec::new(), 0),
        };

        // In search mode the matched windows (with their start_char offsets)
        // are the payload; body paging continues via start_char afterwards.
        let search_mode = request.pattern.is_some() || request.outline;
        let total_chars = document.content.chars().count();
        let (content, truncated, next_start_char) = if search_mode {
            (String::new(), false, None)
        } else {
            let chars: Vec<char> = document.content.chars().collect();
            let start = request.start_char.min(total_chars);
            let end = start.saturating_add(request.max_chars).min(total_chars);
            let content: String = chars[start..end].iter().collect();
            let truncated = end < total_chars;
            (
                content,
                truncated,
                if truncated { Some(end) } else { None },
            )
        };

        Ok(FetchedPage {
            url: request.url,
            final_url: document.final_url.clone(),
            title: document.title.clone(),
            format: document.format,
            returned_chars: content.chars().count(),
            content,
            truncated,
            next_start_char,
            links,
            extractor: document.provider.clone(),
            max_chars: request.max_chars,
            total_chars,
            from_cache,
            cached_age_s,
            stale,
            matches,
            match_count,
            outline: outline_entries,
            github,
            images,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedExtractor {
        fail: std::sync::atomic::AtomicBool,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FixedExtractor {
        fn new() -> Self {
            Self {
                fail: std::sync::atomic::AtomicBool::new(false),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl ExtractProvider for FixedExtractor {
        fn name(&self) -> &'static str {
            "fixed"
        }
        fn extract<'a>(
            &'a self,
            url: &'a str,
        ) -> ProviderFuture<'a, Result<ExtractedDocument, ProviderError>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if self.fail.load(Ordering::SeqCst) {
                    return Err(ProviderError::unavailable("fixed", "down"));
                }
                Ok(ExtractedDocument {
                    provider: "fixed".to_string(),
                    final_url: url.to_string(),
                    title: Some("Doc".to_string()),
                    content: "# A\nalpha slot-save-path beta\n\n## B\ntail".to_string(),
                    format: ContentFormat::Markdown,
                })
            })
        }
    }

    struct NoLinks;
    impl LinkSource for NoLinks {
        fn links<'a>(
            &'a self,
            _url: &'a str,
            _include_external: bool,
        ) -> ProviderFuture<'a, Result<Vec<Link>, ProviderError>> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    fn request(url: &str) -> FetchRequest {
        FetchRequest {
            url: url.to_string(),
            max_chars: 1000,
            start_char: 0,
            include_links: false,
            pattern: None,
            context_chars: 60,
            outline: false,
            max_images: 0,
        }
    }

    fn service(extractor: FixedExtractor, ttl: Duration) -> FetchService {
        FetchService::new(
            vec![Arc::new(extractor)],
            Arc::new(NoLinks),
            UrlPolicy::default(),
            4,
            ttl,
        )
    }

    #[test]
    fn policy_blocks_private_hosts() {
        let policy = UrlPolicy::default();
        for url in [
            "http://localhost:8080/admin",
            "http://127.1.2.3/x",
            "http://10.0.0.5/",
            "http://192.168.1.1/login",
            "http://169.254.169.254/latest/meta-data",
            "http://100.100.1.1/",
            "http://db.corp.internal/x",
            "http://[::1]/",
            "http://[fe80::1]/",
            "http://metadata.internal/",
        ] {
            assert!(
                validate_fetch_url(url, &policy).is_err(),
                "should block {url}"
            );
        }
        for url in [
            "https://docs.rs/axum",
            "https://raw.githubusercontent.com/a/b/c/README.md",
        ] {
            assert!(
                validate_fetch_url(url, &policy).is_ok(),
                "should allow {url}"
            );
        }
        assert!(validate_fetch_url(
            "http://localhost:8080/admin",
            &UrlPolicy {
                allow_private_network: true
            }
        )
        .is_ok());
    }

    #[test]
    fn policy_rejects_credentials_and_bad_schemes() {
        let policy = UrlPolicy::default();
        assert!(matches!(
            validate_fetch_url("https://user:pw@example.com/", &policy),
            Err(FetchError::UrlRejected(_))
        ));
        assert!(matches!(
            validate_fetch_url("ftp://example.com/", &policy),
            Err(FetchError::UnsupportedScheme(_))
        ));
    }

    #[test]
    fn github_blob_is_canonicalized_to_raw() {
        assert_eq!(
            canonicalize_url("https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md"),
            "https://raw.githubusercontent.com/ggml-org/llama.cpp/master/tools/server/README.md"
        );
        // extension-less README is canonicalized too
        assert_eq!(
            canonicalize_url("https://github.com/a/b/blob/main/README"),
            "https://raw.githubusercontent.com/a/b/main/README"
        );
        // binary or image blobs are left alone
        assert_eq!(
            canonicalize_url("https://github.com/a/b/blob/main/logo.png"),
            "https://github.com/a/b/blob/main/logo.png"
        );
        // non-github untouched
        assert_eq!(
            canonicalize_url("https://example.com/blob/x.md"),
            "https://example.com/blob/x.md"
        );
    }

    #[test]
    fn parses_pull_request_urls() {
        assert_eq!(
            parse_github_pull("https://github.com/ggml-org/llama.cpp/pull/28092"),
            Some(("ggml-org".to_string(), "llama.cpp".to_string(), 28092))
        );
        assert_eq!(
            parse_github_pull("https://github.com/a/b/issues/1"),
            None
        );
    }

    #[tokio::test]
    async fn cache_serves_second_fetch_without_extractor() {
        let extractor = FixedExtractor::new();
        let service = service(extractor, Duration::from_secs(60));
        let first = service.fetch(request("https://example.com/doc")).await.unwrap();
        let second = service.fetch(request("https://example.com/doc")).await.unwrap();
        assert!(!first.from_cache);
        assert!(second.from_cache);
        assert_eq!(second.cached_age_s, Some(0));
        assert_eq!(first.total_chars, second.total_chars);
        assert_eq!(second.extractor, "fixed");
    }

    #[tokio::test]
    async fn serves_stale_cache_when_all_extractors_fail() {
        let extractor = Arc::new(FixedExtractor::new());
        let service = FetchService::new(
            vec![Arc::clone(&extractor) as Arc<dyn ExtractProvider>],
            Arc::new(NoLinks),
            UrlPolicy::default(),
            4,
            Duration::ZERO, // every cache entry is instantly expired
        );
        let first = service
            .fetch(request("https://example.com/stale"))
            .await
            .unwrap();
        assert!(!first.stale && !first.from_cache);

        extractor.fail.store(true, Ordering::SeqCst);
        let second = service
            .fetch(request("https://example.com/stale"))
            .await
            .unwrap();
        assert!(second.stale);
        assert!(second.from_cache);
        assert_eq!(second.content, first.content);
    }

    #[tokio::test]
    async fn pattern_search_and_paging_offsets() {
        let service = service(FixedExtractor::new(), Duration::from_secs(60));
        let mut req = request("https://example.com/pattern");
        req.pattern = Some("slot-save-path".to_string());
        req.context_chars = 5;
        let page = service.fetch(req).await.unwrap();
        assert_eq!(page.match_count, 1);
        assert_eq!(page.matches.len(), 1);
        assert!(page.matches[0].snippet.contains("slot-save-path"));
        assert!(page.content.is_empty(), "search mode returns no body");

        // The window offset can be used for follow-up paging.
        let start = page.matches[0].start_char;
        let mut req = request("https://example.com/pattern");
        req.start_char = start;
        let page = service.fetch(req).await.unwrap();
        assert!(page.content.contains("slot-save-path"), "{}", page.content);
    }

    #[tokio::test]
    async fn invalid_pattern_is_invalid_input() {
        let service = service(FixedExtractor::new(), Duration::from_secs(60));
        let mut req = request("https://example.com/bad");
        req.pattern = Some("(unclosed".to_string());
        assert!(matches!(
            service.fetch(req).await,
            Err(FetchError::BadPattern(_))
        ));
    }

    #[tokio::test]
    async fn outline_returns_heading_offsets() {
        let service = service(FixedExtractor::new(), Duration::from_secs(60));
        let mut req = request("https://example.com/outline");
        req.outline = true;
        let page = service.fetch(req).await.unwrap();
        assert_eq!(page.outline.len(), 2);
        assert_eq!(page.outline[0].title, "A");
        assert_eq!(page.outline[1].title, "B");
    }

    #[tokio::test]
    async fn blocked_host_never_reaches_extractor() {
        let extractor = FixedExtractor::new();
        let service = FetchService::new(
            vec![Arc::new(extractor)],
            Arc::new(NoLinks),
            UrlPolicy::default(),
            4,
            Duration::from_secs(60),
        );
        let error = service
            .fetch(request("http://localhost:9/"))
            .await
            .unwrap_err();
        assert!(matches!(error, FetchError::BlockedHost(_)));
    }

    #[test]
    fn markdown_images_extracts_content_candidates_only() {
        let markdown = "![Architecture](https://example.com/img/arch.png) \n
            ![](https://example.com/relative.svg) \n
            ![badge](https://img.shields.io/v.svg) \n
            ![avatar](https://gravatar.com/avatar/abc) \n
            ![setup](/docs/setup.jpg) \n
            ![Architecture](https://example.com/img/arch.png)";
        let images = markdown_images(markdown, "https://example.com/page.html", 5);
        assert_eq!(
            images,
            vec![
                "https://example.com/img/arch.png",
                "https://example.com/docs/setup.jpg"
            ]
        );
    }
}
