//! Bounded search traversal using pinned directory and file handles.

use crate::security::FileReadPolicy;
use haven_platform::filesystem::{DirectoryCapability, DirectoryEntryKind};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const MAX_RULE_BYTES: u64 = 1024 * 1024;
const MAX_TOTAL_RULE_BYTES: usize = 4 * 1024 * 1024;
const MAX_RULE_FILES: usize = 1024;

pub(super) struct Entry {
    pub path: PathBuf,
    pub file: File,
}

pub(super) struct WalkResult {
    pub scanned: usize,
    pub errors: usize,
    pub limit_hit: bool,
}

pub(crate) enum RootCapability {
    File(File),
    Directory(DirectoryCapability),
}

pub(super) struct WalkRequest<'a> {
    pub root: &'a Path,
    pub max_depth: usize,
    pub hidden: bool,
    pub policy: &'a FileReadPolicy,
    pub cancel: &'a CancellationToken,
    pub maximum: usize,
    pub root_capability: Option<RootCapability>,
}

/// Rules are loaded only from files opened relative to the current directory
/// handle and authorized before their contents are consumed.
pub(super) fn walk(
    root: &Path,
    max_depth: usize,
    hidden: bool,
    policy: &FileReadPolicy,
    cancel: &CancellationToken,
    maximum: usize,
    visit: impl FnMut(Entry) -> bool,
) -> WalkResult {
    walk_with_root(
        WalkRequest {
            root,
            max_depth,
            hidden,
            policy,
            cancel,
            maximum,
            root_capability: None,
        },
        visit,
    )
}

