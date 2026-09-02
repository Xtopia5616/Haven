use crate::tool_contract::{ToolBox, ToolDef};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::RwLock;

/// Combined tools + name index under a single RwLock so rebuilds update
/// both atomically — readers never see new `tools` with stale `name_index`.
#[derive(Default, Clone)]
struct RegistrySnapshot {
    tools: Vec<ToolBox>,
    name_index: HashMap<String, ToolBox>,
}

#[derive(Default)]
pub struct ToolRegistry {
    snapshot: Arc<RwLock<RegistrySnapshot>>,
    /// Monotonically incremented on every mutation (register/rebuild).
    /// Consumers (e.g. SystemPromptBuilder) compare this against a cached
    /// value to decide whether the schema snapshot is stale, which is more
    /// robust than comparing tool counts: a rebuild that swaps tools while
    /// keeping the same count still bumps the version.
    version: Arc<AtomicU64>,
}

impl Clone for ToolRegistry {
    fn clone(&self) -> Self {
        Self {
            snapshot: self.snapshot.clone(),
            version: self.version.clone(),
        }
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(RegistrySnapshot::default())),
            version: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Current registry version. Bumps on every `register`/`rebuild`.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    pub async fn register(&self, tool: ToolBox) {
        let name = tool.name();
        let mut snap = self.snapshot.write().await;
        snap.tools.push(tool.clone());
        snap.name_index.insert(name, tool);
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    pub async fn get(&self, name: &str) -> Option<ToolBox> {
        self.snapshot.read().await.name_index.get(name).cloned()
    }

    pub async fn list(&self) -> Vec<ToolBox> {
        self.snapshot.read().await.tools.clone()
    }

    /// Structured tool definitions of every registered tool. The canonical
    /// surface for consumers (agent schema builder, prompt builder, UI).
    pub async fn list_defs(&self) -> Vec<ToolDef> {
        let tools = self.snapshot.read().await.tools.clone();
        tools.iter().map(|t| t.tool_def()).collect()
    }

    pub async fn list_schemas(&self) -> Vec<Value> {
        self.list_defs()
            .await
            .into_iter()
            .map(|d| d.json())
            .collect()
    }

    /// Atomically rebuild the entire registry from a list of tools.
    /// Uses a single write lock so readers see a consistent snapshot.
    pub async fn rebuild(&self, new_tools: Vec<ToolBox>) {
        let mut index = HashMap::new();
        for t in &new_tools {
            index.insert(t.name(), t.clone());
        }
        let mut snap = self.snapshot.write().await;
        snap.tools = new_tools;
        snap.name_index = index;
        drop(snap);
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    /// Non-owning probe into the current snapshot (see [`RegistryProbe`]).
    /// The probe holds a weak handle, so it never keeps the snapshot alive
    /// and cannot create a reference cycle (the snapshot owns the tools, and
    /// a tool holding a strong registry reference would loop back to itself).
    pub fn probe(&self) -> RegistryProbe {
        RegistryProbe {
            snapshot: Arc::downgrade(&self.snapshot),
        }
    }
}

/// Per-session overlay catalog layered on top of [`ToolRegistry`]. The
/// overlay owns both its registrations and its version clock so progressive
/// skill/MCP loading cannot invalidate unrelated sessions or create a second
/// catalog source in `ToolsManager`.
#[derive(Clone)]
pub struct SessionCatalog {
    registrations: Arc<RwLock<HashMap<String, HashMap<String, ToolBox>>>>,
    versions: Arc<RwLock<HashMap<String, u64>>>,
    global_version: Arc<AtomicU64>,
}

impl Default for SessionCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionCatalog {
    pub fn new() -> Self {
        Self {
            registrations: Arc::new(RwLock::new(HashMap::new())),
            versions: Arc::new(RwLock::new(HashMap::new())),
            global_version: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Shared registration handle used by progressive builtin adapters.
    pub fn registrations(&self) -> Arc<RwLock<HashMap<String, HashMap<String, ToolBox>>>> {
        self.registrations.clone()
    }

    /// Shared session-version handle used by progressive builtin adapters.
    pub fn versions(&self) -> Arc<RwLock<HashMap<String, u64>>> {
        self.versions.clone()
    }

    /// Monotonic version of the global catalog plus a session's overlay.
    pub async fn catalog_version_for_session(&self, session_id: &str) -> (u64, u64) {
        let session = self
            .versions
            .read()
            .await
            .get(session_id)
            .copied()
            .unwrap_or(0);
        (self.global_version(), session)
    }

    pub fn global_version(&self) -> u64 {
        self.global_version.load(Ordering::Relaxed)
    }

    pub fn bump_global_version(&self) {
        self.global_version.fetch_add(1, Ordering::Relaxed);
    }

    pub async fn register(&self, session_id: &str, tool: ToolBox) {
        self.registrations
            .write()
            .await
            .entry(session_id.to_string())
            .or_default()
            .insert(tool.name(), tool);
        self.bump_session_version(session_id).await;
    }

    pub async fn unregister(&self, session_id: &str) {
        self.registrations.write().await.remove(session_id);
        self.bump_session_version(session_id).await;
    }

    pub async fn get(&self, session_id: &str, name: &str) -> Option<ToolBox> {
        self.registrations
            .read()
            .await
            .get(session_id)
            .and_then(|tools| tools.get(name))
            .cloned()
    }

    pub async fn list_defs(&self, session_id: &str) -> Vec<ToolDef> {
        let mut defs: Vec<_> = self
            .registrations
            .read()
            .await
            .get(session_id)
            .into_iter()
            .flat_map(|tools| tools.values())
            .map(|tool| tool.tool_def())
            .collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }

    pub async fn bump_session_version(&self, session_id: &str) {
        let mut versions = self.versions.write().await;
        let next = versions
            .get(session_id)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        versions.insert(session_id.to_string(), next);
    }

    /// Whether adding `net_new` unique session tools would exceed the
    /// per-request provider ceiling.
    pub fn tool_budget_would_exceed(
        max: usize,
        global_count: usize,
        session_count: usize,
        net_new: usize,
    ) -> bool {
        if net_new == 0 {
            return false;
        }
        global_count
            .saturating_add(session_count)
            .saturating_add(net_new)
            > max.max(1)
    }
}

/// Weak lookup handle into a [`ToolRegistry`] snapshot. Lets a tool (e.g.
/// `schedule`) validate tool names / risk levels at call time without owning
/// the registry — the snapshot is mutated in place by `rebuild`, so the weak
/// handle always observes the current state. `find` returns `None` for
/// unknown names or when the registry was dropped.
pub struct RegistryProbe {
    snapshot: std::sync::Weak<RwLock<RegistrySnapshot>>,
}

impl RegistryProbe {
    /// Look up a tool by name; `None` when unknown or the registry is gone.
    pub async fn find(&self, name: &str) -> Option<ToolBox> {
        self.snapshot
            .upgrade()?
            .read()
            .await
            .name_index
            .get(name)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_contract::tests::MockTool;
    use crate::tool_contract::{Tool, ToolConcurrency};
    use haven_common::types::RiskLevel;
    use serde_json::json;
    use std::sync::Arc;
    #[tokio::test]
    async fn test_registry_new() {
        let registry = ToolRegistry::new();
        let tools = registry.list().await;
        assert!(tools.is_empty());
    }

    #[test]
    fn undeclared_tools_are_serialized_by_default() {
        assert_eq!(
            MockTool::new("undeclared").concurrency(&json!({})),
            ToolConcurrency::Exclusive
        );
    }

    #[tokio::test]
    async fn test_registry_register_and_get() {
        let registry = ToolRegistry::new();
        let tool = Arc::new(MockTool::new("mock1"));
        registry.register(tool).await;

        let fetched = registry.get("mock1").await;
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().name(), "mock1");
    }

    #[tokio::test]
    async fn test_registry_get_not_found() {
        let registry = ToolRegistry::new();
        let fetched = registry.get("nonexistent").await;
        assert!(fetched.is_none());
    }

    #[tokio::test]
    async fn test_registry_list_multiple() {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(MockTool::new("a"))).await;
        registry.register(Arc::new(MockTool::new("b"))).await;

        let tools = registry.list().await;
        assert_eq!(tools.len(), 2);
    }

    #[tokio::test]
    async fn test_registry_list_empty() {
        let registry = ToolRegistry::new();
        let tools = registry.list().await;
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn test_registry_list_schemas() {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(MockTool::new("mock"))).await;

        let schemas = registry.list_schemas().await;
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0]["name"].as_str().unwrap(), "mock");
        assert_eq!(schemas[0]["description"].as_str().unwrap(), "mock");
        assert!(schemas[0]["input_schema"].is_object());
    }

    #[tokio::test]
    async fn test_registry_list_defs_structured() {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(MockTool::new("mock"))).await;

        let defs = registry.list_defs().await;
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "mock");
        assert_eq!(defs[0].description, "mock");
        assert_eq!(defs[0].risk_level, RiskLevel::Safe);
        assert!(defs[0].input_schema.is_object());
    }

    #[test]
    fn test_tool_def_default_matches_tool_fields() {
        let tool = MockTool::new("mock");
        let def = tool.tool_def();
        assert_eq!(def.name, tool.name());
        assert_eq!(def.description, tool.description());
        assert_eq!(def.input_schema, tool.input_schema());
        assert_eq!(def.risk_level, tool.risk_level(&serde_json::json!({})));
    }

    #[tokio::test]
    async fn test_registry_rebuild() {
        let registry = ToolRegistry::new();
        let old_tool = Arc::new(MockTool::new("old"));
        registry.register(old_tool).await;

        let new_tool = Arc::new(MockTool::new("new"));
        registry.rebuild(vec![new_tool.clone()]).await;

        assert!(registry.get("old").await.is_none());
        assert!(registry.get("new").await.is_some());
    }
}
