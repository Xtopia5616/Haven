use async_trait::async_trait;
use haven_common::tools::ToolCatalogGroup;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::adapters::McpToolAdapter;
use crate::mcp_server_config::McpServerConfigSource;
use crate::registry::SessionToolOverlay;
use crate::{Tool, ToolHandle, ToolRegistry, ToolResult};
use haven_common::config::McpExposureConfig;
use haven_common::mcp::McpToolExposure;
use haven_mcp::McpManager;

pub struct LoadMcpTool {
    pub mcp_manager: Arc<McpManager>,
    pub mcp_server_config_source: McpServerConfigSource,
    /// Global registry (builtins) — used with session overlays for the
    /// per-request tool budget check.
    pub registry: ToolRegistry,
    pub session_tool_overlay: SessionToolOverlay,
    /// Snapshot of `context_limits.max_tools_per_request` at catalog rebuild.
    pub max_tools_per_request: usize,
}

/// Typed parameters for `LoadMcpTool`'s authorization-bound execution path.
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
    /// Injected privately by ToolsFacade when `requires_session_id` is set.
    #[serde(default, rename = "_session_id")]
    pub session_id: Option<String>,
}

struct ValidatedLoadMcpParams {
    server_name: String,
    tool_names: Option<Vec<String>>,
    session_id: String,
}

impl ValidatedLoadMcpParams {
    fn new(params: LoadMcpParams) -> anyhow::Result<Self> {
        let server_name = params.server_name;
        if server_name.is_empty() {
            anyhow::bail!("server_name is required");
        }
        let session_id = params
            .session_id
            .filter(|session_id| !session_id.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "session context required to load MCP server '{}'",
                    server_name
                )
            })?;
        let tool_names = normalize_tool_names(params.tool_names).map_err(anyhow::Error::msg)?;
        Ok(Self {
            server_name,
            tool_names,
            session_id,
        })
    }
}

