//! Temporary compatibility facade for the background-action split.
//!
//! Workspace code uses the domain modules or crate-root exports directly.
//! This module only preserves the old `haven_tools::bg::*` path for one
//! migration window; remove it after downstream consumers have migrated.

pub use crate::{
    BackgroundActionCompletion, BackgroundActions, EventSink, append_windows_diagnostics,
    build_shell_command, build_shell_command_silent, collect_byte_cap, is_progress_clixml,
    output_log_dir, proxy_env_vars, sanitize_shell_output, summarize_error, write_output_log,
};

#[cfg(windows)]
pub use crate::CREATE_NO_WINDOW;
