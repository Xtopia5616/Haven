use crate::registry::{DeferredToolCatalog, SessionCatalog};
use crate::{McpToolAdapter, Tool, ToolBox, ToolRegistry, ToolResult};
use haven_common::tools::{ToolCatalogGroup, ToolDef, ToolSource};
use haven_common::types::RiskLevel;
use haven_mcp::{McpManager, McpToolInfo};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

const DEFAULT_PAGE_SIZE: usize = 32;
const MAX_PAGE_SIZE: usize = 64;

/// Query the host-owned capability catalog and activate selected built-in
/// operation views for the current session. Discovery actions remain read-only;
/// the explicit `load` action is the only path that changes the session
/// provider surface.
pub struct ToolCatalogTool {
    pub deferred_catalog: DeferredToolCatalog,
    pub registry: ToolRegistry,
    pub session_catalog: SessionCatalog,
    pub max_tools_per_request: usize,
    pub mcp_manager: Arc<McpManager>,
    pub server_configs:
        Arc<RwLock<std::collections::HashMap<String, haven_common::McpServerConfig>>>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ToolCatalogParams {
    pub action: String,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub cursor: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// Revision returned by a prior paged list. A cursor from an older
    /// catalog is rejected instead of silently skipping or duplicating items.
    #[serde(default)]
    pub revision: Option<String>,
    /// Exact dotted built-in operation names to activate for this session.
    #[serde(default)]
    pub operations: Option<Vec<String>>,
    /// Built-in roots to activate for this session.
    #[serde(default)]
    pub roots: Option<Vec<String>>,
    #[serde(default, rename = "_session_id")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CatalogSource {
    All,
    Builtin,
    Skill,
    Mcp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogLevel {
    Families,
    Tools,
    Operations,
}

impl CatalogLevel {
    fn parse(value: Option<&str>) -> anyhow::Result<Self> {
        match value
            .unwrap_or("families")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "families" | "family" => Ok(Self::Families),
            "tools" | "tool" | "roots" | "root" => Ok(Self::Tools),
            "operations" | "operation" | "ops" => Ok(Self::Operations),
            other => {
                anyhow::bail!("level must be one of families, tools, or operations; got '{other}'")
            }
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Families => "families",
            Self::Tools => "tools",
            Self::Operations => "operations",
        }
    }
}

impl CatalogSource {
    fn parse(value: Option<&str>) -> anyhow::Result<Self> {
        match value.unwrap_or("all").trim().to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "builtin" | "builtins" => Ok(Self::Builtin),
            "skill" | "skills" => Ok(Self::Skill),
            "mcp" => Ok(Self::Mcp),
            other => {
                anyhow::bail!("source must be one of all, builtin, skill, or mcp; got '{other}'")
            }
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Builtin => "builtin",
            Self::Skill => "skill",
            Self::Mcp => "mcp",
        }
    }

    fn accepts(self, source: Self) -> bool {
        self == Self::All || self == source
    }
}

#[derive(Debug, Clone)]
struct CatalogItem {
    name: String,
    source: CatalogSource,
    kind: &'static str,
    description: String,
    loaded: bool,
    metadata: Value,
}

#[derive(Default)]
struct RootGroup {
    description: Option<String>,
    loaded: bool,
    server_tool_count: Option<usize>,
    children: Vec<CatalogItem>,
}

struct CatalogListRequest<'a> {
    level: CatalogLevel,
    source: CatalogSource,
    query: Option<&'a str>,
    root: Option<&'a str>,
    cursor: usize,
    limit: usize,
}

impl CatalogItem {
    fn list_json(&self) -> Value {
        let mut item = serde_json::json!({
            "name": self.name,
            "source": self.source.as_str(),
            "kind": self.kind,
            "description": self.description,
            "loaded": self.loaded,
        });
        if let Some(extra) = self.metadata.as_object()
            && let Some(target) = item.as_object_mut()
        {
            for (key, value) in extra {
                target.insert(key.clone(), value.clone());
            }
        }
        item
    }
}

