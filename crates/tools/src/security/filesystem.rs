use haven_platform::filesystem::{DirectoryCapability, FilesystemPathCapability};
use haven_platform::filesystem::{
    ResolvedLocalTarget, is_link_or_reparse_point, open_for_metadata, open_read_only,
    same_file_identity,
};
use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(super) struct AuthorizedLocalReadTarget {
    pub(super) requested_path: PathBuf,
    pub(super) target: ResolvedLocalTarget,
}

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
/// Captured path roots for one read operation. Each opened file is checked
/// against these roots by identity before the caller consumes the handle.
#[derive(Clone, Default)]
pub(crate) struct FileReadPolicy {
    pub(super) allowed_roots: Vec<Vec<PathBuf>>,
    pub(super) denied_paths: Vec<FilesystemPathCapability>,
    pub(super) deny_all: bool,
    /// Only the built-in Files contract may opt into verified local links.
    /// Other path consumers retain the historical no-reparse behavior.
    pub(super) allow_local_links: bool,
    /// Re-resolved and identity-checked targets captured after authorization.
    /// These handles live for one execution context only.
    pub(super) authorized_targets: Vec<AuthorizedLocalReadTarget>,
}

/// Per-attempt filesystem authority derived from the same policy revision as
/// an execution proof. Bounded path groups represent intersections: a path
/// must be below one held directory in every group.
#[derive(Clone, Default)]
pub(crate) struct FileWritePolicy {
    pub(super) root_constraints: Vec<Vec<DirectoryCapability>>,
    /// One-call extension roots created only from the exact typed write
    /// resources bound into an accepted confirmation receipt.
    pub(super) extension_roots: Vec<DirectoryCapability>,
    /// Exact targets bound into that receipt. The directory capability is
    /// required for safe handle-relative writes, while this list prevents it
    /// from authorizing sibling paths in the same parent directory.
    pub(super) extension_targets: Vec<PathBuf>,
    /// Writable policy roots that remain readable but cannot be mutated.
    pub(super) readonly_paths: Vec<FilesystemPathCapability>,
    /// Explicit hard-deny paths. These override all root grants.
    pub(super) denied_paths: Vec<FilesystemPathCapability>,
    pub(super) deny_all: bool,
    pub(super) ambient: bool,
}

impl FileWritePolicy {
    #[cfg(test)]
    pub(crate) fn ambient_for_test() -> Self {
        Self {
            root_constraints: Vec::new(),
            extension_roots: Vec::new(),
            extension_targets: Vec::new(),
            readonly_paths: Vec::new(),
            denied_paths: Vec::new(),
            deny_all: false,
            ambient: true,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test_roots(roots: &[PathBuf]) -> Self {
        Self {
            root_constraints: vec![open_directory_roots(roots, "test roots").unwrap()],
            extension_roots: Vec::new(),
            extension_targets: Vec::new(),
            readonly_paths: Vec::new(),
            denied_paths: Vec::new(),
            deny_all: false,
            ambient: false,
        }
    }

    pub(crate) fn resolve(&self, path: &Path) -> io::Result<(DirectoryCapability, PathBuf)> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "file mutation path is not absolute",
            ));
        }
        let resolved = resolve_path_without_reparse(path).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "file mutation path changed or contains a reparse point",
            )
        })?;

        if self.deny_all
            || self
                .denied_paths
                .iter()
                .any(|denied| denied.contains_path(&resolved).unwrap_or(true))
            || self
                .readonly_paths
                .iter()
                .any(|readonly| readonly.contains_path(&resolved).unwrap_or(true))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "file mutation is blocked by a protected path policy",
            ));
        }

        if !self.extension_targets.is_empty()
            && !self
                .extension_targets
                .iter()
                .any(|target| extension_path_matches(target, &resolved))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "file mutation path is outside the exact temporary resource extension targets",
            ));
        }

        let mut selected = Vec::new();
        if !self.extension_roots.is_empty() {
            let capability = self
                .extension_roots
                .iter()
                .filter(|candidate| candidate.relative_path(&resolved).is_some())
                .max_by_key(|candidate| candidate.root().components().count())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "file mutation path is outside its temporary resource extension",
                    )
                })?;
            selected.push(capability.clone());
        }
        for candidates in &self.root_constraints {
            let capability = candidates
                .iter()
                .filter(|candidate| candidate.relative_path(&resolved).is_some())
                .max_by_key(|candidate| candidate.root().components().count())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "file mutation path has no directory capability",
                    )
                })?;
            selected.push(capability.clone());
        }

        if let Some(capability) = selected
            .into_iter()
            .max_by_key(|candidate| candidate.root().components().count())
        {
            let relative = capability.relative_path(&resolved).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "file mutation path is outside its directory capability",
                )
            })?;
            return Ok((capability, relative));
        }

        if self.ambient {
            return DirectoryCapability::open_ambient_for_path(&resolved);
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file mutation has no authorized directory capability",
        ))
    }
}

