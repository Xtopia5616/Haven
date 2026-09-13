use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::grouped_schema::{grouped_schema, operation_branch, rename_operation_branches};
use crate::{OperationIdempotency, Tool, ToolBox, ToolConcurrency, ToolDef, ToolOperationScope};

struct HavenRoute {
    public_operation: String,
    child_operation: Option<String>,
    child: ToolBox,
    schema: Value,
}

/// Model-facing Haven control surface.
///
/// This is an operation router, not a second implementation of the child
/// tools. Each route retains the child tool's schema, risk, retry and
/// concurrency policy, so grouping does not flatten security decisions.
pub struct HavenTool {
    routes: Vec<HavenRoute>,
}

impl HavenTool {
    pub fn new(
        admin_tools: Vec<ToolBox>,
        actions: ToolBox,
        schedule: ToolBox,
        preferences: ToolBox,
        checklist: ToolBox,
    ) -> Self {
        let mut routes = Vec::new();

        for child in admin_tools {
            add_child_routes(&mut routes, child, "");
        }
        add_child_routes(&mut routes, schedule, "schedule_");
        add_child_routes(&mut routes, preferences, "preferences_");
        add_child_routes(&mut routes, checklist, "checklist_");

        // The action board has explicit grouped discriminators for list,
        // inspect, and cancel while retaining the child tool's typed payload.
        routes.push(HavenRoute {
            public_operation: "actions_list".into(),
            child_operation: None,
            child: actions.clone(),
            schema: serde_json::json!({
                "type": "object",
                "oneOf": [
                    {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "operation": { "const": "actions_list" },
                            "status": {
                                "type": "string",
                                "enum": ["running", "completed", "failed", "cancelled"]
                            }
                        },
                        "required": ["operation"]
                    },
                    {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "operation": { "const": "actions_list" },
                            "action_id": { "type": "string", "minLength": 1 }
                        },
                        "required": ["operation", "action_id"]
                    }
                ]
            }),
        });
        routes.push(HavenRoute {
            public_operation: "actions_cancel".into(),
            child_operation: Some("cancel".into()),
            child: actions,
            schema: operation_branch(
                "actions_cancel",
                serde_json::json!({
                    "action_id": { "type": "string", "minLength": 1 }
                }),
                &["operation", "action_id"],
            ),
        });

        Self { routes }
    }

    fn route(&self, input: &Value) -> Option<&HavenRoute> {
        let operation = input.get("operation").and_then(Value::as_str)?;
        self.routes
            .iter()
            .find(|route| route.public_operation == operation)
    }

    fn child_input(route: &HavenRoute, input: &Value) -> Value {
        let mut child_input = input.clone();
        if let Some(object) = child_input.as_object_mut() {
            match route.child_operation.as_deref() {
                Some(operation) => {
                    object.insert("operation".into(), Value::String(operation.into()));
                }
                None => {
                    object.remove("operation");
                }
            }
            // The manager injects the private session id at the aggregate
            // boundary. Only session-owned children may receive it; typed
            // global adapters intentionally reject unknown fields.
            if !route.child.requires_session_id() {
                object.remove("_session_id");
            }
        }
        child_input
    }
}

fn add_child_routes(routes: &mut Vec<HavenRoute>, child: ToolBox, prefix: &str) {
    for (public_operation, schema) in rename_operation_branches(&child.input_schema(), prefix) {
        let child_operation = public_operation
            .strip_prefix(prefix)
            .map(str::to_owned)
            .filter(|operation| !operation.is_empty());
        routes.push(HavenRoute {
            public_operation,
            child_operation,
            child: child.clone(),
            schema,
        });
    }
}

#[async_trait]
impl Tool for HavenTool {
    fn name(&self) -> String {
        "haven".into()
    }

    fn description(&self) -> String {
        "Manage Haven and its session utilities. Use operation=status, config_get, \
         skills_list, mcp_list, logs_tail, sessions, errors, or the prefixed \
         schedule_*, preferences_*, checklist_* and actions_* operations. \
         Each operation keeps its own security and confirmation policy."
            .into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        self.route(input)
            .map(|route| route.child.risk_level(&Self::child_input(route, input)))
            .unwrap_or(RiskLevel::Critical)
    }

    fn idempotency(&self, input: &Value) -> OperationIdempotency {
        self.route(input)
            .map(|route| route.child.idempotency(&Self::child_input(route, input)))
            .unwrap_or(OperationIdempotency::Unknown)
    }

    fn operation_scope(&self, input: &Value) -> ToolOperationScope {
        self.route(input)
            .map(|route| {
                route
                    .child
                    .operation_scope(&Self::child_input(route, input))
            })
            .unwrap_or(ToolOperationScope::Session)
    }