impl ToolCatalogTool {
    pub async fn run(
        &self,
        params: ToolCatalogParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled("cancelled"));
        }
        let source = CatalogSource::parse(params.source.as_deref())?;
        let session_id = params
            .session_id
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("session context required to query tool catalog"))?;

        let initial_revision = self.catalog_revision(&session_id).await;
        if params.cursor.unwrap_or(0) > 0
            && params.revision.as_deref() != Some(initial_revision.as_str())
        {
            return Ok(ToolResult::ok(serde_json::json!({
                "status": "stale_cursor",
                "action": params.action,
                "source": source.as_str(),
                "catalog_revision": initial_revision,
                "restart_cursor": 0,
                "hint": "The capability catalog changed; restart this list from cursor 0.",
            })));
        }

        let result = match params.action.trim() {
            "list" => {
                let level = CatalogLevel::parse(params.level.as_deref())?;
                self.list(
                    &session_id,
                    CatalogListRequest {
                        level,
                        source,
                        query: params.query.as_deref(),
                        root: params.root.as_deref(),
                        cursor: params.cursor.unwrap_or(0),
                        limit: params.limit.unwrap_or(DEFAULT_PAGE_SIZE),
                    },
                )
                .await
            }
            "describe" => {
                let name = params
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("name is required for action 'describe'"))?;
                self.describe(&session_id, source, name).await
            }
            "load" => {
                if !matches!(source, CatalogSource::All | CatalogSource::Builtin) {
                    anyhow::bail!("action 'load' only supports source 'builtin'");
                }
                self.load_builtin_operations(&session_id, params.operations, params.roots, cancel)
                    .await
            }
            other => anyhow::bail!("action must be one of list, describe, or load; got '{other}'"),
        }?;
        let revision = self.catalog_revision(&session_id).await;
        Self::add_revision(result, &revision)
    }

    async fn load_builtin_operations(
        &self,
        session_id: &str,
        operations: Option<Vec<String>>,
        roots: Option<Vec<String>>,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled("cancelled"));
        }
        let operations = normalize_names(operations);
        let roots = normalize_names(roots);
        if operations.is_empty() && roots.is_empty() {
            anyhow::bail!("provide at least one operation or root to load built-in tools");
        }

        let deferred = self.deferred_catalog.list().await;
        let builtin: Vec<_> = deferred
            .into_iter()
            .filter(|tool| is_builtin(&tool.tool_def()))
            .collect();
        let requested: Vec<_> = builtin
            .iter()
            .filter(|tool| {
                let name = tool.name();
                let root = operation_root(&name);
                (operations.is_empty() || operations.iter().any(|value| value == &name))
                    || roots.iter().any(|value| value == root)
            })
            .cloned()
            .collect();
        let requested_names: HashSet<_> = requested.iter().map(|tool| tool.name()).collect();
        let missing_operations: Vec<_> = operations
            .iter()
            .filter(|name| !requested_names.contains(*name))
            .cloned()
            .collect();
        if requested.is_empty() {
            anyhow::bail!(
                "no matching enabled built-in operation; available deferred operations: {}",
                builtin
                    .iter()
                    .map(|tool| tool.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        let global_count = self.registry.list().await.len();
        let max = self.max_tools_per_request.max(1);
        match self
            .session_catalog
            .register_many_if_within_budget(session_id, global_count, max, requested.clone())
            .await
        {
            Ok(loaded) => {
                let mut output = serde_json::json!({
                    "status": if loaded.is_empty() { "already_loaded" } else { "loaded" },
                    "action": "load",
                    "source": "builtin",
                    "operations": loaded,
                    "available_now": requested.iter().map(|tool| tool.name()).collect::<Vec<_>>(),
                });
                if !missing_operations.is_empty() {
                    output["missing_operations"] = serde_json::json!(missing_operations);
                }
                Ok(ToolResult::ok(output))
            }
            Err(net_new) => {
                let session_count = self.session_catalog.list_defs(session_id).await.len();
                let remaining = max.saturating_sub(global_count.saturating_add(session_count));
                Ok(ToolResult::ok(serde_json::json!({
                    "status": "needs_selection",
                    "action": "load",
                    "source": "builtin",
                    "reason": format!(
                        "Loading these {} built-in operations would exceed the per-request limit of {}. Choose at most {} operation(s).",
                        net_new, max, remaining
                    ),
                    "available_operations": compact_entries(&requested),
                    "remaining_budget": remaining,
                    "max_tools_per_request": max,
                })))
            }
        }
    }

    async fn catalog_revision(&self, session_id: &str) -> String {
        let (global, session) = self
            .session_catalog
            .catalog_version_for_session(session_id)
            .await;
        format!("{global}:{session}:{}", self.mcp_manager.catalog_version())
    }

    fn add_revision(mut result: ToolResult, revision: &str) -> anyhow::Result<ToolResult> {
        if let Some(object) = result.output.as_object_mut() {
            object.insert("catalog_revision".into(), Value::String(revision.into()));
        }
        Ok(result)
    }

    async fn list(
        &self,
        session_id: &str,
        request: CatalogListRequest<'_>,
    ) -> anyhow::Result<ToolResult> {
        let CatalogListRequest {
            level,
            source,
            query,
            root,
            cursor,
            limit,
        } = request;
        let mut items = match level {
            CatalogLevel::Families => self.family_items(session_id, source).await,
            CatalogLevel::Tools => self.root_items(session_id, source, root).await,
            CatalogLevel::Operations => self.catalog_items(session_id, source).await,
        };
        if level == CatalogLevel::Operations {
            items.retain(|item| item.kind == "tool");
            if let Some(root) = root.map(str::trim).filter(|value| !value.is_empty()) {
                items.retain(|item| item.metadata["root"].as_str() == Some(root));
            }
        }
        let query = query.map(str::trim).filter(|value| !value.is_empty());
        if let Some(query) = query {
            let query = query.to_ascii_lowercase();
            items.retain(|item| {
                item.name.to_ascii_lowercase().contains(&query)
                    || item.description.to_ascii_lowercase().contains(&query)
            });
        }

        let total = items.len();
        let offset = cursor.min(total);
        let page_size = limit.clamp(1, MAX_PAGE_SIZE);
        let end = offset.saturating_add(page_size).min(total);
        let page = items[offset..end]
            .iter()
            .map(CatalogItem::list_json)
            .collect::<Vec<_>>();
        let mut output = serde_json::json!({
            "status": "ok",
            "action": "list",
            "level": level.as_str(),
            "source": source.as_str(),
            "total": total,
            "cursor": offset,
            "limit": page_size,
            "items": page,
        });
        if end < total {
            output["next_cursor"] = serde_json::json!(end);
        }
        Ok(ToolResult::ok(output))
    }

    async fn describe(
        &self,
        session_id: &str,
        source: CatalogSource,
        name: &str,
    ) -> anyhow::Result<ToolResult> {
        let global_defs = self.registry.list_defs().await;
        let session_defs = self.session_catalog.list_defs(session_id).await;
        let loaded_names: HashSet<String> = global_defs
            .iter()
            .chain(session_defs.iter())
            .map(|def| def.name.clone())
            .collect();

        for def in global_defs.iter().chain(session_defs.iter()) {
            let def_source = source_for_def(def);
            if source.accepts(def_source) && names_match(def, name, def_source) {
                return Ok(ToolResult::ok(tool_detail(
                    def,
                    def_source,
                    loaded_names.contains(&def.name),
                )));
            }
        }

        let deferred_defs = self.deferred_catalog.list_defs().await;
        for def in &deferred_defs {
            let def_source = source_for_def(def);
            if source.accepts(def_source) && names_match(def, name, def_source) {
                return Ok(ToolResult::ok(tool_detail(
                    def,
                    def_source,
                    loaded_names.contains(&def.name),
                )));
            }
        }

        if source.accepts(CatalogSource::Mcp) {
            if let Some(server) = self.describe_mcp_server(name).await {
                return Ok(ToolResult::ok(server));
            }
            if let Some((server_name, tool)) = self.describe_mcp_tool(name).await {
                let loaded = self.session_catalog.get(session_id, name).await.is_some();
                return Ok(ToolResult::ok(mcp_tool_detail(&server_name, &tool, loaded)));
            }
        }

        if let Some(root) = self
            .root_items(session_id, source, Some(name))
            .await
            .into_iter()
            .find(|item| item.name == name)
        {
            return Ok(ToolResult::ok(root_detail(&root)));
        }

        Ok(ToolResult::ok(serde_json::json!({
            "status": "not_found",
            "action": "describe",
            "name": name,
            "source": source.as_str(),
            "hint": "Call tool_catalog with action=list to search exact names. For an MCP tool, call load_mcp for its server to discover and load its schema.",
        })))
    }

    async fn catalog_items(&self, session_id: &str, source: CatalogSource) -> Vec<CatalogItem> {
        let global_defs = self.registry.list_defs().await;
        let session_defs = self.session_catalog.list_defs(session_id).await;
        let loaded_names: HashSet<String> = global_defs
            .iter()
            .chain(session_defs.iter())
            .map(|def| def.name.clone())
            .collect();
        let deferred_defs = self.deferred_catalog.list_defs().await;
        let mut seen = HashSet::new();
        let mut items = Vec::new();

        for def in global_defs
            .iter()
            .chain(deferred_defs.iter())
            .chain(session_defs.iter())
        {
            let def_source = source_for_def(def);
            if source.accepts(def_source) && seen.insert(def.name.clone()) {
                items.push(tool_item(def, def_source, loaded_names.contains(&def.name)));
            }
        }

        if source.accepts(CatalogSource::Mcp) {
            let configs = self.server_configs.read().await.clone();
            for config in configs.values().filter(|config| config.enabled) {
                let Some(client) = self.mcp_manager.get_client(&config.name).await else {
                    items.push(mcp_server_item(&config.name, &[], false));
                    continue;
                };
                let infos = client.tools_cache().await;
                let names = infos
                    .iter()
                    .map(|info| info.name.clone())
                    .collect::<Vec<_>>();
                items.push(mcp_server_item(&config.name, &names, true));
                for info in infos {
                    let qualified = McpToolAdapter::qualified_name_of(&config.name, &info.name);
                    if seen.insert(qualified.clone()) {
                        let loaded = self
                            .session_catalog
                            .get(session_id, &qualified)
                            .await
                            .is_some();
                        items.push(mcp_tool_item(&config.name, &info, loaded));
                    }
                }
            }
        }

        items.sort_by(|left, right| {
            left.source
                .as_str()
                .cmp(right.source.as_str())
                .then_with(|| left.kind.cmp(right.kind))
                .then_with(|| left.name.cmp(&right.name))
        });
        items
    }

    /// Layer 2: return tool roots such as `window` or `files`. When a root is
    /// requested, include its compact child-operation summaries; otherwise
    /// keep the response at root-name level.
    async fn root_items(
        &self,
        session_id: &str,
        source: CatalogSource,
        requested_root: Option<&str>,
    ) -> Vec<CatalogItem> {
        let operations = self.catalog_items(session_id, source).await;
        let requested_root = requested_root
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let mut roots = std::collections::BTreeMap::<(CatalogSource, String), RootGroup>::new();
        for item in operations {
            let root = item.metadata["root"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| item.name.clone());
            if requested_root.is_none_or(|requested| requested == root) {
                let group = roots.entry((item.source, root)).or_default();
                if item.kind == "server" {
                    group.description = Some(item.description);
                    group.loaded = item.loaded;
                    group.server_tool_count = item.metadata["tool_count"]
                        .as_u64()
                        .and_then(|count| usize::try_from(count).ok());
                } else {
                    group.children.push(item);
                }
            }
        }

        roots
            .into_iter()
            .map(|((item_source, root), mut group)| {
                group
                    .children
                    .sort_by(|left, right| left.name.cmp(&right.name));
                let operation_count = group.server_tool_count.unwrap_or(group.children.len());
                let description = group
                    .description
                    .unwrap_or_else(|| crate::prompts::root_description(&root).into());
                let mut metadata = serde_json::json!({
                    "operation_count": operation_count,
                });
                if requested_root.is_some() {
                    metadata["operations"] = serde_json::Value::Array(
                        group.children.iter().map(CatalogItem::list_json).collect(),
                    );
                }
                CatalogItem {
                    name: root,
                    source: item_source,
                    kind: if group.server_tool_count.is_some() {
                        "server"
                    } else {
                        "tool"
                    },
                    description: compact_text(&description, 320),
                    loaded: if group.server_tool_count.is_some() && group.children.is_empty() {
                        group.loaded
                    } else {
                        group.children.iter().all(|child| child.loaded)
                    },
                    metadata,
                }
            })
            .collect()
    }

    /// Layer 1: return only the top-level capability families and their roots.
    async fn family_items(&self, session_id: &str, source: CatalogSource) -> Vec<CatalogItem> {
        let operations = self.catalog_items(session_id, source).await;
        let mut families = std::collections::BTreeMap::<String, Vec<CatalogItem>>::new();
        for item in operations {
            let family = item.metadata["family"]
                .as_str()
                .unwrap_or(item.source.as_str())
                .to_string();
            families.entry(family).or_default().push(item);
        }

        families
            .into_iter()
            .map(|(family, items)| {
                let mut roots = items
                    .iter()
                    .filter_map(|item| item.metadata["root"].as_str())
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                roots.sort();
                roots.dedup();
                CatalogItem {
                    name: family.clone(),
                    source: match family.as_str() {
                        "skills" => CatalogSource::Skill,
                        "mcp" => CatalogSource::Mcp,
                        _ => CatalogSource::Builtin,
                    },
                    kind: "family",
                    description: compact_text(&family_description(&family), 320),
                    loaded: items.iter().all(|item| item.loaded),
                    metadata: serde_json::json!({
                        "root_count": roots.len(),
                        "roots": roots,
                    }),
                }
            })
            .collect()
    }

    async fn describe_mcp_server(&self, name: &str) -> Option<Value> {
        let config = self
            .server_configs
            .read()
            .await
            .get(name)
            .filter(|config| config.enabled)
            .cloned()?;
        let names = match self.mcp_manager.get_client(&config.name).await {
            Some(client) => client
                .tools_cache()
                .await
                .into_iter()
                .map(|info| info.name)
                .collect::<Vec<_>>(),
            None => Vec::new(),
        };
        Some(serde_json::json!({
            "status": "ok",
            "action": "describe",
            "kind": "server",
            "source": "mcp",
            "name": config.name,
            "description": "Configured MCP capability provider",
            "tool_names": names,
            "hint": "Call load_mcp with this server_name and optional raw tool_names to activate selected tools.",
        }))
    }

    async fn describe_mcp_tool(&self, name: &str) -> Option<(String, McpToolInfo)> {
        let configs = self.server_configs.read().await.clone();
        for config in configs.values().filter(|config| config.enabled) {
            let Some(client) = self.mcp_manager.get_client(&config.name).await else {
                continue;
            };
            for info in client.tools_cache().await {
                if McpToolAdapter::qualified_name_of(&config.name, &info.name) == name {
                    return Some((config.name.clone(), info));
                }
            }
        }
        None
    }
}

#[async_trait::async_trait]
impl Tool for ToolCatalogTool {
    fn name(&self) -> String {
        "tool_catalog".into()
    }

    fn description(&self) -> String {
        crate::prompts::TOOL_CATALOG_DESCRIPTION.into()
    }

    fn catalog_group(&self) -> ToolCatalogGroup {
        ToolCatalogGroup::Haven
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Safe
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "describe", "load"],
                    "description": "Use list for discovery, describe for one exact capability, or load to activate selected built-in operations"
                },
                "level": {
                    "type": "string",
                    "enum": ["families", "tools", "operations"],
                    "default": "families",
                    "description": "List level: families, tool roots, or concrete operations"
                },
                "source": {
                    "type": "string",
                    "enum": ["all", "builtin", "skill", "mcp"],
                    "default": "all",
                    "description": "Optional catalog source filter"
                },
                "query": {
                    "type": "string",
                    "maxLength": 256,
                    "description": "Optional case-insensitive name/description filter for list"
                },
                "name": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 256,
                    "description": "Exact name from list, required for describe"
                },
                "root": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 128,
                    "description": "Optional root such as window when listing its operations"
                },
                "cursor": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Pagination cursor returned as next_cursor"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_PAGE_SIZE,
                    "default": DEFAULT_PAGE_SIZE,
                    "description": "Number of compact entries to return"
                },
                "revision": {
                    "type": "string",
                    "maxLength": 128,
                    "description": "Catalog revision returned with a prior page; required when following next_cursor"
                },
                "operations": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 64,
                    "uniqueItems": true,
                    "items": { "type": "string", "minLength": 1, "maxLength": 128 },
                    "description": "Exact dotted built-in operation names to activate for this session"
                },
                "roots": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 32,
                    "uniqueItems": true,
                    "items": { "type": "string", "minLength": 1, "maxLength": 64 },
                    "description": "Built-in roots to activate when the whole root is needed"
                }
            },
            "oneOf": [
                { "required": ["action"], "properties": { "action": { "const": "list" } } },
                { "required": ["action", "name"], "properties": { "action": { "const": "describe" } } },
                {
                    "required": ["action"],
                    "properties": {
                        "action": { "const": "load" },
                        "source": { "const": "builtin" }
                    },
                    "anyOf": [
                        { "required": ["operations"] },
                        { "required": ["roots"] }
                    ]
                }
            ]
        })
    }

    fn requires_session_id(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params =
            crate::tool_contract::parse_tool_input::<ToolCatalogParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

fn source_for_def(def: &ToolDef) -> CatalogSource {
    match def
        .manifest
        .as_ref()
        .map(|manifest| manifest.identity.source)
        .unwrap_or_else(|| {
            if def.name.starts_with("skill__") {
                ToolSource::Skill
            } else if def.name.starts_with("mcp__") {
                ToolSource::Mcp
            } else {
                ToolSource::Builtin
            }
        }) {
        ToolSource::Builtin => CatalogSource::Builtin,
        ToolSource::Skill => CatalogSource::Skill,
        ToolSource::Mcp => CatalogSource::Mcp,
    }
}

fn normalize_names(values: Option<Vec<String>>) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for value in values.into_iter().flatten() {
        let value = value.trim();
        if !value.is_empty() && seen.insert(value.to_string()) {
            names.push(value.to_string());
        }
    }
    names
}

