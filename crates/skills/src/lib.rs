//! Skills registry: discovery, parsing, and management of reusable agent
//! skills (`SKILL.md` + scripts), plus the virtual-environment manager used
//! to run Python skills sandboxed.
//!
//! Execution of a skill as a tool lives in `haven-tools` (the `SkillRunner`
//! bridges skills into the tool-execution result types).

use anyhow::Context;
use haven_common::ConfigLoader;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Manifest types
// ---------------------------------------------------------------------------

pub mod venv;

pub use venv::VenvManager;

/// Scripting language supported by a Skill. First-class is `Python`; anything
/// else is preserved verbatim so the UI/later phases can render it without
/// losing the original value, while the sandbox runner will refuse to execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Language {
    Python,
    Unsupported(String),
}

impl Language {
    /// Parse a metadata `language` value into a typed enum.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "" | "python" => Self::Python,
            other => Self::Unsupported(other.to_string()),
        }
    }

    /// Lowercase identifier suitable for storage/UI display.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Python => "python",
            Self::Unsupported(other) => other.as_str(),
        }
    }
}

/// Structured metadata parsed from `SKILL.md` (搂4.6.3).
#[derive(Debug, Clone)]
pub struct SkillManifest {
    pub name: String,
    pub description: String,
    pub version: Option<String>,
    pub language: Language,
    /// Full text of the `## Instructions` section, verbatim
    /// (`{{param}}` placeholders preserved for later render phases).
    pub instructions: String,
}

/// A discovered Skill on disk.
#[derive(Clone)]
pub struct Skill {
    manifest: SkillManifest,
    root: PathBuf,
    enabled: bool,
}

impl Skill {
    pub fn name(&self) -> &str {
        &self.manifest.name
    }
    pub fn description(&self) -> &str {
        &self.manifest.description
    }
    pub fn version(&self) -> Option<&str> {
        self.manifest.version.as_deref()
    }
    pub fn language(&self) -> &Language {
        &self.manifest.language
    }
    pub fn instructions(&self) -> &str {
        &self.manifest.instructions
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    /// Whether the skill ships an executable entry script under `scripts/`.
    /// Looks for `scripts/main.py` first, then `scripts/<name>.py`.
    pub fn has_script(&self) -> bool {
        self.entry_script().is_some()
    }

    /// Resolve the entry script path for this skill.
    /// Returns `None` when no recognised script exists or its resolved target
    /// escapes the skill directory.
    pub fn entry_script(&self) -> Option<PathBuf> {
        let root = self.root.canonicalize().ok()?;
        let scripts = self.root.join("scripts");
        let main = scripts.join("main.py");
        let named = scripts.join(format!("{}.py", self.manifest.name));
        let resolve_inside_root = |candidate: &Path| {
            let resolved = candidate.canonicalize().ok()?;
            (resolved.starts_with(&root) && resolved.is_file()).then_some(resolved)
        };
        resolve_inside_root(&main).or_else(|| resolve_inside_root(&named))
    }

    /// Construct a Skill without going through the normal scan/parse path.
    /// Used by tests (including downstream crates' tests) to create inline
    /// skills without touching the filesystem.
    #[doc(hidden)]
    pub fn from_manifest_unchecked(manifest: SkillManifest, root: PathBuf, enabled: bool) -> Self {
        Self {
            manifest,
            root,
            enabled,
        }
    }
}

// ---------------------------------------------------------------------------
// Frontend-facing snapshot
// ---------------------------------------------------------------------------

/// Serializable snapshot returned to the bridge/UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub version: Option<String>,
    pub language: String,
    /// Whether the skill is enabled by the configured allowlist.
    pub enabled: bool,
    /// Whether the skill is currently executable (enabled and has an entry script).
    pub executable: bool,
    /// Absolute path (UTF-8 lossy) to the skill directory.
    pub root: String,
    /// Whether a valid manifest resolves to an entry script. For an invalid
    /// manifest this is unknown; inspect `manifest_error` before interpreting it.
    pub has_script: bool,
    /// Why a valid skill cannot currently be executed.
    pub unavailable_reason: Option<String>,
    /// A SKILL.md parsing error. Invalid manifests remain visible in the
    /// catalog for diagnosis, but never enter the executable registry.
    pub manifest_error: Option<String>,
}

impl From<&Skill> for SkillInfo {
    fn from(s: &Skill) -> Self {
        let has_script = s.has_script();
        let enabled = s.enabled();
        let executable = enabled && has_script;
        Self {
            name: s.name().to_string(),
            description: s.description().to_string(),
            version: s.version().map(str::to_string),
            language: s.language().as_str().to_string(),
            // A configured skill without an executable entry point remains
            // enabled in policy, but cannot enter the executable tool catalog.
            enabled,
            executable,
            root: s.root().to_string_lossy().to_string(),
            has_script,
            unavailable_reason: if !has_script {
                Some(
                    "No entry script found (expected scripts/main.py or scripts/<name>.py).".into(),
                )
            } else if !s.enabled() {
                Some("Disabled by the configured skills allowlist.".into())
            } else {
                None
            },
            manifest_error: None,
        }
    }
}

