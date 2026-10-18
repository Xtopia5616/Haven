//! Model-facing session creation and relationship operations.

use super::canonical_entity_id_schema;
use super::messaging::{
    MAX_CAPABILITIES, MAX_CAPABILITY_BYTES, MAX_ROLE_BYTES, MAX_TITLE_BYTES, blocking,
    check_capabilities, check_role, check_title,
};
use crate::{OperationIdempotency, Tool, ToolResult};
use async_trait::async_trait;
use haven_common::types::RiskLevel;
use haven_memory::SessionStore;
use haven_messaging::{AgentSpawnRequest, MessagingService};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const MAX_TASK_BYTES: usize = 16 * 1024;
const MAX_CHILDREN_PER_PARENT: usize = 8;
const OPERATIONS: &[&str] = &["create", "link", "unlink", "links"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionOperation {
    Create,
    Link,
    Unlink,
    Links,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionParams {
    operation: SessionOperation,
    /// Private owner identity injected by the Tools facade.
    #[serde(default, rename = "_session_id")]
    session_id: Option<String>,
    #[serde(default)]
    related_session_id: Option<String>,
    #[serde(default)]
    task: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    capabilities: Option<Vec<String>>,
}

/// Create Agent-owned child sessions and manage explicit session links.
pub struct SessionTool {
    messaging: Arc<MessagingService>,
    session_store: Option<SessionStore>,
}

impl SessionTool {
    pub fn new(messaging: Arc<MessagingService>, session_store: Option<SessionStore>) -> Self {
        Self {
            messaging,
            session_store,
        }
    }

    async fn run(
        &self,
        params: SessionParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        anyhow::ensure!(!cancel.is_cancelled(), "cancelled");
        let session_id = current_session_id(params.session_id.clone())?;
        match params.operation {
            SessionOperation::Create => self.create_child(params, session_id, cancel).await,
            SessionOperation::Link => self.link(params, session_id).await,
            SessionOperation::Unlink => self.unlink(params, session_id).await,
            SessionOperation::Links => self.list_links(session_id).await,
        }
    }

    async fn create_child(
        &self,
        params: SessionParams,
        parent_session_id: String,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let task = check_task(params.task.as_deref().unwrap_or(""))?;
        let title = check_title(params.title)?;
        let role = check_role(params.role)?;
        let capabilities = check_capabilities(params.capabilities)?;
        register_parent(&self.messaging, &parent_session_id, &cancel).await?;

        let messaging = Arc::clone(&self.messaging);
        let count_parent = parent_session_id.clone();
        let child_count = blocking(messaging, move |service| {
            service.count_registered_children(&count_parent)
        })
        .await?;
        anyhow::ensure!(
            child_count < MAX_CHILDREN_PER_PARENT,
            "parent_session_id has {child_count} registered child sessions (maximum {MAX_CHILDREN_PER_PARENT}); offline, ended, and deleted child identities still count, so no additional child can be created"
        );
        anyhow::ensure!(!cancel.is_cancelled(), "cancelled");

        let result = self
            .messaging
            .spawn_peer_session(AgentSpawnRequest {
                parent_session_id: parent_session_id.clone(),
                task,
                title: title.clone(),
                role: role.clone(),
                capabilities: capabilities.clone(),
            })
            .await?;
        let hint = if result.queued {
            format!(
                "Child session created but queued behind other runs ({}/{} slots busy). Wait for it to become active before requesting a response.",
                result.running_sessions, result.max_concurrent
            )
        } else {
            "Use agent operation=request to coordinate; the child should reply with the request id."
                .into()
        };
        Ok(ToolResult::ok(json!({
            "ok": true,
            "session_id": result.session_id,
            "parent_session_id": parent_session_id,
            "title": result.title.or(title),
            "role": result.role.or(role),
            "capabilities": capabilities,
            "queued": result.queued,
            "running_sessions": result.running_sessions,
            "max_concurrent": result.max_concurrent,
            "hint": hint,
        })))
    }

    async fn link(&self, params: SessionParams, session_id: String) -> anyhow::Result<ToolResult> {
        let related_session_id = related_session_id(params.related_session_id)?;
        let store = self.require_store()?;
        let created = store
            .link_sessions(&session_id, &related_session_id)
            .await?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "session_id": session_id,
            "related_session_id": related_session_id,
            "created": created,
        })))
    }

    async fn unlink(
        &self,
        params: SessionParams,
        session_id: String,
    ) -> anyhow::Result<ToolResult> {
        let related_session_id = related_session_id(params.related_session_id)?;
        let store = self.require_store()?;
        let removed = store
            .unlink_sessions(&session_id, &related_session_id)
            .await?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "session_id": session_id,
            "related_session_id": related_session_id,
            "removed": removed,
        })))
    }

    async fn list_links(&self, session_id: String) -> anyhow::Result<ToolResult> {
        let links = self
            .require_store()?
            .list_session_links(&session_id)
            .await?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "session_id": session_id,
            "links": links,
        })))
    }

    fn require_store(&self) -> anyhow::Result<&SessionStore> {
        self.session_store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("session relationship store is unavailable"))
    }
}

