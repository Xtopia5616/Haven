//! Skills registry: discovery, parsing, and management of reusable agent
//! skills (`SKILL.md` + scripts), plus the virtual-environment manager used
//! to run Python skills sandboxed.
//!
//! Execution of a skill as a tool lives in `haven-tools` (the `SkillRunner`
//! bridges skills into the tool-execution result types).

use anyhow::Context;
use haven_common::config::ConfigLoader;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use tokio::sync::{Mutex, RwLock};

/// Counts every direct child in the root, including non-directory entries.
const MAX_SKILL_ROOT_ENTRIES: usize = 1024;

// ---------------------------------------------------------------------------
// Manifest types
// ---------------------------------------------------------------------------

mod venv;

pub use venv::{
    ProcessStartAuthorization, ProcessStartPermit, SkillProcessPurpose, SkillProcessRequest,
    VenvManager,
};

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
    fn parse(raw: &str) -> Self {
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

/// Structured metadata parsed from `SKILL.md`.
#[derive(Debug, Clone)]
struct SkillManifest {
    name: String,
    description: String,
    version: Option<String>,
    language: Language,
    /// Full text of the `## Instructions` section, verbatim
    /// (`{{param}}` placeholders preserved for later render phases).
    instructions: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    license: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    compatibility: Option<String>,
    #[serde(
        default,
        rename = "allowed-tools",
        deserialize_with = "deserialize_optional_string"
    )]
    allowed_tools: Option<String>,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
}

fn deserialize_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
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
    fn version(&self) -> Option<&str> {
        self.manifest.version.as_deref()
    }
    pub fn language(&self) -> &Language {
        &self.manifest.language
    }
    /// Return the author-provided instructions section from `SKILL.md`.
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
        self.open_entry_script().is_some()
    }

    /// Open the entry script and validate the opened file identity against its
    /// resolved path while retaining the handle for callers that need a stable
    /// content snapshot.
    pub fn open_entry_script(&self) -> Option<(PathBuf, File)> {
        let root = self.root.canonicalize().ok()?;
        let scripts = root.join("scripts");
        let main = scripts.join("main.py");
        let named = scripts.join(format!("{}.py", self.manifest.name));
        open_file_within_root(&root, &main)
            .ok()
            .or_else(|| open_file_within_root(&root, &named).ok())
    }
}

/// Open a file first, then verify that the path still resolves to that exact
/// file identity beneath `root`. The returned handle remains the object whose
/// identity was checked, so later pathname changes cannot redirect reads.
fn open_file_within_root(root: &Path, candidate: &Path) -> io::Result<(PathBuf, File)> {
    let file = haven_platform::filesystem::open_read_only(candidate)?;
    let resolved_path = fs::canonicalize(candidate)?;
    let resolved_file = haven_platform::filesystem::open_read_only(&resolved_path)?;
    validate_open_file_path(root, &resolved_path, &file, &resolved_file)?;
    Ok((resolved_path, file))
}

/// Confirm the current resolved pathname identifies the file already held by
/// an open handle. Callers must perform the open before resolving the path.
pub(crate) fn validate_open_file_path(
    root: &Path,
    resolved_path: &Path,
    opened_file: &File,
    resolved_file: &File,
) -> io::Result<()> {
    if !resolved_path.starts_with(root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "resolved file is outside the allowed root",
        ));
    }
    if !opened_file.metadata()?.is_file()
        || !resolved_file.metadata()?.is_file()
        || !haven_platform::filesystem::same_file_identity(opened_file, resolved_file)?
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file changed during path validation",
        ));
    }
    Ok(())
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
    /// Whether a valid manifest resolves to an entry script. For an invalid
    /// manifest this is unknown; inspect `manifest_error` before interpreting it.
    pub has_script: bool,
    /// Why a valid skill cannot currently be executed.
    pub unavailable_reason: Option<String>,
    /// A SKILL.md parsing error. Invalid manifests remain visible in the
    /// catalog for diagnosis, but never enter the executable registry.
    pub manifest_error: Option<String>,
}

/// Skill metadata paired with its local directory for the Agent's explicit
/// `skills_list` tool output. This path must not cross the renderer IPC.
#[derive(Debug, Clone)]
pub struct SkillCatalogEntry {
    pub info: SkillInfo,
    pub root: PathBuf,
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
    fn invalid_manifest(name: String, error: String) -> Self {
        Self {
            name,
            description: String::new(),
            version: None,
            language: "unknown".into(),
            enabled: false,
            executable: false,
            has_script: false,
            unavailable_reason: None,
            manifest_error: Some(error),
        }
    }
}

// ---------------------------------------------------------------------------
// SKILL.md parser
// ---------------------------------------------------------------------------

