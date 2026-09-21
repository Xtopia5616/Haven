use haven_common::types::{CapabilityScope, RiskLevel, permission_key};
use serde_json::{Map, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub use haven_common::tools::{
    ToolAvailability, ToolCatalogGroup, ToolDef, ToolIdentity, ToolManifest, ToolModel, ToolPolicy,
    ToolPresentation, ToolPrompt, ToolRootPresentation, ToolSource,
};

/// The durable meaning of a tool invocation's terminal state.
///
/// `TimedOutUnknown` is deliberately distinct from a normal failure: the
/// caller must assume that an external side effect may still be in flight and
/// must not replay the operation automatically.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionOutcome {
    Succeeded,
    #[default]
    Failed,
    Cancelled,
    TimedOutAndTerminated,
    TimedOutUnknown,
}

/// Structured failure class consumed by retry and recovery policy. The human
/// error string remains a diagnostic, never the policy source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorClass {
    Transient,
    UnknownOutcome,
    Validation,
    Permission,
    SideEffectMayHaveHappened,
    #[default]
    Other,
}

/// Whether replaying a failed invocation is safe after the operation has
/// returned. This is deliberately separate from [`OperationIdempotency`]:
/// idempotency describes the operation, while this value describes the
/// concrete failure that was observed.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ToolRetryability {
    Retryable,
    NotRetryable,
    #[default]
    Unknown,
}

impl ToolRetryability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Retryable => "retryable",
            Self::NotRetryable => "not_retryable",
            Self::Unknown => "unknown",
        }
    }
}

/// Complete machine-readable policy for a tool failure. The diagnostic text
/// remains user/model-facing context; recovery code must consume this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolErrorMetadata {
    pub class: ToolErrorClass,
    pub outcome: ToolExecutionOutcome,
    pub retryability: ToolRetryability,
}

impl ToolErrorMetadata {
    pub const fn other() -> Self {
        Self {
            class: ToolErrorClass::Other,
            outcome: ToolExecutionOutcome::Failed,
            retryability: ToolRetryability::NotRetryable,
        }
    }

    pub const fn validation() -> Self {
        Self {
            class: ToolErrorClass::Validation,
            outcome: ToolExecutionOutcome::Failed,
            retryability: ToolRetryability::NotRetryable,
        }
    }

    pub const fn transient() -> Self {
        Self {
            class: ToolErrorClass::Transient,
            outcome: ToolExecutionOutcome::Failed,
            retryability: ToolRetryability::Retryable,
        }
    }

    pub const fn unknown_outcome() -> Self {
        Self {
            class: ToolErrorClass::UnknownOutcome,
            outcome: ToolExecutionOutcome::TimedOutUnknown,
            retryability: ToolRetryability::Unknown,
        }
    }

    pub const fn unknown_failure() -> Self {
        Self {
            class: ToolErrorClass::UnknownOutcome,
            outcome: ToolExecutionOutcome::Failed,
            retryability: ToolRetryability::Unknown,
        }
    }
}

/// Error wrapper for implementation boundaries that must preserve structured
/// recovery metadata while keeping the object-safe `Tool::execute` contract as
/// `anyhow::Result`. The diagnostic is still a string for display, but policy
/// never needs to inspect it.
#[derive(Debug)]
pub struct StructuredToolError {
    message: String,
    metadata: ToolErrorMetadata,
}

impl StructuredToolError {
    pub fn new(message: impl Into<String>, metadata: ToolErrorMetadata) -> Self {
        Self {
            message: message.into(),
            metadata,
        }
    }

    pub fn metadata(&self) -> ToolErrorMetadata {
        self.metadata
    }
}

impl std::fmt::Display for StructuredToolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for StructuredToolError {}

impl ToolErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::UnknownOutcome => "unknown_outcome",
            Self::Validation => "validation",
            Self::Permission => "permission",
            Self::SideEffectMayHaveHappened => "side_effect_may_have_happened",
            Self::Other => "other",
        }
    }
}

impl ToolExecutionOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOutAndTerminated => "timed_out",
            Self::TimedOutUnknown => "unknown",
        }
    }
}

/// Whether replaying the same operation is safe after a transient failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationIdempotency {
    Idempotent,
    NonIdempotent,
    Unknown,
}

impl OperationIdempotency {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idempotent => "idempotent",
            Self::NonIdempotent => "non_idempotent",
            Self::Unknown => "unknown",
        }
    }

    pub(crate) fn tool_retry_safety(self) -> haven_common::tools::ToolRetrySafety {
        match self {
            Self::Idempotent => haven_common::tools::ToolRetrySafety::SafeToRetry,
            Self::NonIdempotent => haven_common::tools::ToolRetrySafety::UnsafeToRetry,
            Self::Unknown => haven_common::tools::ToolRetrySafety::Unknown,
        }
    }
}

/// Confirmation is intentionally a policy mode, not a frontend boolean. The
/// authorization engine still applies the active security configuration to
/// `SecurityPolicy`; `Required` is reserved for operations with a hard floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationRequirement {
    None,
    SecurityPolicy,
    Required,
}

impl ConfirmationRequirement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SecurityPolicy => "security_policy",
            Self::Required => "required",
        }
    }
}

/// What the operation does. This deliberately does not reuse concurrency:
/// read-only work can still disclose sensitive data, use the network, or
/// create an internal artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationEffect {
    ReadOnly,
    WorkspaceWrite,
    ExternalEffect,
}

impl OperationEffect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::WorkspaceWrite => "workspace_write",
            Self::ExternalEffect => "external_effect",
        }
    }
}

/// How much user-controlled information the operation may disclose or expose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataSensitivity {
    None,
    UserData,
    Sensitive,
}

impl DataSensitivity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::UserData => "user_data",
            Self::Sensitive => "sensitive",
        }
    }
}

/// Network capability of the concrete operation. `Opaque` means an external
/// process or adapter may choose destinations that Haven cannot inspect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkAccess {
    None,
    Public,
    Opaque,
}

impl NetworkAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Public => "public",
            Self::Opaque => "opaque",
        }
    }
}

/// Single runtime policy returned by every tool implementation. The manifest
/// is derived from this value, while the authorization gateway consumes the
/// same risk and capability identity for the actual call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationPolicy {
    pub risk_level: RiskLevel,
    pub capability: CapabilityScope,
    pub confirmation: ConfirmationRequirement,
    pub idempotency: OperationIdempotency,
    pub scope: ToolOperationScope,
    pub concurrency: ToolConcurrency,
    pub effect: OperationEffect,
    pub data_sensitivity: DataSensitivity,
    pub network_access: NetworkAccess,
}

impl OperationPolicy {
    /// Contract for an external adapter whose destination/effect is not
    /// inspectable by Haven. External capabilities intentionally remain
    /// `Opaque` even when the adapter happens to be used for a read request;
    /// the network/sandbox policy must not infer trust from a tool name.
    pub fn external(capability: impl Into<CapabilityScope>, risk_level: RiskLevel) -> Self {
        Self {
            risk_level,
            capability: capability.into(),
            confirmation: if risk_level >= RiskLevel::Critical {
                ConfirmationRequirement::Required
            } else {
                ConfirmationRequirement::SecurityPolicy
            },
            idempotency: OperationIdempotency::Unknown,
            scope: ToolOperationScope::Session,
            concurrency: ToolConcurrency::Exclusive,
            effect: OperationEffect::ExternalEffect,
            data_sensitivity: DataSensitivity::Sensitive,
            network_access: NetworkAccess::Opaque,
        }
    }

    /// Build a contract for a native/UI entry point that does not have a
    /// registered `Tool` object. Callers must still provide the same stable
    /// capability scope and explicitly declare the network capability.
    pub fn native(
        tool_name: &str,
        capability: CapabilityScope,
        risk_level: RiskLevel,
        network_access: NetworkAccess,
    ) -> Self {
        let (effect, data_sensitivity, _) =
            operation_attributes(tool_name, ToolConcurrency::Exclusive);
        Self {
            risk_level,
            capability,
            confirmation: if risk_level >= RiskLevel::Critical {
                ConfirmationRequirement::Required
            } else if risk_level == RiskLevel::Safe {
                ConfirmationRequirement::None
            } else {
                ConfirmationRequirement::SecurityPolicy
            },
            idempotency: OperationIdempotency::Unknown,
            scope: ToolOperationScope::Session,
            concurrency: ToolConcurrency::Exclusive,
            effect,
            data_sensitivity,
            network_access,
        }
    }

    /// Whether the operation is explicitly declared read-only by its
    /// executable scheduling contract. Risk alone is not enough: a Safe
    /// operation can still have an external effect (for example speech).
    pub fn is_read_only(&self) -> bool {
        matches!(self.effect, OperationEffect::ReadOnly)
    }

