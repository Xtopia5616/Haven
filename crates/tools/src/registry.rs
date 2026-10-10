use crate::authorization_policy::ToolAuthorizationRequestResolver;
use crate::tool_contract::{OperationPolicy, ToolDef, ToolHandle};
use haven_common::tools::ToolManifest;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::RwLock;

/// Combined tools + name index under a single RwLock so rebuilds update
/// both atomically — readers never see new `tools` with stale `name_index`.
#[derive(Default, Clone)]
struct RegistrySnapshot {
    tools: Vec<ToolHandle>,
    name_index: HashMap<String, ToolHandle>,
}

#[derive(Default, Clone)]
pub(crate) struct ToolRegistry {
    snapshot: Arc<RwLock<RegistrySnapshot>>,
    /// Test instrumentation only; production catalog invalidation uses
    /// `ToolCatalogVersion` and has no second registry clock.
    #[cfg(test)]
    version: Arc<AtomicU64>,
}

/// Tool implementations that are available to the host but are not part of
/// the default provider-facing catalog. Loaders move selected entries into a
/// session catalog only after the model asks for them.
#[derive(Clone, Default)]
pub(crate) struct DeferredToolCatalog {
    tools: Arc<RwLock<HashMap<String, ToolHandle>>>,
}

impl DeferredToolCatalog {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn replace(&self, tools: Vec<ToolHandle>) {
        let mut index = HashMap::with_capacity(tools.len());
        for tool in tools {
            index.insert(tool.name(), tool);
        }
        *self.tools.write().await = index;
    }

    pub async fn get(&self, name: &str) -> Option<ToolHandle> {
        self.tools.read().await.get(name).cloned()
    }

    pub async fn list(&self) -> Vec<ToolHandle> {
        let mut tools: Vec<_> = self.tools.read().await.values().cloned().collect();
        tools.sort_by_key(|tool| tool.name());
        tools
    }

    pub async fn list_tool_definitions(&self) -> Vec<ToolDef> {
        self.list()
            .await
            .into_iter()
            .map(|tool| tool.tool_def())
            .collect()
    }
}

impl ToolRegistry {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    /// Register a new tool name. Names are unique in a registry snapshot;
    /// callers that intentionally replace an implementation must use
    /// [`Self::replace`] so an accidental duplicate cannot leave a stale tool
    /// in the ordered list.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn register(&self, tool: ToolHandle) -> anyhow::Result<()> {
        let name = tool.name();
        let mut snap = self.snapshot.write().await;
        if snap.name_index.contains_key(&name) {
            anyhow::bail!("tool '{}' is already registered", name);
        }
        snap.tools.push(tool.clone());
        snap.name_index.insert(name, tool);
        #[cfg(test)]
        self.version.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    pub async fn get(&self, name: &str) -> Option<ToolHandle> {
        self.snapshot.read().await.name_index.get(name).cloned()
    }

    pub async fn list(&self) -> Vec<ToolHandle> {
        self.snapshot.read().await.tools.clone()
    }

    /// Structured definitions of every eagerly registered model tool. The
    /// session-aware provider surface is assembled by `ToolsFacade`; deferred
    /// builtin and Skill definitions live in `DeferredToolCatalog` until a
    /// loader activates them.
    pub async fn list_tool_definitions(&self) -> Vec<ToolDef> {
        let tools = self.snapshot.read().await.tools.clone();
        tools.iter().map(|t| t.tool_def()).collect()
    }

    pub async fn list_schemas(&self) -> Vec<Value> {
        self.list_tool_definitions()
            .await
            .into_iter()
            .map(|d| d.json())
            .collect()
    }

