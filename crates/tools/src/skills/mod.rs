use anyhow::Context;
use haven_common::ConfigLoader;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Manifest types
// ---------------------------------------------------------------------------

pub mod runner;
pub mod venv;

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
        let scripts = self.root.join("scripts");
        if scripts.join("main.py").exists() {
            return true;
        }
        scripts.join(format!("{}.py", self.manifest.name)).exists()
    }

    /// Resolve the entry script path for this skill.
    /// Returns `None` when no recognised script exists.
    pub fn entry_script(&self) -> Option<PathBuf> {
        let scripts = self.root.join("scripts");
        let main = scripts.join("main.py");
        if main.exists() {
            return Some(main);
        }
        let named = scripts.join(format!("{}.py", self.manifest.name));
        if named.exists() {
            return Some(named);
        }
        None
    }

    /// Construct a Skill without going through the normal scan/parse path.
    /// Used in tests to create inline skills.
    #[cfg(test)]
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
    pub enabled: bool,
    /// Absolute path (UTF-8 lossy) to the skill directory.
    pub root: String,
    pub has_script: bool,
}

impl From<&Skill> for SkillInfo {
    fn from(s: &Skill) -> Self {
        Self {
            name: s.name().to_string(),
            description: s.description().to_string(),
            version: s.version().map(str::to_string),
            language: s.language().as_str().to_string(),
            enabled: s.enabled(),
            root: s.root().to_string_lossy().to_string(),
            has_script: s.has_script(),
        }
    }
}