    pub fn requires_disclosure_confirmation(&self) -> bool {
        matches!(self.data_sensitivity, DataSensitivity::Sensitive)
            || !matches!(self.network_access, NetworkAccess::None)
    }

    pub fn to_catalog_policy(&self) -> ToolPolicy {
        let concurrency = match self.concurrency {
            ToolConcurrency::ReadOnly => "read_only".to_string(),
            ToolConcurrency::SharedResource(_) => "shared_resource".to_string(),
            ToolConcurrency::Resource(_) => "resource".to_string(),
            ToolConcurrency::Exclusive => "exclusive".to_string(),
        };
        ToolPolicy {
            risk_level: self.risk_level,
            permission_key: self.capability.to_string(),
            confirmation: self.confirmation.as_str().into(),
            idempotency: self.idempotency.as_str().into(),
            scope: self.scope.as_str().into(),
            concurrency,
            effect: self.effect.as_str().into(),
            data_sensitivity: self.data_sensitivity.as_str().into(),
            network_access: self.network_access.as_str().into(),
        }
    }
}

/// Conservative operation attributes shared by the default Tool contract and
/// the operation-view catalog. Unknown operations inherit only their explicit
/// concurrency declaration; they never inherit a permissive network or data
/// classification.
pub(crate) fn operation_attributes(
    name: &str,
    concurrency: ToolConcurrency,
) -> (OperationEffect, DataSensitivity, NetworkAccess) {
    let effect = if matches!(concurrency, ToolConcurrency::ReadOnly) {
        OperationEffect::ReadOnly
    } else {
        OperationEffect::ExternalEffect
    };
    let effect = match name {
        "files.write" | "files.edit" | "files.patch" | "files.create_dir" | "files.copy"
        | "files.move" => OperationEffect::WorkspaceWrite,
        _ => effect,
    };
    let data = match name {
        "system.env.list"
        | "system.env.get"
        | "system.registry.list"
        | "system.registry.get"
        | "clipboard.read"
        | "clipboard.history" => DataSensitivity::Sensitive,
        name if name.starts_with("files.")
            || name.starts_with("media.")
            || name.starts_with("window.")
            || name.starts_with("memory.") =>
        {
            DataSensitivity::UserData
        }
        _ => DataSensitivity::None,
    };
    let network = match name {
        "http" | "files.summary" | "media.describe" | "media.ocr" | "media.transcribe"
        | "media.generate" | "window.ocr" => NetworkAccess::Public,
        "shell" | "load_mcp" | "load_skill" => NetworkAccess::Opaque,
        name if name.starts_with("mcp__")
            || name.starts_with("mcp::")
            || name.starts_with("skill__")
            || name.starts_with("skill::") =>
        {
            NetworkAccess::Opaque
        }
        _ => NetworkAccess::None,
    };
    (effect, data, network)
}

pub(crate) fn operation_attributes_for_input(
    name: &str,
    input: &Value,
    concurrency: ToolConcurrency,
) -> (OperationEffect, DataSensitivity, NetworkAccess) {
    let operation = input.get("operation").and_then(Value::as_str);
    let scope = input.get("scope").and_then(Value::as_str);
    let derived = match (scope, operation) {
        (Some(scope), Some(operation)) if !scope.is_empty() && !operation.is_empty() => {
            Some(format!("{name}.{scope}.{operation}"))
        }
        (None, Some(operation)) if !operation.is_empty() => Some(format!("{name}.{operation}")),
        _ => None,
    };
    derived
        .as_deref()
        .map(|candidate| operation_attributes(candidate, concurrency.clone()))
        .unwrap_or_else(|| operation_attributes(name, concurrency))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: Value,
    pub error: Option<String>,
    /// Machine-readable failure class. `None` for successful results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_class: Option<ToolErrorClass>,
    /// Whether this concrete failure may be replayed. `Unknown` is the safe
    /// default for results constructed by an adapter that cannot prove the
    /// side-effect state.
    #[serde(default, skip_serializing_if = "ToolResult::retryability_is_unknown")]
    pub retryability: ToolRetryability,
    pub truncated: bool,
    #[serde(default)]
    pub outcome: ToolExecutionOutcome,
    /// Number of attempts used by the manager. Direct tool calls use 1.
    #[serde(default = "default_attempts")]
    pub attempts: u32,
    /// Side-channel signals the tool attaches to its own result (an `ask`
    /// question to pause for, a `notify` toast to surface). Populated by
    /// `ToolsManager::execute_tool` from the tool's `signals()` hook BEFORE
    /// any observation truncation, so the ReAct loop reads structured data
    /// instead of name-matching and re-parsing output JSON.
    #[serde(default)]
    pub signals: ToolSignals,
    /// Usage produced by an internal model call owned by the tool. This is a
    /// runtime side channel: it is consumed by the Agent layer for separate
    /// persistence/telemetry and is never serialized into the model-facing
    /// observation.
    #[serde(skip)]
    pub llm_usage: Vec<ToolLlmUsage>,
}

/// Stable metadata envelope emitted with a terminal observation. The
/// operation-specific payload remains in `ToolResult::output`; this envelope
/// only carries fields that Agent, UI and logs must interpret consistently.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolResultEnvelope {
    pub outcome: ToolExecutionOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<ToolErrorClass>,
    pub retry_safety: String,
    pub retryability: ToolRetryability,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    #[serde(default)]
    pub assets: Vec<String>,
}

impl Default for ToolResultEnvelope {
    fn default() -> Self {
        Self::from_parts(
            ToolExecutionOutcome::Failed,
            None,
            ToolRetryability::Unknown,
            OperationIdempotency::Unknown,
        )
    }
}

impl ToolResultEnvelope {
    pub fn from_parts(
        outcome: ToolExecutionOutcome,
        error_class: Option<ToolErrorClass>,
        retryability: ToolRetryability,
        idempotency: OperationIdempotency,
    ) -> Self {
        Self {
            outcome,
            error_class,
            retry_safety: idempotency.as_str().into(),
            retryability,
            verification_hint: None,
            next_action: None,
            assets: Vec::new(),
        }
    }

    pub fn with_default_retry_safety(mut self, idempotency: OperationIdempotency) -> Self {
        if self.retry_safety == OperationIdempotency::Unknown.as_str() {
            self.retry_safety = idempotency.as_str().into();
        }
        self
    }
}

/// One model call made inside a tool. The runtime-only `call_kind` keeps
/// media inference distinct from other internal tool calls even when a
/// composite tool (such as `files`) owns both kinds of work.
#[derive(Debug, Clone)]
pub struct ToolLlmUsage {
    pub call_kind: &'static str,
    pub role: haven_common::config::EndpointRole,
    pub usage: haven_llm::Usage,
    pub model: Option<String>,
    pub duration_ms: Option<u64>,
}

/// Side-channel signals a tool declares through its result. Declared by the
/// tool itself (via `Tool::signals`) so the ReAct loop does not need to know
/// which tool names carry which signals.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ToolSignals {
    /// The `ask` tool's question: when present, the loop pauses the session
    /// and waits for the user's reply.
    pub ask_question: Option<String>,
    /// Quick-reply options for the pending `ask`, surfaced as buttons.
    pub ask_options: Vec<String>,
    /// The `notify` tool's toast title (defaults to "Haven").
    pub notify_title: Option<String>,
    /// The `notify` tool's toast body.
    pub notify_body: Option<String>,
}

/// Scheduling contract for a tool call inside one assistant batch.
///
/// `ReadOnly` calls may overlap without a resource key. `SharedResource`
/// calls may overlap with other readers of the same key but serialize with a
/// writer for that key. `Resource` calls serialize with all calls using the
/// same key. `Exclusive` calls serialize with the whole batch. The
/// conservative default is exclusive unless a tool explicitly opts into a
/// less restrictive mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolConcurrency {
    ReadOnly,
    SharedResource(String),
    Resource(String),
    Exclusive,
}

/// The scope in which a typed operation is allowed to observe or mutate
/// state. This is deliberately separate from the LLM-facing tool name: one
/// aggregate tool can contain both global and session-scoped operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOperationScope {
    Global,
    Session,
}

impl ToolOperationScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Session => "session",
        }
    }
}

/// How an operation responds to cancellation and the outer timeout. The
/// execution wrapper still owns the actual token; this metadata tells the
/// safety, retry, and audit layers what can be concluded after cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCancellationPolicy {
    Cooperative,
    Terminating,
    Unknown,
}

