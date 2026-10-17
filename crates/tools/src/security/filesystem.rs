use super::{is_unc_or_device_path, resolve_path_without_reparse};
use std::path::Path;

/// Check an absolute local path without applying a tool-specific allowlist.
/// This is for native app entry points such as “open skills directory” and
/// “open external path”; the normal tool path goes through
/// `AuthorizationRequest`, which adds
/// configured `allowed_paths` on top of this reparse-point check.
pub fn is_safe_local_path(path: &Path) -> bool {
    if !path.is_absolute() || is_unc_or_device_path(path) {
        return false;
    }
    resolve_path_without_reparse(path).is_some()
}
