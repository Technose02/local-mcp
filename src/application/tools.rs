//! Tool arguments, result DTOs and the thin glue between MCP and the domain.

use std::fmt;

use rmcp::ErrorData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::domain::clock::{TimeQuery, TimeReport, TimeService};
use crate::domain::explore::{ExploreRequest, ExploreService};
use crate::domain::fetch::{FetchRequest, FetchService, Link};
use crate::domain::search::{Freshness, SearchOutcome, SearchQuery, WebSearchService};
use crate::domain::textsearch::{OutlineEntry, TextMatch};
use crate::infrastructure::sanitize::redact;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

/// Errors surfaced by a tool invocation.
#[derive(Debug)]
pub enum ToolError {
    InvalidInput(String),
    Failed(String),
}

impl ToolError {
    pub fn into_error_data(self) -> ErrorData {
        match self {
            Self::InvalidInput(message) => ErrorData::invalid_params(redact(&message), None),
            // Defense in depth: provider errors are constructed redacted, but
            // nothing reaches the model without passing through `redact`.
            Self::Failed(message) => ErrorData::internal_error(redact(&message), None),
        }
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) => write!(f, "invalid input: {message}"),
            Self::Failed(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ToolError {}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

// Today's date (`YYYY-MM-DD`) is injected into every substantial response so
// the model never needs a separate clock call (and freshness filtering is
// sound).

// ---------------------------------------------------------------------------
// web_search
// ---------------------------------------------------------------------------

/// Recency filter accepted by the `web_search` / `explore_site` tools.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FreshnessArg {
    #[default]
    Any,
    Day,
    Week,
    Month,
    Year,
}

impl From<FreshnessArg> for Freshness {
    fn from(value: FreshnessArg) -> Self {
        match value {
            FreshnessArg::Any => Self::Any,
            FreshnessArg::Day => Self::Day,
            FreshnessArg::Week => Self::Week,
            FreshnessArg::Month => Self::Month,
            FreshnessArg::Year => Self::Year,
        }
    }
}

fn default_max_results() -> u8 {
    8
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WebSearchArgs {
    /// The search query. Prefer specific keywords; use `site` to restrict a domain.
    pub query: String,
    /// Maximum number of results to return (1..=20).
    #[serde(default = "default_max_results")]
    pub max_results: u8,
    /// Restrict results to a single domain, e.g. "docs.rs".
    #[serde(default)]
    pub site: Option<String>,
    /// Only return results newer than this.
    #[serde(default)]
    pub freshness: FreshnessArg,
    /// ISO date (YYYY-MM-DD): drop results published before it.
    #[serde(default)]
    pub since: Option<String>,
    /// Domain suffixes to drop from the results, e.g. ["whatsnew.fyi"].
    #[serde(default)]
    pub exclude_domains: Option<Vec<String>>,
    /// Optional language/region hint, e.g. "en-US" or "de".
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchHitDto {
    pub rank: usize,
    pub title: String,
    pub url: String,
    pub snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_date: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WebSearchOutput {
    pub provider: String,
    pub today: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    pub results: Vec<SearchHitDto>,
}

impl WebSearchOutput {
    fn new(outcome: SearchOutcome, today: &str) -> Self {
        Self {
            provider: outcome.provider,
            today: today.to_string(),
            answer: outcome.answer,
            results: outcome
                .results
                .into_iter()
                .enumerate()
                .map(|(index, hit)| SearchHitDto {
                    rank: index + 1,
                    title: hit.title,
                    url: hit.url,
                    snippet: hit.snippet,
                    published_date: hit.published_date,
                })
                .collect(),
        }
    }
}

pub async fn run_web_search(
    service: &WebSearchService,
    args: WebSearchArgs,
    today: &str,
) -> Result<WebSearchOutput, ToolError> {
    let text = args.query.trim().to_string();
    if text.is_empty() {
        return Err(ToolError::InvalidInput(
            "query must not be empty".to_string(),
        ));
    }
    let had_since = args.since.is_some();
    let since = clean_optional(args.since).filter(|since| {
        since.len() == 10
            && since.as_bytes()[4] == b'-'
            && since.as_bytes()[7] == b'-'
            && since
                .bytes()
                .enumerate()
                .all(|(i, b)| if matches!(i, 4 | 7) { b == b'-' } else { b.is_ascii_digit() })
    });
    if had_since && since.is_none() {
        return Err(ToolError::InvalidInput(
            "since must be an ISO date like 2026-06-01".to_string(),
        ));
    }
    let query = SearchQuery {
        text,
        max_results: args.max_results.clamp(1, 20) as usize,
        site: clean_optional(args.site),
        freshness: args.freshness.into(),
        language: clean_optional(args.language),
        since,
        exclude_domains: args
            .exclude_domains
            .unwrap_or_default()
            .into_iter()
            .map(|domain| domain.trim().to_ascii_lowercase())
            .filter(|domain| !domain.is_empty())
            .collect(),
    };
    service
        .search(query)
        .await
        .map(|outcome| WebSearchOutput::new(outcome, today))
        .map_err(|error| ToolError::Failed(error.to_string()))
}

// ---------------------------------------------------------------------------
// fetch_url
// ---------------------------------------------------------------------------

fn default_fetch_chars() -> u32 {
    20_000
}

fn default_pattern_context() -> u32 {
    160
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FetchUrlArgs {
    /// Absolute http(s) URL of the page to read. GitHub `blob/` URLs of text
    /// files are automatically fetched via raw.githubusercontent.com; pull
    /// request URLs come back with merge state attached.
    pub url: String,
    /// Maximum number of content characters to return (200..=100000).
    /// The effective value is echoed back as `max_chars`.
    #[serde(default = "default_fetch_chars")]
    pub max_chars: u32,
    /// Character offset to start from. Pass the previous `next_start_char` or a
    /// match's `start_char` to continue.
    #[serde(default)]
    pub start_char: u32,
    /// Also return the links found on the page (useful to explore around it).
    #[serde(default)]
    pub include_links: bool,
    /// Case-insensitive regex to search *inside* the page. Returns matching
    /// windows (with offsets) instead of the body — grep without paying for
    /// the whole document.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Context characters around each `pattern` match (default 160).
    #[serde(default = "default_pattern_context")]
    pub context: u32,
    /// Return the markdown heading outline (titles + start_char offsets)
    /// instead of the body; page into sections with `start_char`.
    #[serde(default)]
    pub outline: bool,
    /// For multimodal clients: also download up to N content images from the
    /// page (0..=3, default 0) and return them as image content blocks
    /// (icons/badges/trackers are filtered; large images are resized).
    #[serde(default)]
    pub max_images: u8,
}

#[derive(Debug, Serialize)]
pub struct LinkDto {
    pub text: String,
    pub url: String,
}

impl From<Link> for LinkDto {
    fn from(link: Link) -> Self {
        Self {
            text: link.text,
            url: link.url,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct MatchDto {
    pub start_char: usize,
    pub end_char: usize,
    pub snippet: String,
}

impl From<TextMatch> for MatchDto {
    fn from(matched: TextMatch) -> Self {
        Self {
            start_char: matched.start_char,
            end_char: matched.end_char,
            snippet: matched.snippet,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct OutlineEntryDto {
    pub level: u8,
    pub title: String,
    pub start_char: usize,
}

impl From<OutlineEntry> for OutlineEntryDto {
    fn from(entry: OutlineEntry) -> Self {
        Self {
            level: entry.level,
            title: entry.title,
            start_char: entry.start_char,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct GithubPrDto {
    pub kind: &'static str,
    pub number: u64,
    pub title: String,
    pub state: String,
    pub draft: bool,
    pub merged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    pub comments: u64,
}

#[derive(Debug, Serialize)]
pub struct ImageMetaDto {
    pub url: String,
    pub media_type: String,
    /// Encoded image size in bytes (the base64 payload rides as a separate
    /// MCP content block, listed here for accounting).
    pub bytes: usize,
}

/// An image to return as an MCP `ImageContent` block next to the JSON text.
pub struct InlineImage {
    pub data_base64: String,
    pub media_type: String,
}

#[derive(Debug, Serialize)]
pub struct FetchUrlOutput {
    pub url: String,
    pub final_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub content_format: String,
    /// Extractor that produced the content (provenance).
    pub extractor: String,
    pub today: String,
    pub content: String,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_start_char: Option<usize>,
    /// Effective paging parameters of this call.
    pub max_chars: usize,
    pub returned_chars: usize,
    /// Total length of the extracted document.
    pub total_chars: usize,
    pub from_cache: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_age_s: Option<u64>,
    /// Served from cache because every live extractor failed.
    pub stale: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<MatchDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_count: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub outline: Vec<OutlineEntryDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github: Option<GithubPrDto>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageMetaDto>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<LinkDto>,
}

pub async fn run_fetch_url(
    service: &FetchService,
    args: FetchUrlArgs,
    today: &str,
) -> Result<(FetchUrlOutput, Vec<InlineImage>), ToolError> {
    let pattern = clean_optional(args.pattern);
    let request = FetchRequest {
        url: args.url.trim().to_string(),
        max_chars: args.max_chars.clamp(200, 100_000) as usize,
        start_char: args.start_char as usize,
        include_links: args.include_links,
        pattern,
        context_chars: args.context.clamp(20, 2000) as usize,
        outline: args.outline,
        max_images: args.max_images.clamp(0, 3) as usize,
    };
    let page = service
        .fetch(request)
        .await
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    let matched_any = !page.matches.is_empty();
    let match_count = page.match_count;
    let images: Vec<InlineImage> = page
        .images
        .iter()
        .map(|image| InlineImage {
            data_base64: BASE64.encode(&image.bytes),
            media_type: image.media_type.clone(),
        })
        .collect();
    let image_meta: Vec<ImageMetaDto> = page
        .images
        .iter()
        .map(|image| ImageMetaDto {
            url: image.url.clone(),
            media_type: image.media_type.clone(),
            bytes: image.bytes.len(),
        })
        .collect();
    Ok((
        FetchUrlOutput {
        url: page.url,
        final_url: page.final_url,
        title: page.title,
        content_format: page.format.as_str().to_string(),
        extractor: page.extractor,
        today: today.to_string(),
        content: page.content,
        truncated: page.truncated,
        next_start_char: page.next_start_char,
        max_chars: page.max_chars,
        returned_chars: page.returned_chars,
        total_chars: page.total_chars,
        from_cache: page.from_cache,
        cached_age_s: page.cached_age_s,
        stale: page.stale,
        matches: page.matches.into_iter().map(MatchDto::from).collect(),
        match_count: if match_count > 0 || matched_any {
            Some(match_count)
        } else {
            None
        },
        outline: page.outline.into_iter().map(OutlineEntryDto::from).collect(),
        github: page.github.map(|info| GithubPrDto {
            kind: "pull_request",
            number: info.number,
            title: info.title,
            state: info.state,
            draft: info.draft,
            merged: info.merged,
            merged_at: info.merged_at,
            closed_at: info.closed_at,
            comments: info.comments,
        }),
        links: page.links.into_iter().map(LinkDto::from).collect(),
        images: image_meta,
    },
        images,
    ))
}

// ---------------------------------------------------------------------------
// explore_site
// ---------------------------------------------------------------------------

fn default_explore_results() -> u8 {
    20
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExploreSiteArgs {
    /// Domain or URL to explore, e.g. "docs.rs" or `https://docs.rs/tokio`.
    pub target: String,
    /// Optional search query restricted to the target site.
    #[serde(default)]
    pub query: Option<String>,
    /// Maximum number of discovered pages to return (1..=50).
    #[serde(default = "default_explore_results")]
    pub max_results: u8,
    /// Also include links that leave the target domain.
    #[serde(default)]
    pub include_external: bool,
    /// Also return the site's sitemap URLs (capped sample; costs a lot of
    /// tokens, only enable when link mapping really needs it).
    #[serde(default)]
    pub include_sitemap: bool,
    /// Recency filter for the site-restricted search.
    #[serde(default)]
    pub freshness: FreshnessArg,
    /// Optional language/region hint, e.g. "en-US".
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExploreSiteOutput {
    pub domain: String,
    pub base_url: String,
    pub today: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<WebSearchOutput>,
    pub pages: Vec<LinkDto>,
    pub sitemap_urls: Vec<String>,
    pub sitemap_total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub async fn run_explore_site(
    service: &ExploreService,
    args: ExploreSiteArgs,
    today: &str,
) -> Result<ExploreSiteOutput, ToolError> {
    let request = ExploreRequest {
        target: args.target.trim().to_string(),
        query: clean_optional(args.query),
        max_results: args.max_results.clamp(1, 50) as usize,
        include_external: args.include_external,
        freshness: args.freshness.into(),
        language: clean_optional(args.language),
        include_sitemap: args.include_sitemap,
    };
    let result = service
        .explore(request)
        .await
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    Ok(ExploreSiteOutput {
        domain: result.domain,
        base_url: result.base_url,
        today: today.to_string(),
        search: result
            .search
            .map(|outcome| WebSearchOutput::new(outcome, today)),
        pages: result.pages.into_iter().map(LinkDto::from).collect(),
        sitemap_urls: result.sitemap_urls,
        sitemap_total: result.sitemap_total,
        note: result.note,
    })
}

// ---------------------------------------------------------------------------
// current_datetime
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CurrentDatetimeArgs {
    /// Optional explicit timezone: an IANA name like "Europe/Berlin" or a UTC offset like "+02:00".
    #[serde(default)]
    pub timezone: Option<String>,
    /// Optional region hint from the conversation, e.g. "de-DE", "Germany", "US", "New York".
    #[serde(default)]
    pub locale: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CurrentDatetimeOutput {
    pub datetime: String,
    pub date: String,
    pub time: String,
    pub weekday: String,
    pub timezone: String,
    pub utc_offset: String,
    pub unix_seconds: i64,
    pub utc: String,
    pub timezone_source: String,
    pub certainty: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl From<TimeReport> for CurrentDatetimeOutput {
    fn from(report: TimeReport) -> Self {
        Self {
            datetime: report.datetime,
            date: report.date,
            time: report.time,
            weekday: report.weekday,
            timezone: report.timezone,
            utc_offset: report.utc_offset,
            unix_seconds: report.unix_seconds,
            utc: report.utc,
            timezone_source: report.source.as_str().to_string(),
            certainty: report.certainty.as_str().to_string(),
            note: report.note,
        }
    }
}

pub fn run_current_datetime(
    service: &TimeService,
    args: CurrentDatetimeArgs,
    request_locale: Option<String>,
) -> Result<CurrentDatetimeOutput, ToolError> {
    let query = TimeQuery {
        timezone: clean_optional(args.timezone),
        locale: clean_optional(args.locale),
        request_locale,
    };
    service
        .current(query)
        .map(CurrentDatetimeOutput::from)
        .map_err(|error| ToolError::Failed(error.to_string()))
}

/// Cheap `YYYY-MM-DD` stamp for response envelopes; empty string when the
/// clock cannot be resolved (should never happen with bundled tzdb).
pub fn today_for(service: &TimeService) -> String {
    service
        .current(TimeQuery::default())
        .map(|report| report.date)
        .unwrap_or_default()
}