/// Complete runtime policy for one typed operation.
///
/// `Tool` remains the provider-facing object-safe boundary and therefore
/// accepts JSON at its edge. Implementations behind that boundary use
/// [`TypedToolOperation`] and expose this metadata from the same typed
/// operation that parses and executes the arguments. This prevents risk,
/// retry, concurrency, and timeout decisions from drifting into unrelated
/// string matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOperationMetadata {
    pub capability: &'static str,
    pub operation: &'static str,
    pub scope: ToolOperationScope,
    pub risk_level: RiskLevel,
    pub idempotency: OperationIdempotency,
    pub cancellation: ToolCancellationPolicy,
    pub timeout_secs: u64,
    pub concurrency: ToolConcurrency,
}

/// A typed runtime operation. The only JSON conversion is performed by
/// [`TypedToolAdapter`] at the provider boundary; the operation itself sees
/// typed arguments, returns a typed output, and reports a typed error.
#[async_trait::async_trait]
pub trait TypedToolOperation: Send + Sync {
    type Args: serde::de::DeserializeOwned + Send;
    type Output: serde::Serialize + Send;
    type Error: std::fmt::Display + Send + Sync + 'static;

    fn metadata(&self, args: &Self::Args) -> ToolOperationMetadata;
    fn default_metadata(&self) -> ToolOperationMetadata;
    fn input_schema(&self) -> Value;

    /// Structured side-channel signals produced by the typed operation.
    /// The adapter forwards these without making the provider-facing layer
    /// inspect operation names or output JSON.
    fn signals(&self, _output: &Value) -> ToolSignals {
        ToolSignals::default()
    }

    /// Map a domain error to the complete recovery contract at the typed
    /// operation boundary. Implementations should override this whenever the
    /// error can be transient, permission-related, or have an unknown
    /// side-effect outcome. The default is fail-closed.
    fn error_metadata(&self, _error: &Self::Error) -> ToolErrorMetadata {
        ToolErrorMetadata::other()
    }

    async fn execute_typed(
        &self,
        args: Self::Args,
        cancel: CancellationToken,
    ) -> Result<Self::Output, Self::Error>;
}

/// Per-session side effects a tool declares through its result. The session
/// executor applies them (registering MCP adapters, attaching
/// background actions) without hard-coding tool names, so a new tool that needs
/// a side effect declares it here instead of adding a name check in the
/// executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolRegistration {
    /// Load an MCP server (by name) for the current session.
    McpServer(String),
    /// Attach a background action (an action of kind `action`) to the current
    /// session (end/rollback cleanup).
    Action(String),
}

impl ToolResult {
    /// Project the common result metadata without changing the operation's
    /// payload shape. Asset handles are collected from the canonical top-level
    /// and nested media/assets fields, never from host paths.
    pub fn envelope(&self, idempotency: OperationIdempotency) -> ToolResultEnvelope {
        let mut envelope = ToolResultEnvelope::from_parts(
            self.outcome,
            self.error_class,
            self.retryability,
            idempotency,
        );
        if let Some(value) = self.output.get("retry_safety").and_then(Value::as_str) {
            envelope.retry_safety = value.into();
        }
        envelope.verification_hint = self
            .output
            .get("verification_hint")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        envelope.next_action = self
            .output
            .get("next_action")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        collect_asset_ids(&self.output, &mut envelope.assets);
        envelope
    }

    /// Build a successful result while keeping the transport-level truncation
    /// bit in sync with the structured output.  Builtin tools often include a
    /// `truncated` field in their JSON so the model can see it; callers must
    /// also set the top-level bit because the executor and UI use that field
    /// for observation compaction and follow-up decisions.
    pub fn from_output(mut output: Value, truncated: bool) -> Self {
        let truncated = truncated
            || output
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        if truncated {
            if let Some(object) = output.as_object_mut() {
                object.insert("truncated".into(), Value::Bool(true));
            }
            Self::truncated(output)
        } else {
            Self::ok(output)
        }
    }

    pub fn ok(output: Value) -> Self {
        Self {
            success: true,
            output,
            error: None,
            error_class: None,
            retryability: ToolRetryability::Unknown,
            truncated: false,
            outcome: ToolExecutionOutcome::Succeeded,
            attempts: 1,
            signals: ToolSignals::default(),
            llm_usage: Vec::new(),
        }
    }

    pub fn truncated(output: Value) -> Self {
        Self {
            success: true,
            output,
            error: None,
            error_class: None,
            retryability: ToolRetryability::Unknown,
            truncated: true,
            outcome: ToolExecutionOutcome::Succeeded,
            attempts: 1,
            signals: ToolSignals::default(),
            llm_usage: Vec::new(),
        }
    }

    pub fn failed(output: Value, error: impl Into<String>) -> Self {
        Self::failed_with_class(output, error, ToolErrorClass::Other)
    }

    pub fn failed_with_class(
        output: Value,
        error: impl Into<String>,
        error_class: ToolErrorClass,
    ) -> Self {
        let retryability = match error_class {
            ToolErrorClass::Transient => ToolRetryability::Retryable,
            ToolErrorClass::UnknownOutcome | ToolErrorClass::SideEffectMayHaveHappened => {
                ToolRetryability::Unknown
            }
            ToolErrorClass::Validation | ToolErrorClass::Permission | ToolErrorClass::Other => {
                ToolRetryability::NotRetryable
            }
        };
        Self::failed_with_metadata(
            output,
            error,
            ToolErrorMetadata {
                class: error_class,
                outcome: ToolExecutionOutcome::Failed,
                retryability,
            },
        )
    }

    pub fn failed_with_metadata(
        output: Value,
        error: impl Into<String>,
        metadata: ToolErrorMetadata,
    ) -> Self {
        Self {
            success: false,
            output,
            error: Some(error.into()),
            error_class: Some(metadata.class),
            retryability: metadata.retryability,
            truncated: false,
            outcome: metadata.outcome,
            attempts: 1,
            signals: ToolSignals::default(),
            llm_usage: Vec::new(),
        }
    }

    pub fn cancelled(error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: Value::Null,
            error: Some(error.into()),
            error_class: Some(ToolErrorClass::UnknownOutcome),
            retryability: ToolRetryability::Unknown,
            truncated: false,
            outcome: ToolExecutionOutcome::Cancelled,
            attempts: 1,
            signals: ToolSignals::default(),
            llm_usage: Vec::new(),
        }
    }

    pub fn timed_out(outcome: ToolExecutionOutcome, error: impl Into<String>) -> Self {
        debug_assert!(matches!(
            outcome,
            ToolExecutionOutcome::TimedOutAndTerminated | ToolExecutionOutcome::TimedOutUnknown
        ));
        Self {
            success: false,
            output: Value::Null,
            error: Some(error.into()),
            error_class: Some(match outcome {
                ToolExecutionOutcome::TimedOutUnknown => ToolErrorClass::UnknownOutcome,
                ToolExecutionOutcome::TimedOutAndTerminated => ToolErrorClass::Transient,
                _ => ToolErrorClass::Other,
            }),
            retryability: match outcome {
                ToolExecutionOutcome::TimedOutAndTerminated => ToolRetryability::Retryable,
                ToolExecutionOutcome::TimedOutUnknown => ToolRetryability::Unknown,
                _ => ToolRetryability::Unknown,
            },
            truncated: false,
            outcome,
            attempts: 1,
            signals: ToolSignals::default(),
            llm_usage: Vec::new(),
        }
    }

    fn retryability_is_unknown(value: &ToolRetryability) -> bool {
        matches!(value, ToolRetryability::Unknown)
    }

    /// Plain-text summary of the result: the serialized output on success,
    /// the error message on failure. Plain-string outputs are returned as-is
    /// (unquoted) so a tool returning "text" reads like text, not JSON; only
    /// structured outputs are serialized. On failure with an empty/absent
    /// error (e.g. a shell command that exited non-zero without stderr), fall
    /// back to the serialized output so the result is never an empty string —
    /// an empty observation looks to the model and the chat like the tool
    /// never returned anything. Callers that need truncation apply it on top.
    pub fn summary_text(&self) -> String {
        if self.success {
            match &self.output {
                Value::String(s) => s.clone(),
                _ => serde_json::to_string(&self.output).unwrap_or_else(|_| "success".into()),
            }
        } else {
            match self.error.as_deref() {
                Some(e) if !e.trim().is_empty() => e.to_string(),
                _ => {
                    let out = serde_json::to_string(&self.output).unwrap_or_default();
                    if out.is_empty() || out == "null" {
                        "unknown failure".into()
                    } else {
                        out
                    }
                }
            }
        }
    }

    /// Build the bounded observation shared by canonical, history and step
    /// projections. The signal fields are intentionally not derived from this
    /// string; callers must read `signals` before applying the cap.
    pub fn observation_text(&self, max_chars: usize) -> String {
        let text = self
            .structured_observation()
            .map(|value| bounded_json_object(value, max_chars))
            .unwrap_or_else(|| self.summary_text());
        let char_count = text.chars().count();
        if char_count <= max_chars {
            return text;
        }
        if max_chars == 0 {
            return String::new();
        }

        // The marker is part of the observation budget. Appending it after a
        // `max_chars` prefix made every result exceed the configured cap and
        // compounded context pressure across a tool batch.
        let mut prefix_chars = max_chars.saturating_sub(32);
        let mut marker = String::new();
        for _ in 0..4 {
            let omitted = char_count.saturating_sub(prefix_chars);
            marker = format!("[... truncated {omitted} chars omitted]");
            let next_prefix = max_chars.saturating_sub(marker.chars().count());
            if next_prefix == prefix_chars {
                break;
            }
            prefix_chars = next_prefix;
        }
        if marker.chars().count() > max_chars {
            return text.chars().take(max_chars).collect();
        }
        let prefix: String = text.chars().take(prefix_chars).collect();
        format!("{prefix}{marker}")
    }

    /// Keep structured recovery data together when an observation is capped.
    /// The previous string-prefix truncation could cut away `next_offset`,
    /// `next_start_line`, `path`, or `hint` while retaining a large body.
    fn structured_observation(&self) -> Option<Value> {
        if self.success && !self.output.is_object() {
            return None;
        }
        let mut object = Map::new();

        if !self.success {
            object.insert("success".into(), Value::Bool(false));
        }
        if !self.success
            && let Some(error) = self.error.as_deref().filter(|error| !error.is_empty())
        {
            object.insert("error".into(), Value::String(error.into()));
        }
        if !self.success {
            if let Some(error_class) = self.error_class {
                object.insert(
                    "error_class".into(),
                    Value::String(error_class.as_str().into()),
                );
            }
            object.insert(
                "retryability".into(),
                Value::String(self.retryability.as_str().into()),
            );
        }
        if !self.success && self.outcome != ToolExecutionOutcome::Failed {
            object.insert(
                "outcome".into(),
                Value::String(self.outcome.as_str().into()),
            );
        }

        // Recovery and routing fields are emitted first. JSON object order is
        // not semantic, but it matters to a bounded model observation because
        // the tail is the part most likely to be dropped.
        const PRIORITY_KEYS: &[&str] = &[
            "success",
            "error",
            "error_class",
            "retryability",
            "outcome",
            "asset_id",
            "notes",
            "path",
            "root",
            "next_offset",
            "next_start_line",
            "hint",
            "retry_safety",
            "truncated",
            "action_id",
            "operation",
            "status",
            "available",
        ];
        for key in PRIORITY_KEYS {
            if let Some(value) = self.output.get(*key) {
                object.entry(*key).or_insert_with(|| value.clone());
            }
        }
        if let Some(output) = self.output.as_object() {
            for (key, value) in output {
                object.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }

        (!object.is_empty()).then_some(Value::Object(object))
    }
}