impl SkillInfo {
    fn invalid_manifest(name: String, root: &Path, error: String) -> Self {
        Self {
            name,
            description: String::new(),
            version: None,
            language: "unknown".into(),
            enabled: false,
            executable: false,
            root: root.to_string_lossy().to_string(),
            has_script: false,
            unavailable_reason: None,
            manifest_error: Some(error),
        }
    }
}

// ---------------------------------------------------------------------------
// SKILL.md parser
// ---------------------------------------------------------------------------

/// Validate a Skill name before it is used as a tool identity or filesystem
/// component. The ASCII-only alphabet is portable across Windows and Unix,
/// while the length bound keeps generated tool names predictable.
pub fn validate_skill_name(name: &str) -> anyhow::Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 128
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        });
    if !valid {
        anyhow::bail!(
            "invalid skill name '{}': use 1-128 ASCII letters, digits, '-' or '_'",
            name
        );
    }

    let uppercase = name.to_ascii_uppercase();
    if matches!(
        uppercase.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        anyhow::bail!(
            "invalid skill name '{}': Windows device names are reserved",
            name
        );
    }

    Ok(())
}

fn case_insensitive_collisions<'a>(names: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    let mut counts = BTreeMap::<String, usize>::new();
    for name in names {
        *counts.entry(name.to_ascii_lowercase()).or_default() += 1;
    }
    counts
        .into_iter()
        .filter_map(|(name, count)| (count > 1).then_some(name))
        .collect()
}

fn skip_case_insensitive_skill_collisions(candidates: Vec<Skill>) -> Vec<Skill> {
    let ambiguous_names = case_insensitive_collisions(candidates.iter().map(Skill::name));
    for name in &ambiguous_names {
        tracing::warn!(
            name = %name,
            "skipping all Skills with Windows case-insensitive name collision"
        );
    }
    candidates
        .into_iter()
        .filter(|skill| !ambiguous_names.contains(&skill.name().to_ascii_lowercase()))
        .collect()
}

fn skip_case_insensitive_directory_collisions(
    directories: Vec<(String, PathBuf, PathBuf)>,
) -> Vec<(String, PathBuf, PathBuf)> {
    let ambiguous_names =
        case_insensitive_collisions(directories.iter().map(|(name, _, _)| name.as_str()));
    for name in &ambiguous_names {
        tracing::warn!(
            name = %name,
            "skipping all Skill directories with Windows case-insensitive name collision"
        );
    }
    directories
        .into_iter()
        .filter(|(name, _, _)| !ambiguous_names.contains(&name.to_ascii_lowercase()))
        .collect()
}

