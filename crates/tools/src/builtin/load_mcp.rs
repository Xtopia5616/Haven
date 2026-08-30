use async_trait::async_trait;
use haven_common::config::McpServerConfig;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::{McpToolAdapter, Tool, ToolBox, ToolRegistry, ToolResult, ToolsManager};
use haven_mcp::McpManager;

pub struct LoadMcpTool {
    pub mcp_manager: Arc<McpManager>,
    pub server_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
    /// Global registry (builtins) — used with session overlays for the
    /// per-request tool budget check.
    pub registry: ToolRegistry,
    pub session_registrations: Arc<RwLock<HashMap<String, HashMap<String, ToolBox>>>>,
    pub catalog_version: Arc<AtomicU64>,
    /// Snapshot of `context_limits.max_tools_per_request` at catalog rebuild.
    pub max_tools_per_request: usize,
}

/// Typed parameters for `LoadMcpTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `LoadMcpTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LoadMcpParams {
    /// The name of the MCP server to load.
    pub server_name: String,
    /// Optional subset of tool names from that server. When omitted (or empty)
    /// and the full server fits the budget, every tool is activated. When the
    /// full set would exceed `max_tools_per_request`, the call returns a
    /// `needs_selection` catalog instead of registering anything — call again
    /// with `tool_names` set to the tools you need.
    #[serde(default)]
    pub tool_names: Option<Vec<String>>,
    /// Injected privately by ToolsManager when `requires_session_id` is set.
    #[serde(default, rename = "_session_id")]
    pub session_id: Option<String>,
}