fn collect_asset_ids(value: &Value, assets: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if let Some(asset_id) = object.get("asset_id").and_then(Value::as_str)
                && !assets.iter().any(|existing| existing == asset_id)
            {
                assets.push(asset_id.into());
            }
            for key in ["assets", "media"] {
                if let Some(value) = object.get(key) {
                    collect_asset_ids(value, assets);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_asset_ids(value, assets);
            }
        }
        _ => {}
    }
}

const STRUCTURED_PRIORITY_KEYS: &[&str] = &[
    "success",
    "error",
    "error_class",
    "retryability",
    "outcome",
    "asset_id",
    "notes",
    "path",
    "root",
    "next_offset",
    "next_start_line",
    "hint",
    "retry_safety",
    "truncated",
    "action_id",
    "operation",
    "status",
    "available",
];

fn bounded_json_object(value: Value, max_chars: usize) -> String {
    let Value::Object(mut object) = value else {
        return serde_json::to_string(&value).unwrap_or_default();
    };
    let full = ordered_json_object(&object);
    if full.chars().count() <= max_chars {
        return full;
    }
    if max_chars == 0 {
        return String::new();
    }

    // Drop large body collections before touching recovery metadata. This is
    // deterministic and turns the cap into a useful model view rather than a
    // byte slice through an arbitrary JSON token.
    let keys: Vec<String> = object.keys().cloned().collect();
    for key in keys.iter().rev() {
        if !STRUCTURED_PRIORITY_KEYS.contains(&key.as_str())
            && !object.get(key).is_some_and(Value::is_string)
            && ordered_json_object(&object).chars().count() > max_chars
        {
            object.remove(key);
        }
    }

    // Shrink string payloads (usually content/stdout/stderr/summary) while
    // retaining every priority key. A bounded marker makes the loss explicit.
    let string_keys: Vec<String> = object
        .iter()
        .filter_map(|(key, value)| {
            (value.is_string() && !STRUCTURED_PRIORITY_KEYS.contains(&key.as_str()))
                .then_some(key.clone())
        })
        .collect();
    for key in string_keys.iter().rev() {
        let Some(Value::String(original)) = object.get(key).cloned() else {
            continue;
        };
        if ordered_json_object(&object).chars().count() <= max_chars {
            break;
        }
        let mut low = 0usize;
        let mut high = original.chars().count();
        let mut best = String::new();
        while low <= high {
            let mid = low + (high - low) / 2;
            let candidate = bounded_string(&original, mid);
            object.insert(key.clone(), Value::String(candidate.clone()));
            let fits = ordered_json_object(&object).chars().count() <= max_chars;
            if fits {
                best = candidate;
                low = mid.saturating_add(1);
            } else {
                if mid == 0 {
                    break;
                }
                high = mid - 1;
            }
        }
        object.insert(key.clone(), Value::String(best));
    }

    let rendered = ordered_json_object(&object);
    if rendered.chars().count() <= max_chars {
        return rendered;
    }
    // Extremely small budgets cannot hold all field names and values. Keep a
    // bounded fallback; normal observation budgets preserve recovery keys.
    rendered.chars().take(max_chars).collect()
}

/// Serialize a JSON object with recovery fields first. `serde_json::Map`
/// defaults to a sorted map in this workspace, so inserting priority fields
/// first is not sufficient to protect them from prefix-based observation
/// truncation; the model-facing bounded view needs an explicit key order.
fn ordered_json_object(object: &Map<String, Value>) -> String {
    let mut keys = Vec::with_capacity(object.len());
    for key in STRUCTURED_PRIORITY_KEYS {
        if object.contains_key(*key) {
            keys.push((*key).to_owned());
        }
    }
    for key in object.keys() {
        if !STRUCTURED_PRIORITY_KEYS.contains(&key.as_str()) {
            keys.push(key.clone());
        }
    }

    let mut rendered = String::from("{");
    for (index, key) in keys.iter().enumerate() {
        if index > 0 {
            rendered.push(',');
        }
        rendered.push_str(&serde_json::to_string(key).unwrap_or_default());
        rendered.push(':');
        rendered.push_str(
            &serde_json::to_string(object.get(key).expect("key came from the object"))
                .unwrap_or_default(),
        );
    }
    rendered.push('}');
    rendered
}

fn bounded_string(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    if max_chars <= 1 {
        return "…".chars().take(max_chars).collect();
    }
    let mut output: String = value.chars().take(max_chars - 1).collect();
    output.push('…');
    output
}

fn default_attempts() -> u32 {
    1
}

/// Extract the `ask` signal from a tool result's structured output: the
/// question text and optional suggested answers. `(None, vec![])` when the
/// output does not carry a question. The signal must be read BEFORE any
/// truncation: parsing truncated text would yield invalid JSON when the
/// output exceeds the observation budget, silently dropping the question
/// and never pausing the session.
pub fn extract_ask_signal(output: &Value) -> (Option<String>, Vec<String>) {
    let question = output
        .get("question")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let options = output
        .get("options")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|o| o.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    (question, options)
}

/// Extract the `notify` signal from a tool result's structured output: the
/// notification title (default "Haven") and body. `(None, None)` when the
/// output does not request a notification.
pub fn extract_notify_signal(output: &Value) -> (Option<String>, Option<String>) {
    if output.get("notify").and_then(|v| v.as_bool()) != Some(true) {
        return (None, None);
    }
    let title = output
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Haven")
        .to_string();
    let body = output
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    (Some(title), Some(body))
}

