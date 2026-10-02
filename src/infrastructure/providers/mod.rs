//! Concrete provider adapters and the factory that wires the configured chains.

pub mod direct;
pub mod duckduckgo;
pub mod firecrawl;
pub mod tavily;
pub mod wikipedia;

use std::sync::Arc;

use super::http::HttpClient;
use crate::config::AppConfig;
use crate::domain::fetch::ExtractProvider;
use crate::domain::search::SearchProvider;

pub use direct::DirectFetcher;

/// Build the ordered search-provider chain from configuration.
pub fn build_search_providers(
    config: &AppConfig,
    http: &HttpClient,
) -> Vec<Arc<dyn SearchProvider>> {
    let mut providers: Vec<Arc<dyn SearchProvider>> = Vec::new();

    for name in &config.providers.order {
        match name.as_str() {
            "tavily" => {
                if !config.providers.tavily.enabled {
                    tracing::debug!("tavily search provider disabled by config");
                    continue;
                }
                providers.push(Arc::new(tavily::TavilySearchProvider::new(
                    http.clone(),
                    config.providers.tavily.resolved_key("TAVILY_API_KEY"),
                )));
            }
            "firecrawl" => {
                if !config.providers.firecrawl.enabled {
                    tracing::debug!("firecrawl search provider disabled by config");
                    continue;
                }
                providers.push(Arc::new(firecrawl::FirecrawlSearchProvider::new(
                    http.clone(),
                    config.providers.firecrawl.resolved_key("FIRECRAWL_API_KEY"),
                )));
            }
            "duckduckgo" => {
                if !config.providers.duckduckgo.enabled {
                    tracing::debug!("duckduckgo search provider disabled by config");
                    continue;
                }
                if config.usage.commercial {
                    tracing::warn!(
                        "duckduckgo search provider disabled: HTML scraping is not licensed for commercial use"
                    );
                    continue;
                }
                providers.push(Arc::new(duckduckgo::DuckDuckGoSearchProvider::new(
                    http.clone(),
                )));
            }
            "wikipedia" => {
                if !config.providers.wikipedia.enabled {
                    tracing::debug!("wikipedia search provider disabled by config");
                    continue;
                }
                providers.push(Arc::new(wikipedia::WikipediaSearchProvider::new(
                    http.clone(),
                )));
            }
            unknown => tracing::warn!(provider = unknown, "unknown search provider; ignoring"),
        }
    }

    providers
}

/// Build the ordered content-extraction chain from configuration.
pub fn build_extract_providers(
    config: &AppConfig,
    http: &HttpClient,
    direct: &Arc<DirectFetcher>,
) -> Vec<Arc<dyn ExtractProvider>> {
    let mut providers: Vec<Arc<dyn ExtractProvider>> = Vec::new();

    for name in &config.providers.extract_order {
        match name.as_str() {
            "tavily" => {
                if !config.providers.tavily.enabled {
                    tracing::debug!("tavily extract provider disabled by config");
                    continue;
                }
                providers.push(Arc::new(tavily::TavilyExtractProvider::new(
                    http.clone(),
                    config.providers.tavily.resolved_key("TAVILY_API_KEY"),
                )));
            }
            "firecrawl" => {
                if !config.providers.firecrawl.enabled {
                    tracing::debug!("firecrawl extract provider disabled by config");
                    continue;
                }
                providers.push(Arc::new(firecrawl::FirecrawlScrapeProvider::new(
                    http.clone(),
                    config.providers.firecrawl.resolved_key("FIRECRAWL_API_KEY"),
                )));
            }
            "direct" => {
                let extractor: Arc<dyn ExtractProvider> = direct.clone();
                providers.push(extractor);
            }
            unknown => tracing::warn!(provider = unknown, "unknown extract provider; ignoring"),
        }
    }

    providers
}
