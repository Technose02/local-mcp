//! Direct HTTP fetch adapter.
//!
//! Acts as the final link in both chains: it implements the extraction port
//! (HTML -> readable text, or raw body for non-HTML), plus link and sitemap
//! discovery used by `fetch_url` and `explore_site`.

use std::collections::HashSet;

use reqwest::StatusCode;

use crate::domain::error::{ProviderError, ProviderFuture};
use crate::domain::explore::host_of;
use crate::domain::fetch::{
    ContentFormat, ExtractProvider, ExtractedDocument, Link, LinkSource, SiteMapSource,
};
use crate::infrastructure::html::{extract_links, extract_readable, extract_sitemap_locations};
use crate::infrastructure::http::HttpClient;

/// Maximum number of URLs returned from sitemaps.
const SITEMAP_URL_LIMIT: usize = 200;

#[derive(Clone)]
pub struct DirectFetcher {
    http: HttpClient,
}

impl DirectFetcher {
    pub fn new(http: HttpClient) -> Self {
        Self { http }
    }

    async fn get_text(
        &self,
        url: &str,
    ) -> Result<(StatusCode, Option<String>, String), ProviderError> {
        let request = self
            .http
            .client()
            .get(url)
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header("Accept-Language", "en-US,en;q=0.9");
        self.http.text_with_content_type(request, "direct").await
    }
}

impl ExtractProvider for DirectFetcher {
    fn name(&self) -> &'static str {
        "direct"
    }

    fn extract<'a>(
        &'a self,
        url: &'a str,
    ) -> ProviderFuture<'a, Result<ExtractedDocument, ProviderError>> {
        Box::pin(async move {
            let (status, content_type, body) = self.get_text(url).await?;
            if !status.is_success() {
                return Err(ProviderError::http(
                    "direct",
                    format!("HTTP {status} for {url}"),
                ));
            }

            let is_html = content_type
                .as_deref()
                .map(|value| value.contains("html") || value.contains("xml"))
                .unwrap_or(true);

            if is_html {
                let (title, text) = extract_readable(&body);
                if text.trim().is_empty() {
                    return Err(ProviderError::unavailable(
                        "direct",
                        "no readable text extracted",
                    ));
                }
                Ok(ExtractedDocument {
                    final_url: url.to_string(),
                    title,
                    content: text,
                    format: ContentFormat::Text,
                })
            } else {
                Ok(ExtractedDocument {
                    final_url: url.to_string(),
                    title: None,
                    content: body,
                    format: ContentFormat::Text,
                })
            }
        })
    }
}

impl LinkSource for DirectFetcher {
    fn links<'a>(
        &'a self,
        url: &'a str,
        include_external: bool,
    ) -> ProviderFuture<'a, Result<Vec<Link>, ProviderError>> {
        Box::pin(async move {
            let (status, _, body) = self.get_text(url).await?;
            if !status.is_success() {
                return Err(ProviderError::http(
                    "direct",
                    format!("HTTP {status} for {url}"),
                ));
            }
            Ok(extract_links(&body, url, include_external))
        })
    }
}

impl SiteMapSource for DirectFetcher {
    fn discover<'a>(
        &'a self,
        base_url: &'a str,
    ) -> ProviderFuture<'a, Result<Vec<String>, ProviderError>> {
        Box::pin(async move {
            let domain = host_of(base_url);
            if domain.is_empty() {
                return Err(ProviderError::unavailable("direct", "invalid base URL"));
            }
            let origin = format!("https://{domain}");

            let mut sitemaps: Vec<String> = Vec::new();
            if let Ok((status, _, body)) = self.get_text(&format!("{origin}/robots.txt")).await
                && status.is_success()
            {
                for line in body.lines() {
                    if let Some((key, value)) = line.split_once(':')
                        && key.trim().eq_ignore_ascii_case("sitemap")
                    {
                        let value = value.trim();
                        if !value.is_empty() {
                            sitemaps.push(value.to_string());
                        }
                    }
                }
            }
            if sitemaps.is_empty() {
                sitemaps.push(format!("{origin}/sitemap.xml"));
            }
            sitemaps.truncate(3);

            let mut urls = Vec::new();
            let mut seen = HashSet::new();
            'outer: for sitemap in sitemaps {
                let Ok((status, _, body)) = self.get_text(&sitemap).await else {
                    continue;
                };
                if !status.is_success() {
                    continue;
                }
                for location in extract_sitemap_locations(&body) {
                    if seen.insert(location.clone()) {
                        urls.push(location);
                    }
                    if urls.len() >= SITEMAP_URL_LIMIT {
                        break 'outer;
                    }
                }
            }

            if urls.is_empty() {
                return Err(ProviderError::unavailable(
                    "direct",
                    "no sitemap URLs found",
                ));
            }
            Ok(urls)
        })
    }
}