fn is_builtin(def: &ToolDef) -> bool {
    def.manifest
        .as_ref()
        .map(|manifest| manifest.identity.source == ToolSource::Builtin)
        .unwrap_or_else(|| !def.name.starts_with("skill__") && !def.name.starts_with("mcp__"))
}

fn operation_root(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

fn compact_entries(tools: &[ToolBox]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "name": tool.name(),
                "description": compact_text(&tool.description(), 160),
            })
        })
        .collect()
}

fn names_match(def: &ToolDef, requested: &str, source: CatalogSource) -> bool {
    def.name == requested
        || (source == CatalogSource::Skill
            && def.name == format!("skill__{}", requested.trim_start_matches("skill__")))
}

fn tool_item(def: &ToolDef, source: CatalogSource, loaded: bool) -> CatalogItem {
    let root = tool_root(def);
    CatalogItem {
        name: def.name.clone(),
        source,
        kind: "tool",
        description: compact_text(&def.description, 240),
        loaded,
        metadata: serde_json::json!({
            "root": root,
            "family": family_for_def(def, source),
        }),
    }
}

fn tool_detail(def: &ToolDef, source: CatalogSource, loaded: bool) -> Value {
    let mut detail = def.json();
    let Some(object) = detail.as_object_mut() else {
        return detail;
    };
    object.insert("status".into(), serde_json::json!("ok"));
    object.insert("action".into(), serde_json::json!("describe"));
    object.insert("kind".into(), serde_json::json!("tool"));
    object.insert("source".into(), serde_json::json!(source.as_str()));
    object.insert("loaded".into(), serde_json::json!(loaded));
    if let Some(prompt) = def
        .manifest
        .as_ref()
        .map(|manifest| &manifest.prompt)
        .or(def.prompt.as_ref())
    {
        object.insert(
            "guidance".into(),
            serde_json::json!({
                "when_to_use": compact_text(&prompt.when_to_use, 320),
                "when_not_to_use": compact_text(&prompt.when_not_to_use, 320),
            }),
        );
    }
    if !loaded {
        match source {
            CatalogSource::Builtin => {
                object.insert(
                    "load".into(),
                    serde_json::json!({
                        "tool": "tool_catalog",
                        "arguments": {
                            "action": "load",
                            "source": "builtin",
                            "operations": [def.name]
                        }
                    }),
                );
            }
            CatalogSource::Skill => {
                object.insert(
                    "load".into(),
                    serde_json::json!({
                        "tool": "load_skill",
                        "arguments": { "skill_names": [def.name.trim_start_matches("skill__")] }
                    }),
                );
            }
            CatalogSource::All | CatalogSource::Mcp => {}
        }
    }
    detail
}