/// Whether an action should be hidden from the chat UI. `ask` must never be
/// silent: hiding the question while the session pauses for an answer would
/// leave the user waiting on a question they can't see.
pub fn is_silent_action(tool_name: &str, input: &Value) -> bool {
    tool_name != "ask"
        && input
            .get("silent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
}

fn tool_source_for_name(name: &str) -> ToolSource {
    if name.starts_with("skill__") {
        ToolSource::Skill
    } else if name.starts_with("mcp__") {
        ToolSource::Mcp
    } else {
        ToolSource::Builtin
    }
}

fn display_source_for_name(name: &str) -> ToolSource {
    match name {
        "load_skill" => ToolSource::Skill,
        "load_mcp" => ToolSource::Mcp,
        _ => tool_source_for_name(name),
    }
}

fn default_tool_root(name: &str, represented_source: ToolSource) -> String {
    match represented_source {
        ToolSource::Skill => "skills".into(),
        ToolSource::Mcp => "mcp".into(),
        ToolSource::Builtin => name.split('.').next().unwrap_or(name).into(),
    }
}

fn default_tool_operation(
    name: &str,
    root: &str,
    represented_source: ToolSource,
) -> Option<String> {
    match represented_source {
        ToolSource::Skill => name
            .strip_prefix("skill__")
            .or_else(|| (name == "load_skill").then_some("load"))
            .map(ToString::to_string),
        ToolSource::Mcp => name
            .strip_prefix("mcp__")
            .or_else(|| (name == "load_mcp").then_some("load"))
            .map(ToString::to_string),
        ToolSource::Builtin => name
            .strip_prefix(&format!("{root}."))
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
    }
}

fn default_tool_label(name: &str) -> String {
    let label = match name {
        "ask" => "询问用户",
        "notify" => "发送通知",
        "load_builtin" => "加载内置工具",
        "load_skill" => "加载 Skill",
        "load_mcp" => "加载 MCP",
        "tool_catalog" => "工具目录",
        "files" => "文件与搜索",
        "media" => "媒体",
        "http" => "HTTP 请求",
        "system" => "系统与桌面",
        "shell" => "终端输出",
        "haven" => "Haven 管理与会话工具",
        "memory" => "记忆",
        "agent" => "Agent 协作",
        "process" => "进程",
        "clipboard" => "剪贴板",
        "input" => "输入控制",
        "window" => "窗口与屏幕",
        "preferences" => "会话偏好",
        "checklist" => "检查清单",
        "actions" => "后台任务",
        "schedule" => "定时任务",
        "web_search" => "联网搜索",
        _ => name
            .strip_prefix("skill__")
            .or_else(|| name.strip_prefix("mcp__"))
            .unwrap_or(name),
    };
    label.into()
}

pub(crate) fn default_root_presentation(
    root: &str,
    represented_source: ToolSource,
) -> ToolRootPresentation {
    let (label, icon) = match represented_source {
        ToolSource::Skill => ("Skills", "sparkles"),
        ToolSource::Mcp => ("MCP", "network"),
        ToolSource::Builtin => match root {
            "files" => ("文件", "folder"),
            "media" => ("媒体", "image"),
            "system" => ("系统", "settings"),
            "process" => ("进程", "activity"),
            "clipboard" => ("剪贴板", "clipboard"),
            "input" => ("输入控制", "keyboard"),
            "window" => ("窗口与屏幕", "monitor"),
            "memory" => ("记忆", "memory"),
            "agent" => ("Agent 协作", "users"),
            "actions" => ("后台任务", "clock"),
            "schedule" => ("定时任务", "bell"),
            "preferences" => ("会话偏好", "settings"),
            "checklist" => ("检查清单", "checklist"),
            "haven" => ("Haven", "settings"),
            _ => (root, "tools"),
        },
    };
    ToolRootPresentation {
        label: label.into(),
        description: format!("{label}相关能力"),
        icon: icon.into(),
    }
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn risk_level(&self, input: &Value) -> RiskLevel;

    /// Canonical policy for the concrete invocation. All catalog and runtime
    /// consumers should use this method instead of independently combining
    /// risk, permission, idempotency, scope and concurrency fields.
    fn operation_policy(&self, input: &Value) -> OperationPolicy {
        let name = self.name();
        let risk_level = self.risk_level(input);
        let concurrency = self.concurrency(input);
        let (effect, data_sensitivity, network_access) =
            operation_attributes_for_input(&name, input, concurrency.clone());
        OperationPolicy {
            risk_level,
            capability: permission_key(&name, &self.authorization_input(input)).into(),
            confirmation: if risk_level >= RiskLevel::Critical {
                ConfirmationRequirement::Required
            } else if risk_level == RiskLevel::Safe {
                ConfirmationRequirement::None
            } else {
                ConfirmationRequirement::SecurityPolicy
            },
            idempotency: self.idempotency(input),
            scope: self.operation_scope(input),
            concurrency,
            effect,
            data_sensitivity,
            network_access,
        }
    }

    /// Backend-owned metadata for prompt/UI/catalog consumers. It is not
    /// serialized into provider-facing tool definitions.
    fn tool_manifest(&self) -> ToolManifest {
        let name = self.name();
        let source = tool_source_for_name(&name);
        let represented_source = self.represented_source();
        let root = default_tool_root(&name, represented_source);
        let operation = default_tool_operation(&name, &root, represented_source);
        let description = self.description();
        let policy = self.operation_policy(&Value::Object(Default::default()));
        ToolManifest {
            identity: ToolIdentity {
                source,
                catalog_group: self.catalog_group(),
                root: root.clone(),
                operation,
                stable_name: name.clone(),
            },
            model: ToolModel {
                name: name.clone(),
                description: description.clone(),
                input_schema: self.input_schema(),
            },
            availability: ToolAvailability {
                requires_permission: policy.risk_level >= RiskLevel::Medium,
                ..ToolAvailability::default()
            },
            policy: policy.to_catalog_policy(),
            presentation: ToolPresentation {
                label: default_tool_label(&name),
                renderer: root.clone(),
                icon: "tools".into(),
                represented_source,
            },
            root_presentation: default_root_presentation(&root, represented_source),
            prompt: ToolPrompt {
                when_to_use: description,
                when_not_to_use: "Use a narrower operation when one is available.".into(),
                key_operations: vec![name],
            },
        }
    }

    /// Source represented by the UI card. Activation tools execute in Haven
    /// but represent the capability family they make available.
    fn represented_source(&self) -> ToolSource {
        display_source_for_name(&self.name())
    }

    /// Canonical input used by the authorization layer. Operation views add
    /// their fixed discriminator here so disabled-operation and path rules
    /// see the same operation that execution and risk policy see.
    fn authorization_input(&self, input: &Value) -> Value {
        input.clone()
    }
    /// Retry policy is an operation property, not a safety-risk property.
    /// The default is conservative because an unknown operation may have
    /// performed an external side effect before returning an error.
    fn idempotency(&self, _input: &Value) -> OperationIdempotency {
        OperationIdempotency::Unknown
    }

    /// Scope of the operation represented by this invocation.  The agent
    /// records this beside the outcome so an unknown result can be reviewed
    /// with enough context to decide whether a replay is safe.
    fn operation_scope(&self, _input: &Value) -> ToolOperationScope {
        ToolOperationScope::Session
    }

    /// Whether an outer timeout can establish that this invocation stopped.
    /// Tools backed by child processes or remote servers should return
    /// `TimedOutUnknown` unless they can prove termination.
    fn timeout_outcome(&self) -> ToolExecutionOutcome {
        ToolExecutionOutcome::TimedOutUnknown
    }

    /// Convert an implementation error into structured recovery metadata.
    /// This hook is intentionally policy-bearing; the manager must never
    /// infer retry or outcome semantics by scanning the diagnostic string.
    fn error_metadata(&self, error: &anyhow::Error) -> ToolErrorMetadata {
        error
            .downcast_ref::<StructuredToolError>()
            .map(StructuredToolError::metadata)
            .unwrap_or_else(ToolErrorMetadata::other)
    }

    /// Intrinsic retry budget used when no per-tool configuration exists.
    fn default_max_retries(&self) -> u32 {
        0
    }

    fn default_retry_backoff_secs(&self) -> u64 {
        0
    }
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult>;
    fn input_schema(&self) -> Value;

    /// Declare how calls from one assistant batch may overlap. Tools are
    /// exclusive by default; read-only/resource contracts must be explicit so
    /// a non-idempotent implementation cannot accidentally run in parallel.
    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        let _ = input;
        ToolConcurrency::Exclusive
    }

    /// Canonical structured definition of this tool (name / description /
    /// schema / default risk). `ToolDef` is the unified abstraction the
    /// registry and manager surface; per-call risk is still refined via
    /// `risk_level(input)`. Tools may override to e.g. memoize the schema.
    fn tool_def(&self) -> ToolDef {
        let manifest = self.tool_manifest();
        ToolDef::new(
            self.name(),
            self.description(),
            self.input_schema(),
            self.risk_level(&Value::Object(Default::default())),
        )
        .with_catalog_group(self.catalog_group())
        .with_retry_safety(
            self.idempotency(&Value::Object(Default::default()))
                .tool_retry_safety(),
        )
        .with_manifest(manifest)
    }

    /// High-level catalog grouping shared by the Agent prompt and UI. The
    /// default keeps test/custom tools valid without inventing a category.
    fn catalog_group(&self) -> ToolCatalogGroup {
        ToolCatalogGroup::Other
    }

    fn default_timeout_secs(&self) -> u64 {
        30
    }

    /// Per-call timeout. Defaults to [`Self::default_timeout_secs`]; tools with
    /// op-dependent waits (e.g. `agent` request) override this.
    fn timeout_secs_for(&self, _input: &Value) -> u64 {
        self.default_timeout_secs()
    }

    /// Whether this tool needs the private `_session_id` input field injected
    /// before execution (e.g. `schedule`/`actions` scope to the current session).
    /// The id is injected after the LLM-facing input was captured, so it
    /// never reaches the tool schema, the step history, or the LLM.
    fn requires_session_id(&self) -> bool {
        false
    }

    /// Whether this tool streams live output to the chat card while running
    /// (`agent:tool_output`). When true, `_session_id` and `_step_id` are
    /// injected privately so the tool can key preview events to the card.
    fn supports_live_output(&self) -> bool {
        false
    }

    /// Side-channel signals carried by this tool's result (`ask` question /
    /// `notify` toast). Parsed from the structured output by the tool itself,
    /// BEFORE the loop truncates the observation text.
    fn signals(&self, output: &Value) -> ToolSignals {
        let _ = output;
        ToolSignals::default()
    }

    /// Per-session side effects to apply after a successful execution
    /// (skill/MCP adapters, background-action attachment).
    fn registrations(&self, output: &Value) -> Vec<ToolRegistration> {
        let _ = output;
        Vec::new()
    }

    fn validate_input(&self, input: &Value) -> anyhow::Result<()> {
        let schema = self.input_schema();
        if schema.is_null() || schema == serde_json::Value::Null {
            return Ok(());
        }
        let validator = jsonschema::validator_for(&schema)
            .map_err(|e| anyhow::anyhow!("invalid tool schema for '{}': {}", self.name(), e))?;
        let errors: Vec<_> = validator.iter_errors(input).collect();
        if !errors.is_empty() {
            // Make the most common mistakes loud: missing required fields are
            // listed up front (with allowed enum values when the schema
            // declares them) instead of being buried in generic messages like
            // "required property 'operation' was not present".
            let mut missing: Vec<String> = Vec::new();
            let mut rest: Vec<String> = Vec::new();
            for e in errors {
                if let jsonschema::error::ValidationErrorKind::Required { property } = e.kind()
                    && let Some(s) = property.as_str()
                {
                    missing.push(s.to_string());
                    continue;
                }
                rest.push(e.to_string());
            }
            let mut msg = format!("input validation failed for '{}'", self.name());
            if !missing.is_empty() {
                msg.push_str(&format!(
                    ": MISSING REQUIRED FIELD(S): {}",
                    missing.join(", ")
                ));
                if let Some(props) = schema.get("properties") {
                    let hints: Vec<String> = missing
                        .iter()
                        .filter_map(|m| {
                            let prop = props.get(m)?;
                            let vals = prop.get("enum")?.as_array()?;
                            let vals: Vec<&str> = vals.iter().filter_map(|v| v.as_str()).collect();
                            if vals.is_empty() {
                                None
                            } else {
                                Some(format!("{m} must be one of: {}", vals.join(", ")))
                            }
                        })
                        .collect();
                    if !hints.is_empty() {
                        msg.push_str(&format!("\nAllowed values: {}", hints.join("; ")));
                    }
                }
            }
            if !rest.is_empty() {
                msg.push_str(&format!("\nOther: {}", rest.join("; ")));
            }
            anyhow::bail!(msg);
        }
        Ok(())
    }

    async fn execute_with_timeout(
        &self,
        input: Value,
        cancel: CancellationToken,
        timeout_secs: u64,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled(format!(
                "tool '{}' cancelled before execution",
                self.name()
            )));
        }
        let execution_cancel = cancel.child_token();
        tokio::select! {
            result = self.execute(input, execution_cancel.clone()) => result,
            _ = cancel.cancelled() => {
                execution_cancel.cancel();
                Ok(ToolResult::cancelled(format!("tool '{}' cancelled", self.name())))
            }
            _ = tokio::time::sleep(Duration::from_secs(timeout_secs)) => {
                execution_cancel.cancel();
                Ok(ToolResult::timed_out(
                    self.timeout_outcome(),
                    format!("tool '{}' timed out after {}s", self.name(), timeout_secs),
                ))
            }
        }
    }
}

