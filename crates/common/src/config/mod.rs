//! Application configuration: TOML schema types, sub-config structs, and the
//! [`ConfigLoader`].
//!
//! Split into submodules by concern — endpoints/models/router, media, misc
//! slices, and the loader (which aggregates everything into `AppConfig` /
//! `Settings`). The public surface is re-exported below, so downstream crates
//! keep using `haven_common::config::*` unchanged.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub(super) fn deserialize_runtime_secret<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.is_empty() {
        Ok(value)
    } else {
        Err(serde::de::Error::custom(
            "plaintext credentials are not supported in config.toml",
        ))
    }
}

use crate::types::{
    HotkeyMode, McpTransportType, NetworkPolicy, PermissionMode, RiskLevel, SandboxMode,
    ShellChoice,
};

mod credentials;
mod endpoint;
mod loader;
mod media;
mod misc;
mod service;

pub use credentials::*;
pub use endpoint::*;
pub use loader::*;
pub use media::*;
pub use misc::*;
pub use service::*;