fn mcp_server_item(name: &str, tool_names: &[String], discovered: bool) -> CatalogItem {
    let safe_name = compact_text(name, 160);
    let safe_names = tool_names
        .iter()
        .map(|name| compact_text(name, 160))
        .collect::<Vec<_>>();
    CatalogItem {
        name: safe_name.clone(),
        source: CatalogSource::Mcp,
        kind: "server",
        description: format!("Configured MCP capability provider '{safe_name}'"),
        loaded: false,
        metadata: serde_json::json!({
            "root": safe_name,
            "family": "mcp",
            "discovered": discovered,
            "tool_count": safe_names.len(),
            "tool_names": safe_names,
        }),
    }
}

fn mcp_tool_item(server_name: &str, info: &McpToolInfo, loaded: bool) -> CatalogItem {
    let root = compact_text(server_name, 160);
    CatalogItem {
        name: McpToolAdapter::qualified_name_of(server_name, &info.name),
        source: CatalogSource::Mcp,
        kind: "tool",
        description: compact_text(&info.description, 240),
        loaded,
        metadata: serde_json::json!({
            "root": root,
            "family": "mcp",
            "server_name": compact_text(server_name, 160),
            "tool_name": compact_text(&info.name, 160),
        }),
    }
}

fn mcp_tool_detail(server_name: &str, info: &McpToolInfo, loaded: bool) -> Value {
    // The server name is recovered from the qualified lookup before this
    // helper is called by the caller, so the detail is intentionally limited
    // to the MCP tool payload and the common activation hint is added there.
    serde_json::json!({
        "status": "ok",
        "action": "describe",
        "kind": "tool",
        "source": "mcp",
        "name": McpToolAdapter::qualified_name_of(server_name, &info.name),
        "server_name": server_name,
        "tool_name": info.name,
        "description": compact_text(&info.description, 240),
        "risk_level": RiskLevel::High,
        "input_schema": crate::adapters::sanitize_external_schema(&info.input_schema),
        "loaded": loaded,
        "load": {
            "tool": "load_mcp",
            "arguments": { "server_name": server_name, "tool_names": [info.name] }
        },
    })
}