    /// Atomically rebuild the entire registry from a list of tools.
    /// Uses a single write lock so readers see a consistent snapshot.
    pub async fn rebuild(&self, new_tools: Vec<ToolHandle>) -> anyhow::Result<()> {
        let mut index = HashMap::new();
        for t in &new_tools {
            let name = t.name();
            if index.insert(name.clone(), t.clone()).is_some() {
                anyhow::bail!("duplicate tool '{}' in registry rebuild", name);
            }
        }
        let mut snap = self.snapshot.write().await;
        snap.tools = new_tools;
        snap.name_index = index;
        #[cfg(test)]
        self.version.fetch_add(1, Ordering::SeqCst);
        Ok(())
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

/// Per-session executable tool overlay layered on top of [`ToolRegistry`].
/// The overlay owns its registrations and version clock so progressive MCP
/// loading cannot invalidate unrelated sessions or expand the global registry.
#[derive(Clone)]
pub(crate) struct SessionToolOverlay {
    registrations: Arc<RwLock<HashMap<String, HashMap<String, ToolHandle>>>>,
    versions: Arc<RwLock<HashMap<String, u64>>>,
    global_version: Arc<AtomicU64>,
}

/// Version identity of one session's complete tool catalog view.
///
/// The global catalog clock changes when installed tools change; the session
/// overlay clock changes only when that session's progressively loaded tools
/// change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ToolCatalogVersion {
    pub global_catalog_version: u64,
    pub session_overlay_version: u64,
}

/// Counts used to explain whether a proposed session catalog fits the
/// provider's per-request tool limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolRegistrationBudget {
    pub(crate) max: usize,
    pub(crate) global_count: usize,
    pub(crate) session_count: usize,
    pub(crate) net_new: usize,
}

impl ToolRegistrationBudget {
    pub(crate) fn exceeds_limit(self) -> bool {
        self.global_count
            .saturating_add(self.session_count)
            .saturating_add(self.net_new)
            > self.max
    }
}