/// Validate a Skill name against the Agent Skills naming rules before it is
/// used as a tool identity or filesystem component.
pub fn validate_skill_name(name: &str) -> anyhow::Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == b'-'
        })
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--");
    if !valid {
        anyhow::bail!(
            "invalid skill name '{}': use 1-64 lowercase ASCII letters, digits, and single hyphens",
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
    if !ambiguous_names.is_empty() {
        tracing::warn!(
            collision_count = ambiguous_names.len(),
            "skipping Skills with Windows case-insensitive name collision"
        );
    }
    candidates
        .into_iter()
        .filter(|skill| !ambiguous_names.contains(&skill.name().to_ascii_lowercase()))
        .collect()
}

fn skip_case_insensitive_directory_collisions(
    directories: Vec<(String, PathBuf)>,
) -> Vec<(String, PathBuf)> {
    let ambiguous_names =
        case_insensitive_collisions(directories.iter().map(|(name, _)| name.as_str()));
    if !ambiguous_names.is_empty() {
        tracing::warn!(
            collision_count = ambiguous_names.len(),
            "skipping Skill directories with Windows case-insensitive name collision"
        );
    }
    directories
        .into_iter()
        .filter(|(name, _)| !ambiguous_names.contains(&name.to_ascii_lowercase()))
        .collect()
}

/// Parse an Agent Skills `SKILL.md` document into Haven's runtime metadata.
/// The YAML frontmatter must be followed by Markdown instructions. Haven keeps
/// its Python runner metadata in the standard `metadata` extension map.
///
/// **Safety:** The caller reads the file under a byte cap; this parser also
/// enforces maximum line count and UTF-8 bytes per line from `context_limits`.
fn parse_skill_md(
    input: &str,
    max_parse_lines: usize,
    max_line_bytes: usize,
) -> anyhow::Result<SkillManifest> {
    let input = input.strip_prefix('\u{FEFF}').unwrap_or(input);

    for (i, line) in input.lines().enumerate() {
        if i >= max_parse_lines {
            anyhow::bail!("SKILL.md exceeds {max_parse_lines} lines");
        }
        if line.len() > max_line_bytes {
            anyhow::bail!(
                "SKILL.md line {} exceeds {max_line_bytes} UTF-8 bytes",
                i + 1
            );
        }
    }

    let mut lines = input.split_inclusive('\n');
    let opening_line = lines
        .next()
        .context("SKILL.md must start with YAML frontmatter")?;
    if strip_line_ending(opening_line) != "---" {
        anyhow::bail!("SKILL.md must start with YAML frontmatter");
    }

    let yaml_start = opening_line.len();
    let mut offset = yaml_start;
    let mut yaml_end = None;
    let mut body_start = None;
    for line in lines {
        let line_start = offset;
        offset += line.len();
        if strip_line_ending(line) == "---" {
            yaml_end = Some(line_start);
            body_start = Some(offset);
            break;
        }
    }
    let yaml_end = yaml_end.context("SKILL.md frontmatter is missing its closing '---'")?;
    let body_start = body_start.context("SKILL.md frontmatter is missing its closing '---'")?;
    let frontmatter: SkillFrontmatter = serde_saphyr::from_str(&input[yaml_start..yaml_end])
        .map_err(|_| anyhow::anyhow!("invalid SKILL.md YAML frontmatter"))?;

    validate_skill_name(&frontmatter.name)?;
    if frontmatter.description.trim().is_empty() {
        anyhow::bail!("SKILL.md description must not be empty");
    }
    if frontmatter.description.chars().count() > 1_024 {
        anyhow::bail!("SKILL.md description exceeds 1024 characters");
    }
    if frontmatter
        .compatibility
        .as_deref()
        .is_some_and(|value| value.trim().is_empty() || value.chars().count() > 500)
    {
        anyhow::bail!("SKILL.md compatibility must contain 1-500 characters");
    }

    // These standard fields are accepted, but they do not grant permissions
    // or alter Haven's tool authorization policy.
    let _ = (frontmatter.license, frontmatter.allowed_tools);
    let version = frontmatter
        .metadata
        .get("version")
        .filter(|value| !value.is_empty())
        .cloned();
    let language = frontmatter
        .metadata
        .get("haven.language")
        .map(|value| Language::parse(value))
        .unwrap_or(Language::Python);
    let instructions = input[body_start..].trim().to_string();
    Ok(SkillManifest {
        name: frontmatter.name,
        description: frontmatter.description.trim().to_string(),
        version,
        language,
        instructions,
    })
}

fn strip_line_ending(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

// ---------------------------------------------------------------------------
// Directory scanning
// ---------------------------------------------------------------------------

/// Project successfully parsed registry entries for scanner unit tests.
/// Production refresh must consume the complete `scan_skill_directory` result.
#[cfg(test)]
fn scan_parsed_skill_entries(
    root: &Path,
    enabled_skill_allowlist: Option<&[String]>,
    limits: &haven_common::config::ContextLimitsConfig,
) -> anyhow::Result<Vec<Skill>> {
    Ok(scan_skill_directory(root, enabled_skill_allowlist, limits)?.skills)
}

struct SkillScanResult {
    skills: Vec<Skill>,
    diagnostics: Vec<SkillCatalogEntry>,
}

fn read_bounded<R: Read>(reader: R, max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    reader
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        Ok(None)
    } else {
        Ok(Some(bytes))
    }
}

fn scan_skill_directory(
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
        .context("failed to canonicalize skills root")?;

    let entries = std::fs::read_dir(&root_canon).context("failed to read skills root")?;

    let mut skill_dirs = Vec::new();
    let mut root_entry_count = 0;
    for entry in entries {
        root_entry_count += 1;
        if root_entry_count > MAX_SKILL_ROOT_ENTRIES {
            anyhow::bail!("skills root exceeds the {MAX_SKILL_ROOT_ENTRIES}-entry scan limit");
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                tracing::warn!("skipping unreadable skill entry");
                continue;
            }
        };
        let p = entry.path();

        // Canonicalise to catch symlink/junction traversal (M4-01 review).
        let p_canon = match p.canonicalize() {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!("skipping skill entry that cannot be canonicalised");
                continue;
            }
        };
        if !p_canon.starts_with(&root_canon) {
            tracing::warn!("skipping skill entry outside skills root");
            continue;
        }

        if !p_canon.is_dir() {
            continue;
        }

        skill_dirs.push((entry.file_name().to_string_lossy().into_owned(), p_canon));
    }

    let skill_dirs = skip_case_insensitive_directory_collisions(skill_dirs);

    let mut candidates = Vec::new();
    for (directory_name, p_canon) in skill_dirs {
        let skill_md = p_canon.join("SKILL.md");
        // Open first, then validate the resolved path against the identity of
        // that same handle. Reading the handle below cannot be redirected by a
        // concurrent rename or symlink swap in the Skills directory.
        let (_, file) = match open_file_within_root(&p_canon, &skill_md) {
            Ok(opened) => opened,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                tracing::warn!("skipping SKILL.md that failed path validation");
                continue;
            }
            Err(_) => continue,
        };

        // File size cap (M4-01 review) comes from the opened file handle.
        let md_len = match file.metadata() {
            Ok(metadata) => metadata.len(),
            Err(_) => {
                tracing::warn!("cannot stat SKILL.md");
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

        let content = match read_bounded(file, limits.skills_max_md_bytes) {
            Ok(Some(bytes)) => haven_common::encoding::decode_lossy(&bytes),
            Ok(None) => {
                tracing::warn!("skipping SKILL.md that exceeded its byte cap while reading");
                continue;
            }
            Err(_) => {
                tracing::warn!("skipping unreadable SKILL.md");
                continue;
            }
        };
        let max_parse_lines = limits.skills_max_parse_lines;
        let max_line_bytes = limits.skills_max_line_bytes;
        match parse_skill_md(&content, max_parse_lines, max_line_bytes) {
            Ok(manifest) => {
                if manifest.name != directory_name {
                    diagnostics.push(SkillCatalogEntry {
                        info: SkillInfo::invalid_manifest(
                            directory_name,
                            "Invalid SKILL.md: name must match the skill directory name".into(),
                        ),
                        root: p_canon,
                    });
                    continue;
                }
                let enabled = enabled_skill_allowlist
                    .map(|allowlist| allowlist.contains(&manifest.name))
                    .unwrap_or(true);
                candidates.push(Skill {
                    manifest,
                    root: p_canon,
                    enabled,
                });
            }
            Err(e) => {
                let error = format!("Invalid SKILL.md: {}", e);
                tracing::warn!("skill manifest could not be loaded");
                diagnostics.push(SkillCatalogEntry {
                    info: SkillInfo::invalid_manifest(
                        directory_name,
                        haven_common::error::sanitize_error_text(&error),
                    ),
                    root: p_canon,
                });
            }
        }
    }

    out.extend(skip_case_insensitive_skill_collisions(candidates));
    let valid_names: BTreeSet<_> = out
        .iter()
        .map(|skill| skill.name().to_ascii_lowercase())
        .collect();
    diagnostics.retain(|entry| !valid_names.contains(&entry.info.name.to_ascii_lowercase()));
    diagnostics.sort_by(|a, b| a.info.name.cmp(&b.info.name));
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
    skill_diagnostics: Vec<SkillCatalogEntry>,
    /// Unified context limits (SKILL.md size / parse caps).
    limits: haven_common::config::ContextLimitsConfig,
}