impl LoadMcpTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: LoadMcpParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let server_name = params.server_name;
        if server_name.is_empty() {
            anyhow::bail!("server_name is required");
        }
        let session_id = params.session_id.filter(|s| !s.is_empty()).ok_or_else(|| {
            anyhow::anyhow!(
                "session context required to load MCP server '{}'",
                server_name
            )
        })?;
        let tool_names = normalize_tool_names(params.tool_names).map_err(|e| anyhow::anyhow!(e))?;

        // Read config and the available-server list under one lock.
        let (config, available) = {
            let configs = self.server_configs.read().await;
            let available = configs.keys().cloned().collect::<Vec<_>>().join(", ");
            (configs.get(&server_name).cloned(), available)
        };
        let config = config.ok_or_else(|| {
            anyhow::anyhow!(
                "MCP server '{}' not found in config. Available servers: {}",
                server_name,
                available
            )
        })?;
        if !config.enabled {
            anyhow::bail!("MCP server '{}' is disabled", server_name);
        }

        // Connect if not already connected
        if self.mcp_manager.get_client(&server_name).await.is_none() {
            self.mcp_manager.connect_server(&config).await?;
        }

        let client = self
            .mcp_manager
            .get_client(&server_name)
            .await
            .ok_or_else(|| {
                anyhow::anyhow!("MCP server '{}' not available after connect", server_name)
            })?;

        // Same wait as resume registration so budget and schemas see the
        // populated tools/list, not an empty in-flight cache.
        let all_tools = client.wait_for_tools(Duration::from_secs(3)).await;
        let (selected, missing) = select_tools(&all_tools, tool_names.as_deref());

        if tool_names.is_none() {
            let max = self.max_tools_per_request.max(1);
            let global_count = self.registry.list().await.len();
            let (session_count, net_new) = {
                let reg = self.session_registrations.read().await;
                let entry = reg.get(&session_id);
                let session_count = entry.map(|m| m.len()).unwrap_or(0);
                let net_new = selected
                    .iter()
                    .filter(|info| {
                        let name = McpToolAdapter::qualified_name_of(&server_name, &info.name);
                        entry.is_none_or(|e| !e.contains_key(&name))
                    })
                    .count();
                (session_count, net_new)
            };
            if ToolsManager::tool_budget_would_exceed(max, global_count, session_count, net_new) {
                let current = global_count.saturating_add(session_count);
                let remaining = max.saturating_sub(current);
                return Ok(ToolResult::ok(serde_json::json!({
                    "status": "needs_selection",
                    "server_name": server_name,
                    "reason": format!(
                        "Server exposes {} tools; loading all would exceed the per-request limit of {} (currently {} tools: {} builtin + {} session). Call load_mcp again with tool_names set to at most {} tools you need.",
                        all_tools.len(),
                        max,
                        current,
                        global_count,
                        session_count,
                        remaining
                    ),
                    "available_tools": catalog_entries(&all_tools),
                    "tool_count": all_tools.len(),
                    "remaining_budget": remaining,
                    "max_tools_per_request": max,
                })));
            }
        } else if selected.is_empty() {
            anyhow::bail!(
                "None of the requested tool_names were found on MCP server '{}'. Available: {}",
                server_name,
                all_tools
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        match self
            .activate_server_tools(&session_id, &server_name, client.clone(), selected)
            .await?
        {
            ActivateOutcome::Loaded(tool_schemas) => {
                let mut result = serde_json::json!({
                    "server": {
                        "name": server_name,
                        "tools": tool_schemas,
                    },
                    "status": "loaded",
                    "server_name": server_name,
                });
                if let Some(names) = tool_names {
                    result["requested_tool_names"] = serde_json::json!(names);
                }
                if !missing.is_empty() {
                    result["missing_tool_names"] = serde_json::json!(missing);
                }
                // Zero tools after a successful handshake is usually a client/server
                // incompatibility, not an empty server: surface the handshake
                // diagnostic so the model can distinguish the two.
                if tool_schemas.is_empty()
                    && let Some(diagnostic) = client.diagnostic().await
                {
                    result["diagnostic"] = serde_json::json!(diagnostic);
                }
                Ok(ToolResult::ok(result))
            }
            // Soft failure: observation for the model, ReAct loop continues.
            ActivateOutcome::BudgetExceeded {
                net_new,
                max,
                global_count,
                session_count,
            } => {
                let current = global_count.saturating_add(session_count);
                let remaining = max.saturating_sub(current);
                Ok(ToolResult::ok(serde_json::json!({
                    "status": "budget_exceeded",
                    "server_name": server_name,
                    "requested_tool_names": tool_names,
                    "missing_tool_names": missing,
                    "reason": format!(
                        "Cannot load MCP server '{}': adding {} tools would exceed the per-request limit of {} (currently {} tools: {} builtin + {} session). Choose fewer tool_names (at most {}), unload unused session tools by starting a new session, or raise context_limits.max_tools_per_request. The conversation continues — do not stop.",
                        server_name,
                        net_new,
                        max,
                        current,
                        global_count,
                        session_count,
                        remaining
                    ),
                    "net_new": net_new,
                    "remaining_budget": remaining,
                    "max_tools_per_request": max,
                })))
            }
        }
    }

    /// Atomically budget-check + register under the session write lock so
    /// parallel `load_mcp` calls in one ReAct step cannot both pass a stale
    /// read and then partially activate. Reloading an already-loaded server
    /// (zero net new names) is always allowed. Over-budget is a soft
    /// `BudgetExceeded` outcome — never a hard error that ends the turn.
    async fn activate_server_tools(
        &self,
        session_id: &str,
        server_name: &str,
        client: Arc<haven_mcp::McpClient>,
        tools: Vec<haven_mcp::McpToolInfo>,
    ) -> anyhow::Result<ActivateOutcome> {
        let max = self.max_tools_per_request.max(1);
        let global_count = self.registry.list().await.len();
        let mut map = self.session_registrations.write().await;
        let entry = map.entry(session_id.to_string()).or_default();
        let session_count = entry.len();
        let net_new = tools
            .iter()
            .filter(|info| {
                let name = McpToolAdapter::qualified_name_of(server_name, &info.name);
                !entry.contains_key(&name)
            })
            .count();
        if ToolsManager::tool_budget_would_exceed(max, global_count, session_count, net_new) {
            return Ok(ActivateOutcome::BudgetExceeded {
                net_new,
                max,
                global_count,
                session_count,
            });
        }

        let mut tool_schemas = Vec::with_capacity(tools.len());
        for info in tools {
            let adapter = McpToolAdapter::new(client.clone(), server_name, info);
            tool_schemas.push(adapter.tool_def().json());
            entry.insert(adapter.name(), Arc::new(adapter));
        }
        drop(map);
        self.catalog_version.fetch_add(1, Ordering::Relaxed);
        Ok(ActivateOutcome::Loaded(tool_schemas))
    }
}

enum ActivateOutcome {
    Loaded(Vec<Value>),
    BudgetExceeded {
        net_new: usize,
        max: usize,
        global_count: usize,
        session_count: usize,
    },
}

/// Normalize optional `tool_names`:
/// - omitted / `null` → `Ok(None)` (load-all when budget allows)
/// - non-empty → `Ok(Some(deduped))` subset
/// - explicit empty / all-whitespace → `Err` (do not collapse to load-all)
fn normalize_tool_names(names: Option<Vec<String>>) -> Result<Option<Vec<String>>, String> {
    let Some(names) = names else {
        return Ok(None);
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for name in names {
        let trimmed = name.trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
            continue;
        }
        out.push(trimmed.to_string());
    }
    if out.is_empty() {
        Err(
            "tool_names was provided but empty; omit tool_names to load all tools that fit the budget, or pass at least one tool name"
                .into(),
        )
    } else {
        Ok(Some(out))
    }
}