fn tool_root(def: &ToolDef) -> String {
    def.manifest
        .as_ref()
        .map(|manifest| manifest.identity.root.clone())
        .unwrap_or_else(|| def.name.split('.').next().unwrap_or(&def.name).to_string())
}

fn family_for_def(def: &ToolDef, source: CatalogSource) -> String {
    match source {
        CatalogSource::Skill => "skills".into(),
        CatalogSource::Mcp => "mcp".into(),
        CatalogSource::All => def.catalog_group.as_str().into(),
        CatalogSource::Builtin => def
            .manifest
            .as_ref()
            .map(|manifest| manifest.identity.catalog_group.as_str().to_string())
            .unwrap_or_else(|| def.catalog_group.as_str().into()),
    }
}

fn family_description(family: &str) -> String {
    match family {
        "system" => "Inspect or control the local PC: files, shell, windows, input, media, network, and notifications.".into(),
        "agent" => "Delegate work or exchange low-trust messages with peer agents.".into(),
        "haven" => "Manage Haven session state, memory, preferences, tasks, and capability settings.".into(),
        "skills" => "Run an enabled installed Skill when its specialization matches the task.".into(),
        "mcp" => "Use a configured external MCP capability after discovering and loading the needed tools.".into(),
        _ => format!("Discover capabilities in the {family} family."),
    }
}

