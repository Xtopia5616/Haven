//! Application configuration: TOML schema types, sub-config structs, and the
//! [`ConfigLoader`].
//!
//! Split into submodules by concern — endpoints/roles/router, media, misc
//! slices, and the loader (which aggregates everything into `AppConfig` /
//! `Settings`). The public surface is re-exported below, so downstream crates
//! keep using `haven_common::config::*` unchanged.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::types::{ConfirmationMode, HotkeyMode, McpTransportType, RiskLevel, ShellChoice};

mod endpoint;
mod loader;
mod media;
mod misc;

pub use endpoint::*;
pub use loader::*;
pub use media::*;
pub use misc::*;