fn extension_path_matches(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        fn ordinary_local_path(path: &Path) -> std::borrow::Cow<'_, str> {
            let value = path.to_string_lossy();
            match value.strip_prefix(r"\\?\") {
                Some(ordinary) => std::borrow::Cow::Owned(ordinary.to_owned()),
                None => value,
            }
        }
        ordinary_local_path(left).eq_ignore_ascii_case(&ordinary_local_path(right))
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

impl FileReadPolicy {
    #[cfg(test)]
    pub(crate) fn for_test_roots(roots: &[PathBuf]) -> Self {
        Self {
            allowed_roots: vec![roots.to_vec()],
            ..Self::default()
        }
    }

    pub(crate) fn is_link_bound_path(&self, path: &Path) -> bool {
        self.authorized_targets.iter().any(|expected| {
            expected.target.followed_links()
                && normalized_local_path(path) == normalized_local_path(&expected.requested_path)
        })
    }

    pub(crate) fn open_read(&self, path: &Path) -> io::Result<File> {
        #[cfg(windows)]
        if self.allow_local_links {
            let target = ResolvedLocalTarget::open(path)?;
            if target.followed_links() {
                self.validate_bound_target(path, &target)?;
                let opened = target.try_clone_target_handle()?;
                self.validate_opened(target.path(), &opened, true)?;
                self.validate_link_aliases(path, &target)?;
                return Ok(opened);
            }
        }
        let opened = open_read_only(path)?;
        self.validate_opened(path, &opened, true)?;
        Ok(opened)
    }

    pub(crate) fn open_metadata(&self, path: &Path) -> io::Result<File> {
        #[cfg(windows)]
        if self.allow_local_links {
            let target = ResolvedLocalTarget::open(path)?;
            if target.followed_links() {
                self.validate_bound_target(path, &target)?;
                let opened = target.try_clone_target_handle()?;
                self.validate_opened(target.path(), &opened, false)?;
                self.validate_link_aliases(path, &target)?;
                return Ok(opened);
            }
        }
        let opened = open_for_metadata(path)?;
        self.validate_opened(path, &opened, false)?;
        Ok(opened)
    }

    #[cfg(windows)]
    pub(crate) fn open_directory(&self, path: &Path) -> io::Result<DirectoryCapability> {
        if self.allow_local_links {
            let target = ResolvedLocalTarget::open(path)?;
            if target.followed_links() {
                self.validate_bound_target(path, &target)?;
                self.validate_link_aliases(path, &target)?;
                let directory = target.directory_capability()?;
                let handle = directory.directory_handle()?;
                self.validate_opened(target.path(), &handle, false)?;
                return Ok(directory);
            }
        }
        let directory = DirectoryCapability::open(path)?;
        let handle = directory.directory_handle()?;
        self.validate_opened(path, &handle, false)?;
        Ok(directory)
    }

    #[cfg(not(windows))]
    pub(crate) fn open_directory(&self, path: &Path) -> io::Result<DirectoryCapability> {
        let directory = DirectoryCapability::open(path)?;
        let handle = directory.directory_handle()?;
        self.validate_opened(path, &handle, false)?;
        Ok(directory)
    }