pub(super) fn walk_with_root(
    request: WalkRequest<'_>,
    mut visit: impl FnMut(Entry) -> bool,
) -> WalkResult {
    let WalkRequest {
        root,
        max_depth,
        hidden,
        policy,
        cancel,
        maximum,
        root_capability,
    } = request;
    let mut result = WalkResult {
        scanned: 0,
        errors: 0,
        limit_hit: false,
    };
    let dir = match root_capability {
        Some(RootCapability::File(file)) => {
            let validation_path = policy.bound_target_path(root).unwrap_or(root);
            if file.metadata().is_ok_and(|metadata| metadata.is_file())
                && policy.validate_opened(validation_path, &file, true).is_ok()
            {
                result.scanned += 1;
                visit(Entry {
                    path: root.to_path_buf(),
                    file,
                });
            } else {
                result.errors += 1;
            }
            return result;
        }
        Some(RootCapability::Directory(directory)) => directory,
        None => {
            let file = match policy.open_metadata_cancellable(root, || cancel.is_cancelled()) {
                Ok(file) => file,
                Err(_) => {
                    result.errors += 1;
                    return result;
                }
            };
            if file.metadata().is_ok_and(|metadata| metadata.is_file()) {
                match policy.open_read_cancellable(root, || cancel.is_cancelled()) {
                    Ok(file) => {
                        result.scanned += 1;
                        visit(Entry {
                            path: root.to_path_buf(),
                            file,
                        });
                    }
                    Err(_) => result.errors += 1,
                }
                return result;
            }
            match policy.open_directory_cancellable(root, || cancel.is_cancelled()) {
                Ok(directory) => directory,
                Err(_) => {
                    result.errors += 1;
                    return result;
                }
            }
        }
    };
    let traversal_root = dir.root().to_path_buf();
    let handle = match dir.directory_handle() {
        Ok(handle)
            if policy
                .validate_opened(&traversal_root, &handle, false)
                .is_ok() =>
        {
            handle
        }
        _ => {
            result.errors += 1;
            return result;
        }
    };
    result.scanned += 1;
    if !visit(Entry {
        path: traversal_root.clone(),
        file: handle,
    }) {
        return result;
    }
    let mut pending = vec![(dir, traversal_root, 0usize, Vec::<Arc<Gitignore>>::new())];
    let mut rule_bytes = 0usize;
    let mut rule_files = 0usize;
    while let Some((dir, path, depth, mut rules)) = pending.pop() {
        if cancel.is_cancelled() {
            break;
        }
        if max_depth != 0 && depth >= max_depth {
            continue;
        }
        let sources = [
            load_git_exclude(&dir, &path, policy),
            load_rules(&dir, &path, ".gitignore", &path, policy),
            load_rules(&dir, &path, ".ignore", &path, policy),
        ];
        for source in sources {
            match source {
                Ok(Some((rule, size))) => {
                    rule_bytes = rule_bytes.saturating_add(size);
                    if !rule.is_empty() {
                        rule_files += 1;
                    }
                    if rule_bytes > MAX_TOTAL_RULE_BYTES || rule_files > MAX_RULE_FILES {
                        result.limit_hit = true;
                        return result;
                    }
                    if !rule.is_empty() {
                        rules.push(Arc::new(rule));
                    }
                }
                Ok(None) => {}
                Err(_) => result.errors += 1,
            }
        }
        let entries = match dir.entries_cancellable(maximum.saturating_sub(result.scanned), || {
            cancel.is_cancelled()
        }) {
            Ok(entries) => entries,
            Err(e) => {
                if cancel.is_cancelled() {
                    break;
                }
                if e.kind() == io::ErrorKind::InvalidData {
                    result.limit_hit = true;
                } else {
                    result.errors += 1;
                }
                break;
            }
        };
        for entry in entries {
            if cancel.is_cancelled() {
                break;
            }
            if result.scanned >= maximum {
                result.limit_hit = true;
                break;
            }
            result.scanned += 1;
            if entry.kind == DirectoryEntryKind::ReparsePoint
                || entry.kind == DirectoryEntryKind::Other
            {
                continue;
            }
            if hidden && entry.name.to_string_lossy().starts_with('.') {
                continue;
            }
            let child_path = path.join(&entry.name);
            let child = match dir.open_child(&entry.name) {
                Ok(file) => file,
                Err(_) => {
                    result.errors += 1;
                    continue;
                }
            };
            if policy.validate_opened(&child_path, &child, false).is_err() {
                result.errors += 1;
                continue;
            }
            let metadata = match child.metadata() {
                Ok(m) => m,
                Err(_) => {
                    result.errors += 1;
                    continue;
                }
            };
            let is_dir = metadata.is_dir();
            #[cfg(windows)]
            if hidden {
                use std::os::windows::fs::MetadataExt;
                const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
                if metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0 {
                    continue;
                }
            }
            if !is_dir && !metadata.is_file() {
                continue;
            }
            let mut ignored = false;
            for rule in &rules {
                let matched = rule.matched_path_or_any_parents(&child_path, is_dir);
                if matched.is_ignore() {
                    ignored = true;
                }
                if matched.is_whitelist() {
                    ignored = false;
                }
            }
            if ignored {
                continue;
            }
            if is_dir {
                match child
                    .try_clone()
                    .and_then(|file| dir.child_directory(&entry.name, file))
                {
                    Ok(child_dir) => {
                        if pending.len() >= 1024 {
                            result.limit_hit = true;
                            break;
                        }
                        pending.push((child_dir, child_path.clone(), depth + 1, rules.clone()));
                    }
                    Err(_) => {
                        result.errors += 1;
                        continue;
                    }
                }
            }
            if !visit(Entry {
                path: child_path,
                file: child,
            }) {
                return result;
            }
        }
        if result.limit_hit {
            break;
        }
    }
    result
}

fn load_rules(
    dir: &DirectoryCapability,
    path: &Path,
    name: &str,
    rule_root: &Path,
    policy: &FileReadPolicy,
) -> io::Result<Option<(Gitignore, usize)>> {
    let mut file = match dir.open_child(name.as_ref()) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    policy.validate_opened(&path.join(name), &file, true)?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_RULE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RULE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ignore rules exceed limit",
        ));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid ignore rules"))?;
    let mut builder = GitignoreBuilder::new(rule_root);
    for line in text.lines() {
        builder.add_line(None, line).map_err(io::Error::other)?;
    }
    builder
        .build()
        .map(|rule| Some((rule, bytes.len())))
        .map_err(io::Error::other)
}