/// Registry of discovered Skills, backed by an in-memory map protected by a
/// `tokio::sync::RwLock` so disk refreshes and bridge queries share one
/// authoritative set of metadata and enablement state.
#[derive(Clone)]
pub struct SkillRegistry {
    inner: Arc<RwLock<Inner>>,
    /// Serializes catalog/config mutations while filesystem scans run outside
    /// the async executor and without holding `inner`'s reader/writer lock.
    refresh_lock: Arc<Mutex<()>>,
    catalog_version: Arc<AtomicU64>,
    last_watcher_fingerprints: Arc<Mutex<Option<Vec<SkillFileFingerprint>>>>,
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
            refresh_lock: Arc::new(Mutex::new(())),
            catalog_version: Arc::new(AtomicU64::new(0)),
            last_watcher_fingerprints: Arc::new(Mutex::new(None)),
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
        let _refresh = self.refresh_lock.lock().await;
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
        let _refresh = self.refresh_lock.lock().await;
        let limits = self.inner.read().await.limits.clone();
        let effective = Self::resolve_root(root.as_deref());
        let scanned =
            scan_skill_directory_async(effective, enabled_skill_allowlist.clone(), limits).await?;

        // Publish configuration and the catalog from the same successful
        // scan. If scanning fails, the prior runtime snapshot stays intact.
        let mut g = self.inner.write().await;
        g.root = root;
        g.enabled_skill_allowlist = enabled_skill_allowlist;
        Self::replace_catalog(&mut g, scanned);
        self.catalog_version.fetch_add(1, Ordering::Relaxed);
        Ok(())
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
        let _refresh = self.refresh_lock.lock().await;
        let (root, enabled_skill_allowlist, limits) = {
            let g = self.inner.read().await;
            (
                Self::resolve_root(g.root.as_deref()),
                g.enabled_skill_allowlist.clone(),
                g.limits.clone(),
            )
        };
        let scanned = scan_skill_directory_async(root, enabled_skill_allowlist, limits).await?;
        let mut g = self.inner.write().await;
        Self::replace_catalog(&mut g, scanned);
        self.catalog_version.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn replace_catalog(g: &mut Inner, scanned: SkillScanResult) {
        g.skills.clear();
        for s in scanned.skills {
            g.skills.insert(s.name().to_string(), s);
        }
        g.skill_diagnostics = scanned.diagnostics;
    }