    #[cfg(windows)]
    fn validate_bound_target(
        &self,
        requested: &Path,
        current: &ResolvedLocalTarget,
    ) -> io::Result<()> {
        let denied = || {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "local target was not bound to this authorization proof",
            )
        };
        let requested = normalized_local_path(requested);
        for expected in &self.authorized_targets {
            if normalized_local_path(&expected.requested_path) == requested {
                if expected.target.binding_matches(current) {
                    return Ok(());
                }
                return Err(denied());
            }
        }
        Err(denied())
    }

    #[cfg(windows)]
    fn validate_link_aliases(
        &self,
        requested_path: &Path,
        target: &ResolvedLocalTarget,
    ) -> io::Result<()> {
        let denied = || io::Error::new(io::ErrorKind::PermissionDenied, "path is not allowed");
        if self.deny_all {
            return Err(denied());
        }
        let mut aliases = Vec::with_capacity(target.link_chain().len() + 1);
        aliases.push(requested_path);
        aliases.extend(target.link_chain().iter().map(|link| link.path.as_path()));
        for alias in aliases {
            if self
                .denied_paths
                .iter()
                .any(|denied_path| denied_path.contains_path(alias).unwrap_or(true))
            {
                return Err(denied());
            }
        }
        Ok(())
    }

    pub(crate) fn validate_opened(
        &self,
        path: &Path,
        opened: &File,
        require_file: bool,
    ) -> io::Result<()> {
        let denied = || io::Error::new(io::ErrorKind::PermissionDenied, "path is not allowed");
        if !path.is_absolute() || is_unc_or_device_path(path) {
            return Err(denied());
        }
        let opened_metadata = opened.metadata()?;
        if (require_file && !opened_metadata.is_file())
            || (!opened_metadata.is_file() && !opened_metadata.is_dir())
            || is_link_or_reparse_point(&opened_metadata)
        {
            return Err(denied());
        }
        let Some(resolved) = resolve_path_without_reparse(path) else {
            return Err(denied());
        };
        let resolved_file = if opened_metadata.is_dir() {
            open_for_metadata(&resolved)?
        } else {
            open_read_only(&resolved)?
        };
        if !same_file_identity(opened, &resolved_file)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "file changed during path validation",
            ));
        }
        if self.deny_all
            || self.denied_paths.iter().any(|denied| {
                denied
                    .contains_opened_path(&resolved, opened)
                    .unwrap_or(true)
            })
        {
            return Err(denied());
        }
        for roots in &self.allowed_roots {
            if !resolved_path_is_allowed(&resolved, roots) {
                return Err(denied());
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn normalized_local_path(path: &Path) -> std::borrow::Cow<'_, str> {
    #[cfg(windows)]
    {
        let value = path.to_string_lossy();
        match value.strip_prefix(r"\\?\") {
            Some(ordinary) => std::borrow::Cow::Owned(ordinary.to_ascii_lowercase()),
            None => std::borrow::Cow::Owned(value.to_ascii_lowercase()),
        }
    }
    #[cfg(not(windows))]
    {
        std::borrow::Cow::Owned(path.to_string_lossy().into_owned())
    }
}

pub(super) fn path_is_allowed(path: &Path, allowed: &[PathBuf]) -> bool {
    // Relative paths and UNC/device paths are rejected even when the current
    // working directory happens to be inside an allowed root. Otherwise the
    // meaning of the same tool input changes with process launch context.
    if !path.is_absolute() || is_unc_or_device_path(path) {
        return false;
    }
    let Some(canon) = resolve_path_without_reparse(path) else {
        return false;
    };
    resolved_path_is_allowed(&canon, allowed)
}

pub(crate) fn open_directory_roots(
    paths: &[PathBuf],
    setting: &str,
) -> Result<Vec<DirectoryCapability>, String> {
    paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            DirectoryCapability::open(path).map_err(|_| {
                format!(
                    "{setting}[{index}] must be an available local directory that can be safely opened"
                )
            })
        })
        .collect()
}

pub(crate) fn open_filesystem_paths(
    paths: &[PathBuf],
    setting: &str,
) -> Result<Vec<FilesystemPathCapability>, String> {
    let mut opened = Vec::with_capacity(paths.len());
    for (index, path) in paths.iter().enumerate() {
        let capability = FilesystemPathCapability::open(path).map_err(|_| {
            format!("{setting}[{index}] must be an available safe local file or directory")
        })?;
        if !opened
            .iter()
            .any(|existing: &FilesystemPathCapability| existing.same_object(&capability))
        {
            opened.push(capability);
        }
    }
    Ok(opened)
}