impl Default for SessionToolOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionToolOverlay {
    pub fn new() -> Self {
        Self {
            registrations: Arc::new(RwLock::new(HashMap::new())),
            versions: Arc::new(RwLock::new(HashMap::new())),
            global_version: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Monotonic version of the global catalog plus a session's overlay.
    pub async fn catalog_version_for_session(&self, session_id: &str) -> ToolCatalogVersion {
        let session = self
            .versions
            .read()
            .await
            .get(session_id)
            .copied()
            .unwrap_or(0);
        ToolCatalogVersion {
            global_catalog_version: self.global_version(),
            session_overlay_version: session,
        }
    }

    pub fn global_version(&self) -> u64 {
        self.global_version.load(Ordering::Relaxed)
    }

    pub fn bump_global_version(&self) {
        self.global_version.fetch_add(1, Ordering::Relaxed);
    }

    /// Preview a proposed registration without exposing the mutable overlay.
    pub async fn preview_registration_budget(
        &self,
        session_id: &str,
        global_count: usize,
        max: usize,
        proposed_names: &[String],
    ) -> ToolRegistrationBudget {
        let registrations = self.registrations.read().await;
        let current = registrations.get(session_id);
        let session_count = current.map_or(0, HashMap::len);
        let net_new = proposed_names
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .filter(|name| current.is_none_or(|tools| !tools.contains_key(*name)))
            .count();
        ToolRegistrationBudget {
            max: max.max(1),
            global_count,
            session_count,
            net_new,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn register(&self, session_id: &str, tool: ToolHandle) {
        self.registrations
            .write()
            .await
            .entry(session_id.to_string())
            .or_default()
            .insert(tool.name(), tool);
        self.bump_session_version(session_id).await;
    }

    /// Atomically add a batch of session tools under the shared provider
    /// budget. Already-loaded names are ignored, so replaying a loader during
    /// resume is idempotent. The returned names are the newly inserted tools.
    pub async fn register_many_if_within_budget(
        &self,
        session_id: &str,
        global_count: usize,
        max: usize,
        tools: Vec<ToolHandle>,
    ) -> Result<Vec<String>, ToolRegistrationBudget> {
        let mut registrations = self.registrations.write().await;
        let entry = registrations.entry(session_id.to_string()).or_default();
        let net_new = tools
            .iter()
            .map(|tool| tool.name())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .filter(|name| !entry.contains_key(name))
            .count();
        let budget = ToolRegistrationBudget {
            max: max.max(1),
            global_count,
            session_count: entry.len(),
            net_new,
        };
        if budget.exceeds_limit() {
            return Err(budget);
        }

        let mut loaded = Vec::with_capacity(net_new);
        for tool in tools {
            let name = tool.name();
            if entry.insert(name.clone(), tool).is_none() {
                loaded.push(name);
            }
        }
        drop(registrations);
        if !loaded.is_empty() {
            self.bump_session_version(session_id).await;
        }
        Ok(loaded)
    }

    pub async fn unregister(&self, session_id: &str) {
        self.registrations.write().await.remove(session_id);
        self.bump_session_version(session_id).await;
    }

    pub async fn get(&self, session_id: &str, name: &str) -> Option<ToolHandle> {
        self.registrations
            .read()
            .await
            .get(session_id)
            .and_then(|tools| tools.get(name))
            .cloned()
    }

    pub async fn list(&self, session_id: &str) -> Vec<ToolHandle> {
        self.registrations
            .read()
            .await
            .get(session_id)
            .map(|tools| tools.values().cloned().collect())
            .unwrap_or_default()
    }

    pub async fn list_tool_definitions(&self, session_id: &str) -> Vec<ToolDef> {
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

    async fn bump_session_version(&self, session_id: &str) {
        let mut versions = self.versions.write().await;
        let next = versions
            .get(session_id)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        versions.insert(session_id.to_string(), next);
    }
}

/// Immutable tool lookup and metadata view for one ReAct turn.
///
/// The runtime still validates again at the execution boundary, but batch
/// admission must not repeatedly walk the async registry for the same turn.
/// Holding the `ToolHandle` values keeps the implementation alive for the whole
/// batch while all policy/manifest reads remain synchronous and derived from
/// the same catalog generation.
#[derive(Clone)]
pub struct ToolCatalogSnapshot {
    catalog_version: ToolCatalogVersion,
    tools: Arc<HashMap<String, SnapshotTool>>,
    provider_definitions: Arc<Vec<ToolDef>>,
}

struct SnapshotTool {
    tool: ToolHandle,
    manifest: ToolManifest,
}

impl ToolCatalogSnapshot {
    pub(crate) fn new_with_definitions(
        catalog_version: ToolCatalogVersion,
        tools: HashMap<String, ToolHandle>,
        provider_definitions: Vec<ToolDef>,
    ) -> Self {
        let tools = tools
            .into_iter()
            .map(|(name, tool)| {
                let manifest = tool.tool_manifest();
                (name, SnapshotTool { tool, manifest })
            })
            .collect();
        Self {
            catalog_version,
            tools: Arc::new(tools),
            provider_definitions: Arc::new(provider_definitions),
        }
    }

    pub fn catalog_version(&self) -> ToolCatalogVersion {
        self.catalog_version
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// The provider-facing schema selected from the same immutable tool view
    /// as validation and execution.  Keeping this on the snapshot prevents a
    /// catalog mutation between the LLM request and the tool batch from
    /// changing the advertised surface under the same turn.
    pub fn provider_definitions(&self) -> &[ToolDef] {
        self.provider_definitions.as_slice()
    }

    pub fn get(&self, name: &str) -> Option<&ToolHandle> {
        self.tools.get(name).map(|entry| &entry.tool)
    }

    pub fn validate_input(
        &self,
        name: &str,
        input: &serde_json::Value,
    ) -> Option<anyhow::Result<()>> {
        self.get(name).map(|tool| tool.validate_input(input))
    }

    pub fn operation_policy(&self, name: &str, input: &serde_json::Value) -> OperationPolicy {
        ToolAuthorizationRequestResolver::operation_policy_for(self.get(name), name, input)
    }

    pub fn manifest(&self, name: &str) -> Option<ToolManifest> {
        self.tools.get(name).map(|entry| entry.manifest.clone())
    }
}

/// Weak lookup handle into a [`ToolRegistry`] snapshot. Lets a tool (e.g.
/// `schedule`) validate tool names / risk levels at call time without owning
/// the registry — the snapshot is mutated in place by `rebuild`, so the weak
/// handle always observes the current state. `find` returns `None` for
/// unknown names or when the registry was dropped.
pub(crate) struct RegistryProbe {
    snapshot: std::sync::Weak<RwLock<RegistrySnapshot>>,
}

impl RegistryProbe {
    /// Look up a tool by name; `None` when unknown or the registry is gone.
    pub async fn find(&self, name: &str) -> Option<ToolHandle> {
        self.snapshot
            .upgrade()?
            .read()
            .await
            .name_index
            .get(name)
            .cloned()
    }
}

/// Static builtins plus dynamically loaded MCP and Skill operations.
///
/// The installed registry is the host catalog. Deferred operations stay out
/// of the provider surface until a loader moves them into a session overlay.
#[derive(Clone, Default)]
pub(crate) struct OperationRegistry {
    pub(crate) installed: ToolRegistry,
    pub(crate) deferred: DeferredToolCatalog,
    pub(crate) session_tool_overlay: SessionToolOverlay,
}

impl OperationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn installed(&self) -> &ToolRegistry {
        &self.installed
    }

    #[cfg(test)]
    pub fn deferred(&self) -> &DeferredToolCatalog {
        &self.deferred
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
        registry.register(tool).await.unwrap();

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
        registry
            .register(Arc::new(MockTool::new("a")))
            .await
            .unwrap();
        registry
            .register(Arc::new(MockTool::new("b")))
            .await
            .unwrap();

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
        registry
            .register(Arc::new(MockTool::new("mock")))
            .await
            .unwrap();

        let schemas = registry.list_schemas().await;
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0]["name"].as_str().unwrap(), "mock");
        assert_eq!(schemas[0]["description"].as_str().unwrap(), "mock");
        assert!(schemas[0]["input_schema"].is_object());
    }

    #[tokio::test]
    async fn test_registry_list_tool_definitions_structured() {
        let registry = ToolRegistry::new();
        registry
            .register(Arc::new(MockTool::new("mock")))
            .await
            .unwrap();

        let defs = registry.list_tool_definitions().await;
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
        registry.register(old_tool).await.unwrap();

        let new_tool = Arc::new(MockTool::new("new"));
        registry.rebuild(vec![new_tool.clone()]).await.unwrap();

        assert!(registry.get("old").await.is_none());
        assert!(registry.get("new").await.is_some());
    }

    #[tokio::test]
    async fn test_registry_rejects_duplicate_names_without_shadowing() {
        let registry = ToolRegistry::new();
        registry
            .register(Arc::new(MockTool::new("same")))
            .await
            .unwrap();
        let error = registry
            .register(Arc::new(MockTool::new("same")))
            .await
            .expect_err("duplicate registration must be explicit");
        assert!(error.to_string().contains("already registered"));
        assert_eq!(registry.list().await.len(), 1);
    }

    #[tokio::test]
    async fn test_registry_rebuild_rejects_duplicate_names_atomically() {
        let registry = ToolRegistry::new();
        registry
            .register(Arc::new(MockTool::new("old")))
            .await
            .unwrap();
        let error = registry
            .rebuild(vec![
                Arc::new(MockTool::new("new")),
                Arc::new(MockTool::new("new")),
            ])
            .await
            .expect_err("duplicate rebuild must be explicit");
        assert!(error.to_string().contains("duplicate tool"));
        assert!(registry.get("old").await.is_some());
        assert!(registry.get("new").await.is_none());
    }
}