/// Parse a `SKILL.md` document into structured metadata.
///
/// Expected layout:
///
/// ```markdown
/// # Skill: <name>
///
/// ## Metadata
/// - name: <name>
/// - description: <desc>
/// - version: 1.0.0
/// - language: python
///
/// ## Instructions
/// ...natural language...
/// ```
///
/// The H1 line is parsed for `<name>` and the `name:` metadata field (if
/// present) takes precedence —this lets a directory's `SKILL.md` carry a name
/// differing from its folder name without surprising the registry.
///
/// **Safety:** The parser enforces a maximum line count and a maximum per-line
/// length (from `context_limits`) to prevent unbounded memory accumulation
/// from crafted/oversized input.
pub fn parse_skill_md(
    input: &str,
    max_parse_lines: usize,
    max_line_len: usize,
) -> anyhow::Result<SkillManifest> {
    let input = input.strip_prefix('\u{FEFF}').unwrap_or(input);

    let mut name: Option<String> = None;
    let mut description = String::new();
    let mut version: Option<String> = None;
    let mut language = Language::Python;

    let mut current_section: Option<String> = None;
    let mut metadata_lines: Vec<(usize, String)> = Vec::new();
    let mut instruction_lines: Vec<String> = Vec::new();

    for (i, line) in input.lines().enumerate() {
        if i >= max_parse_lines {
            anyhow::bail!("SKILL.md exceeds {max_parse_lines} lines");
        }
        if line.len() > max_line_len {
            anyhow::bail!("SKILL.md line {} exceeds {max_line_len} characters", i + 1);
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            // Preserve blank lines inside instruction section for readability.
            if matches!(current_section.as_deref(), Some("instructions")) {
                instruction_lines.push(String::new());
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            if let Some(n) = rest.strip_prefix("Skill:") {
                name = Some(n.trim().to_string());
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            current_section = Some(rest.trim().to_lowercase());
            continue;
        }
        match current_section.as_deref() {
            Some("metadata") => metadata_lines.push((i + 1, trimmed.to_string())),
            Some("instructions") => instruction_lines.push(trimmed.to_string()),
            _ => {}
        }
    }

    for (line_number, ml) in &metadata_lines {
        let entry = ml.trim_start();
        let Some(line) = entry.strip_prefix("- ") else {
            anyhow::bail!(
                "invalid SKILL.md metadata entry on line {line_number}: expected '- key: value'"
            );
        };
        let (key, val) = match line.split_once(':') {
            Some(pair) => pair,
            None => anyhow::bail!(
                "invalid SKILL.md metadata entry on line {line_number}: expected '- key: value'"
            ),
        };
        let key = key.trim().to_lowercase();
        if key.is_empty() {
            anyhow::bail!(
                "invalid SKILL.md metadata entry on line {line_number}: metadata key is empty"
            );
        }
        let val = val.trim().to_string();
        match key.as_str() {
            "name" => name = Some(val),
            "description" => description = val,
            "version" => version = Some(val),
            "language" => language = Language::parse(&val),
            _ => anyhow::bail!("unknown SKILL.md metadata field: {key}"),
        }
    }

    // Trim trailing blank lines from instructions.
    while instruction_lines
        .last()
        .map(|s| s.is_empty())
        .unwrap_or(false)
    {
        instruction_lines.pop();
    }
    let instructions = instruction_lines.join("\n").trim().to_string();

    let name = name.context("SKILL.md missing '# Skill: <name>' header or 'name' metadata")?;
    validate_skill_name(&name)?;
    Ok(SkillManifest {
        name,
        description,
        version,
        language,
        instructions,
    })
}

// ---------------------------------------------------------------------------
// Directory scanning
// ---------------------------------------------------------------------------

/// Scan `<root>/<skill-name>/SKILL.md` for all skills under `root`.
///
/// `enabled_skill_allowlist` semantics:
/// - `None` → all skills are enabled.
/// - `Some(list)` → only skills whose names are in `list` are enabled (empty
///   `Some([])` disables everything).
///
/// Invalid SKILL.md files produce a `warn!` and are skipped (non-fatal).
/// Directory names and parsed Skill names that collide case-insensitively are
/// skipped as a group to avoid Windows path and tool identity ambiguity.
///
/// **Safety:** The scan canonicalises both `root` and each entry, plus every
/// manifest target, to guard against symlink/junction traversal outside the
/// skills directory. Files larger than `limits.skills_max_md_bytes` are
/// skipped with a warning.
pub fn scan_dir(
    root: &Path,
    enabled_skill_allowlist: Option<&[String]>,
    limits: &haven_common::config::ContextLimitsConfig,
) -> anyhow::Result<Vec<Skill>> {
    Ok(scan_dir_with_diagnostics(root, enabled_skill_allowlist, limits)?.skills)
}

struct SkillScanResult {
    skills: Vec<Skill>,
    diagnostics: Vec<SkillInfo>,
}

fn scan_dir_with_diagnostics(
    root: &Path,
    enabled_skill_allowlist: Option<&[String]>,
    limits: &haven_common::config::ContextLimitsConfig,
) -> anyhow::Result<SkillScanResult> {
    let mut out = Vec::new();
    let mut diagnostics = Vec::new();
    if !root.exists() {
        return Ok(SkillScanResult {
            skills: out,
            diagnostics,
        });
    }

    let root_canon = root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize skills root: {}", root.display()))?;

    let entries = std::fs::read_dir(root)
        .with_context(|| format!("failed to read skills root: {}", root.display()))?;

    let mut skill_dirs = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("skipping unreadable skill entry: {e}");
                continue;
            }
        };
        let p = entry.path();

        // Canonicalise to catch symlink/junction traversal (M4-01 review).
        let p_canon = match p.canonicalize() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "skipping skill entry {} (cannot canonicalise: {e})",
                    p.display()
                );
                continue;
            }
        };
        if !p_canon.starts_with(&root_canon) {
            tracing::warn!(
                "skipping skill entry outside skills root: {}",
                p_canon.display()
            );
            continue;
        }

        if !p.is_dir() {
            continue;
        }

        skill_dirs.push((entry.file_name().to_string_lossy().into_owned(), p, p_canon));
    }

    let skill_dirs = skip_case_insensitive_directory_collisions(skill_dirs);

    let mut candidates = Vec::new();
    for (directory_name, p, p_canon) in skill_dirs {
        let skill_md = p.join("SKILL.md");
        let skill_md = match skill_md.canonicalize() {
            Ok(canonical) if canonical.starts_with(&p_canon) => canonical,
            Ok(canonical) => {
                tracing::warn!(
                    "skipping SKILL.md outside skill root: {}",
                    canonical.display()
                );
                continue;
            }
            Err(_) => continue,
        };

        // File size cap (M4-01 review).
        let md_len = match std::fs::metadata(&skill_md) {
            Ok(m) => m.len(),
            Err(e) => {
                tracing::warn!(
                    error = %haven_common::error::sanitize_error_text(&e.to_string()),
                    "cannot stat SKILL.md"
                );
                continue;
            }
        };
        if md_len > limits.skills_max_md_bytes {
            tracing::warn!(
                "skipping oversized SKILL.md ({} bytes > {} cap)",
                md_len,
                limits.skills_max_md_bytes
            );
            continue;
        }

        let content = match std::fs::read(&skill_md) {
            Ok(bytes) => haven_common::encoding::decode_lossy(&bytes),
            Err(e) => {
                tracing::warn!(
                    error = %haven_common::error::sanitize_error_text(&e.to_string()),
                    "skipping unreadable SKILL.md"
                );
                continue;
            }
        };
        let max_parse_lines = limits.skills_max_parse_lines;
        let max_line_len = limits.skills_max_line_len;
        match parse_skill_md(&content, max_parse_lines, max_line_len) {
            Ok(manifest) => {
                let enabled = enabled_skill_allowlist
                    .map(|allowlist| allowlist.contains(&manifest.name))
                    .unwrap_or(true);
                candidates.push(Skill {
                    manifest,
                    root: p,
                    enabled,
                });
            }
            Err(e) => {
                let error = format!("Invalid SKILL.md: {}", e);
                tracing::warn!(
                    error = %haven_common::error::sanitize_error_text(&error),
                    "skill manifest could not be loaded"
                );
                diagnostics.push(SkillInfo::invalid_manifest(
                    directory_name,
                    &p,
                    haven_common::error::sanitize_error_text(&error),
                ));
            }
        }
    }

    out.extend(skip_case_insensitive_skill_collisions(candidates));
    diagnostics.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(SkillScanResult {
        skills: out,
        diagnostics,
    })
}