// ---------------------------------------------------------------------------
// SKILL.md parser
// ---------------------------------------------------------------------------

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
    let mut metadata_lines: Vec<String> = Vec::new();
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
            Some("metadata") => metadata_lines.push(trimmed.to_string()),
            Some("instructions") => instruction_lines.push(trimmed.to_string()),
            _ => {}
        }
    }

    for ml in &metadata_lines {
        let line = ml.trim_start_matches('-').trim();
        if line.is_empty() {
            continue;
        }
        let (key, val) = match line.split_once(':') {
            Some(pair) => pair,
            None => continue,
        };
        let key = key.trim().to_lowercase();
        let val = val.trim().to_string();
        match key.as_str() {
            "name" => name = Some(val),
            "description" => description = val,
            "version" => version = Some(val),
            "language" => language = Language::parse(&val),
            _ => {}
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
/// `enabled_filter` semantics:
/// - `None` → all skills are enabled.
/// - `Some(list)` → only skills whose names are in `list` are enabled (empty
///   `Some([])` disables everything).
///
/// Invalid SKILL.md files produce a `warn!` and are skipped (non-fatal).
///
/// **Safety:** The scan canonicalises both `root` and each entry to guard
/// against symlink/junction traversal outside the skills directory. Files
/// larger than `limits.skills_max_md_bytes` are skipped with a warning.
pub fn scan_dir(
    root: &Path,
    enabled_filter: Option<&[String]>,
    limits: &haven_common::config::ContextLimitsConfig,
) -> anyhow::Result<Vec<Skill>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }

    let root_canon = root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize skills root: {}", root.display()))?;

    let entries = std::fs::read_dir(root)
        .with_context(|| format!("failed to read skills root: {}", root.display()))?;

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

        let skill_md = p.join("SKILL.md");
        if !skill_md.exists() {
            continue;
        }

        // File size cap (M4-01 review).
        let md_len = match std::fs::metadata(&skill_md) {
            Ok(m) => m.len(),
            Err(e) => {
                tracing::warn!("cannot stat SKILL.md at {}: {e}", skill_md.display());
                continue;
            }
        };
        if md_len > limits.skills_max_md_bytes {
            tracing::warn!(
                "skipping oversized SKILL.md ({} bytes > {} cap): {}",
                md_len,
                limits.skills_max_md_bytes,
                skill_md.display()
            );
            continue;
        }

        let content = match std::fs::read(&skill_md) {
            Ok(bytes) => haven_common::encoding::decode_lossy(&bytes),
            Err(e) => {
                tracing::warn!("Skipping unreadable SKILL.md at {}: {e}", p.display());
                continue;
            }
        };
        let max_parse_lines = limits.skills_max_parse_lines;
        let max_line_len = limits.skills_max_line_len;
        match parse_skill_md(&content, max_parse_lines, max_line_len) {
            Ok(manifest) => {
                let enabled = enabled_filter
                    .map(|f| f.contains(&manifest.name))
                    .unwrap_or(true);
                out.push(Skill {
                    manifest,
                    root: p.clone(),
                    enabled,
                });
            }
            Err(e) => tracing::warn!("Skipping invalid SKILL.md at {}: {e}", skill_md.display()),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// SkillsEngine
// ---------------------------------------------------------------------------

struct Inner {
    root: Option<PathBuf>,
    /// `None` = all enabled, `Some(list)` = exhaustive allowlist.
    enabled: Option<Vec<String>>,
    skills: HashMap<String, Skill>,
    /// Unified context limits (SKILL.md size / parse caps).
    limits: haven_common::config::ContextLimitsConfig,
}

/// Registry of discovered Skills, backed by an in-memory map protected by a
/// `tokio::sync::RwLock` so `refresh_from_disk` and the bridge queries can
/// share state across `Arc<ToolsManager>`.
#[derive(Clone)]
pub struct SkillsEngine {
    inner: Arc<RwLock<Inner>>,
}

impl Default for SkillsEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillsEngine {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner {
                root: None,
                enabled: None,
                skills: HashMap::new(),
                limits: haven_common::config::ContextLimitsConfig::default(),
            })),
        }
    }

    /// Replace the unified context limits (SKILL.md size / parse caps).
    pub async fn set_limits(&self, limits: &haven_common::config::ContextLimitsConfig) {
        self.inner.write().await.limits = limits.clone();
    }

    /// Configure the skills root + optional exhaustive enabled allowlist, and
    /// trigger an immediate disk refresh.
    ///
    /// `enabled` semantics: `None` → all enabled; `Some(list)` → allowlist.
    pub async fn set_config(
        &self,
        root: Option<PathBuf>,
        enabled: Option<Vec<String>>,
    ) -> anyhow::Result<()> {
        {
            let mut g = self.inner.write().await;
            g.root = root;
            g.enabled = enabled;
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
        let (root, enabled, limits) = {
            let g = self.inner.read().await;
            (g.root.clone(), g.enabled.clone(), g.limits.clone())
        };
        let effective = Self::resolve_root(root.as_deref());
        let scanned = scan_dir(&effective, enabled.as_deref(), &limits)?;
        let mut g = self.inner.write().await;
        g.skills.clear();
        for s in scanned {
            g.skills.insert(s.name().to_string(), s);
        }
        Ok(())
    }

    pub async fn list(&self) -> Vec<SkillInfo> {
        let g = self.inner.read().await;
        g.skills.values().map(SkillInfo::from).collect()
    }

    pub async fn get(&self, name: &str) -> Option<SkillInfo> {
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

    /// Toggle the enabled flag on a discovered skill and keep the engine-level
    /// allowlist (`Inner.enabled`) in sync so the change survives
    /// `refresh_from_disk` and app restart (M4-01 review).
    ///
    /// When `enabled = false` and the allowlist was `None` (all enabled), the
    /// engine converts to an exhaustive `Some(list)` excluding the toggled
    /// skill, so the lone-disable edge case persists correctly.
    pub async fn set_enabled(&self, name: &str, enabled: bool) -> anyhow::Result<()> {
        let mut g = self.inner.write().await;
        let s = g
            .skills
            .get_mut(name)
            .ok_or_else(|| anyhow::anyhow!("skill '{name}' not loaded"))?;
        s.enabled = enabled;

        match enabled {
            true => {
                if let Some(list) = g.enabled.as_mut()
                    && !list.contains(&name.to_string())
                {
                    list.push(name.to_string());
                }
                // None means all enabled —no change.
            }
            false => {
                let all_names: Vec<String> = g.skills.keys().cloned().collect();
                match g.enabled.take() {
                    None => {
                        // Was all enabled; produce exhaustive allowlist minus name.
                        g.enabled = Some(all_names.into_iter().filter(|n| n != name).collect());
                    }
                    Some(mut list) => {
                        list.retain(|n| n != name);
                        g.enabled = Some(list);
                    }
                }
            }
        }
        Ok(())
    }

    /// Return the current engine-level enabled allowlist for persistence
    /// (used by the `set_skill_enabled` bridge to write back to `config.toml`).
    pub async fn enabled_filter(&self) -> Option<Vec<String>> {
        self.inner.read().await.enabled.clone()
    }

    /// The effective skills root path (resolved default if unset).
    pub async fn resolved_root(&self) -> PathBuf {
        let g = self.inner.read().await;
        Self::resolve_root(g.root.as_deref())
    }

    /// Cheap fingerprint of the skills directory: for every `<root>/<skill>/`
    /// entry holding a `SKILL.md`, record `(path, mtime, length)`. Detects
    /// added / removed / modified skills without reading file contents. Used
    /// by the auto-refresh watcher to know when a rescan is worth doing.
    pub async fn folder_signature(&self) -> Vec<(PathBuf, SystemTime, u64)> {
        let root = self.resolved_root().await;
        let mut sig = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                let skill_md = entry.path().join("SKILL.md");
                if let Ok(meta) = std::fs::metadata(&skill_md)
                    && let Ok(mtime) = meta.modified()
                {
                    sig.push((skill_md, mtime, meta.len()));
                }
            }
        }
        sig.sort();
        sig
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
    fn parse_ignores_unknown_metadata_fields() {
        // `allowed_tools` is no longer part of the metadata schema; unknown
        // fields must be ignored so legacy SKILL.md files keep parsing.
        let md = "# Skill: x\n## Metadata\n- allowed_tools: [a, b]\n- description: d\n## Instructions\ni\n";
        let m = parse_skill_md(md, 5000, 4096).unwrap();
        assert_eq!(m.name, "x");
        assert_eq!(m.description, "d");
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
    fn scan_dir_enabled_filter() {
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

    // -----------------------------------------------------------------------
    // SkillsEngine
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn engine_folder_signature_tracks_changes() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let eng = SkillsEngine::new();
        eng.set_config(Some(dir.clone()), None).await.unwrap();

        // Empty folder → empty signature.
        assert!(eng.folder_signature().await.is_empty());

        write_skill(
            &dir,
            "a",
            "# Skill: a\n## Metadata\n- description: a\n## Instructions\ni\n",
            false,
        );
        let sig1 = eng.folder_signature().await;
        assert_eq!(sig1.len(), 1);

        // Modified SKILL.md (different length) → signature changes.
        write_skill(
            &dir,
            "a",
            "# Skill: a\n## Metadata\n- description: a much longer description\n## Instructions\ni\n",
            false,
        );
        let sig2 = eng.folder_signature().await;
        assert_ne!(sig1, sig2);

        // Added skill → signature gains an entry.
        write_skill(
            &dir,
            "b",
            "# Skill: b\n## Metadata\n- description: b\n## Instructions\ni\n",
            false,
        );
        let sig3 = eng.folder_signature().await;
        assert_eq!(sig3.len(), 2);

        // Removed skill → signature loses the entry.
        std::fs::remove_dir_all(dir.join("a")).unwrap();
        let sig4 = eng.folder_signature().await;
        assert_eq!(sig4.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn engine_refresh_and_query() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "alpha",
            "# Skill: alpha\n## Metadata\n- description: a\n- version: 2.0\n- language: python\n## Instructions\ni\n",
            true,
        );

        let eng = SkillsEngine::new();
        eng.set_config(Some(dir.clone()), None).await.unwrap();

        let list = eng.list().await;
        assert_eq!(list.len(), 1);
        let s = &list[0];
        assert_eq!(s.name, "alpha");
        assert_eq!(s.version.as_deref(), Some("2.0"));
        assert!(s.has_script);
        assert!(s.enabled);

        // Disable → persisted as Some exhaustive list minus alpha
        eng.set_enabled("alpha", false).await.unwrap();
        let updated = eng.get("alpha").await.unwrap();
        assert!(!updated.enabled);

        // The inner filter should now be Some([]) (lone skill disabled).
        let inner_enabled = eng.enabled_filter().await;
        assert_eq!(inner_enabled, Some(vec![] as Vec<String>));

        // refresh_from_disk should NOT re-enable alpha (the filter is now
        // Some([]) which means "none enabled").
        eng.refresh_from_disk().await.unwrap();
        let after_refresh = eng.get("alpha").await.unwrap();
        assert!(
            !after_refresh.enabled,
            "alpha must stay disabled after refresh"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn engine_refresh_clears_removed_skills() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "a",
            "# Skill: a\n## Metadata\n- description: a\n## Instructions\ni\n",
            false,
        );
        let eng = SkillsEngine::new();
        eng.set_config(Some(dir.clone()), None).await.unwrap();
        assert_eq!(eng.list().await.len(), 1);
        std::fs::remove_dir_all(dir.join("a")).unwrap();
        eng.refresh_from_disk().await.unwrap();
        assert!(eng.list().await.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn engine_set_enabled_syncs_filter() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir,
            "a",
            "# Skill: a\n## Metadata\n- description: a\n## Instructions\ni\n",
            false,
        );
        write_skill(
            &dir,
            "b",
            "# Skill: b\n## Metadata\n- description: b\n## Instructions\ni\n",
            false,
        );
        let eng = SkillsEngine::new();
        eng.set_config(Some(dir.clone()), None).await.unwrap();

        // Disable a, enable b explicitly
        eng.set_enabled("a", false).await.unwrap();
        // b should still be enabled (None → all, but we transitioned to Some(["b"]) after disabling a)
        let list = eng.list().await;
        let a = list.iter().find(|s| s.name == "a").unwrap();
        let b = list.iter().find(|s| s.name == "b").unwrap();
        assert!(!a.enabled);
        assert!(b.enabled);

        // Inner filter should be Some(["b"])
        let filter = eng.enabled_filter().await;
        assert_eq!(filter, Some(vec!["b".to_string()]));

        // Re-enable a
        eng.set_enabled("a", true).await.unwrap();
        let list = eng.list().await;
        assert!(list.iter().all(|s| s.enabled));
        let filter = eng.enabled_filter().await;
        assert_eq!(filter, Some(vec!["b".to_string(), "a".to_string()]));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
