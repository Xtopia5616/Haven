use anyhow::Context;
use haven_common::ConfigLoader;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum size of a single SKILL.md file (256 KiB). Larger files are skipped
/// with a warning to prevent accidental OOM on crafted/gigabyte files.
const MAX_SKILL_MD_BYTES: u64 = 256 * 1024;

/// Maximum lines the parser will process from a single SKILL.md.
const MAX_PARSE_LINES: usize = 5000;

/// Maximum length of a single line in the SKILL.md parser.
const MAX_LINE_LEN: usize = 4096;

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

/// Structured metadata parsed from `SKILL.md` (§4.6.3).
#[derive(Debug, Clone)]
pub struct SkillManifest {
    pub name: String,
    pub description: String,
    pub version: Option<String>,
    pub language: Language,
    pub allowed_tools: Vec<String>,
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
    pub fn allowed_tools(&self) -> &[String] {
        &self.manifest.allowed_tools
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
    pub allowed_tools: Vec<String>,
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
            allowed_tools: s.allowed_tools().to_vec(),
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
/// - allowed_tools: [a, b]
/// - version: 1.0.0
/// - language: python
///
/// ## Instructions
/// ...natural language...
/// ```
///
/// The H1 line is parsed for `<name>` and the `name:` metadata field (if
/// present) takes precedence — this lets a directory's `SKILL.md` carry a name
/// differing from its folder name without surprising the registry.
///
/// **Safety:** The parser enforces a maximum line count (`MAX_PARSE_LINES`)
/// and a maximum per-line length (`MAX_LINE_LEN`) to prevent unbounded memory
/// accumulation from crafted/oversized input.
pub fn parse_skill_md(input: &str) -> anyhow::Result<SkillManifest> {
    let input = input.strip_prefix('\u{FEFF}').unwrap_or(input);

    let mut name: Option<String> = None;
    let mut description = String::new();
    let mut version: Option<String> = None;
    let mut language = Language::Python;
    let mut allowed_tools: Vec<String> = Vec::new();

    let mut current_section: Option<String> = None;
    let mut metadata_lines: Vec<String> = Vec::new();
    let mut instruction_lines: Vec<String> = Vec::new();

    for (i, line) in input.lines().enumerate() {
        if i >= MAX_PARSE_LINES {
            anyhow::bail!("SKILL.md exceeds {MAX_PARSE_LINES} lines");
        }
        if line.len() > MAX_LINE_LEN {
            anyhow::bail!("SKILL.md line {} exceeds {MAX_LINE_LEN} characters", i + 1);
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
            "allowed_tools" => {
                let inner = val.trim_start_matches('[').trim_end_matches(']');
                if !inner.is_empty() {
                    allowed_tools = inner
                        .split(',')
                        .map(|s| {
                            s.trim()
                                .trim_matches(|c: char| c == '"' || c == '\'')
                                .to_string()
                        })
                        .filter(|s| !s.is_empty())
                        .collect();
                }
            }
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
        allowed_tools,
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
/// larger than `MAX_SKILL_MD_BYTES` are skipped with a warning.
pub fn scan_dir(root: &Path, enabled_filter: Option<&[String]>) -> anyhow::Result<Vec<Skill>> {
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
        if md_len > MAX_SKILL_MD_BYTES {
            tracing::warn!(
                "skipping oversized SKILL.md ({} bytes > {MAX_SKILL_MD_BYTES} cap): {}",
                md_len,
                skill_md.display()
            );
            continue;
        }

        let content = match std::fs::read_to_string(&skill_md) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Skipping unreadable SKILL.md at {}: {e}", p.display());
                continue;
            }
        };
        match parse_skill_md(&content) {
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
            })),
        }
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
        let (root, enabled) = {
            let g = self.inner.read().await;
            (g.root.clone(), g.enabled.clone())
        };
        let effective = Self::resolve_root(root.as_deref());
        let scanned = scan_dir(&effective, enabled.as_deref())?;
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
                // None means all enabled — no change.
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
        let md = "# Skill: file-organizer\n\n## Metadata\n- name: file-organizer\n- description: org files\n- allowed_tools: [\"file_read\", file_move]\n- version: 1.0.0\n- language: python\n\n## Instructions\nDo the thing.\n";
        let m = parse_skill_md(md).unwrap();
        assert_eq!(m.name, "file-organizer");
        assert_eq!(m.description, "org files");
        assert_eq!(m.version.as_deref(), Some("1.0.0"));
        assert_eq!(m.language, Language::Python);
        assert_eq!(m.allowed_tools, vec!["file_read", "file_move"]);
        assert!(m.instructions.contains("Do the thing."));
    }

    #[test]
    fn parse_missing_name_errors() {
        let md = "## Metadata\n- description: x\n";
        assert!(parse_skill_md(md).is_err());
    }

    #[test]
    fn parse_h1_provides_name_when_metadata_omitted() {
        let md = "# Skill: fallback-named\n\n## Instructions\nonly instructions\n";
        let m = parse_skill_md(md).unwrap();
        assert_eq!(m.name, "fallback-named");
    }

    #[test]
    fn parse_unsupported_language_preserved() {
        let md = "# Skill: x\n## Metadata\n- language: bash\n## Instructions\ni\n";
        let m = parse_skill_md(md).unwrap();
        assert_eq!(m.language, Language::Unsupported("bash".to_string()));
        assert_eq!(m.language.as_str(), "bash");
    }

    #[test]
    fn parse_strips_bom() {
        let md = "\u{FEFF}# Skill: bom\n## Metadata\n- description: d\n## Instructions\ni\n";
        let m = parse_skill_md(md).unwrap();
        assert_eq!(m.name, "bom");
    }

    #[test]
    fn parse_allowed_tools_empty_when_brackets() {
        let md =
            "# Skill: x\n## Metadata\n- allowed_tools: []\n- description: d\n## Instructions\ni\n";
        let m = parse_skill_md(md).unwrap();
        assert!(m.allowed_tools.is_empty());
    }

    #[test]
    fn parse_rejects_oversized_line() {
        let long_line = "a".repeat(MAX_LINE_LEN + 1);
        let md =
            format!("# Skill: x\n## Metadata\n- description: {long_line}\n## Instructions\ni\n");
        assert!(parse_skill_md(&md).is_err());
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

        let skills = scan_dir(&dir, None).unwrap();
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
        let skills = scan_dir(&dir, Some(&["two".to_string()])).unwrap();
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
        let skills = scan_dir(&dir, None).unwrap();
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
        let skills = scan_dir(&dir, Some(&[] as &[String])).unwrap();
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
            "x".repeat(MAX_SKILL_MD_BYTES as usize)
        );
        std::fs::write(big.join("SKILL.md"), &big_content).unwrap();

        let skills = scan_dir(&dir, None).unwrap();
        let names: Vec<&str> = skills.iter().map(|s| s.name()).collect();
        assert_eq!(names, vec!["small"], "oversized entry should be skipped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // SkillsEngine
    // -----------------------------------------------------------------------

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
