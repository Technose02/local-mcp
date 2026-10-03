//! Tool arguments, result DTOs and the thin glue between MCP and the domain.

use std::fmt;

use rmcp::ErrorData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::domain::clock::{TimeQuery, TimeReport, TimeService};
use crate::domain::explore::{ExploreRequest, ExploreService};
use crate::domain::fetch::{FetchRequest, FetchService, Link};
use crate::domain::search::{Freshness, SearchOutcome, WebSearchService};

/// Errors surfaced by a tool invocation.
#[derive(Debug)]
pub enum ToolError {
    InvalidInput(String),
    Failed(String),
}

impl ToolError {
    pub fn into_error_data(self) -> ErrorData {
        match self {
            Self::InvalidInput(message) => ErrorData::invalid_params(message, None),
            Self::Failed(message) => ErrorData::internal_error(message, None),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    pub results: Vec<SearchHitDto>,
}

impl From<SearchOutcome> for WebSearchOutput {
    fn from(outcome: SearchOutcome) -> Self {
        Self {
            provider: outcome.provider,
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
) -> Result<WebSearchOutput, ToolError> {
    let text = args.query.trim().to_string();
    if text.is_empty() {
        return Err(ToolError::InvalidInput(
            "query must not be empty".to_string(),
        ));
    }
    let query = crate::domain::search::SearchQuery {
        text,
        max_results: args.max_results.clamp(1, 20) as usize,
        site: clean_optional(args.site),
        freshness: args.freshness.into(),
        language: clean_optional(args.language),
    };
    service
        .search(query)
        .await
        .map(WebSearchOutput::from)
        .map_err(|error| ToolError::Failed(error.to_string()))
}

// ---------------------------------------------------------------------------
// fetch_url
// ---------------------------------------------------------------------------

fn default_fetch_chars() -> u32 {
    20_000
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FetchUrlArgs {
    /// Absolute http(s) URL of the page to read.
    pub url: String,
    /// Maximum number of content characters to return (200..=100000).
    #[serde(default = "default_fetch_chars")]
    pub max_chars: u32,
    /// Character offset to start from. Pass the previous `next_start_char` to continue.
    #[serde(default)]
    pub start_char: u32,
    /// Also return the links found on the page (useful to explore around it).
    #[serde(default)]
    pub include_links: bool,
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
pub struct FetchUrlOutput {
    pub url: String,
    pub final_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub content_format: String,
    pub content: String,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_start_char: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<LinkDto>,
}

pub async fn run_fetch_url(
    service: &FetchService,
    args: FetchUrlArgs,
) -> Result<FetchUrlOutput, ToolError> {
    let request = FetchRequest {
        url: args.url.trim().to_string(),
        max_chars: args.max_chars.clamp(200, 100_000) as usize,
        start_char: args.start_char as usize,
        include_links: args.include_links,
    };
    let page = service
        .fetch(request)
        .await
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    Ok(FetchUrlOutput {
        url: page.url,
        final_url: page.final_url,
        title: page.title,
        content_format: page.format.as_str().to_string(),
        content: page.content,
        truncated: page.truncated,
        next_start_char: page.next_start_char,
        links: page.links.into_iter().map(LinkDto::from).collect(),
    })
}

// ---------------------------------------------------------------------------
// explore_site
// ---------------------------------------------------------------------------

fn default_explore_results() -> u8 {
    20
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExploreSiteArgs {
    /// Domain or URL to explore, e.g. "docs.rs" or "https://docs.rs/tokio".
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<WebSearchOutput>,
    pub pages: Vec<LinkDto>,
    pub sitemap_urls: Vec<String>,
}

pub async fn run_explore_site(
    service: &ExploreService,
    args: ExploreSiteArgs,
) -> Result<ExploreSiteOutput, ToolError> {
    let request = ExploreRequest {
        target: args.target.trim().to_string(),
        query: clean_optional(args.query),
        max_results: args.max_results.clamp(1, 50) as usize,
        include_external: args.include_external,
        freshness: args.freshness.into(),
        language: clean_optional(args.language),
    };
    let result = service
        .explore(request)
        .await
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    Ok(ExploreSiteOutput {
        domain: result.domain,
        base_url: result.base_url,
        search: result.search.map(WebSearchOutput::from),
        pages: result.pages.into_iter().map(LinkDto::from).collect(),
        sitemap_urls: result.sitemap_urls,
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
