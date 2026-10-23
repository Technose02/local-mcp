//! Per-provider pacing: a small reservation-queue in front of every adapter.
//!
//! Parallel tool calls from one harness used to hit the same provider at the
//! same time and turn the model into the throttle (post-mortem finding #5).
//! Each adapter instance owns a slot scheduler: callers take the next free
//! slot (serialized by a mutex) and sleep until it is due, so concurrent calls
//! are spaced by `providers.min_interval_ms` instead of colliding upstream.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::{Instant, sleep_until};

use crate::domain::error::{ProviderError, ProviderFuture};
use crate::domain::fetch::{ExtractProvider, ExtractedDocument};
use crate::domain::search::{SearchOutcome, SearchProvider, SearchQuery};

#[derive(Clone)]
struct Pacer {
    next_slot: Arc<Mutex<Instant>>,
    gap: Duration,
}

impl Pacer {
    fn new(gap: Duration) -> Self {
        Self {
            next_slot: Arc::new(Mutex::new(Instant::now())),
            gap,
        }
    }

    async fn reserve(&self) {
        if self.gap.is_zero() {
            return;
        }
        let due = {
            let mut next = self.next_slot.lock().await;
            let now = Instant::now();
            let due = (*next).max(now);
            *next = due + self.gap;
            due
        };
        let now = Instant::now();
        if due > now {
            sleep_until(due).await;
        }
    }
}

/// Wraps a search provider with its pacing queue.
pub struct PacedSearchProvider {
    inner: Arc<dyn SearchProvider>,
    pacer: Pacer,
}

impl PacedSearchProvider {
    pub fn new(inner: Arc<dyn SearchProvider>, gap: Duration) -> Self {
        Self {
            inner,
            pacer: Pacer::new(gap),
        }
    }
}

impl SearchProvider for PacedSearchProvider {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn search<'a>(
        &'a self,
        query: &'a SearchQuery,
    ) -> ProviderFuture<'a, Result<SearchOutcome, ProviderError>> {
        Box::pin(async move {
            self.pacer.reserve().await;
            self.inner.search(query).await
        })
    }
}

/// Wraps an extract provider with its pacing queue.
pub struct PacedExtractProvider {
    inner: Arc<dyn ExtractProvider>,
    pacer: Pacer,
}

impl PacedExtractProvider {
    pub fn new(inner: Arc<dyn ExtractProvider>, gap: Duration) -> Self {
        Self {
            inner,
            pacer: Pacer::new(gap),
        }
    }
}

impl ExtractProvider for PacedExtractProvider {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn extract<'a>(
        &'a self,
        url: &'a str,
    ) -> ProviderFuture<'a, Result<ExtractedDocument, ProviderError>> {
        Box::pin(async move {
            self.pacer.reserve().await;
            self.inner.extract(url).await
        })
    }
}