// ---------------------------------------------------------------------------
// SkillRegistry
// ---------------------------------------------------------------------------

struct Inner {
    root: Option<PathBuf>,
    /// `None` = all enabled, `Some(list)` = exhaustive allowlist.
    enabled_skill_allowlist: Option<Vec<String>>,
    skills: HashMap<String, Skill>,
    /// Invalid manifests shown in the catalog for diagnosis but never eligible
    /// for execution.
    skill_diagnostics: Vec<SkillInfo>,
    /// Unified context limits (SKILL.md size / parse caps).
    limits: haven_common::config::ContextLimitsConfig,
}

/// Registry of discovered Skills, backed by an in-memory map protected by a
/// `tokio::sync::RwLock` so disk refreshes and bridge queries share one
/// authoritative set of metadata and enablement state.
#[derive(Clone)]
pub struct SkillRegistry {
    inner: Arc<RwLock<Inner>>,
    catalog_version: Arc<AtomicU64>,
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner {
                root: None,
                enabled_skill_allowlist: None,
                skills: HashMap::new(),
                skill_diagnostics: Vec::new(),
                limits: haven_common::config::ContextLimitsConfig::default(),
            })),
            catalog_version: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Monotonic in-process version for the discovered/enabled Skill catalog.
    /// Consumers use this to invalidate derived prompt and capability views
    /// even when a caller has not rebuilt the broader tool registry yet.
    pub fn catalog_version(&self) -> u64 {
        self.catalog_version.load(Ordering::Relaxed)
    }

    /// Replace the unified context limits (SKILL.md size / parse caps).
    pub async fn set_limits(&self, limits: &haven_common::config::ContextLimitsConfig) {
        self.inner.write().await.limits = limits.clone();
    }

    /// Configure the skills root + optional exhaustive enabled allowlist, and
    /// trigger an immediate disk refresh.
    ///
    /// `enabled_skill_allowlist` semantics: `None` → all enabled;
    /// `Some(list)` → only skills whose names appear in that allowlist.
    pub async fn set_config(
        &self,
        root: Option<PathBuf>,
        enabled_skill_allowlist: Option<Vec<String>>,
    ) -> anyhow::Result<()> {
        {
            let mut g = self.inner.write().await;
            g.root = root;
            g.enabled_skill_allowlist = enabled_skill_allowlist;
        }
        self.refresh_from_disk().await
    }

    /// Resolve the effective skills root: configured root or the default
    /// `<app_data_dir>/skills`.
    fn resolve_root(configured: Option<&Path>) -> PathBuf {
        configured
            .map(PathBuf::from)
            .unwrap_or_else(ConfigLoader::default_skills_dir)
    }

    /// Re-scan the skills directory from disk, replacing the in-memory map.
    pub async fn refresh_from_disk(&self) -> anyhow::Result<()> {
        let (root, enabled_skill_allowlist, limits) = {
            let g = self.inner.read().await;
            (
                g.root.clone(),
                g.enabled_skill_allowlist.clone(),
                g.limits.clone(),
            )
        };
        let effective = Self::resolve_root(root.as_deref());
        let scanned =
            scan_dir_with_diagnostics(&effective, enabled_skill_allowlist.as_deref(), &limits)?;
        let mut g = self.inner.write().await;
        g.skills.clear();
        for s in scanned.skills {
            g.skills.insert(s.name().to_string(), s);
        }
        g.skill_diagnostics = scanned.diagnostics;
        self.catalog_version.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub async fn list_skill_infos(&self) -> Vec<SkillInfo> {
        let g = self.inner.read().await;
        let mut skills: Vec<_> = g.skills.values().map(SkillInfo::from).collect();
        skills.extend(g.skill_diagnostics.iter().cloned());
        // The short skill index is part of the cacheable system-prompt prefix.
        // Never let HashMap iteration order create a semantically identical but
        // byte-different prompt after a refresh or restart.
        skills.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.root.cmp(&b.root)));
        skills
    }

    pub async fn get_skill_info(&self, name: &str) -> Option<SkillInfo> {
        let g = self.inner.read().await;
        g.skills.get(name).map(SkillInfo::from)
    }

    /// Return the raw `Skill` object for execution (M4-02).
    pub async fn get_skill(&self, name: &str) -> Option<Skill> {
        let g = self.inner.read().await;
        g.skills.get(name).cloned()
    }

    /// Return all raw `Skill` objects (including disabled ones).
    pub async fn list_skills(&self) -> Vec<Skill> {
        let g = self.inner.read().await;
        g.skills.values().cloned().collect()
    }

    /// Toggle the enabled flag on a discovered skill and keep the registry's
    /// configured allowlist (`Inner.enabled_skill_allowlist`) in sync so the
    /// change survives `refresh_from_disk` and app restart (M4-01 review).
    ///
    /// When `enabled = false` and the allowlist was `None` (all enabled), the
    /// registry converts to an exhaustive `Some(list)` excluding the toggled
    /// skill, so the lone-disable edge case persists correctly.
    pub async fn set_enabled(&self, name: &str, enabled: bool) -> anyhow::Result<()> {
        let mut g = self.inner.write().await;
        let s = g
            .skills
            .get_mut(name)
            .ok_or_else(|| anyhow::anyhow!("skill '{name}' not loaded"))?;
        if enabled && !s.has_script() {
            anyhow::bail!(
                "skill '{name}' cannot be enabled because it has no entry script (expected scripts/main.py or scripts/{name}.py)"
            );
        }
        let changed = s.enabled != enabled;
        s.enabled = enabled;

        match enabled {
            true => {
                if let Some(list) = g.enabled_skill_allowlist.as_mut()
                    && !list.contains(&name.to_string())
                {
                    list.push(name.to_string());
                }
                // None means all enabled —no change.
            }
            false => {
                let all_names: Vec<String> = g.skills.keys().cloned().collect();
                match g.enabled_skill_allowlist.take() {
                    None => {
                        // Was all enabled; produce exhaustive allowlist minus name.
                        g.enabled_skill_allowlist =
                            Some(all_names.into_iter().filter(|n| n != name).collect());
                    }
                    Some(mut list) => {
                        list.retain(|n| n != name);
                        g.enabled_skill_allowlist = Some(list);
                    }
                }
            }
        }
        if changed {
            self.catalog_version.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Return the configured enabled-skill allowlist for persistence (used by
    /// the `set_skill_enabled` bridge to write back to `config.toml`).
    pub async fn enabled_skill_allowlist(&self) -> Option<Vec<String>> {
        self.inner.read().await.enabled_skill_allowlist.clone()
    }

    /// The effective skills root path (resolved default if unset).
    pub async fn resolved_root(&self) -> PathBuf {
        let g = self.inner.read().await;
        Self::resolve_root(g.root.as_deref())
    }

    /// Cheap fingerprints for every `<root>/<skill>/SKILL.md`. Detects added,
    /// removed, or modified skills without reading file contents. Used by the
    /// auto-refresh watcher to know when a rescan is worth doing.
    pub async fn list_skill_file_fingerprints(&self) -> Vec<SkillFileFingerprint> {
        let root = self.resolved_root().await;
        let mut fingerprints = Vec::new();
        let Ok(root_canon) = root.canonicalize() else {
            return fingerprints;
        };
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                let skill_dir = entry.path();
                let Ok(skill_dir_canon) = skill_dir.canonicalize() else {
                    continue;
                };
                if !skill_dir_canon.starts_with(&root_canon) {
                    continue;
                }
                let skill_md = skill_dir.join("SKILL.md");
                let Ok(skill_md_canon) = skill_md.canonicalize() else {
                    continue;
                };
                if !skill_md_canon.starts_with(&skill_dir_canon) {
                    continue;
                }
                if let Ok(meta) = std::fs::metadata(&skill_md_canon)
                    && let Ok(mtime) = meta.modified()
                {
                    fingerprints.push(SkillFileFingerprint {
                        path: skill_md,
                        modified_at: mtime,
                        byte_length: meta.len(),
                    });
                }
            }
        }
        fingerprints.sort();
        fingerprints
    }
}

