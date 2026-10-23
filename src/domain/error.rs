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
///
/// Reasons must stay safe to show to the model: never embed raw upstream body
/// text here (see `infrastructure::sanitize`); the HTTP status and an optional
/// `retry_after_s` carry the structured signal instead.
#[derive(Debug, Clone)]
pub enum ProviderError {
    /// The provider is reachable but returned nothing usable.
    Unavailable { provider: String, reason: String },
    /// The provider request failed at the transport or HTTP level.
    Http {
        provider: String,
        reason: String,
        status: Option<u16>,
        retry_after_s: Option<u64>,
    },
    /// The provider response could not be understood.
    Decode { provider: String, reason: String },
    /// The provider rejected us (bot challenge, rate limit, ...).
    Blocked {
        provider: String,
        reason: String,
        status: Option<u16>,
        retry_after_s: Option<u64>,
    },
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
            status: None,
            retry_after_s: None,
        }
    }

    /// HTTP failure carrying the response status and, for 429/503, the
    /// `Retry-After` hint in whole seconds.
    #[allow(clippy::too_many_arguments)]
    pub fn with_status(
        kind: ProviderErrorKind,
        provider: &str,
        reason: impl Into<String>,
        status: Option<u16>,
        retry_after_s: Option<u64>,
    ) -> Self {
        let (provider, reason) = (provider.to_string(), reason.into());
        match kind {
            ProviderErrorKind::Http => Self::Http {
                provider,
                reason,
                status,
                retry_after_s,
            },
            ProviderErrorKind::Blocked => Self::Blocked {
                provider,
                reason,
                status,
                retry_after_s,
            },
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
            status: None,
            retry_after_s: None,
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

    /// HTTP status that produced this error, when known.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status, .. } | Self::Blocked { status, .. } => *status,
            _ => None,
        }
    }

    /// `Retry-After` hint in seconds, when the provider supplied one.
    pub fn retry_after_s(&self) -> Option<u64> {
        match self {
            Self::Http {
                retry_after_s, ..
            }
            | Self::Blocked {
                retry_after_s, ..
            } => *retry_after_s,
            _ => None,
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

/// Which variant [`ProviderError::with_status`] builds.
#[derive(Debug, Clone, Copy)]
pub enum ProviderErrorKind {
    Http,
    Blocked,
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} ({}", self.provider(), self.reason(), self.kind())?;
        if let Some(status) = self.status() {
            write!(f, ", HTTP {status}")?;
        }
        if let Some(retry_after_s) = self.retry_after_s() {
            write!(f, ", retry_after_s={retry_after_s}")?;
        }
        write!(f, ")")
    }
}

impl std::error::Error for ProviderError {}
