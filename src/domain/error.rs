//! Errors and shared helpers of the domain layer.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

/// Boxed future returned by the domain ports.
///
/// Using an explicit boxed future (instead of an async-trait macro) keeps the
/// ports object-safe (`dyn SearchProvider`) without pulling in extra macros.
pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A failure reported by a concrete provider adapter.
#[derive(Debug, Clone)]
pub enum ProviderError {
    /// The provider is reachable but returned nothing usable.
    Unavailable { provider: String, reason: String },
    /// The provider request failed at the transport or HTTP level.
    Http { provider: String, reason: String },
    /// The provider response could not be understood.
    Decode { provider: String, reason: String },
    /// The provider rejected us (bot challenge, rate limit, ...).
    Blocked { provider: String, reason: String },
}

impl ProviderError {
    pub fn unavailable(provider: &str, reason: impl Into<String>) -> Self {
        Self::Unavailable {
            provider: provider.to_string(),
            reason: reason.into(),
        }
    }

    pub fn http(provider: &str, reason: impl Into<String>) -> Self {
        Self::Http {
            provider: provider.to_string(),
            reason: reason.into(),
        }
    }

    pub fn decode(provider: &str, reason: impl Into<String>) -> Self {
        Self::Decode {
            provider: provider.to_string(),
            reason: reason.into(),
        }
    }

    pub fn blocked(provider: &str, reason: impl Into<String>) -> Self {
        Self::Blocked {
            provider: provider.to_string(),
            reason: reason.into(),
        }
    }

    /// Name of the provider that produced the error.
    pub fn provider(&self) -> &str {
        match self {
            Self::Unavailable { provider, .. }
            | Self::Http { provider, .. }
            | Self::Decode { provider, .. }
            | Self::Blocked { provider, .. } => provider,
        }
    }

    /// Human readable reason.
    pub fn reason(&self) -> &str {
        match self {
            Self::Unavailable { reason, .. }
            | Self::Http { reason, .. }
            | Self::Decode { reason, .. }
            | Self::Blocked { reason, .. } => reason,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Unavailable { .. } => "unavailable",
            Self::Http { .. } => "http error",
            Self::Decode { .. } => "decode error",
            Self::Blocked { .. } => "blocked",
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {} ({})",
            self.provider(),
            self.reason(),
            self.kind()
        )
    }
}

impl std::error::Error for ProviderError {}