/// Filesystem metadata used by the skill-directory watcher to detect changes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SkillFileFingerprint {
    pub path: PathBuf,
    pub modified_at: SystemTime,
    pub byte_length: u64,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    fn write_skill(parent: &Path, name: &str, md: &str, has_script: bool) -> PathBuf {
        let dir = parent.join(name);
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("SKILL.md"), md).unwrap();
        if has_script {
            std::fs::write(dir.join("scripts").join("main.py"), "print('hi')").unwrap();
        }
        dir
    }

    fn tmp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("haven_skills_test_{}", uuid::Uuid::new_v4()))
    }

    // -----------------------------------------------------------------------
    // Parser
    // -----------------------------------------------------------------------

    #[test]
    fn parse_full_skill_md() {
        let md = "# Skill: file-organizer\n\n## Metadata\n- name: file-organizer\n- description: org files\n- version: 1.0.0\n- language: python\n\n## Instructions\nDo the thing.\n";
        let m = parse_skill_md(md, 5000, 4096).unwrap();
        assert_eq!(m.name, "file-organizer");
        assert_eq!(m.description, "org files");
        assert_eq!(m.version.as_deref(), Some("1.0.0"));
        assert_eq!(m.language, Language::Python);
        assert!(m.instructions.contains("Do the thing."));
    }

    #[test]
    fn validate_skill_name_enforces_portable_names_and_reserved_devices() {
        let max_length = "x".repeat(128);
        for valid in ["a", "file-organizer_2", max_length.as_str()] {
            validate_skill_name(valid).unwrap();
        }
        let overlong = "x".repeat(129);
        for invalid in ["", overlong.as_str(), "bad name", "../skill", "技能"] {
            assert!(
                validate_skill_name(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
        for reserved in [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ] {
            assert!(
                validate_skill_name(reserved).is_err(),
                "accepted {reserved}"
            );
            assert!(
                validate_skill_name(&reserved.to_ascii_lowercase()).is_err(),
                "accepted lowercase {reserved}"
            );
        }
    }

    #[test]
    fn parse_rejects_invalid_and_reserved_skill_names() {
        for name in ["bad name", "../skill", "CON", "lpt9"] {
            let markdown = format!("# Skill: {name}\n## Instructions\nDo it.\n");
            assert!(
                parse_skill_md(&markdown, 5000, 4096).is_err(),
                "accepted {name:?}"
            );
        }
    }

    #[test]
    fn parse_missing_name_errors() {
        let md = "## Metadata\n- description: x\n";
        assert!(parse_skill_md(md, 5000, 4096).is_err());
    }

    #[test]
    fn parse_h1_provides_name_when_metadata_omitted() {
        let md = "# Skill: fallback-named\n\n## Instructions\nonly instructions\n";
        let m = parse_skill_md(md, 5000, 4096).unwrap();
        assert_eq!(m.name, "fallback-named");
    }

    #[test]
    fn parse_unsupported_language_preserved() {
        let md = "# Skill: x\n## Metadata\n- language: bash\n## Instructions\ni\n";
        let m = parse_skill_md(md, 5000, 4096).unwrap();
        assert_eq!(m.language, Language::Unsupported("bash".to_string()));
        assert_eq!(m.language.as_str(), "bash");
    }

    #[test]
    fn parse_strips_bom() {
        let md = "\u{FEFF}# Skill: bom\n## Metadata\n- description: d\n## Instructions\ni\n";
        let m = parse_skill_md(md, 5000, 4096).unwrap();
        assert_eq!(m.name, "bom");
    }

    #[test]
    fn parse_rejects_unknown_metadata_fields() {
        let md = "# Skill: x\n## Metadata\n- allowed_tools: [a, b]\n- description: d\n## Instructions\ni\n";
        assert!(parse_skill_md(md, 5000, 4096).is_err());
    }

    #[test]
    fn parse_rejects_oversized_line() {
        let long_line = "a".repeat(4096 + 1);
        let md =
            format!("# Skill: x\n## Metadata\n- description: {long_line}\n## Instructions\ni\n");
        assert!(parse_skill_md(&md, 5000, 4096).is_err());
    }

    // -----------------------------------------------------------------------
    // scan_dir
    // -----------------------------------------------------------------------

    #[test]
    fn scan_dir_picks_valid_skips_invalid() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "good-a",
            "# Skill: good-a\n## Metadata\n- description: a\n## Instructions\ni\n",
            true,
        );
        write_skill(
            &dir,
            "good-b",
            "# Skill: good-b\n## Metadata\n- description: b\n## Instructions\ni\n",
            false,
        );
        // invalid SKILL.md missing name
        let bad = dir.join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(
            bad.join("SKILL.md"),
            "## Metadata\n- description: no name\n",
        )
        .unwrap();
        // not-a-dir SKILL.md-less
        std::fs::create_dir_all(dir.join("no-skill-md")).unwrap();

        let skills = scan_dir(&dir, None, &Default::default()).unwrap();
        let names: Vec<&str> = skills.iter().map(|s| s.name()).collect();
        assert_eq!(names, vec!["good-a", "good-b"]);
        assert!(
            skills
                .iter()
                .find(|s| s.name() == "good-a")
                .unwrap()
                .has_script()
        );
        assert!(
            !skills
                .iter()
                .find(|s| s.name() == "good-b")
                .unwrap()
                .has_script()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_dir_skips_all_case_insensitive_directory_and_manifest_collisions() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let upper_dir = write_skill(
            &dir,
            "Echo",
            "# Skill: upper-name\n## Metadata\n- name: Echo\n- description: upper\n## Instructions\ni\n",
            false,
        );
        let lower_dir = dir.join("echo");
        if std::fs::create_dir_all(&lower_dir).is_err()
            || upper_dir.canonicalize().ok() == lower_dir.canonicalize().ok()
        {
            // A case-insensitive filesystem cannot represent this directory
            // pair; the portable collision helper is covered below.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        std::fs::write(
            lower_dir.join("SKILL.md"),
            "# Skill: lower-name\n## Metadata\n- name: echo\n- description: lower\n## Instructions\ni\n",
        )
        .unwrap();
        write_skill(
            &dir,
            "separate-one",
            "# Skill: shared\n## Metadata\n- name: shared\n- description: one\n## Instructions\ni\n",
            false,
        );
        write_skill(
            &dir,
            "separate-two",
            "# Skill: SHARED\n## Metadata\n- name: SHARED\n- description: two\n## Instructions\ni\n",
            false,
        );
        write_skill(
            &dir,
            "unique",
            "# Skill: unique\n## Metadata\n- name: unique\n- description: unique\n## Instructions\ni\n",
            false,
        );

        let skills = scan_dir(&dir, None, &Default::default()).unwrap();
        let names: Vec<&str> = skills.iter().map(Skill::name).collect();
        assert_eq!(names, ["unique"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn case_insensitive_collision_detection_is_ascii_portable() {
        assert_eq!(
            case_insensitive_collisions(["Echo", "echo", "single"]),
            BTreeSet::from(["echo".to_string()])
        );

        let directories = skip_case_insensitive_directory_collisions(vec![
            ("Echo".into(), PathBuf::from("Echo"), PathBuf::from("Echo")),
            ("echo".into(), PathBuf::from("echo"), PathBuf::from("echo")),
            (
                "single".into(),
                PathBuf::from("single"),
                PathBuf::from("single"),
            ),
        ]);
        assert_eq!(directories.len(), 1);
        assert_eq!(directories[0].0, "single");

        let make_skill = |name: &str| {
            Skill::from_manifest_unchecked(
                SkillManifest {
                    name: name.to_string(),
                    description: String::new(),
                    version: None,
                    language: Language::Python,
                    instructions: String::new(),
                },
                PathBuf::new(),
                true,
            )
        };
        let filtered = skip_case_insensitive_skill_collisions(vec![
            make_skill("Echo"),
            make_skill("echo"),
            make_skill("single"),
        ]);
        assert_eq!(
            filtered.iter().map(Skill::name).collect::<Vec<_>>(),
            ["single"]
        );
    }

    #[test]
    fn scan_dir_enabled_skill_allowlist() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "one",
            "# Skill: one\n## Metadata\n- description: o\n## Instructions\ni\n",
            false,
        );
        write_skill(
            &dir,
            "two",
            "# Skill: two\n## Metadata\n- description: t\n## Instructions\ni\n",
            false,
        );
        let skills = scan_dir(&dir, Some(&["two".to_string()]), &Default::default()).unwrap();
        let enabled: Vec<bool> = skills.iter().map(|s| s.enabled()).collect();
        assert_eq!(enabled, vec![false, true]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn scriptless_skills_are_projected_disabled_and_cannot_be_enabled() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "instruction-only",
            "# Skill: instruction-only\n\n## Metadata\n- description: no executable entry point\n\n## Instructions\nDo the task.\n",
            false,
        );

        let registry = SkillRegistry::new();
        registry.set_config(Some(dir.clone()), None).await.unwrap();
        let listed = registry.list_skill_infos().await;
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].has_script);
        assert!(!listed[0].enabled);

        let error = registry
            .set_enabled("instruction-only", true)
            .await
            .expect_err("scriptless skill must not be enabled as an executable tool");
        assert!(error.to_string().contains("no entry script"));
        assert!(
            registry
                .set_enabled("instruction-only", false)
                .await
                .is_ok()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_dir_none_all_enabled() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "a",
            "# Skill: a\n## Metadata\n- description: a\n## Instructions\ni\n",
            false,
        );
        let skills = scan_dir(&dir, None, &Default::default()).unwrap();
        assert!(skills.iter().all(|s| s.enabled()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_dir_empty_some_disables_all() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "a",
            "# Skill: a\n## Metadata\n- description: a\n## Instructions\ni\n",
            false,
        );
        let skills = scan_dir(&dir, Some(&[] as &[String]), &Default::default()).unwrap();
        assert!(!skills[0].enabled());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_dir_skips_oversized_file() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "small",
            "# Skill: small\n## Metadata\n- description: ok\n## Instructions\ni\n",
            false,
        );
        // Create a SKILL.md larger than the cap
        let big = dir.join("big");
        std::fs::create_dir_all(&big).unwrap();
        let big_content = format!(
            "# Skill: big\n## Metadata\n- description: {}\n## Instructions\ni\n",
            "x".repeat(256 * 1024)
        );
        std::fs::write(big.join("SKILL.md"), &big_content).unwrap();

        let skills = scan_dir(&dir, None, &Default::default()).unwrap();
        let names: Vec<&str> = skills.iter().map(|s| s.name()).collect();
        assert_eq!(names, vec!["small"], "oversized entry should be skipped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_dir_rejects_skill_manifest_symlink_outside_skill_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("skills");
        let skill_dir = root.join("external-manifest");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let outside_manifest = outside.join("SKILL.md");
        std::fs::write(
            &outside_manifest,
            "# Skill: external-manifest\n## Metadata\n- description: outside\n",
        )
        .unwrap();
        if symlink_file(&outside_manifest, &skill_dir.join("SKILL.md")).is_err() {
            // Windows CI may not grant symlink privileges to the test process.
            return;
        }

        let skills = scan_dir(&root, None, &Default::default()).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn entry_script_rejects_symlink_target_outside_skill_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("skills");
        let skill_dir = write_skill(
            &root,
            "linked-script",
            "# Skill: linked-script\n## Metadata\n- description: linked\n",
            false,
        );
        let outside_script = temp.path().join("outside.py");
        std::fs::write(&outside_script, "print('outside')").unwrap();
        if symlink_file(&outside_script, &skill_dir.join("scripts").join("main.py")).is_err() {
            // Windows CI may not grant symlink privileges to the test process.
            return;
        }

        let skill = scan_dir(&root, None, &Default::default())
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert!(skill.entry_script().is_none());
        assert!(!SkillInfo::from(&skill).has_script);
    }

    // -----------------------------------------------------------------------
    // SkillRegistry
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn registry_file_fingerprints_track_changes() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let registry = SkillRegistry::new();
        registry.set_config(Some(dir.clone()), None).await.unwrap();

        // Empty folder → no file fingerprints.
        assert!(registry.list_skill_file_fingerprints().await.is_empty());

        write_skill(
            &dir,
            "a",
            "# Skill: a\n## Metadata\n- description: a\n## Instructions\ni\n",
            false,
        );
        let fingerprints_before_edit = registry.list_skill_file_fingerprints().await;
        assert_eq!(fingerprints_before_edit.len(), 1);
        assert!(fingerprints_before_edit[0].path.ends_with("SKILL.md"));
        let original_byte_length = fingerprints_before_edit[0].byte_length;

        // Modified SKILL.md (different length) → its fingerprint changes.
        write_skill(
            &dir,
            "a",
            "# Skill: a\n## Metadata\n- description: a much longer description\n## Instructions\ni\n",
            false,
        );
        let fingerprints_after_edit = registry.list_skill_file_fingerprints().await;
        assert_ne!(fingerprints_before_edit, fingerprints_after_edit);
        assert!(fingerprints_after_edit[0].byte_length > original_byte_length);

        // Added skill → fingerprint list gains an entry.
        write_skill(
            &dir,
            "b",
            "# Skill: b\n## Metadata\n- description: b\n## Instructions\ni\n",
            false,
        );
        let fingerprints_after_add = registry.list_skill_file_fingerprints().await;
        assert_eq!(fingerprints_after_add.len(), 2);

        // Removed skill → fingerprint list loses the entry.
        std::fs::remove_dir_all(dir.join("a")).unwrap();
        let fingerprints_after_remove = registry.list_skill_file_fingerprints().await;
        assert_eq!(fingerprints_after_remove.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn registry_refresh_and_query() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "alpha",
            "# Skill: alpha\n## Metadata\n- description: a\n- version: 2.0\n- language: python\n## Instructions\ni\n",
            true,
        );

        let registry = SkillRegistry::new();
        registry.set_config(Some(dir.clone()), None).await.unwrap();
        let after_initial_refresh = registry.catalog_version();
        assert!(after_initial_refresh > 0);

        let list = registry.list_skill_infos().await;
        assert_eq!(list.len(), 1);
        let s = &list[0];
        assert_eq!(s.name, "alpha");
        assert_eq!(s.version.as_deref(), Some("2.0"));
        assert!(s.has_script);
        assert!(s.enabled);

        // Disable → persisted as Some exhaustive list minus alpha
        registry.set_enabled("alpha", false).await.unwrap();
        assert!(registry.catalog_version() > after_initial_refresh);
        let updated = registry.get_skill_info("alpha").await.unwrap();
        assert!(!updated.enabled);

        // The inner allowlist should now be Some([]) (lone skill disabled).
        let inner_enabled = registry.enabled_skill_allowlist().await;
        assert_eq!(inner_enabled, Some(vec![] as Vec<String>));

        // refresh_from_disk should NOT re-enable alpha (the allowlist is now
        // Some([]) which means "none enabled").
        registry.refresh_from_disk().await.unwrap();
        let after_refresh = registry.get_skill_info("alpha").await.unwrap();
        assert!(
            !after_refresh.enabled,
            "alpha must stay disabled after refresh"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn registry_refresh_clears_removed_skills() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "a",
            "# Skill: a\n## Metadata\n- description: a\n## Instructions\ni\n",
            false,
        );
        let registry = SkillRegistry::new();
        registry.set_config(Some(dir.clone()), None).await.unwrap();
        assert_eq!(registry.list_skill_infos().await.len(), 1);
        std::fs::remove_dir_all(dir.join("a")).unwrap();
        registry.refresh_from_disk().await.unwrap();
        assert!(registry.list_skill_infos().await.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn registry_set_enabled_syncs_allowlist() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "a",
            "# Skill: a\n## Metadata\n- description: a\n## Instructions\ni\n",
            true,
        );
        write_skill(
            &dir,
            "b",
            "# Skill: b\n## Metadata\n- description: b\n## Instructions\ni\n",
            true,
        );
        let registry = SkillRegistry::new();
        registry.set_config(Some(dir.clone()), None).await.unwrap();

        // Disable a, enable b explicitly
        registry.set_enabled("a", false).await.unwrap();
        // b should still be enabled (None → all, but we transitioned to Some(["b"]) after disabling a)
        let list = registry.list_skill_infos().await;
        let a = list.iter().find(|s| s.name == "a").unwrap();
        let b = list.iter().find(|s| s.name == "b").unwrap();
        assert!(!a.enabled);
        assert!(b.enabled);

        // Inner allowlist should be Some(["b"])
        let enabled_skill_allowlist = registry.enabled_skill_allowlist().await;
        assert_eq!(enabled_skill_allowlist, Some(vec!["b".to_string()]));

        // Re-enable a
        registry.set_enabled("a", true).await.unwrap();
        let list = registry.list_skill_infos().await;
        assert!(list.iter().all(|s| s.enabled));
        let enabled_skill_allowlist = registry.enabled_skill_allowlist().await;
        assert_eq!(
            enabled_skill_allowlist,
            Some(vec!["b".to_string(), "a".to_string()])
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn registry_list_is_sorted_for_prompt_stability() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["zeta", "alpha", "middle"] {
            write_skill(
                &dir,
                name,
                &format!(
                    "# Skill: {name}\n## Metadata\n- description: {name}\n## Instructions\ni\n"
                ),
                false,
            );
        }

        let registry = SkillRegistry::new();
        registry.set_config(Some(dir.clone()), None).await.unwrap();
        let names: Vec<String> = registry
            .list_skill_infos()
            .await
            .into_iter()
            .map(|skill| skill.name)
            .collect();
        assert_eq!(names, ["alpha", "middle", "zeta"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