impl LoadMcpTool {
    #[cfg(test)]
    async fn run(
        &self,
        params: LoadMcpParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let request = ValidatedLoadMcpParams::new(params)?;

        let configs = self.mcp_server_config_source.list().await?;
        let available = configs
            .iter()
            .map(|config| config.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let config = configs
            .into_iter()
            .find(|config| config.name == request.server_name);
        let config = config.ok_or_else(|| {
            anyhow::anyhow!(
                "MCP server '{}' not found in config. Available servers: {}",
                request.server_name,
                available
            )
        })?;
        self.run_with_config(request, cancel, config).await
    }

    async fn run_with_config(
        &self,
        request: ValidatedLoadMcpParams,
        cancel: CancellationToken,
        config: haven_common::config::McpServerConfig,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let ValidatedLoadMcpParams {
            server_name,
            session_id,
            tool_names,
        } = request;
        if !config.enabled {
            anyhow::bail!("MCP server '{}' is disabled", server_name);
        }

        // A server name can still refer to a stale client generation while a
        // config apply is reconciling. Never reuse a live client whose exact
        // launch/endpoint config differs from the authorized snapshot.
        if self.mcp_manager.has_client(&server_name).await {
            if !self
                .mcp_manager
                .client_matches_config(&server_name, &config)
                .await
            {
                anyhow::bail!(
                    "MCP server '{}' has a live client from a different configuration; retry after reconciliation",
                    server_name
                );
            }
        } else {
            self.mcp_manager.connect_server(&config).await?;
        }

        let discovered_tools = self
            .mcp_manager
            .wait_for_tools(&server_name, Duration::from_secs(3))
            .await
            .ok_or_else(|| {
                anyhow::anyhow!("MCP server '{}' not available after connect", server_name)
            })?;

        if !self
            .mcp_manager
            .client_matches_config(&server_name, &config)
            .await
        {
            anyhow::bail!(
                "MCP server '{}' configuration changed during tool discovery",
                server_name
            );
        }

        // `load_mcp` is the Legacy/deferred activation route. Direct tools
        // already enter through native definitions; CodeMode and Hidden must
        // never be made callable through this surface.
        let all_tools = deferred_tools_for_exposure(discovered_tools, &config.exposure);
        if all_tools.is_empty() {
            return Ok(ToolResult::ok(serde_json::json!({
                "status": "no_deferred_tools",
                "server_name": server_name,
                "reason": "This server has no tools available through deferred loading.",
            })));
        }

        // Same wait as resume registration so budget and schemas see the
        // populated tools/list, not an empty in-flight cache.
        let selection = select_tools(&all_tools, tool_names.as_deref());
        let selected_tools = selection.selected_tools;
        let missing_tool_names = selection.missing_tool_names;

        if tool_names.is_none() {
            let max = self.max_tools_per_request.max(1);
            let global_count = self.registry.list().await.len();
            let proposed_names: Vec<_> = selected_tools
                .iter()
                .map(|info| McpToolAdapter::qualified_name_of(&server_name, &info.name))
                .collect();
            let budget = self
                .session_tool_overlay
                .preview_registration_budget(&session_id, global_count, max, &proposed_names)
                .await;
            if budget.exceeds_limit() {
                let current = budget.global_count.saturating_add(budget.session_count);
                let remaining = budget.max.saturating_sub(current);
                return Ok(ToolResult::ok(serde_json::json!({
                    "status": "needs_selection",
                    "server_name": server_name,
                    "reason": format!(
                        "Server exposes {} tools; loading all would exceed the per-request limit of {} (currently {} tools: {} builtin + {} session). Call load_mcp again with tool_names set to at most {} tools you need.",
                        all_tools.len(),
                        budget.max,
                        current,
                        budget.global_count,
                        budget.session_count,
                        remaining
                    ),
                    "available_tools": catalog_entries(&all_tools),
                    "tool_count": all_tools.len(),
                    "remaining_budget": remaining,
                    "max_tools_per_request": budget.max,
                })));
            }
        } else if selected_tools.is_empty() {
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
            .activate_server_tools(
                &session_id,
                &server_name,
                self.mcp_manager.as_ref().clone(),
                config.transport.clone(),
                selected_tools,
            )
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
                if !missing_tool_names.is_empty() {
                    result["missing_tool_names"] = serde_json::json!(missing_tool_names);
                }
                // Zero tools after a successful handshake is usually a client/server
                // incompatibility, not an empty server: surface the handshake
                // diagnostic so the model can distinguish the two.
                if tool_schemas.is_empty()
                    && let Some(diagnostic) = self
                        .mcp_manager
                        .client_snapshot(&server_name)
                        .await
                        .and_then(|snapshot| snapshot.diagnostic)
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
                    "missing_tool_names": missing_tool_names,
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
        manager: McpManager,
        transport: haven_common::mcp::McpServerTransport,
        tools: Vec<haven_mcp::McpToolInfo>,
    ) -> anyhow::Result<ActivateOutcome> {
        let global_count = self.registry.list().await.len();
        let mut tool_schemas = Vec::with_capacity(tools.len());
        let mut adapters = Vec::with_capacity(tools.len());
        for info in tools {
            let tool_name = info.name.clone();
            let description = crate::adapters::sanitize_external_description(&info.description);
            let adapter = McpToolAdapter::new_with_transport(
                manager.clone(),
                server_name,
                info,
                transport.clone(),
            );
            // The next provider request receives the full schema through
            // `tools[]`; returning it here would duplicate the schema inside
            // the conversation transcript and inflate every later request.
            tool_schemas.push(serde_json::json!({
                "name": tool_name,
                "description": description,
            }));
            adapters.push(Arc::new(adapter) as ToolHandle);
        }
        match self
            .session_tool_overlay
            .register_many_if_within_budget(
                session_id,
                global_count,
                self.max_tools_per_request,
                adapters,
            )
            .await
        {
            Ok(_) => Ok(ActivateOutcome::Loaded(tool_schemas)),
            Err(budget) => Ok(ActivateOutcome::BudgetExceeded {
                net_new: budget.net_new,
                max: budget.max,
                global_count: budget.global_count,
                session_count: budget.session_count,
            }),
        }
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

struct McpToolSelection {
    selected_tools: Vec<haven_mcp::McpToolInfo>,
    missing_tool_names: Vec<String>,
}

/// Filter cached tools by requested names. Selected tools retain server order;
/// missing names retain request order.
fn select_tools(all: &[haven_mcp::McpToolInfo], tool_names: Option<&[String]>) -> McpToolSelection {
    let Some(names) = tool_names else {
        return McpToolSelection {
            selected_tools: all.to_vec(),
            missing_tool_names: Vec::new(),
        };
    };
    let want: HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
    let mut selected_tools = Vec::new();
    let mut found = HashSet::new();
    for info in all {
        if want.contains(info.name.as_str()) {
            found.insert(info.name.as_str());
            selected_tools.push(info.clone());
        }
    }
    let missing_tool_names = names
        .iter()
        .filter(|n| !found.contains(n.as_str()))
        .cloned()
        .collect();
    McpToolSelection {
        selected_tools,
        missing_tool_names,
    }
}

fn catalog_entries(tools: &[haven_mcp::McpToolInfo]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": crate::adapters::sanitize_external_text(&t.description, 160),
            })
        })
        .collect()
}

fn deferred_tools_for_exposure(
    tools: Vec<haven_mcp::McpToolInfo>,
    exposure: &McpExposureConfig,
) -> Vec<haven_mcp::McpToolInfo> {
    tools
        .into_iter()
        .filter(|info| exposure.for_tool(&info.name) == McpToolExposure::Deferred)
        .collect()
}