    /// Refresh the catalog when the configured skill files have changed.
    ///
    /// The watcher owns polling cadence and notifications; this registry owns
    /// filesystem change detection so its fingerprint representation never
    /// crosses the Skills boundary.
    pub async fn refresh_if_changed(&self) -> anyhow::Result<bool> {
        let mut last_fingerprints = self.last_watcher_fingerprints.lock().await;
        let fingerprints = self.current_file_fingerprints().await?;
        if last_fingerprints.as_ref() == Some(&fingerprints) {
            return Ok(false);
        }

        self.refresh_from_disk().await?;
        *last_fingerprints = Some(fingerprints);
        Ok(true)
    }

    pub async fn list_skill_infos(&self) -> Vec<SkillInfo> {
        let g = self.inner.read().await;
        let mut skills: Vec<_> = g.skills.values().map(SkillInfo::from).collect();
        skills.extend(g.skill_diagnostics.iter().map(|entry| entry.info.clone()));
        // The short skill index is part of the cacheable system-prompt prefix.
        // Never let HashMap iteration order create a semantically identical but
        // byte-different prompt after a refresh or restart.
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        skills
    }

    /// Return Skill metadata with local paths for the Agent-only diagnostic
    /// tool. Renderer command DTOs must use [`Self::list_skill_infos`].
    pub async fn list_skill_catalog_entries(&self) -> Vec<SkillCatalogEntry> {
        let g = self.inner.read().await;
        let mut entries: Vec<_> = g
            .skills
            .values()
            .map(|skill| SkillCatalogEntry {
                info: SkillInfo::from(skill),
                root: skill.root().to_path_buf(),
            })
            .collect();
        entries.extend(g.skill_diagnostics.iter().cloned());
        entries.sort_by(|a, b| a.info.name.cmp(&b.info.name));
        entries
    }

    pub async fn get_skill_info(&self, name: &str) -> Option<SkillInfo> {
        let g = self.inner.read().await;
        g.skills.get(name).map(SkillInfo::from)
    }

    /// Return the raw `Skill` object for execution (M4-02).
    #[cfg(any(test, feature = "test-support"))]
    pub async fn get_skill(&self, name: &str) -> Option<Skill> {
        let g = self.inner.read().await;
        g.skills.get(name).cloned()
    }

    /// Return a skill only when it is currently enabled and has a valid entry
    /// script. Execution callers use this owner-checked lookup immediately
    /// before starting work; a skill disabled after this snapshot is treated
    /// as already in flight.
    pub async fn get_executable_skill(&self, name: &str) -> anyhow::Result<Skill> {
        let g = self.inner.read().await;
        let skill = g
            .skills
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("skill '{name}' not found"))?;
        anyhow::ensure!(skill.enabled(), "skill '{name}' is disabled");
        anyhow::ensure!(skill.has_script(), "skill '{name}' has no entry script");
        Ok(skill.clone())
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
        let _refresh = self.refresh_lock.lock().await;
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

