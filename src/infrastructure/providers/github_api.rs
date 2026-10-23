//! GitHub REST adapter: pull-request metadata used to answer
//! "is it merged?" without a second round-trip.
//!
//! Unauthenticated `api.github.com` allows 60 requests/hour per IP; results
//! are cached briefly (PR state changes rarely) and failures are non-fatal —
//! the fetch result simply omits the `github` field. An optional
//! `GITHUB_TOKEN` environment variable raises the rate limit.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::domain::error::{ProviderError, ProviderFuture};
use crate::domain::fetch::{PullRequestInfo, PullRequestSource};
use crate::infrastructure::http::HttpClient;

const API_BASE: &str = "https://api.github.com";
const CACHE_TTL: Duration = Duration::from_secs(300);
const CACHE_CAP: usize = 128;

pub struct GithubApi {
    http: HttpClient,
    token: Option<String>,
    cache: Mutex<HashMap<String, (Instant, std::sync::Arc<PullRequestInfo>)>>,
}

impl GithubApi {
    pub fn new(http: HttpClient) -> Self {
        let token = std::env::var("GITHUB_TOKEN")
            .ok()
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty());
        Self {
            http,
            token,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

#[derive(Deserialize)]
struct PullResponse {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    merged: bool,
    #[serde(default)]
    merged_at: Option<String>,
    #[serde(default)]
    closed_at: Option<String>,
    #[serde(default)]
    comments: u64,
}

impl PullRequestSource for GithubApi {
    fn pull<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
    ) -> ProviderFuture<'a, Result<PullRequestInfo, ProviderError>> {
        Box::pin(async move {
            let key = format!("{owner}/{repo}/{number}");
            let cached = {
                let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
                match cache.get(&key) {
                    Some((fetched_at, info)) if fetched_at.elapsed() < CACHE_TTL => {
                        Some(info.as_ref().clone())
                    }
                    _ => None,
                }
            };
            if let Some(info) = cached {
                return Ok(info);
            }

            let mut request = self
                .http
                .client()
                .get(format!("{API_BASE}/repos/{owner}/{repo}/pulls/{number}"))
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28");
            if let Some(token) = &self.token {
                request = request.bearer_auth(token);
            }
            let response: PullResponse = self.http.json(request, "github").await?;
            let info = Arc::new(PullRequestInfo {
                number: response.number,
                title: response.title,
                state: response.state,
                draft: response.draft,
                merged: response.merged,
                merged_at: response.merged_at,
                closed_at: response.closed_at,
                comments: response.comments,
            });

            let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            if cache.len() >= CACHE_CAP {
                cache.clear();
            }
            cache.insert(key, (Instant::now(), Arc::clone(&info)));
            Ok(PullRequestInfo::clone(&info))
        })
    }
}