fn load_git_exclude(
    dir: &DirectoryCapability,
    path: &Path,
    policy: &FileReadPolicy,
) -> io::Result<Option<(Gitignore, usize)>> {
    let mut current = dir.clone();
    let mut current_path = path.to_path_buf();
    for name in [".git", "info"] {
        let file = match current.open_child(name.as_ref()) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        current_path.push(name);
        policy.validate_opened(&current_path, &file, false)?;
        current = current.child_directory(name.as_ref(), file)?;
    }
    load_rules(&current, &current_path, "exclude", path, policy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_applies_authorized_rules_and_depth() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("nested/deep")).unwrap();
        std::fs::write(temp.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(temp.path().join("ignored.txt"), "ignored").unwrap();
        std::fs::write(temp.path().join("visible.txt"), "visible").unwrap();
        std::fs::write(temp.path().join("nested/deep/too-deep.txt"), "deep").unwrap();
        let policy = FileReadPolicy::for_test_roots(&[temp.path().to_path_buf()]);
        let mut paths = Vec::new();
        let result = walk(
            temp.path(),
            2,
            true,
            &policy,
            &CancellationToken::new(),
            100,
            |entry| {
                paths.push(entry.path);
                true
            },
        );
        assert_eq!(result.errors, 0);
        assert!(paths.iter().any(|p| p.ends_with("visible.txt")));
        assert!(
            !paths
                .iter()
                .any(|p| p.ends_with("ignored.txt") || p.ends_with("too-deep.txt"))
        );
    }

    #[test]
    fn traversal_reports_the_entry_budget() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..4 {
            std::fs::write(temp.path().join(index.to_string()), "x").unwrap();
        }
        let policy = FileReadPolicy::for_test_roots(&[temp.path().to_path_buf()]);
        let result = walk(
            temp.path(),
            0,
            true,
            &policy,
            &CancellationToken::new(),
            2,
            |_| true,
        );
        assert!(result.limit_hit);
        assert!(result.scanned <= 2);
    }

    #[test]
    fn traversal_consumes_a_preopened_directory_capability() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("visible.txt"), "visible").unwrap();
        let policy = FileReadPolicy::for_test_roots(&[temp.path().to_path_buf()]);
        let directory = DirectoryCapability::open(temp.path()).unwrap();
        let mut paths = Vec::new();
        let result = walk_with_root(
            WalkRequest {
                root: temp.path(),
                max_depth: 0,
                hidden: true,
                policy: &policy,
                cancel: &CancellationToken::new(),
                maximum: 100,
                root_capability: Some(RootCapability::Directory(directory)),
            },
            |entry| {
                paths.push(entry.path);
                true
            },
        );
        assert_eq!(result.errors, 0);
        assert!(paths.iter().any(|path| path.ends_with("visible.txt")));
    }

    #[test]
    fn traversal_loads_repository_exclude_from_authorized_handles() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(".git/info")).unwrap();
        std::fs::write(temp.path().join(".git/info/exclude"), "excluded.txt\n").unwrap();
        std::fs::write(temp.path().join("excluded.txt"), "hidden").unwrap();
        std::fs::write(temp.path().join("visible.txt"), "visible").unwrap();
        let policy = FileReadPolicy::for_test_roots(&[temp.path().to_path_buf()]);
        let mut paths = Vec::new();
        let result = walk(
            temp.path(),
            0,
            true,
            &policy,
            &CancellationToken::new(),
            100,
            |entry| {
                paths.push(entry.path);
                true
            },
        );
        assert_eq!(result.errors, 0);
        assert!(paths.iter().any(|p| p.ends_with("visible.txt")));
        assert!(!paths.iter().any(|p| p.ends_with("excluded.txt")));
    }

    #[test]
    fn symlinked_ignore_file_is_not_consumed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let target = temp.path().join("outside-rules");
        std::fs::write(&target, "visible.txt\n").unwrap();
        std::fs::write(root.join("visible.txt"), "visible").unwrap();
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&target, root.join(".ignore"));
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&target, root.join(".ignore"));
        if linked.is_err() {
            return;
        }
        let policy = FileReadPolicy::for_test_roots(&[root.clone()]);
        let mut paths = Vec::new();
        let result = walk(
            &root,
            0,
            true,
            &policy,
            &CancellationToken::new(),
            100,
            |entry| {
                paths.push(entry.path);
                true
            },
        );
        assert!(result.errors > 0);
        assert!(paths.iter().any(|p| p.ends_with("visible.txt")));
    }
}