fn current_session_id(session_id: Option<String>) -> anyhow::Result<String> {
    let session_id = session_id
        .filter(|session_id| !session_id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("session tool requires a session context"))?;
    crate::tool_contract::validate_entity_id(&session_id, "session_id", "ses")?;
    Ok(session_id)
}

fn related_session_id(session_id: Option<String>) -> anyhow::Result<String> {
    let session_id = session_id.ok_or_else(|| anyhow::anyhow!("related_session_id is required"))?;
    crate::tool_contract::validate_entity_id(&session_id, "related_session_id", "ses")?;
    Ok(session_id)
}

fn check_task(task: &str) -> anyhow::Result<String> {
    let task = task.trim();
    anyhow::ensure!(!task.is_empty(), "task must not be empty");
    anyhow::ensure!(
        task.len() <= MAX_TASK_BYTES,
        "task too long (max {MAX_TASK_BYTES} bytes)"
    );
    let cleaned: String = task
        .chars()
        .map(|character| {
            if character == '\n' || character == '\t' || !character.is_control() {
                character
            } else {
                ' '
            }
        })
        .collect();
    Ok(cleaned
        .replace("</delegated_task>", "[/delegated_task]")
        .replace("<delegated_task>", "[delegated_task]"))
}

async fn register_parent(
    messaging: &Arc<MessagingService>,
    session_id: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    anyhow::ensure!(!cancel.is_cancelled(), "cancelled");
    let messaging = Arc::clone(messaging);
    let session_id = session_id.to_owned();
    blocking(messaging, move |service| service.register(&session_id, &[])).await
}

#[async_trait]
impl Tool for SessionTool {
    fn name(&self) -> String {
        "session".into()
    }

    fn description(&self) -> String {
        crate::prompts::SESSION_DESCRIPTION.into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input.get("operation").and_then(Value::as_str) {
            Some("create") => RiskLevel::Medium,
            Some("link") | Some("unlink") => RiskLevel::Low,
            _ => RiskLevel::Safe,
        }
    }

    fn idempotency(&self, input: &Value) -> OperationIdempotency {
        match input.get("operation").and_then(Value::as_str) {
            Some("links") | Some("link") | Some("unlink") => OperationIdempotency::Idempotent,
            Some("create") => OperationIdempotency::NonIdempotent,
            _ => OperationIdempotency::Unknown,
        }
    }

    fn requires_session_id(&self) -> bool {
        true
    }