/// Filter cached tools by requested names. Returns `(selected, missing)`.
fn select_tools(
    all: &[haven_mcp::McpToolInfo],
    tool_names: Option<&[String]>,
) -> (Vec<haven_mcp::McpToolInfo>, Vec<String>) {
    let Some(names) = tool_names else {
        return (all.to_vec(), Vec::new());
    };
    let want: HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
    let mut selected = Vec::new();
    let mut found = HashSet::new();
    for info in all {
        if want.contains(info.name.as_str()) {
            found.insert(info.name.as_str());
            selected.push(info.clone());
        }
    }
    let missing = names
        .iter()
        .filter(|n| !found.contains(n.as_str()))
        .cloned()
        .collect();
    (selected, missing)
}

fn catalog_entries(tools: &[haven_mcp::McpToolInfo]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            let desc = t.description.trim();
            let desc = if desc.chars().count() > 160 {
                let truncated: String = desc.chars().take(157).collect();
                format!("{truncated}...")
            } else {
                desc.to_string()
            };
            serde_json::json!({
                "name": t.name,
                "description": desc,
            })
        })
        .collect()
}

#[async_trait]
impl Tool for LoadMcpTool {
    fn name(&self) -> String {
        "load_mcp".into()
    }
    fn description(&self) -> String {
        "Load an MCP server's tools by server name, activating them for this session. Optional tool_names loads only that subset (required when the server has too many tools for the per-request budget). Prefer this over weaker built-in tools when the server's tools fit the session.".into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Safe
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "server_name": { "type": "string", "description": "The name of the MCP server to load" },
                "tool_names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional subset of tool names from that server. Omit (or null) to load all when they fit the budget; never pass an empty array. If the server is too large, the first call returns a catalog and you must call again with tool_names."
                }
            },
            "required": ["server_name"]
        })
    }

    fn requires_session_id(&self) -> bool {
        true
    }

    /// Entry ②: LLM JSON entry — convert/validate into `LoadMcpParams`,
    /// then land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool::parse_tool_input::<LoadMcpParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }

    /// Registration is performed atomically inside `run` before success is
    /// returned, so the executor must not re-apply `McpServer` (that would
    /// race parallel loads and re-wait the tools cache). Resume restores
    /// from history via `register_mcp_for_session` directly.
    fn registrations(&self, _output: &Value) -> Vec<crate::tool::ToolRegistration> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use haven_common::config::McpServerConfig;

    fn tool_for_tests() -> LoadMcpTool {
        LoadMcpTool {
            mcp_manager: Arc::new(McpManager::new()),
            server_configs: Arc::new(RwLock::new(HashMap::new())),
            registry: ToolRegistry::new(),
            session_registrations: Arc::new(RwLock::new(HashMap::new())),
            catalog_version: Arc::new(AtomicU64::new(0)),
            max_tools_per_request: 128,
        }
    }

    fn fake_info(name: &str, desc: &str) -> haven_mcp::McpToolInfo {
        haven_mcp::McpToolInfo {
            name: name.into(),
            description: desc.into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    #[test]
    fn test_load_mcp_name() {
        let tool = tool_for_tests();
        assert_eq!(tool.name(), "load_mcp");
    }

    #[test]
    fn test_load_mcp_input_schema() {
        let tool = tool_for_tests();
        let schema = tool.input_schema();
        assert!(schema["properties"]["server_name"].is_object());
        assert!(schema["properties"]["tool_names"].is_object());
        assert_eq!(schema["required"][0], "server_name");
        assert!(
            schema.get("_session_id").is_none(),
            "private _session_id must not leak into the LLM schema"
        );
    }

    #[test]
    fn test_load_mcp_requires_session_id() {
        assert!(tool_for_tests().requires_session_id());
    }

    #[test]
    fn test_load_mcp_registrations_empty_after_inline_activate() {
        let tool = tool_for_tests();
        let regs = tool.registrations(&serde_json::json!({"server_name": "srv"}));
        assert!(regs.is_empty());
    }

    #[test]
    fn test_normalize_tool_names_omitted_is_all() {
        assert_eq!(normalize_tool_names(None).unwrap(), None);
    }

    #[test]
    fn test_normalize_tool_names_explicit_empty_errors() {
        assert!(normalize_tool_names(Some(vec![])).is_err());
        assert!(normalize_tool_names(Some(vec!["  ".into()])).is_err());
    }

    #[test]
    fn test_normalize_tool_names_dedupes() {
        let names =
            normalize_tool_names(Some(vec!["a".into(), " a ".into(), "b".into(), "a".into()]))
                .unwrap();
        assert_eq!(names, Some(vec!["a".into(), "b".into()]));
    }

    #[test]
    fn test_select_tools_subset_and_missing() {
        let all = vec![
            fake_info("alpha", "A"),
            fake_info("beta", "B"),
            fake_info("gamma", "C"),
        ];
        let (selected, missing) =
            select_tools(&all, Some(&["beta".into(), "nope".into(), "alpha".into()]));
        assert_eq!(
            selected.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
        // Selection follows server order; missing keeps request order.
        assert_eq!(missing, vec!["nope".to_string()]);
    }

    #[test]
    fn test_select_tools_all_when_unfiltered() {
        let all = vec![fake_info("a", ""), fake_info("b", "")];
        let (selected, missing) = select_tools(&all, None);
        assert_eq!(selected.len(), 2);
        assert!(missing.is_empty());
    }

    #[tokio::test]
    async fn test_load_mcp_rejects_disabled() {
        let configs = Arc::new(RwLock::new(HashMap::from([(
            "srv".to_string(),
            McpServerConfig {
                name: "srv".into(),
                enabled: false,
                ..Default::default()
            },
        )])));
        let tool = LoadMcpTool {
            mcp_manager: Arc::new(McpManager::new()),
            server_configs: configs,
            registry: ToolRegistry::new(),
            session_registrations: Arc::new(RwLock::new(HashMap::new())),
            catalog_version: Arc::new(AtomicU64::new(0)),
            max_tools_per_request: 128,
        };
        let result = tool
            .execute(
                serde_json::json!({"server_name": "srv", "_session_id": "ses-x"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err(), "disabled server should be rejected");
        assert!(result.unwrap_err().to_string().contains("disabled"));
    }

    #[tokio::test]
    async fn test_load_mcp_rejects_unknown() {
        let tool = tool_for_tests();
        let result = tool
            .execute(
                serde_json::json!({"server_name": "nope", "_session_id": "ses-x"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err(), "unknown server should be rejected");
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_load_mcp_requires_session_context() {
        let tool = tool_for_tests();
        let result = tool
            .run(
                LoadMcpParams {
                    server_name: "nope".into(),
                    tool_names: None,
                    session_id: None,
                },
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("session context"));
    }

    #[tokio::test]
    async fn test_activate_refuses_oversized_add() {
        let registry = ToolRegistry::new();
        let session_registrations: Arc<RwLock<HashMap<String, HashMap<String, ToolBox>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        {
            let mut map = session_registrations.write().await;
            let entry = map.entry("ses-x".into()).or_default();
            for i in 0..5 {
                entry.insert(
                    format!("pad_{i}"),
                    Arc::new(crate::builtin::notify::NotifyTool) as ToolBox,
                );
            }
        }
        let tool = LoadMcpTool {
            mcp_manager: Arc::new(McpManager::new()),
            server_configs: Arc::new(RwLock::new(HashMap::new())),
            registry,
            session_registrations: session_registrations.clone(),
            catalog_version: Arc::new(AtomicU64::new(0)),
            max_tools_per_request: 6,
        };
        assert!(ToolsManager::tool_budget_would_exceed(6, 0, 5, 3));
        assert!(!ToolsManager::tool_budget_would_exceed(6, 0, 5, 0));
        let _ = tool;
        let map = session_registrations.read().await;
        assert_eq!(map.get("ses-x").map(|m| m.len()), Some(5));
    }

    #[tokio::test]
    async fn test_activate_server_tools_registers_under_budget() {
        let session_registrations: Arc<RwLock<HashMap<String, HashMap<String, ToolBox>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let catalog_version = Arc::new(AtomicU64::new(0));
        let tool = LoadMcpTool {
            mcp_manager: Arc::new(McpManager::new()),
            server_configs: Arc::new(RwLock::new(HashMap::new())),
            registry: ToolRegistry::new(),
            session_registrations: session_registrations.clone(),
            catalog_version: catalog_version.clone(),
            max_tools_per_request: 10,
        };
        let name = McpToolAdapter::qualified_name_of("srv", "only");
        {
            let mut map = session_registrations.write().await;
            map.entry("ses-x".into()).or_default().insert(
                name.clone(),
                Arc::new(crate::builtin::notify::NotifyTool) as ToolBox,
            );
        }
        assert!(!ToolsManager::tool_budget_would_exceed(1, 0, 1, 0));
        assert_eq!(catalog_version.load(Ordering::Relaxed), 0);
        let _ = tool;
        let _ = name;
    }

    #[test]
    fn test_catalog_entries_truncates_long_descriptions() {
        let long = "x".repeat(200);
        let entries = catalog_entries(&[fake_info("t", &long)]);
        let desc = entries[0]["description"].as_str().unwrap();
        assert!(desc.ends_with("..."));
        assert!(desc.chars().count() <= 160);
    }
}