#[async_trait]
impl Tool for LoadMcpTool {
    fn name(&self) -> String {
        "load_mcp".into()
    }
    fn description(&self) -> String {
        crate::prompts::LOAD_MCP_DESCRIPTION.into()
    }

    fn catalog_group(&self) -> ToolCatalogGroup {
        ToolCatalogGroup::Extensions
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Safe
    }

    fn operation_spec(&self) -> Option<crate::operation_view::OperationSpec> {
        Some(crate::operation_view::root_tool_spec(
            "load_mcp",
            crate::prompts::LOAD_MCP_DESCRIPTION,
            self.input_schema(),
            RiskLevel::Safe,
            crate::OperationIdempotency::Unknown,
            crate::ToolOperationScope::Session,
            crate::ToolConcurrency::Exclusive,
            ToolCatalogGroup::Extensions,
            None,
        ))
    }

    async fn authorization_contract(
        &self,
        input: &Value,
    ) -> Result<crate::ToolAuthorizationContract, String> {
        let server_name = input
            .get("server_name")
            .and_then(Value::as_str)
            .filter(|server_name| !server_name.is_empty())
            .ok_or_else(|| "server_name is required".to_owned())?;
        let snapshot = self
            .mcp_server_config_source
            .get_versioned(server_name)
            .await
            .map_err(|error| format!("cannot resolve MCP server configuration: {error}"))?
            .ok_or_else(|| format!("MCP server '{server_name}' not found in config"))?;
        if !snapshot.server.enabled {
            return Err(format!("MCP server '{server_name}' is disabled"));
        }

        let mut policy = self.try_operation_policy(input)?;
        match snapshot.server.transport {
            haven_common::mcp::McpServerTransport::Http => {
                // The configured endpoint is validated and DNS-pinned by the
                // HTTP transport under Restricted policy. It is an inspectable
                // public destination, not an opaque local-process network path.
                policy.execution_boundary = crate::ExecutionBoundary::InProcess;
                policy.network_access = crate::NetworkAccess::Public;
            }
            haven_common::mcp::McpServerTransport::Stdio => {
                policy.execution_boundary = crate::ExecutionBoundary::HostProcess;
                policy.network_access = crate::NetworkAccess::Opaque;
            }
        }
        Ok(crate::ToolAuthorizationContract {
            policy,
            target_binding: Some(snapshot.authorization_target_binding()),
            resources: Vec::new(),
        })
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "server_name": { "type": "string", "minLength": 1, "maxLength": 128, "description": "The name of the MCP server to load" },
                "tool_names": {
                    "type": "array",
                    "maxItems": 128,
                    "items": { "type": "string", "minLength": 1, "maxLength": 200 },
                    "minItems": 1,
                    "uniqueItems": true,
                    "description": "Optional subset of tool names from that server. Omit (or null) to load all when they fit the budget; never pass an empty array. If the server is too large, the first call returns a catalog and you must call again with tool_names."
                }
            },
            "required": ["server_name"]
        })
    }

    fn requires_session_id(&self) -> bool {
        true
    }

    /// The unbound Tool entry cannot authorize a server configuration and is
    /// deliberately disabled. Callers must use the target-bound executor.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let _ = (input, cancel);
        anyhow::bail!("load_mcp requires an authorization-bound server configuration")
    }

    /// Parse the LLM request, re-resolve its trusted server config, and require
    /// the exact target binding that was captured before authorization.
    async fn execute_with_authorization_target(
        &self,
        input: Value,
        cancel: CancellationToken,
        target_binding: Option<&str>,
    ) -> anyhow::Result<ToolResult> {
        let params = crate::tool_contract::parse_tool_input::<LoadMcpParams>(&self.name(), input)?;
        let request = ValidatedLoadMcpParams::new(params)?;
        let snapshot = self
            .mcp_server_config_source
            .get_versioned(&request.server_name)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("MCP server '{}' not found in config", request.server_name)
            })?;
        if !snapshot.server.enabled {
            anyhow::bail!("MCP server '{}' is disabled", request.server_name);
        }
        let current_binding = snapshot.authorization_target_binding();
        if target_binding != Some(current_binding.as_str()) {
            anyhow::bail!("MCP server configuration changed after authorization");
        }
        self.run_with_config(request, cancel, snapshot.server).await
    }

    /// Registration is performed atomically inside `run_with_config` before success is
    /// returned, so the executor must not re-apply `McpServer` (that would
    /// race parallel loads and re-wait the tools cache). Resume restores
    /// from history via `register_mcp_for_session` directly.
    fn registrations(&self, _output: &Value) -> Vec<crate::tool_contract::ToolRegistration> {
        Vec::new()
    }
}
#[cfg(test)]
#[path = "load_mcp_tests.rs"]
mod tests;