fn resolved_path_is_allowed(canon: &Path, allowed: &[PathBuf]) -> bool {
    for base in allowed {
        if !base.is_absolute() || is_unc_or_device_path(base) {
            continue;
        }
        let Some(base_abs) = resolve_path_without_reparse(base) else {
            continue;
        };
        if path_is_within(canon, &base_abs) {
            return true;
        }
    }
    false
}

/// Resolve the existing prefix of a path while rejecting every symlink or
/// Windows reparse point encountered on that prefix. The non-existing suffix
/// is appended only after the trusted prefix has been canonicalized. This is
/// intentionally fail-closed: a metadata/canonicalization error denies the
/// operation instead of falling back to lexical prefix matching.
pub(super) fn resolve_path_without_reparse(path: &Path) -> Option<PathBuf> {
    let abs = normalize_path(path)?;
    let components: Vec<_> = abs.components().collect();
    let mut existing = PathBuf::new();
    let mut suffix: Vec<OsString> = Vec::new();
    let mut missing_started = false;

    for (index, component) in components.iter().enumerate() {
        if missing_started {
            suffix.push(component.as_os_str().to_owned());
            continue;
        }

        existing.push(component.as_os_str());
        match std::fs::symlink_metadata(&existing) {
            Ok(metadata) => {
                if is_link_or_reparse_point(&metadata) {
                    return None;
                }
                if index + 1 < components.len() && !metadata.file_type().is_dir() {
                    return None;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                existing.pop();
                missing_started = true;
                suffix.push(component.as_os_str().to_owned());
            }
            Err(_) => return None,
        }
    }

    let mut resolved = std::fs::canonicalize(&existing).ok()?;
    for component in suffix {
        resolved.push(component);
    }
    Some(resolved)
}

pub(super) fn is_unc_or_device_path(path: &Path) -> bool {
    let value = path.to_string_lossy();
    value.starts_with("\\\\") || value.starts_with("//")
}

pub(super) fn path_is_within(path: &Path, base: &Path) -> bool {
    #[cfg(windows)]
    {
        let path = path.to_string_lossy().to_ascii_lowercase();
        let base = base.to_string_lossy().to_ascii_lowercase();
        Path::new(&path).starts_with(Path::new(&base))
    }
    #[cfg(not(windows))]
    {
        path.starts_with(base)
    }
}

/// Lexically normalize `.` / `..` after making the path absolute so
/// `allowed\..\Windows` cannot prefix-match `allowed`.
fn normalize_path(path: &Path) -> Option<PathBuf> {
    let abs = std::path::absolute(path).ok()?;
    let mut out = PathBuf::new();
    for comp in abs.components() {
        match comp {
            std::path::Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn test_path_sandbox_rejects_symlink_reparse_escape() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = allowed.path().join("link");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();

        assert!(!path_is_allowed(
            &link.join("secret.txt"),
            &[allowed.path().to_path_buf()]
        ));
    }

    #[test]
    #[cfg(unix)]
    fn file_read_policy_rejects_replacement_after_open_and_keeps_handle_bound() {
        use std::io::Read;

        let allowed = tempfile::tempdir().unwrap();
        let path = allowed.path().join("note.txt");
        let moved = allowed.path().join("original.txt");
        std::fs::write(&path, b"authorized contents").unwrap();
        let opened = open_read_only(&path).unwrap();
        let policy = FileReadPolicy {
            allowed_roots: vec![vec![allowed.path().to_path_buf()]],
            ..FileReadPolicy::default()
        };

        std::fs::rename(&path, &moved).unwrap();
        std::fs::write(&path, b"replacement contents").unwrap();

        assert!(policy.validate_opened(&path, &opened, true).is_err());
        let mut contents = Vec::new();
        let mut opened = opened;
        opened.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"authorized contents");
    }

    #[test]
    fn file_read_policy_rejects_file_outside_configured_root() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = outside.path().join("secret.txt");
        std::fs::write(&path, b"secret").unwrap();
        let policy = FileReadPolicy {
            allowed_roots: vec![vec![allowed.path().to_path_buf()]],
            ..FileReadPolicy::default()
        };

        assert_eq!(
            policy.open_read(&path).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn temporary_write_extension_does_not_authorize_sibling_targets() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("approved.txt");
        let sibling = root.path().join("sibling.txt");
        let policy = FileWritePolicy {
            root_constraints: Vec::new(),
            extension_roots: open_directory_roots(&[root.path().to_path_buf()], "test root")
                .unwrap(),
            extension_targets: vec![resolve_path_without_reparse(&target).unwrap()],
            ..FileWritePolicy::default()
        };

        assert!(policy.resolve(&target).is_ok());
        assert_eq!(
            policy.resolve(&sibling).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn readonly_subtree_blocks_file_mutations_inside_a_writable_root() {
        let root = tempfile::tempdir().unwrap();
        let protected = root.path().join(".git");
        std::fs::create_dir(&protected).unwrap();
        let policy = FileWritePolicy {
            root_constraints: vec![
                open_directory_roots(&[root.path().to_path_buf()], "test writable root").unwrap(),
            ],
            readonly_paths: open_filesystem_paths(
                std::slice::from_ref(&protected),
                "test read-only root",
            )
            .unwrap(),
            ..FileWritePolicy::default()
        };

        assert!(policy.resolve(&root.path().join("notes.md")).is_ok());
        assert_eq!(
            policy
                .resolve(&protected.join("config"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn readonly_file_blocks_its_mutation_without_blocking_sibling_files() {
        let root = tempfile::tempdir().unwrap();
        let protected = root.path().join("settings.json");
        std::fs::write(&protected, b"protected").unwrap();
        let policy = FileWritePolicy {
            root_constraints: vec![
                open_directory_roots(&[root.path().to_path_buf()], "test writable root").unwrap(),
            ],
            readonly_paths: open_filesystem_paths(
                std::slice::from_ref(&protected),
                "test read-only file",
            )
            .unwrap(),
            ..FileWritePolicy::default()
        };

        assert!(policy.resolve(&root.path().join("notes.md")).is_ok());
        assert_eq!(
            policy.resolve(&protected).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn explicit_deny_overrides_temporary_write_extensions() {
        let root = tempfile::tempdir().unwrap();
        let denied = root.path().join("secrets");
        std::fs::create_dir(&denied).unwrap();
        let target = denied.join("token.txt");
        let policy = FileWritePolicy {
            extension_roots: open_directory_roots(
                &[root.path().to_path_buf()],
                "test extension root",
            )
            .unwrap(),
            extension_targets: vec![resolve_path_without_reparse(&target).unwrap()],
            denied_paths: open_filesystem_paths(std::slice::from_ref(&denied), "test denied path")
                .unwrap(),
            ..FileWritePolicy::default()
        };

        assert_eq!(
            policy.resolve(&target).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn explicit_deny_blocks_file_reads_inside_a_configured_read_root() {
        use std::io::Write;

        let root = tempfile::tempdir().unwrap();
        let denied = root.path().join("private");
        std::fs::create_dir(&denied).unwrap();
        let path = denied.join("note.txt");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"private")
            .unwrap();
        let policy = FileReadPolicy {
            allowed_roots: vec![vec![root.path().to_path_buf()]],
            denied_paths: open_filesystem_paths(std::slice::from_ref(&denied), "test denied path")
                .unwrap(),
            deny_all: false,
            ..FileReadPolicy::default()
        };

        assert_eq!(
            policy.open_read(&path).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    #[cfg(unix)]
    fn file_read_policy_rejects_a_symlink_path() {
        let allowed = tempfile::tempdir().unwrap();
        let target = allowed.path().join("target.txt");
        let link = allowed.path().join("link.txt");
        std::fs::write(&target, b"target").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let policy = FileReadPolicy {
            allowed_roots: vec![vec![allowed.path().to_path_buf()]],
            ..FileReadPolicy::default()
        };

        assert!(policy.open_read(&link).is_err());
    }

    #[test]
    fn file_read_policy_returns_the_validated_file_handle() {
        use std::io::Read;

        let allowed = tempfile::tempdir().unwrap();
        let path = allowed.path().join("note.txt");
        std::fs::write(&path, b"same handle").unwrap();
        let policy = FileReadPolicy {
            allowed_roots: vec![vec![allowed.path().to_path_buf()]],
            ..FileReadPolicy::default()
        };

        let mut opened = policy.open_read(&path).unwrap();
        let mut contents = Vec::new();
        opened.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"same handle");
    }

    #[test]
    #[cfg(windows)]
    fn file_read_policy_blocks_replacement_while_the_handle_is_open() {
        use std::io::Read;

        let allowed = tempfile::tempdir().unwrap();
        let path = allowed.path().join("note.txt");
        let moved = allowed.path().join("moved.txt");
        std::fs::write(&path, b"authorized contents").unwrap();
        let policy = FileReadPolicy {
            allowed_roots: vec![vec![allowed.path().to_path_buf()]],
            ..FileReadPolicy::default()
        };

        let mut opened = policy.open_read(&path).unwrap();
        assert!(std::fs::rename(&path, &moved).is_err());
        let mut contents = Vec::new();
        opened.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"authorized contents");
    }

    #[test]
    #[cfg(windows)]
    fn file_read_policy_consumes_only_the_confirmed_junction_target() {
        use std::io::Read;

        fn create_junction(link: &Path, target: &Path) {
            let output = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "junction creation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let alias_root = tempfile::tempdir().unwrap();
        let first_target = tempfile::tempdir().unwrap();
        let second_target = tempfile::tempdir().unwrap();
        let link = alias_root.path().join("approved-link");
        let requested = link.join("note.txt");
        std::fs::write(first_target.path().join("note.txt"), b"first target").unwrap();
        std::fs::write(second_target.path().join("note.txt"), b"second target").unwrap();
        create_junction(&link, first_target.path());

        let target = ResolvedLocalTarget::open(&requested).unwrap();
        assert!(target.followed_links());
        let policy = FileReadPolicy {
            allowed_roots: vec![vec![first_target.path().to_path_buf()]],
            allow_local_links: true,
            authorized_targets: vec![AuthorizedLocalReadTarget {
                requested_path: requested.clone(),
                target,
            }],
            ..FileReadPolicy::default()
        };

        let mut opened = policy.open_read(&requested).unwrap();
        let mut contents = Vec::new();
        opened.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"first target");
        drop(opened);

        std::fs::remove_dir(&link).unwrap();
        create_junction(&link, second_target.path());
        assert_eq!(
            policy.open_read(&requested).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    #[cfg(windows)]
    fn file_read_policy_lists_only_the_confirmed_junction_directory() {
        fn create_junction(link: &Path, target: &Path) {
            let output = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "junction creation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let alias_root = tempfile::tempdir().unwrap();
        let first_target = tempfile::tempdir().unwrap();
        let second_target = tempfile::tempdir().unwrap();
        let link = alias_root.path().join("approved-directory");
        let nested_link = first_target.path().join("nested-link");
        std::fs::write(first_target.path().join("note.txt"), b"approved").unwrap();
        std::fs::write(second_target.path().join("other.txt"), b"unapproved").unwrap();
        create_junction(&nested_link, second_target.path());
        create_junction(&link, first_target.path());

        let target = ResolvedLocalTarget::open(&link).unwrap();
        let policy = FileReadPolicy {
            allowed_roots: vec![vec![first_target.path().to_path_buf()]],
            allow_local_links: true,
            authorized_targets: vec![AuthorizedLocalReadTarget {
                requested_path: link.clone(),
                target,
            }],
            ..FileReadPolicy::default()
        };

        let directory = policy.open_directory(&link).unwrap();
        let entries = directory.entries(16).unwrap();
        let kinds = entries
            .into_iter()
            .map(|entry| (entry.name, entry.kind))
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            kinds.get(std::ffi::OsStr::new("note.txt")),
            Some(&haven_platform::filesystem::DirectoryEntryKind::File)
        );
        assert_eq!(
            kinds.get(std::ffi::OsStr::new("nested-link")),
            Some(&haven_platform::filesystem::DirectoryEntryKind::ReparsePoint)
        );

        std::fs::remove_dir(&link).unwrap();
        create_junction(&link, second_target.path());
        assert_eq!(
            policy.open_directory(&link).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn test_path_sandbox_allows_only_absolute_paths_inside_canonical_root() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let inside = allowed.path().join("new").join("file.txt");

        assert!(path_is_allowed(&inside, &[allowed.path().to_path_buf()]));
        assert!(!path_is_allowed(
            &outside.path().join("file.txt"),
            &[allowed.path().to_path_buf()]
        ));
        assert!(!path_is_allowed(
            Path::new("relative.txt"),
            &[allowed.path().to_path_buf()]
        ));
        assert!(!path_is_allowed(
            Path::new("//server/share/file.txt"),
            &[allowed.path().to_path_buf()]
        ));
    }
}