/// Object-safe provider adapter for one typed operation group. It performs
/// JSON parsing only at the LLM/provider edge and delegates all policy and
/// execution decisions to the typed operation.
pub struct TypedToolAdapter<O> {
    name: String,
    description: String,
    operation: O,
    catalog_group: ToolCatalogGroup,
}

impl<O> TypedToolAdapter<O> {
    pub fn new(name: impl Into<String>, description: impl Into<String>, operation: O) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            operation,
            catalog_group: ToolCatalogGroup::Other,
        }
    }

    pub fn with_catalog_group(mut self, catalog_group: ToolCatalogGroup) -> Self {
        self.catalog_group = catalog_group;
        self
    }

    pub fn operation(&self) -> &O {
        &self.operation
    }
}

#[async_trait::async_trait]
impl<O> Tool for TypedToolAdapter<O>
where
    O: TypedToolOperation,
{
    fn name(&self) -> String {
        self.name.clone()
    }

    fn description(&self) -> String {
        self.description.clone()
    }

    fn catalog_group(&self) -> ToolCatalogGroup {
        self.catalog_group
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        serde_json::from_value::<O::Args>(input.clone())
            .ok()
            .map(|args| self.operation.metadata(&args).risk_level)
            // Authorization can run before argument validation. Use the
            // operation's declared baseline for malformed input so the
            // adapter preserves the tool's public risk contract while the
            // execution path still rejects the invalid arguments.
            .unwrap_or_else(|| self.operation.default_metadata().risk_level)
    }

    fn idempotency(&self, input: &Value) -> OperationIdempotency {
        serde_json::from_value::<O::Args>(input.clone())
            .ok()
            .map(|args| self.operation.metadata(&args).idempotency)
            .unwrap_or(OperationIdempotency::Unknown)
    }

    fn operation_scope(&self, input: &Value) -> ToolOperationScope {
        serde_json::from_value::<O::Args>(input.clone())
            .ok()
            .map(|args| self.operation.metadata(&args).scope)
            .unwrap_or(ToolOperationScope::Session)
    }

    fn timeout_outcome(&self) -> ToolExecutionOutcome {
        match self.operation.default_metadata().cancellation {
            ToolCancellationPolicy::Terminating => ToolExecutionOutcome::TimedOutAndTerminated,
            ToolCancellationPolicy::Cooperative | ToolCancellationPolicy::Unknown => {
                ToolExecutionOutcome::TimedOutUnknown
            }
        }
    }

    fn input_schema(&self) -> Value {
        self.operation.input_schema()
    }

    fn signals(&self, output: &Value) -> ToolSignals {
        self.operation.signals(output)
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        serde_json::from_value::<O::Args>(input.clone())
            .ok()
            .map(|args| self.operation.metadata(&args).concurrency)
            .unwrap_or(ToolConcurrency::Exclusive)
    }

    fn default_timeout_secs(&self) -> u64 {
        self.operation.default_metadata().timeout_secs
    }

    fn timeout_secs_for(&self, input: &Value) -> u64 {
        serde_json::from_value::<O::Args>(input.clone())
            .ok()
            .map(|args| self.operation.metadata(&args).timeout_secs)
            .unwrap_or_else(|| self.default_timeout_secs())
    }

    fn error_metadata(&self, error: &anyhow::Error) -> ToolErrorMetadata {
        error
            .downcast_ref::<StructuredToolError>()
            .map(StructuredToolError::metadata)
            .unwrap_or_else(ToolErrorMetadata::other)
    }

    fn tool_def(&self) -> ToolDef {
        let metadata = self.operation.default_metadata();
        let manifest = self.tool_manifest();
        ToolDef::new(
            self.name(),
            self.description(),
            self.input_schema(),
            metadata.risk_level,
        )
        .with_catalog_group(self.catalog_group)
        .with_retry_safety(metadata.idempotency.tool_retry_safety())
        .with_manifest(manifest)
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let args = parse_tool_input::<O::Args>(&self.name(), input.clone()).map_err(|error| {
            anyhow::Error::new(StructuredToolError::new(
                error.to_string(),
                ToolErrorMetadata::validation(),
            ))
        })?;
        // Serde intentionally accepts extra fields for unit variants of an
        // internally tagged enum. Validate after parsing so missing/typed
        // field diagnostics retain serde's precise message while unknown
        // fields are still rejected before the operation runs.
        self.validate_input(&input).map_err(|error| {
            anyhow::Error::new(StructuredToolError::new(
                error.to_string(),
                ToolErrorMetadata::validation(),
            ))
        })?;
        let output = self
            .operation
            .execute_typed(args, cancel)
            .await
            .map_err(|error| {
                anyhow::Error::new(StructuredToolError::new(
                    error.to_string(),
                    self.operation.error_metadata(&error),
                ))
            })?;
        let output = serde_json::to_value(output)
            .map_err(|error| anyhow::anyhow!("serialize typed tool output: {error}"))?;
        Ok(ToolResult::ok(output))
    }
}