fn root_detail(root: &CatalogItem) -> Value {
    let mut detail = serde_json::json!({
        "status": "ok",
        "action": "describe",
        "kind": root.kind,
        "source": root.source.as_str(),
        "name": root.name,
        "description": root.description,
        "operation_count": root.metadata["operation_count"],
        "operations": root.metadata["operations"],
    });
    if root.source == CatalogSource::Builtin {
        detail["hint"] = serde_json::json!(
            "Choose one exact operation from operations, call tool_catalog describe on it for its schema, then call tool_catalog with action=load, source=builtin, and that operation name."
        );
    } else if root.source == CatalogSource::Mcp {
        detail["hint"] = serde_json::json!(
            "For an MCP server, call load_mcp with this server_name and optional raw tool_names to activate selected tools."
        );
    }
    detail
}

fn compact_text(value: &str, max_chars: usize) -> String {
    haven_common::text::sanitize_prompt_field(value.trim(), max_chars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolBox;
    use crate::builtin::notify::NotifyTool;

    #[test]
    fn source_aliases_are_normalized() {
        assert_eq!(
            CatalogSource::parse(Some("builtins")).unwrap(),
            CatalogSource::Builtin
        );
        assert_eq!(
            CatalogSource::parse(Some("skills")).unwrap(),
            CatalogSource::Skill
        );
        assert!(CatalogSource::parse(Some("unknown")).is_err());
    }

    #[test]
    fn detail_contains_schema_and_builtin_load_hint() {
        let tool: ToolBox = Arc::new(NotifyTool);
        let def = tool.tool_def();
        let detail = tool_detail(&def, CatalogSource::Builtin, false);
        assert_eq!(detail["name"], "notify");
        assert!(detail["input_schema"].is_object());
        assert_eq!(detail["load"]["tool"], "tool_catalog");
        assert_eq!(detail["load"]["arguments"]["action"], "load");
        assert_eq!(detail["load"]["arguments"]["source"], "builtin");
    }

    #[test]
    fn list_item_does_not_include_schema() {
        let tool: ToolBox = Arc::new(NotifyTool);
        let item = tool_item(&tool.tool_def(), CatalogSource::Builtin, true);
        assert!(item.list_json().get("input_schema").is_none());
    }
}