    fn timeout_secs_for(&self, input: &Value) -> u64 {
        if input["operation"] == "create" {
            60
        } else {
            30
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": OPERATIONS },
                "task": { "type": "string", "minLength": 1, "maxLength": MAX_TASK_BYTES, "description": "Self-contained, low-trust task brief for the child session" },
                "title": { "type": "string", "minLength": 1, "maxLength": MAX_TITLE_BYTES, "description": "Optional short child-session title" },
                "role": { "type": "string", "minLength": 1, "maxLength": MAX_ROLE_BYTES, "description": "Optional role for peer discovery" },
                "capabilities": { "type": "array", "minItems": 1, "maxItems": MAX_CAPABILITIES, "uniqueItems": true, "items": { "type": "string", "minLength": 1, "maxLength": MAX_CAPABILITY_BYTES } },
                "related_session_id": canonical_entity_id_schema("ses", Some("Session to link or unlink with the current session"))
            },
            "required": ["operation"],
            "oneOf": [
                { "type": "object", "additionalProperties": false, "properties": {
                    "operation": { "const": "create" },
                    "task": { "type": "string", "minLength": 1, "maxLength": MAX_TASK_BYTES },
                    "title": { "type": "string", "minLength": 1, "maxLength": MAX_TITLE_BYTES },
                    "role": { "type": "string", "minLength": 1, "maxLength": MAX_ROLE_BYTES },
                    "capabilities": { "type": "array", "minItems": 1, "maxItems": MAX_CAPABILITIES, "uniqueItems": true, "items": { "type": "string", "minLength": 1, "maxLength": MAX_CAPABILITY_BYTES } }
                }, "required": ["operation", "task"] },
                { "type": "object", "additionalProperties": false, "properties": {
                    "operation": { "const": "link" },
                    "related_session_id": canonical_entity_id_schema("ses", None)
                }, "required": ["operation", "related_session_id"] },
                { "type": "object", "additionalProperties": false, "properties": {
                    "operation": { "const": "unlink" },
                    "related_session_id": canonical_entity_id_schema("ses", None)
                }, "required": ["operation", "related_session_id"] },
                { "type": "object", "additionalProperties": false, "properties": {
                    "operation": { "const": "links" }
                }, "required": ["operation"] }
            ]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params: SessionParams = crate::tool_contract::parse_tool_input(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_messaging::{
        AgentControlOperation, AgentControlRequest, AgentControlResult, AgentSpawnResult,
        MessageTransport, MessagingRuntime,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestRuntime {
        transport: Arc<dyn MessageTransport>,
        create_count: AtomicUsize,
    }

    #[async_trait]
    impl MessagingRuntime for TestRuntime {
        async fn spawn_peer_session(
            &self,
            request: AgentSpawnRequest,
        ) -> anyhow::Result<haven_messaging::AgentSpawnResult> {
            self.create_count.fetch_add(1, Ordering::Relaxed);
            let session_id = "ses-00000000000000000000000000000001".to_owned();
            self.transport.register_with_profile(
                &session_id,
                &request.capabilities,
                request.title.as_deref(),
                request.role.as_deref(),
                Some(&request.parent_session_id),
            )?;
            Ok(AgentSpawnResult {
                session_id,
                title: request.title,
                role: request.role,
                queued: false,
                running_sessions: 0,
                max_concurrent: 3,
            })
        }

        async fn control_peer_session(
            &self,
            request: AgentControlRequest,
        ) -> anyhow::Result<AgentControlResult> {
            Ok(AgentControlResult {
                session_id: request.target_session_id,
                status: match request.operation {
                    AgentControlOperation::Status => "running".into(),
                    AgentControlOperation::Wait | AgentControlOperation::Stop => "completed".into(),
                },
                terminal: request.operation != AgentControlOperation::Status,
                timed_out: false,
                title: Some("child".into()),
            })
        }
    }

    fn session_tool(
        store: Option<SessionStore>,
    ) -> (
        tempfile::TempDir,
        SessionTool,
        Arc<dyn MessageTransport>,
        Arc<TestRuntime>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let transport = haven_messaging::file_transport_for_test(temp.path());
        let runtime = Arc::new(TestRuntime {
            transport: transport.clone(),
            create_count: AtomicUsize::new(0),
        });
        let service = Arc::new(MessagingService::new_for_test(transport.clone()));
        service.bind_runtime(runtime.clone()).unwrap();
        (temp, SessionTool::new(service, store), transport, runtime)
    }

    #[tokio::test]
    async fn create_registers_a_child_session_through_session_tool() {
        let (_temp, tool, transport, runtime) = session_tool(None);
        let result = tool
            .execute(
                json!({
                    "operation": "create",
                    "_session_id": "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "task": "Review the parser",
                    "title": "parser-review",
                    "role": "reviewer",
                    "capabilities": ["code"]
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(
            result.output["session_id"],
            "ses-00000000000000000000000000000001"
        );
        assert_eq!(runtime.create_count.load(Ordering::Relaxed), 1);
        assert_eq!(
            transport.list_agents().unwrap()[0]
                .parent_session_id
                .as_deref(),
            Some("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
    }

    #[tokio::test]
    async fn child_session_limit_remains_enforced_after_tool_move() {
        let (_temp, tool, transport, runtime) = session_tool(None);
        let parent_session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        for index in 0..MAX_CHILDREN_PER_PARENT {
            let child_session_id = format!("ses-{:032x}", index + 1);
            transport
                .register_with_profile(&child_session_id, &[], None, None, Some(parent_session_id))
                .unwrap();
        }

        let error = tool
            .execute(
                json!({
                    "operation": "create",
                    "_session_id": parent_session_id,
                    "task": "one more child"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("maximum 8"));
        assert_eq!(runtime.create_count.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn session_links_can_be_read_from_either_endpoint_and_removed() {
        let db = Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let store = SessionStore::new(db.clone());
        let first = db.create_session("first").unwrap();
        let second = db.create_session("second").unwrap();
        let (_temp, tool, _, _) = session_tool(Some(store.clone()));

        let linked = tool
            .execute(
                json!({
                    "operation": "link",
                    "_session_id": first.session_id,
                    "related_session_id": second.session_id
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(linked.output["created"], true);
        assert_eq!(
            store
                .list_session_links(&second.session_id)
                .await
                .unwrap()
                .len(),
            1
        );

        let removed = tool
            .execute(
                json!({
                    "operation": "unlink",
                    "_session_id": first.session_id,
                    "related_session_id": second.session_id
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(removed.output["removed"], true);
        assert!(
            store
                .list_session_links(&first.session_id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn session_tool_rejects_invalid_link_target_without_creating_a_link() {
        let db = Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let first = db.create_session("first").unwrap();
        let store = SessionStore::new(db.clone());
        let (_temp, tool, _, _) = session_tool(Some(store));
        let error = tool
            .execute(
                json!({
                    "operation": "link",
                    "_session_id": first.session_id,
                    "related_session_id": "ses-invalid"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("canonical"));
        assert!(db.list_session_links(&first.session_id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancelled_link_does_not_mutate_session_relationships() {
        let db = Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let first = db.create_session("first").unwrap();
        let second = db.create_session("second").unwrap();
        let store = SessionStore::new(db.clone());
        let (_temp, tool, _, _) = session_tool(Some(store));
        let cancel = CancellationToken::new();
        cancel.cancel();

        let error = tool
            .execute(
                json!({
                    "operation": "link",
                    "_session_id": first.session_id,
                    "related_session_id": second.session_id
                }),
                cancel,
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("cancelled"));
        assert!(db.list_session_links(&first.session_id).unwrap().is_empty());
    }
}