    fn timeout_secs_for(&self, input: &Value) -> u64 {
        self.route(input)
            .map(|route| {
                route
                    .child
                    .timeout_secs_for(&Self::child_input(route, input))
            })
            .unwrap_or_else(|| self.default_timeout_secs())
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        self.route(input)
            .map(|route| route.child.concurrency(&Self::child_input(route, input)))
            .unwrap_or(ToolConcurrency::Exclusive)
    }

    fn tool_def(&self) -> ToolDef {
        ToolDef::new(
            self.name(),
            self.description(),
            self.input_schema(),
            self.routes
                .iter()
                .map(|route| {
                    route.child.risk_level(&Self::child_input(
                        route,
                        &serde_json::json!({ "operation": route.child_operation }),
                    ))
                })
                .fold(RiskLevel::Safe, |current_max, risk| {
                    if risk > current_max {
                        risk
                    } else {
                        current_max
                    }
                }),
        )
    }

    fn requires_session_id(&self) -> bool {
        true
    }

    fn input_schema(&self) -> Value {
        let operation_names = self
            .routes
            .iter()
            .map(|route| route.public_operation.clone())
            .collect::<Vec<_>>();
        grouped_schema(
            &operation_names,
            self.routes
                .iter()
                .map(|route| route.schema.clone())
                .collect(),
        )
    }

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<crate::ToolResult> {
        let route = self
            .route(&input)
            .ok_or_else(|| anyhow::anyhow!("unknown Haven operation"))?;
        let public_operation = route.public_operation.clone();
        let mut result = route
            .child
            .execute(Self::child_input(route, &input), cancel)
            .await?;
        if let Some(object) = result.output.as_object_mut() {
            object.insert("operation".into(), Value::String(public_operation));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::actions::ActionsTool;
    use crate::builtin::admin::{ConfigAdminContext, new_config_admin_tool};
    use crate::builtin::checklist::ChecklistTool;
    use crate::builtin::preferences::PreferencesTool;
    use crate::builtin::scheduled_action::{ScheduledActionCenter, ScheduledActionTool};
    use haven_common::config::{ConfigLoader, ConfigService};
    use std::sync::Arc;
    use tempfile::TempDir;

    #[test]
    fn grouped_routes_keep_distinct_operation_risk() {
        let actions: ToolBox = Arc::new(ActionsTool {
            actions: Arc::new(crate::BackgroundActions::new()),
        });
        let schedule: ToolBox = Arc::new(ScheduledActionTool {
            center: Arc::new(ScheduledActionCenter::new()),
            registry: None,
        });
        let tool = HavenTool::new(
            Vec::new(),
            actions,
            schedule,
            Arc::new(PreferencesTool::default()),
            Arc::new(ChecklistTool::default()),
        );

        assert_eq!(
            tool.risk_level(&serde_json::json!({"operation": "actions_list"})),
            RiskLevel::Safe
        );
        assert_eq!(
            tool.risk_level(&serde_json::json!({"operation": "actions_cancel"})),
            RiskLevel::Medium
        );
        assert_eq!(
            tool.risk_level(&serde_json::json!({"operation": "schedule_set"})),
            RiskLevel::Low
        );
        assert!(
            tool.validate_input(&serde_json::json!({
                "operation": "preferences_set",
                "key": "style",
                "value": "concise"
            }))
            .is_ok()
        );
        assert!(
            tool.validate_input(&serde_json::json!({
                "operation": "preferences_set",
                "key": "style"
            }))
            .is_err()
        );
        assert!(
            tool.validate_input(&serde_json::json!({
                "operation": "actions_list",
                "action_id": "act-1",
                "status": "running"
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn grouped_global_child_does_not_receive_private_session_field() {
        let dir = TempDir::new().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let config = Arc::new(new_config_admin_tool(ConfigAdminContext {
            config_service: Some(Arc::new(ConfigService::new(loader))),
            set_log_level: None,
        }));
        let actions: ToolBox = Arc::new(ActionsTool {
            actions: Arc::new(crate::BackgroundActions::new()),
        });
        let schedule: ToolBox = Arc::new(ScheduledActionTool {
            center: Arc::new(ScheduledActionCenter::new()),
            registry: None,
        });
        let tool = HavenTool::new(
            vec![config],
            actions,
            schedule,
            Arc::new(PreferencesTool::default()),
            Arc::new(ChecklistTool::default()),
        );

        let result = tool
            .execute(
                serde_json::json!({
                    "operation": "config_get",
                    "_session_id": "ses-0123456789abcdef0123456789abcdef"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["operation"], "config_get");
    }
}