    /// Cheap private fingerprints for every `<root>/<skill>/SKILL.md`.
    /// Detects added, removed, or modified skills without reading contents.
    async fn current_file_fingerprints(&self) -> anyhow::Result<Vec<SkillFileFingerprint>> {
        let root = self.resolved_root().await;
        tokio::task::spawn_blocking(move || current_file_fingerprints_from_disk(&root))
            .await
            .context("skill fingerprint scan task failed")?
    }
}

/// Private filesystem metadata used to detect skill-directory changes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SkillFileFingerprint {
    pub path: PathBuf,
    pub modified_at: SystemTime,
    pub byte_length: u64,
}

fn current_file_fingerprints_from_disk(root: &Path) -> anyhow::Result<Vec<SkillFileFingerprint>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let root_canon = root
        .canonicalize()
        .context("failed to canonicalize skills root")?;
    let entries = std::fs::read_dir(&root_canon).context("failed to read skills root")?;
    let mut fingerprints = Vec::new();
    let mut root_entry_count = 0;
    for entry in entries {
        root_entry_count += 1;
        if root_entry_count > MAX_SKILL_ROOT_ENTRIES {
            anyhow::bail!("skills root exceeds the {MAX_SKILL_ROOT_ENTRIES}-entry scan limit");
        }
        let Ok(entry) = entry else {
            continue;
        };
        let skill_dir = entry.path();
        let Ok(skill_dir_canon) = skill_dir.canonicalize() else {
            continue;
        };
        if !skill_dir_canon.starts_with(&root_canon) {
            continue;
        }
        let skill_md = skill_dir_canon.join("SKILL.md");
        let Ok((_, file)) = open_file_within_root(&skill_dir_canon, &skill_md) else {
            continue;
        };
        if let Ok(metadata) = file.metadata()
            && let Ok(modified_at) = metadata.modified()
        {
            fingerprints.push(SkillFileFingerprint {
                path: skill_md,
                modified_at,
                byte_length: metadata.len(),
            });
        }
    }
    fingerprints.sort();
    Ok(fingerprints)
}

