//! Domain (business-logic) layer.
//!
//! This module contains the models, ports and services that describe *what* the
//! application does, with no knowledge of MCP, HTTP or any concrete provider.

pub mod clock;
pub mod error;
pub mod explore;
pub mod fetch;
pub mod search;
