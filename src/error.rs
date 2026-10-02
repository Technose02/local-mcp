//! Top-level application errors.
//!
//! Errors are declared by hand as enums (no `thiserror`/`anyhow`).

use std::fmt;
use std::path::{Path, PathBuf};

use crate::domain::error::ProviderError;

/// Errors while loading/parsing the configuration file.
#[derive(Debug)]
pub enum ConfigError {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        message: String,
    },
}

impl ConfigError {
    pub fn read(path: &Path, source: std::io::Error) -> Self {
        Self::Read {
            path: path.to_path_buf(),
            source,
        }
    }

    pub fn parse(path: &Path, error: toml::de::Error) -> Self {
        Self::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(f, "failed to read config '{}': {source}", path.display())
            }
            Self::Parse { path, message } => {
                write!(f, "failed to parse config '{}': {message}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Parse { .. } => None,
        }
    }
}

/// Errors returned from `main`.
#[derive(Debug)]
pub enum AppError {
    Config(ConfigError),
    Http(ProviderError),
    Io(std::io::Error),
    Server(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(f, "{error}"),
            Self::Http(error) => write!(f, "{error}"),
            Self::Io(error) => write!(f, "io error: {error}"),
            Self::Server(message) => write!(f, "server error: {message}"),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Http(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Server(_) => None,
        }
    }
}

impl From<ConfigError> for AppError {
    fn from(value: ConfigError) -> Self {
        Self::Config(value)
    }
}

impl From<ProviderError> for AppError {
    fn from(value: ProviderError) -> Self {
        Self::Http(value)
    }
}

impl From<std::io::Error> for AppError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