async fn scan_skill_directory_async(
    root: PathBuf,
    enabled_skill_allowlist: Option<Vec<String>>,
    limits: haven_common::config::ContextLimitsConfig,
) -> anyhow::Result<SkillScanResult> {
    tokio::task::spawn_blocking(move || {
        scan_skill_directory(&root, enabled_skill_allowlist.as_deref(), &limits)
    })
    .await
    .context("skill directory scan task failed")?
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

    #[test]
    fn bounded_skill_read_accepts_limit_and_rejects_one_byte_over() {
        assert_eq!(
            read_bounded(&b"abcd"[..], 4).unwrap().as_deref(),
            Some(&b"abcd"[..])
        );
        assert_eq!(read_bounded(&b"abcde"[..], 4).unwrap(), None);
    }

    // -----------------------------------------------------------------------
    // Parser
    // -----------------------------------------------------------------------

    #[test]
    fn parse_full_skill_md() {
        let md = "---\nname: \"file-organizer\"\ndescription: \"org files\"\nmetadata:\n  version: \"1.0.0\"\n  haven.language: \"python\"\n---\n\nDo the thing.\n";
        let m = parse_skill_md(md, 5000, 4096).unwrap();
        assert_eq!(m.name, "file-organizer");
        assert_eq!(m.description, "org files");
        assert_eq!(m.version.as_deref(), Some("1.0.0"));
        assert_eq!(m.language, Language::Python);
        assert!(m.instructions.contains("Do the thing."));
    }

    #[test]
    fn validate_skill_name_enforces_portable_names_and_reserved_devices() {
        let max_length = "x".repeat(64);
        for valid in ["a", "file-organizer-2", max_length.as_str()] {
            validate_skill_name(valid).unwrap();
        }
        let overlong = "x".repeat(65);
        for invalid in [
            "",
            overlong.as_str(),
            "bad name",
            "../skill",
            "技能",
            "Uppercase",
            "file_name",
            "-leading",
            "trailing-",
            "double--hyphen",
        ] {
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
            let markdown = format!(
                "---\nname: \"{name}\"\ndescription: \"A short description.\"\n---\n\nDo it.\n"
            );
            assert!(
                parse_skill_md(&markdown, 5000, 4096).is_err(),
                "accepted {name:?}"
            );
        }
    }

    #[test]
    fn parse_missing_name_errors() {
        let md = "---\ndescription: \"x\"\n---\n\n";
        assert!(parse_skill_md(md, 5000, 4096).is_err());
    }

    #[test]
    fn parse_standard_frontmatter_without_optional_metadata() {
        let md = "---\nname: \"fallback-named\"\ndescription: \"A short description.\"\n---\n\nonly instructions\n";
        let m = parse_skill_md(md, 5000, 4096).unwrap();
        assert_eq!(m.name, "fallback-named");
    }

    #[test]
    fn parse_unsupported_language_preserved() {
        let md = "---\nname: \"x\"\ndescription: \"Unsupported language fixture.\"\nmetadata:\n  haven.language: \"bash\"\n---\n\ni\n";
        let m = parse_skill_md(md, 5000, 4096).unwrap();
        assert_eq!(m.language, Language::Unsupported("bash".to_string()));
        assert_eq!(m.language.as_str(), "bash");
    }

    #[test]
    fn parse_strips_bom() {
        let md = "﻿---\nname: \"bom\"\ndescription: \"d\"\n---\n\ni\n";
        let m = parse_skill_md(md, 5000, 4096).unwrap();
        assert_eq!(m.name, "bom");
    }

    #[test]
    fn parse_rejects_unknown_metadata_fields() {
        let md = "---\nname: \"x\"\ndescription: \"d\"\nnot-standard: \"value\"\n---\n\ni\n";
        assert!(parse_skill_md(md, 5000, 4096).is_err());
    }

    #[test]
    fn parse_rejects_ascii_line_over_byte_limit() {
        let long_line = "a".repeat(4096 + 1);
        let md = format!("---\nname: \"x\"\ndescription: \"{long_line}\"\n---\n\ni\n");
        let error = parse_skill_md(&md, 5000, 4096).unwrap_err();
        assert!(error.to_string().contains("UTF-8 bytes"));
    }

    #[test]
    fn parse_rejects_multibyte_line_over_byte_limit_by_utf8_size() {
        let long_line = format!("description: {}", "界".repeat(1_366));
        assert!(long_line.chars().count() < 4_096);
        assert!(long_line.len() > 4_096);
        let md = format!("---\nname: \"x\"\n{long_line}\n---\n\ni\n");

        let error = parse_skill_md(&md, 5_000, 4_096).unwrap_err();
        assert!(error.to_string().contains("UTF-8 bytes"));
    }

    // -----------------------------------------------------------------------
    // Skill directory scanning
    // -----------------------------------------------------------------------

    #[test]
    fn skill_directory_scan_picks_valid_skips_invalid() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "good-a",
            "---\nname: \"good-a\"\ndescription: \"a\"\n---\n\ni\n",
            true,
        );
        write_skill(
            &dir,
            "good-b",
            "---\nname: \"good-b\"\ndescription: \"b\"\n---\n\ni\n",
            false,
        );
        // invalid SKILL.md missing name
        let bad = dir.join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(
            bad.join("SKILL.md"),
            "---\ndescription: \"no name\"\n---\n\n",
        )
        .unwrap();
        // not-a-dir SKILL.md-less
        std::fs::create_dir_all(dir.join("no-skill-md")).unwrap();

        let skills = scan_parsed_skill_entries(&dir, None, &Default::default()).unwrap();
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
    fn skill_directory_scan_skips_all_case_insensitive_directory_and_manifest_collisions() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let upper_dir = write_skill(
            &dir,
            "Echo",
            "---\nname: \"Echo\"\ndescription: \"upper\"\n---\n\ni\n",
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
            "---\nname: \"echo\"\ndescription: \"lower\"\n---\n\ni\n",
        )
        .unwrap();
        write_skill(
            &dir,
            "separate-one",
            "---\nname: \"shared\"\ndescription: \"one\"\n---\n\ni\n",
            false,
        );
        write_skill(
            &dir,
            "separate-two",
            "---\nname: \"SHARED\"\ndescription: \"two\"\n---\n\ni\n",
            false,
        );
        write_skill(
            &dir,
            "unique",
            "---\nname: \"unique\"\ndescription: \"unique\"\n---\n\ni\n",
            false,
        );

        let skills = scan_parsed_skill_entries(&dir, None, &Default::default()).unwrap();
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
            ("Echo".into(), PathBuf::from("Echo")),
            ("echo".into(), PathBuf::from("echo")),
            ("single".into(), PathBuf::from("single")),
        ]);
        assert_eq!(directories.len(), 1);
        assert_eq!(directories[0].0, "single");

        let make_skill = |name: &str| Skill {
            manifest: SkillManifest {
                name: name.to_string(),
                description: String::new(),
                version: None,
                language: Language::Python,
                instructions: String::new(),
            },
            root: PathBuf::new(),
            enabled: true,
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
    fn skill_directory_scan_applies_enabled_skill_allowlist() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "one",
            "---\nname: \"one\"\ndescription: \"o\"\n---\n\ni\n",
            false,
        );
        write_skill(
            &dir,
            "two",
            "---\nname: \"two\"\ndescription: \"t\"\n---\n\ni\n",
            false,
        );
        let skills =
            scan_parsed_skill_entries(&dir, Some(&["two".to_string()]), &Default::default())
                .unwrap();
        let enabled: Vec<bool> = skills.iter().map(|s| s.enabled()).collect();
        assert_eq!(enabled, vec![false, true]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn scriptless_skills_remain_allowlisted_but_cannot_become_executable() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "instruction-only",
            "---\nname: \"instruction-only\"\ndescription: \"no executable entry point\"\n---\n\nDo the task.\n",
            false,
        );

        let registry = SkillRegistry::new();
        registry.set_config(Some(dir.clone()), None).await.unwrap();
        let listed = registry.list_skill_infos().await;
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].has_script);
        assert!(listed[0].enabled);
        assert!(!listed[0].executable);

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

    #[tokio::test]
    async fn failed_config_scan_keeps_the_previous_runtime_snapshot() {
        let root = tmp_dir();
        std::fs::create_dir_all(&root).unwrap();
        write_skill(
            &root,
            "stable",
            "---\nname: \"stable\"\ndescription: \"stable\"\n---\n\nKeep this skill.\n",
            false,
        );
        let invalid_root = root.join("not-a-directory");
        std::fs::write(&invalid_root, "file").unwrap();

        let registry = SkillRegistry::new();
        registry
            .set_config(Some(root.clone()), Some(vec!["stable".into()]))
            .await
            .unwrap();
        let version = registry.catalog_version();
        let skills_before = registry.list_skill_infos().await;
        let allowlist_before = registry.enabled_skill_allowlist().await;

        assert!(
            registry
                .set_config(Some(invalid_root), Some(vec!["other".into()]))
                .await
                .is_err()
        );

        assert_eq!(registry.resolved_root().await, root);
        assert_eq!(registry.enabled_skill_allowlist().await, allowlist_before);
        let skills_after = registry.list_skill_infos().await;
        assert_eq!(skills_after.len(), skills_before.len());
        assert_eq!(skills_after[0].name, skills_before[0].name);
        assert_eq!(skills_after[0].description, skills_before[0].description);
        assert_eq!(registry.catalog_version(), version);
    }

    #[tokio::test]
    async fn oversized_skill_root_scan_keeps_the_previous_runtime_snapshot() {
        let root = tmp_dir();
        std::fs::create_dir_all(&root).unwrap();
        write_skill(
            &root,
            "stable",
            "---\nname: \"stable\"\ndescription: \"stable\"\n---\n\nKeep this skill.\n",
            false,
        );

        let registry = SkillRegistry::new();
        registry.set_config(Some(root.clone()), None).await.unwrap();
        registry.refresh_if_changed().await.unwrap();
        let version = registry.catalog_version();
        let skills_before = registry.list_skill_infos().await;

        // Include the existing skill directory so total root entries exceed
        // the hard cap by exactly one.
        for index in 0..MAX_SKILL_ROOT_ENTRIES {
            std::fs::write(root.join(format!("entry-{index}")), b"").unwrap();
        }

        let fingerprint_error = registry.refresh_if_changed().await.unwrap_err();
        assert!(fingerprint_error.to_string().contains("scan limit"));
        let scan_error = registry.refresh_from_disk().await.unwrap_err();
        assert!(scan_error.to_string().contains("scan limit"));

        let skills_after = registry.list_skill_infos().await;
        assert_eq!(skills_after.len(), skills_before.len());
        assert_eq!(skills_after[0].name, skills_before[0].name);
        assert_eq!(skills_after[0].description, skills_before[0].description);
        assert_eq!(registry.catalog_version(), version);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skill_directory_scan_without_allowlist_enables_all() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "a",
            "---\nname: \"a\"\ndescription: \"a\"\n---\n\ni\n",
            false,
        );
        let skills = scan_parsed_skill_entries(&dir, None, &Default::default()).unwrap();
        assert!(skills.iter().all(|s| s.enabled()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skill_directory_scan_empty_allowlist_disables_all() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "a",
            "---\nname: \"a\"\ndescription: \"a\"\n---\n\ni\n",
            false,
        );
        let skills =
            scan_parsed_skill_entries(&dir, Some(&[] as &[String]), &Default::default()).unwrap();
        assert!(!skills[0].enabled());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skill_directory_scan_skips_oversized_file() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "small",
            "---\nname: \"small\"\ndescription: \"ok\"\n---\n\ni\n",
            false,
        );
        // Create a SKILL.md larger than the cap
        let big = dir.join("big");
        std::fs::create_dir_all(&big).unwrap();
        let big_content = format!(
            "---\nname: \"big\"\ndescription: \"{}\"\n---\n\ni\n",
            "x".repeat(256 * 1024)
        );
        std::fs::write(big.join("SKILL.md"), &big_content).unwrap();

        let skills = scan_parsed_skill_entries(&dir, None, &Default::default()).unwrap();
        let names: Vec<&str> = skills.iter().map(|s| s.name()).collect();
        assert_eq!(names, vec!["small"], "oversized entry should be skipped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn registry_lists_invalid_manifests_without_shadowing_valid_skills() {
        let root = tmp_dir();
        let valid = write_skill(
            &root,
            "duplicate",
            "---\nname: \"duplicate\"\ndescription: \"valid\"\n---\n\nrun\n",
            false,
        );
        let invalid = root.join("broken-skill");
        std::fs::create_dir_all(&invalid).unwrap();
        std::fs::write(invalid.join("SKILL.md"), "not a valid skill manifest\n").unwrap();

        let registry = SkillRegistry::new();
        registry.set_config(Some(root.clone()), None).await.unwrap();

        let listed = registry.list_skill_infos().await;
        assert_eq!(listed.len(), 2);
        assert_eq!(
            listed
                .iter()
                .filter(|skill| skill.name == "duplicate")
                .count(),
            1
        );
        assert!(listed.iter().any(|skill| skill.name == "broken-skill"));
        assert!(listed.iter().any(|skill| {
            skill.name == "duplicate"
                && skill.description == "valid"
                && skill.manifest_error.is_none()
        }));
        let catalog = registry.list_skill_catalog_entries().await;
        assert_eq!(catalog.len(), 2);
        let valid_entry = catalog
            .iter()
            .find(|entry| entry.info.name == "duplicate")
            .expect("valid skill remains in the diagnostic catalog");
        assert_eq!(valid_entry.root, valid.canonicalize().unwrap());
        assert!(valid_entry.info.manifest_error.is_none());
        assert!(catalog.iter().any(|entry| {
            entry.info.name == "broken-skill" && entry.info.manifest_error.is_some()
        }));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skill_directory_scan_rejects_manifest_symlink_outside_skill_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("skills");
        let skill_dir = root.join("external-manifest");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let outside_manifest = outside.join("SKILL.md");
        std::fs::write(
            &outside_manifest,
            "---\nname: \"external-manifest\"\ndescription: \"outside\"\n---\n\n",
        )
        .unwrap();
        if symlink_file(&outside_manifest, &skill_dir.join("SKILL.md")).is_err() {
            // Windows CI may not grant symlink privileges to the test process.
            return;
        }

        let skills = scan_parsed_skill_entries(&root, None, &Default::default()).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn entry_script_rejects_symlink_target_outside_skill_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("skills");
        let skill_dir = write_skill(
            &root,
            "linked-script",
            "---\nname: \"linked-script\"\ndescription: \"linked\"\n---\n\n",
            false,
        );
        let outside_script = temp.path().join("outside.py");
        std::fs::write(&outside_script, "print('outside')").unwrap();
        if symlink_file(&outside_script, &skill_dir.join("scripts").join("main.py")).is_err() {
            // Windows CI may not grant symlink privileges to the test process.
            return;
        }

        let skill = scan_parsed_skill_entries(&root, None, &Default::default())
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert!(skill.open_entry_script().is_none());
        assert!(!SkillInfo::from(&skill).has_script);
    }

    // -----------------------------------------------------------------------
    // SkillRegistry
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn registry_watcher_refreshes_only_when_skill_files_change() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let registry = SkillRegistry::new();
        registry.set_config(Some(dir.clone()), None).await.unwrap();

        // The first watcher pass refreshes even when the directory is empty.
        assert!(registry.refresh_if_changed().await.unwrap());
        assert!(!registry.refresh_if_changed().await.unwrap());

        write_skill(
            &dir,
            "a",
            "---\nname: \"a\"\ndescription: \"a\"\n---\n\ni\n",
            false,
        );
        assert!(registry.refresh_if_changed().await.unwrap());
        assert_eq!(registry.list_skill_infos().await.len(), 1);

        // Modified SKILL.md (different length) → the registry refreshes its snapshot.
        write_skill(
            &dir,
            "a",
            "---\nname: \"a\"\ndescription: \"a much longer description\"\n---\n\ni\n",
            false,
        );
        assert!(registry.refresh_if_changed().await.unwrap());
        let edited_skill = registry.get_skill_info("a").await.unwrap();
        assert_eq!(edited_skill.description, "a much longer description");

        // Added skill → fingerprint list gains an entry.
        write_skill(
            &dir,
            "b",
            "---\nname: \"b\"\ndescription: \"b\"\n---\n\ni\n",
            false,
        );
        assert!(registry.refresh_if_changed().await.unwrap());
        assert_eq!(registry.list_skill_infos().await.len(), 2);

        // Removed skill → fingerprint list loses the entry.
        std::fs::remove_dir_all(dir.join("a")).unwrap();
        assert!(registry.refresh_if_changed().await.unwrap());
        assert_eq!(registry.list_skill_infos().await.len(), 1);
        assert!(!registry.refresh_if_changed().await.unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn registry_refresh_and_query() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "alpha",
            "---\nname: \"alpha\"\ndescription: \"a\"\nmetadata:\n  version: \"2.0\"\n  haven.language: \"python\"\n---\n\ni\n",
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
            "---\nname: \"a\"\ndescription: \"a\"\n---\n\ni\n",
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
            "---\nname: \"a\"\ndescription: \"a\"\n---\n\ni\n",
            true,
        );
        write_skill(
            &dir,
            "b",
            "---\nname: \"b\"\ndescription: \"b\"\n---\n\ni\n",
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
                &format!("---\nname: \"{name}\"\ndescription: \"{name}\"\n---\n\ni\n"),
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