/// Convert LLM JSON input into a builtin tool's typed params (entry ② of the
/// builtin two-entry contract: ① `XxxTool::run(&XxxParams)` native call with
/// zero serialization, ② `Tool::execute(Value)` JSON entry that converts and
/// validates here, then lands in the same `run`). Serde reports missing
/// fields, wrong types, and unknown enum variants with their allowed values.
pub fn parse_tool_input<T: serde::de::DeserializeOwned>(
    tool_name: &str,
    input: Value,
) -> anyhow::Result<T> {
    serde_json::from_value(input)
        .map_err(|e| anyhow::anyhow!("invalid '{}' input: {}", tool_name, e))
}

pub type ToolBox = Arc<dyn Tool>;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn test_tool_result_ok() {
        let result = ToolResult::ok(json!({"status": "done"}));
        assert!(result.success);
        assert!(!result.truncated);
        assert_eq!(result.output, json!({"status": "done"}));
        assert!(result.error.is_none());
    }

    #[test]
    fn test_tool_result_truncated() {
        let result = ToolResult::truncated(json!({"content": "partial"}));
        assert!(result.success);
        assert!(result.truncated);
        assert_eq!(result.output, json!({"content": "partial"}));
        assert!(result.error.is_none());
    }

    #[test]
    fn test_tool_result_from_output_keeps_transport_flag_in_sync() {
        let truncated = ToolResult::from_output(json!({"truncated": true}), true);
        assert!(truncated.success);
        assert!(truncated.truncated);

        let inferred = ToolResult::from_output(json!({"truncated": true}), false);
        assert!(inferred.truncated);

        let annotated = ToolResult::from_output(json!({"status": "partial"}), true);
        assert_eq!(annotated.output["truncated"], true);

        let complete = ToolResult::from_output(json!({"truncated": false}), false);
        assert!(complete.success);
        assert!(!complete.truncated);
    }

    #[test]
    fn result_envelope_keeps_recovery_metadata_and_asset_handles() {
        let result = ToolResult::ok(json!({
            "asset_id": "asset-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "verification_hint": "Check the generated file.",
            "next_action": "Use media.inspect with the asset_id.",
            "media": {"asset_id": "asset-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},
        }));
        let envelope = result.envelope(OperationIdempotency::Idempotent);
        assert_eq!(envelope.outcome, ToolExecutionOutcome::Succeeded);
        assert_eq!(envelope.retry_safety, "idempotent");
        assert_eq!(
            envelope.verification_hint.as_deref(),
            Some("Check the generated file.")
        );
        assert_eq!(
            envelope.next_action.as_deref(),
            Some("Use media.inspect with the asset_id.")
        );
        assert_eq!(envelope.assets.len(), 2);
    }

    #[test]
    fn test_tool_result_summary_text_success() {
        let result = ToolResult::ok(json!({"status": "done"}));
        assert_eq!(result.summary_text(), r#"{"status":"done"}"#);
    }

    #[test]
    fn structured_error_classes_are_stable_on_the_wire() {
        let classes = [
            ToolErrorClass::Transient,
            ToolErrorClass::UnknownOutcome,
            ToolErrorClass::Validation,
            ToolErrorClass::Permission,
            ToolErrorClass::SideEffectMayHaveHappened,
            ToolErrorClass::Other,
        ];
        let encoded = serde_json::to_value(classes).unwrap();
        assert_eq!(
            encoded,
            json!([
                "transient",
                "unknown_outcome",
                "validation",
                "permission",
                "side_effect_may_have_happened",
                "other"
            ])
        );
        assert_eq!(ToolErrorClass::Permission.as_str(), "permission");
    }

    #[test]
    fn timeout_outcome_and_error_class_cannot_drift() {
        let unknown = ToolResult::timed_out(
            ToolExecutionOutcome::TimedOutUnknown,
            "the request may still be running",
        );
        assert_eq!(unknown.error_class, Some(ToolErrorClass::UnknownOutcome));
        let terminated = ToolResult::timed_out(
            ToolExecutionOutcome::TimedOutAndTerminated,
            "the request was terminated",
        );
        assert_eq!(terminated.error_class, Some(ToolErrorClass::Transient));
    }

    #[test]
    fn test_tool_result_summary_text_plain_string_unquoted() {
        // A tool returning a plain string must read as text, not JSON-quoted.
        let result = ToolResult::ok(json!("some plain text"));
        assert_eq!(result.summary_text(), "some plain text");
    }

    #[test]
    fn test_tool_result_summary_text_error() {
        let result = ToolResult {
            success: false,
            output: json!(null),
            error: Some("boom".into()),
            error_class: Some(ToolErrorClass::Other),
            retryability: ToolRetryability::NotRetryable,
            truncated: false,
            outcome: ToolExecutionOutcome::Failed,
            attempts: 1,
            signals: ToolSignals::default(),
            llm_usage: Vec::new(),
        };
        assert_eq!(result.summary_text(), "boom");
    }

    #[test]
    fn test_tool_result_summary_text_error_fallback() {
        let result = ToolResult {
            success: false,
            output: json!(null),
            error: None,
            error_class: None,
            retryability: ToolRetryability::Unknown,
            truncated: false,
            outcome: ToolExecutionOutcome::Failed,
            attempts: 1,
            signals: ToolSignals::default(),
            llm_usage: Vec::new(),
        };
        assert_eq!(result.summary_text(), "unknown failure");
    }

    #[test]
    fn test_tool_result_summary_text_empty_error_falls_back_to_output() {
        // A failure with an empty error string (e.g. a shell command that
        // exited non-zero without stderr) must still yield a non-empty
        // summary — otherwise the tool appears to return no result at all.
        let result = ToolResult {
            success: false,
            output: json!({"output": "some stdout"}),
            error: Some(String::new()),
            error_class: Some(ToolErrorClass::Other),
            retryability: ToolRetryability::NotRetryable,
            truncated: false,
            outcome: ToolExecutionOutcome::Failed,
            attempts: 1,
            signals: ToolSignals::default(),
            llm_usage: Vec::new(),
        };
        assert_eq!(result.summary_text(), r#"{"output":"some stdout"}"#);
    }

    #[test]
    fn test_tool_result_summary_text_whitespace_error_falls_back() {
        let result = ToolResult {
            success: false,
            output: json!(null),
            error: Some("   ".into()),
            error_class: Some(ToolErrorClass::Other),
            retryability: ToolRetryability::NotRetryable,
            truncated: false,
            outcome: ToolExecutionOutcome::Failed,
            attempts: 1,
            signals: ToolSignals::default(),
            llm_usage: Vec::new(),
        };
        assert_eq!(result.summary_text(), "unknown failure");
    }

    #[test]
    fn observation_text_has_one_unicode_safe_budgeted_shape() {
        let result = ToolResult::ok(json!("你好世界"));
        let observation = result.observation_text(3);
        assert_eq!(observation.chars().count(), 3);
        assert!(observation.is_char_boundary(observation.len()));
    }

    #[test]
    fn structured_observation_keeps_recovery_fields_before_body() {
        let result = ToolResult::ok(json!({
            "content": "x".repeat(2_000),
            "path": "D:/workspace/Haven/docs/architecture.md",
            "next_offset": 4096,
            "hint": "continue with the returned cursor",
        }));

        let observation = result.observation_text(220);
        assert!(observation.chars().count() <= 220);
        let parsed: Value = serde_json::from_str(&observation).expect("bounded JSON object");
        assert_eq!(parsed["path"], "D:/workspace/Haven/docs/architecture.md");
        assert_eq!(parsed["next_offset"], 4096);
        assert_eq!(parsed["hint"], "continue with the returned cursor");
        assert!(parsed["content"].as_str().unwrap().ends_with('…'));
    }

    #[test]
    fn structured_observation_puts_asset_handle_before_large_body_fields() {
        let result = ToolResult::ok(json!({
            "content": "x".repeat(2_000),
            "asset_id": "asset-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "notes": "Prefer the asset_id from the previous tool result",
            "mime_type": "image/png",
        }));

        let observation = result.observation_text(220);
        assert!(
            observation.find("asset_id").unwrap() < observation.find("content").unwrap(),
            "asset handles must survive before large body fields: {observation}"
        );
        assert!(
            observation.find("notes").unwrap() < observation.find("content").unwrap(),
            "asset navigation notes must survive before large body fields: {observation}"
        );
        let parsed: Value = serde_json::from_str(&observation).expect("bounded JSON object");
        assert_eq!(parsed["asset_id"], "asset-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    }

    #[test]
    fn structured_failure_keeps_error_next_to_output_metadata() {
        let result = ToolResult::failed(
            json!({"summary_error": true, "path": "notes.md"}),
            "summarizer call failed",
        );

        let observation = result.observation_text(200);
        let parsed: Value = serde_json::from_str(&observation).expect("bounded JSON object");
        assert_eq!(parsed["success"], false);
        assert_eq!(parsed["error"], "summarizer call failed");
        assert_eq!(parsed["error_class"], "other");
        assert_eq!(parsed["retryability"], "not_retryable");
        assert_eq!(parsed["path"], "notes.md");
        assert_eq!(parsed["summary_error"], true);
    }

    #[test]
    fn test_extract_ask_signal() {
        let (q, opts) = extract_ask_signal(&json!({
            "ask": true,
            "question": "which?",
            "options": ["A", "B"],
        }));
        assert_eq!(q.as_deref(), Some("which?"));
        assert_eq!(opts, vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn test_extract_ask_signal_missing() {
        let (q, opts) = extract_ask_signal(&json!({"result": 42}));
        assert!(q.is_none());
        assert!(opts.is_empty());
    }

    #[test]
    fn test_extract_notify_signal() {
        let (title, body) = extract_notify_signal(&json!({
            "notify": true,
            "title": "ScheduledAction",
            "body": "Take a break",
        }));
        assert_eq!(title.as_deref(), Some("ScheduledAction"));
        assert_eq!(body.as_deref(), Some("Take a break"));
    }

    #[test]
    fn test_extract_notify_signal_defaults() {
        let (title, body) = extract_notify_signal(&json!({"notify": true}));
        assert_eq!(title.as_deref(), Some("Haven"));
        assert_eq!(body.as_deref(), Some(""));
    }

    #[test]
    fn test_extract_notify_signal_not_requested() {
        let (title, body) = extract_notify_signal(&json!({"notify": false}));
        assert!(title.is_none());
        assert!(body.is_none());
    }

    #[test]
    fn test_is_silent_action() {
        assert!(is_silent_action("shell", &json!({"silent": true})));
        assert!(!is_silent_action("shell", &json!({"silent": false})));
        assert!(!is_silent_action("shell", &json!({})));
        // `ask` must never be silent, even when the input asks for it.
        assert!(!is_silent_action("ask", &json!({"silent": true})));
    }

    pub(crate) struct MockTool {
        name: String,
        schema: Value,
        execute_delay: Option<Duration>,
    }

    impl MockTool {
        pub(crate) fn new(name: &str) -> Self {
            Self {
                name: name.into(),
                schema: json!({"type": "object"}),
                execute_delay: None,
            }
        }

        pub(crate) fn with_schema(name: &str, schema: Value) -> Self {
            Self {
                name: name.into(),
                schema,
                execute_delay: None,
            }
        }

        pub(crate) fn with_delay(name: &str, delay: Duration) -> Self {
            Self {
                name: name.into(),
                schema: json!({"type": "object"}),
                execute_delay: Some(delay),
            }
        }
    }

    /// The schema-validation mock used by the `validate_input` tests.
    fn schema_mock() -> MockTool {
        MockTool::with_schema(
            "schema_mock",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "count": { "type": "integer" }
                },
                "required": ["name"]
            }),
        )
    }

    #[async_trait::async_trait]
    impl Tool for MockTool {
        fn name(&self) -> String {
            self.name.clone()
        }
        fn description(&self) -> String {
            "mock".into()
        }
        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::Safe
        }
        async fn execute(
            &self,
            _input: Value,
            _cancel: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            if let Some(delay) = self.execute_delay {
                tokio::time::sleep(delay).await;
            }
            Ok(ToolResult::ok(json!({"ok": true})))
        }
        fn input_schema(&self) -> Value {
            self.schema.clone()
        }
    }

    #[test]
    fn test_validate_input_valid() {
        let tool = schema_mock();
        let result = tool.validate_input(&json!({"name": "test", "count": 5}));
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_input_missing_required() {
        let tool = schema_mock();
        let result = tool.validate_input(&json!({"count": 5}));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_input_missing_required_message_lists_fields() {
        let tool = schema_mock();
        let err = tool.validate_input(&json!({})).unwrap_err().to_string();
        assert!(
            err.contains("MISSING REQUIRED FIELD(S): name"),
            "missing fields must be called out explicitly, got: {err}"
        );
    }

    #[test]
    fn test_validate_input_enum_hint_included() {
        // The file tool schema declares an enum on `operation`; a missing
        // operation must surface the allowed values so the model can self-correct.
        let tool = crate::builtin::files::FilesTool::default();
        let err = tool
            .validate_input(&json!({"path": "x.txt"}))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("operation"),
            "operation must be named, got: {err}"
        );
        assert!(
            err.contains("read, inspect, stat"),
            "allowed operations should be listed, got: {err}"
        );
    }

    #[test]
    fn test_validate_input_wrong_type() {
        let tool = schema_mock();
        let result = tool.validate_input(&json!({"name": 123}));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_with_timeout_quick() {
        let tool = MockTool::new("quick");
        let result = tool
            .execute_with_timeout(json!({}), CancellationToken::new(), 30)
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().success);
    }

    #[tokio::test]
    async fn test_execute_with_timeout_slow() {
        let tool = MockTool::with_delay("slow", Duration::from_secs(10));
        let result = tool
            .execute_with_timeout(json!({}), CancellationToken::new(), 1)
            .await;
        let result = result.unwrap();
        assert_eq!(result.outcome, ToolExecutionOutcome::TimedOutUnknown);
        assert!(!result.success);
    }

    #[tokio::test]
    async fn test_execute_with_timeout_cancelled_is_structured() {
        let tool = MockTool::with_delay("cancelled", Duration::from_secs(10));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = tool
            .execute_with_timeout(json!({}), cancel, 30)
            .await
            .unwrap();
        assert_eq!(result.outcome, ToolExecutionOutcome::Cancelled);
        assert!(!result.success);
    }

    struct SlowTypedOperation;

    #[async_trait::async_trait]
    impl TypedToolOperation for SlowTypedOperation {
        type Args = Value;
        type Output = Value;
        type Error = std::convert::Infallible;

        fn metadata(&self, _args: &Self::Args) -> ToolOperationMetadata {
            self.default_metadata()
        }

        fn default_metadata(&self) -> ToolOperationMetadata {
            ToolOperationMetadata {
                capability: "test.slow_typed",
                operation: "slow_typed",
                scope: ToolOperationScope::Session,
                risk_level: RiskLevel::Safe,
                idempotency: OperationIdempotency::Unknown,
                cancellation: ToolCancellationPolicy::Cooperative,
                timeout_secs: 1,
                concurrency: ToolConcurrency::Exclusive,
            }
        }

        fn input_schema(&self) -> Value {
            json!({"type": "object", "additionalProperties": false})
        }

        async fn execute_typed(
            &self,
            _args: Self::Args,
            _cancel: CancellationToken,
        ) -> Result<Self::Output, Self::Error> {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(json!({"done": true}))
        }
    }

    #[tokio::test]
    async fn typed_adapter_timeout_preserves_unknown_outcome_metadata() {
        let tool = TypedToolAdapter::new(
            "test.slow_typed",
            "slow typed operation",
            SlowTypedOperation,
        );
        let result = tool
            .execute_with_timeout(json!({}), CancellationToken::new(), 1)
            .await
            .unwrap();
        assert!(!result.success);
        assert_eq!(result.outcome, ToolExecutionOutcome::TimedOutUnknown);
        assert_eq!(result.error_class, Some(ToolErrorClass::UnknownOutcome));
        assert_eq!(result.retryability, ToolRetryability::Unknown);
        assert_eq!(
            tool.timeout_outcome(),
            ToolExecutionOutcome::TimedOutUnknown
        );
    }

    #[test]
    fn manifest_separates_loader_execution_and_display_sources() {
        let manifest = MockTool::new("load_mcp").tool_manifest();
        assert_eq!(manifest.identity.source, ToolSource::Builtin);
        assert_eq!(manifest.presentation.represented_source, ToolSource::Mcp);
        assert_eq!(manifest.presentation.label, "加载 MCP");
    }
}
