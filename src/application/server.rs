//! MCP server handler: exposes the domain services as MCP tools.
//!
//! The `#[tool]` / `#[tool_router]` / `#[tool_handler]` macros are the mechanism
//! recommended by the `rmcp` authors. Each tool method stays intentionally thin:
//! argument DTOs and use-case glue live in [`crate::application::tools`].

use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, tool, tool_handler, tool_router};

use crate::application::tools::{
    CurrentDatetimeArgs, ExploreSiteArgs, FetchUrlArgs, ToolError, WebSearchArgs,
    run_current_datetime, run_explore_site, run_fetch_url, run_web_search,
};
use crate::domain::clock::TimeService;
use crate::domain::explore::ExploreService;
use crate::domain::fetch::FetchService;
use crate::domain::search::WebSearchService;

#[derive(Clone)]
pub struct LocalToolsServer {
    search: Arc<WebSearchService>,
    fetch: Arc<FetchService>,
    explore: Arc<ExploreService>,
    time: Arc<TimeService>,
    disabled_tools: Arc<Vec<String>>,
}

#[tool_router]
impl LocalToolsServer {
    pub fn new(
        search: Arc<WebSearchService>,
        fetch: Arc<FetchService>,
        explore: Arc<ExploreService>,
        time: Arc<TimeService>,
        disabled_tools: Vec<String>,
    ) -> Self {
        Self {
            search,
            fetch,
            explore,
            time,
            disabled_tools: Arc::new(disabled_tools),
        }
    }

    #[tool(
        description = "Search the public web for current information. Use this whenever the answer may be newer than your training data, is missing from your knowledge, or needs a source. Returns ranked results with title, URL and snippet; follow up with fetch_url to read a promising page in full."
    )]
    async fn web_search(
        &self,
        Parameters(args): Parameters<WebSearchArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_enabled("web_search")?;
        let output = run_web_search(&self.search, args)
            .await
            .map_err(ToolError::into_error_data)?;
        to_json_result(&output)
    }

    #[tool(
        description = "Download and read a single web page as clean readable text/markdown. Use for documentation, changelogs, release notes, articles or any specific URL. Long pages are paged: use next_start_char with start_char to continue. Set include_links=true to also list the page's links and explore around the destination."
    )]
    async fn fetch_url(
        &self,
        Parameters(args): Parameters<FetchUrlArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_enabled("fetch_url")?;
        let output = run_fetch_url(&self.fetch, args)
            .await
            .map_err(ToolError::into_error_data)?;
        to_json_result(&output)
    }

    #[tool(
        description = "Explore around a destination: discover pages on a website. Provide `query` to run a site-restricted search, and/or let it list the pages linked from the target plus URLs found in robots.txt/sitemap.xml. Useful to map documentation, find related pages, or locate content on a site you already know."
    )]
    async fn explore_site(
        &self,
        Parameters(args): Parameters<ExploreSiteArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_enabled("explore_site")?;
        let output = run_explore_site(&self.explore, args)
            .await
            .map_err(ToolError::into_error_data)?;
        to_json_result(&output)
    }

    #[tool(
        description = "Get the current local date and time. Call this whenever the exact current date/time matters (scheduling, 'today', 'latest', age/expiry, relative time references) - models do not know the current time. Returns the date, time, weekday, timezone and UTC offset. Pass `timezone` (IANA name or UTC offset) or `locale` (e.g. 'de-DE', 'US') when the relevant region is known; otherwise a default region is assumed and flagged in the response."
    )]
    async fn current_datetime(
        &self,
        Parameters(args): Parameters<CurrentDatetimeArgs>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_enabled("current_datetime")?;
        let request_locale = request_locale_from_context(&context);
        let output = run_current_datetime(&self.time, args, request_locale)
            .map_err(ToolError::into_error_data)?;
        to_json_result(&output)
    }

    fn ensure_enabled(&self, name: &str) -> Result<(), McpError> {
        if self.disabled_tools.iter().any(|tool| tool == name) {
            Err(McpError::invalid_params(
                format!("tool '{name}' is disabled in config"),
                None,
            ))
        } else {
            Ok(())
        }
    }
}

/// Best-effort region clue from the HTTP `Accept-Language` header (browser clients).
fn request_locale_from_context(context: &RequestContext<RoleServer>) -> Option<String> {
    let parts = context.extensions.get::<axum::http::request::Parts>()?;
    let header = parts
        .headers
        .get(axum::http::header::ACCEPT_LANGUAGE)?
        .to_str()
        .ok()?;
    header
        .split(',')
        .next()
        .map(|entry| entry.split(';').next().unwrap_or(entry).trim().to_string())
        .filter(|value| !value.is_empty())
}

#[tool_handler]
impl ServerHandler for LocalToolsServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Local tools for grounding. `current_datetime` returns the real current date/time (call it before answering anything time-dependent). `web_search` finds current information beyond your knowledge cutoff, `fetch_url` reads a specific page (optionally listing its links), and `explore_site` discovers pages around a destination. Search providers are free/keyless; results may be rate-limited, so retry or adjust the query if a tool reports a provider failure.",
            )
    }
}

fn to_json_result<T: serde::Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string_pretty(value).map_err(|error| {
        McpError::internal_error(format!("failed to serialize tool result: {error}"), None)
    })?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}
