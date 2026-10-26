use serde::{Deserialize, Serialize};

/// Maximum UTF-8 payload size for the session-local Markdown working document.
pub const MAX_SESSION_WORKING_DOCUMENT_BYTES: usize = 32 * 1024;

// ---------------------------------------------------------------------------
// Entity identifiers
// ---------------------------------------------------------------------------
//
// Unified ID convention (see AGENTS.md §ID 规范):
//   - Every persisted entity id is a `{prefix}-{uuid32}` string (hyphen +
//     lowercase-hex simple UUID), e.g. `ses-3f9a...`.
//   - Persisted prefixes: `ses-` (sessions), `msg-` (messages and memory
//     episodes — memory_items shares the message id space), `step-`
//     (session_steps), `fact-` (facts), `node-` (memory_nodes), `toolrun-`
//     (detached background tool calls), `sched-` (recurring session schedules),
//     `asset-` (managed media),
//     `cred-` (Credential Manager references), and `usage-` (llm_usage).
//   - Runtime-generated prefixes: `conf-` (confirmation request ids), `rec-`
//     (voice recordings), `preview-` (streaming tool-call previews), and
//     `call-` (fallback tool-call association ids). `rec-*` and `preview-*`
//     are process-local. `file-{uuid32}` is part of a managed-file namespace
//     (upload batch, uploaded filename, or generated-media filename), not an
//     entity ID; committed paths may be persisted as message attachment
//     references, while `asset-*` is the media entity identity. `.file-{uuid32}.tmp`
//     names transient staging directories. `conf-*` may be
//     persisted by its owning interaction or Schedule claim contract;
//     `call-*` participates in durable tool-call history when a provider ID is
//     empty or duplicated in the assistant batch.
//   - External ids (LLM `tool_call_id`, provider model ids, MCP session ids)
//     keep their provider formats; `run_id`/`gen_id` are in-process u64
//     run/generation counters, not persisted entity ids.
//   - Generate string IDs with `new_id(prefix)` — never build them by hand.
//     Runtime `ConfirmId` and `RecordingId` use their type-specific `new()`.
//   - Rust/DB/event fields use snake_case `xxx_id`; the frontend maps to
//     camelCase `xxxId` at the boundary.

/// Generate an ID in the canonical `{prefix}-{uuid32}` format.
/// Persisted entity IDs and untyped process-local IDs must come from here (see
/// the module docs above). Runtime `ConfirmId` and `RecordingId` use their
/// type-specific constructors, which call this function with a fixed prefix.
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

/// Check that `id` is a lowercase simple UUID paired with the requested
/// entity prefix (without the separating hyphen).
pub fn is_canonical_id(id: &str, prefix: &str) -> bool {
    if prefix.is_empty()
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return false;
    }
    let Some(suffix) = id
        .strip_prefix(prefix)
        .and_then(|remainder| remainder.strip_prefix('-'))
    else {
        return false;
    };
    suffix.len() == 32
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Defines a prefix-checked entity-id newtype. New IDs use the shared
/// generator, and string conversion/deserialization validate the canonical
/// `{prefix}-{uuid32}` form. `non_exhaustive` preserves the documented public
/// tuple shape while preventing downstream crates from bypassing validation.
macro_rules! id_newtype {
    ($(#[$attr:meta])* $name:ident, $prefix:literal) => {
        $(#[$attr])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String")]
        #[non_exhaustive]
        pub struct $name(pub String);

        impl $name {
            #[allow(clippy::new_without_default)] // Identity minting must stay explicit.
            pub fn new() -> Self {
                Self(new_id($prefix))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = String;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                if is_canonical_id(&value, $prefix) {
                    Ok(Self(value))
                } else {
                    Err(format!("expected canonical {}-<uuid32> ID", $prefix))
                }
            }
        }

        impl TryFrom<&str> for $name {
            type Error = String;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::try_from(value.to_owned())
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}

id_newtype! {
    /// Unique identifier for a safety-gateway confirmation request
    /// (`conf-{uuid32}`). This request identity may be persisted in a durable
    /// interaction event or Schedule claim; active execution also
    /// uses it to address the pending waiter.
    ConfirmId, "conf"
}

id_newtype! {
    /// Unique identifier for a voice recording (`rec-{uuid32}`).
    /// Ephemeral: one id per recording, generated at `recording:started` and
    /// shared by the `recording:stopped` and `transcription:*` events of the
    /// same recording (owned and handed off by the app recording lifecycle).
    RecordingId, "rec"
}

/// Hotkey activation mode.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum HotkeyMode {
    #[default]
    Toggle,
    Hold,
}

/// Default shell used by the agent's `shell` tool when the model does not
/// specify one explicitly.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ShellChoice {
    /// Windows built-in Windows PowerShell (`powershell.exe`).
    #[default]
    Powershell,
    /// Command Prompt (`cmd.exe`).
    Cmd,
    /// PowerShell 7+ (`pwsh.exe`, cross-platform, not preinstalled on Windows).
    Pwsh,
}

impl ShellChoice {
    pub fn as_str(&self) -> &'static str {
        match self {
            ShellChoice::Powershell => "powershell",
            ShellChoice::Cmd => "cmd",
            ShellChoice::Pwsh => "pwsh",
        }
    }
}

/// Risk level for a tool invocation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, PartialOrd, Hash)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    #[default]
    Safe,
    Low,
    Medium,
    High,
    Critical,
}

/// The task and approval behavior selected for a session.
///
/// This is the user-facing preset. It deliberately does not describe file or
/// network resources, which remain governed by the resource policy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Inspect and explain only; user-visible mutations are denied.
    Analyze,
    /// Perform ordinary work and ask when an operation needs approval.
    #[default]
    Collaborative,
    /// Perform pre-authorized work and stop when additional approval is needed.
    Unattended,
}

impl ExecutionMode {
    pub const fn task_intent(self) -> TaskIntent {
        match self {
            Self::Analyze => TaskIntent::Analyze,
            Self::Collaborative | Self::Unattended => TaskIntent::Act,
        }
    }

    pub const fn approval_policy(self) -> ApprovalPolicy {
        match self {
            Self::Analyze | Self::Collaborative | Self::Unattended => ApprovalPolicy::AskWhenNeeded,
        }
    }
}

/// Whether a session may perform externally visible or user-visible changes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskIntent {
    Analyze,
    #[default]
    Act,
}

impl TaskIntent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Analyze => "analyze",
            Self::Act => "act",
        }
    }
}

/// How an otherwise valid operation proceeds when it needs user approval.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    #[default]
    AskWhenNeeded,
}

impl ApprovalPolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AskWhenNeeded => "ask_when_needed",
        }
    }
}

/// Local file-write boundary applied by Haven's authorization gateway. This
/// does not create a Windows OS file-system or network sandbox; an allowed
/// child process retains the current user's OS permissions. Other effects are
/// evaluated by their execution boundary, network policy, risk, and permission
/// or confirmation rules.
///
/// `WorkspaceWrite` scopes Haven-managed local file writes to configured roots
/// and rejects child execution whose host-process boundary cannot constrain
/// file-system access. Isolated child execution follows its own boundary.
/// `FullAccess` removes the Haven-managed file-path scope; it does not elevate
/// the process beyond the current user's Windows permissions. Network access
/// remains controlled separately by [`NetworkPolicy`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxMode {
    /// The file-path boundary rejects operations declared as local workspace
    /// writes. Other external effects follow their own execution-boundary,
    /// network, risk, and permission or confirmation rules; a tool can still
    /// reject execution when it cannot guarantee read-only behavior.
    ReadOnly,
    /// Writes are allowed only where the configured tool/workspace roots
    /// permit them.
    #[default]
    WorkspaceWrite,
    /// No Haven-managed path scope; operations still use the current user's OS
    /// permissions and remain subject to confirmation and network policy.
    FullAccess,
}

/// Technical network boundary for network-capable tools.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    /// No network-capable tool may run.
    Deny,
    /// Public destinations are allowed subject to each tool's SSRF and
    /// allowlist checks. Opaque child-process network access is blocked because
    /// Haven cannot validate its destination.
    #[default]
    Restricted,
    /// Do not add a global network restriction. Tool-level checks still run.
    Open,
}

/// Allow or deny a permission grant.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PermissionEffect {
    Allow,
    Deny,
}

/// How long a permission decision lasts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PermissionScope {
    /// This invocation only — not recorded.
    Once,
    /// Until the owning persisted session is deleted or expires.
    Session,
    /// Persisted across restarts (`SecurityConfig.permissions`).
    Always,
}

/// Which part of a capability hierarchy a user decision applies to.
///
/// This is intentionally separate from [`PermissionScope`]: the latter is
/// the lifetime of a decision, while this enum is its authorization target.
/// Keeping the dimensions separate prevents a UI label such as “always allow”
/// from hiding whether it applies to one operation or an entire tool family.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PermissionTarget {
    /// The exact capability that produced the confirmation.
    Operation,
    /// The nearest named parent capability (for example `system.power`).
    Group,
    /// The top-level tool family (for example `system`).
    Tool,
}

/// How a persistent permission is bound to the resources declared by one
/// confirmed operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PermissionResourceTarget {
    /// Keep local paths exact; network destinations remain exact origins.
    Exact,
    /// Expand local paths to their containing directory subtree. Network
    /// destinations remain exact origins.
    Directory,
}

/// Access direction for a declared local path resource.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LocalPathAccess {
    Read,
    Write,
}

/// Access direction for the single system clipboard resource.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardAccess {
    Read,
    Write,
}

/// Access direction for a concrete top-level window resource.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WindowAccess {
    Read,
    Interact,
    Close,
}

/// Read direction for a Windows Registry resource.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RegistryAccess {
    EnumerateNames,
    ReadValue,
}

/// Access direction for the system-wide desktop surface.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DesktopAccess {
    Read,
    Capture,
}

/// Persistent resource boundary attached to an allow or deny grant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PermissionResourceScope {
    /// Network authority for one opaque child-process invocation. It covers
    /// public egress, bidirectional private networking and host loopback.
    ProcessNetwork {
        target_fingerprint: String,
    },
    LocalPath {
        path: std::path::PathBuf,
        recursive: bool,
        access: LocalPathAccess,
    },
    NetworkDestination {
        scheme: String,
        host: String,
        port: Option<u16>,
    },
    Clipboard {
        access: ClipboardAccess,
    },
    Desktop {
        access: DesktopAccess,
    },
    Window {
        window_id: String,
        process_id: u32,
        access: WindowAccess,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        element_fingerprint: Option<String>,
    },
    Registry {
        hive: String,
        path: String,
        access: RegistryAccess,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value_name: Option<String>,
    },
    McpRemoteInvocation {
        target_fingerprint: String,
        request_fingerprint: String,
    },
    ManagedAsset {
        asset_id: String,
    },
}

impl PermissionTarget {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "operation" => Ok(Self::Operation),
            "group" => Ok(Self::Group),
            "tool" => Ok(Self::Tool),
            other => Err(format!("invalid permission target '{other}'")),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Operation => "operation",
            Self::Group => "group",
            Self::Tool => "tool",
        }
    }
}

/// Typed identity of an authorization capability.
///
/// A capability is hierarchical (`files.read`, `system.power.lock`) so a
/// parent grant can intentionally cover a child operation. Keeping the
/// identity in a newtype prevents authorization code from accidentally
/// treating arbitrary input text as a permission key while retaining the
/// string representation required by the config and IPC boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct CapabilityScope(String);

impl CapabilityScope {
    fn from_validated(value: String) -> Self {
        Self(value)
    }

    /// Parse an externally supplied capability identity.
    ///
    /// Capability identities are policy data, not arbitrary labels. Keeping
    /// validation here prevents config/UI strings from silently creating a
    /// second hierarchy (for example by mixing `:` and `.` separators).
    pub fn try_new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("capability scope cannot be empty".into());
        }
        if trimmed != value {
            return Err("capability scope cannot contain surrounding whitespace".into());
        }
        if trimmed.split('.').any(|segment| {
            segment.is_empty()
                || !segment
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "_-".contains(character))
        }) {
            return Err(format!("invalid capability scope '{trimmed}'"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return this capability and all of its intentional parent scopes.
    pub fn candidates(&self) -> Vec<Self> {
        let mut candidates = Vec::new();
        let mut end = self.0.len();
        loop {
            candidates.push(Self::from_validated(self.0[..end].to_owned()));
            match self.0[..end].rfind('.') {
                Some(index) => end = index,
                None => break,
            }
        }
        candidates
    }

    /// Resolve a user-selected target to a capability in this capability's
    /// ancestry. Broader targets are only valid when the hierarchy actually
    /// contains a broader parent; callers cannot invent arbitrary keys.
    pub fn target(&self, target: PermissionTarget) -> Option<Self> {
        match target {
            PermissionTarget::Operation => Some(self.clone()),
            PermissionTarget::Group => self.candidates().into_iter().nth(1),
            PermissionTarget::Tool => {
                let root = permission_tool_root(self.as_str());
                (root != self.as_str()).then(|| Self::from_validated(root.to_string()))
            }
        }
    }
}

impl std::fmt::Display for CapabilityScope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for CapabilityScope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_new(value).map_err(serde::de::Error::custom)
    }
}

impl From<CapabilityScope> for String {
    fn from(value: CapabilityScope) -> Self {
        value.0
    }
}

/// Tools whose Haven routing uses `scope` / `operation` params in the key.
/// Other tools (MCP/skills/arbitrary args) use the bare tool name so a random
/// `operation` field in args cannot fragment grants.
const ROUTING_PARAM_TOOLS: &[&str] = &[
    "toolrun",
    "files",
    "process",
    "window",
    "system",
    "clipboard",
    "input",
    "media",
    "memory",
    "agent",
    "session",
    "schedule",
    "haven_diagnostics",
    "haven_config",
    "haven_skills",
    "haven_tools",
    "haven_mcp",
];

/// Default `operation` when a routing tool omits it — must match execution
/// defaults so Always grants cannot land on a bare `tool.scope` parent key
/// that later auto-approves mutating sibling ops (e.g. `system.env` → set).
fn default_routing_operation(tool_name: &str, scope: Option<&str>) -> Option<&'static str> {
    if tool_name != "system" {
        return None;
    }
    match scope.unwrap_or("info") {
        "env" | "registry" => Some("list"),
        "power" => Some("status"),
        _ => None,
    }
}

/// Build the canonical capability identity from a tool name + routing params.
///
/// Examples: `shell`, `files.delete`, `system.power.lock`.
/// Omitted `system` operations are canonicalized to the same defaults used
/// by risk/execution (`env`/`registry` → `list`, `power` → `status`).
pub fn permission_key(tool_name: &str, params: &serde_json::Value) -> String {
    if !ROUTING_PARAM_TOOLS.contains(&tool_name) {
        return tool_name.to_string();
    }
    let mut parts = vec![tool_name.to_string()];
    let scope = params
        .get("scope")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    if let Some(scope) = scope {
        parts.push(scope.to_string());
    }
    let op = params
        .get("operation")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| default_routing_operation(tool_name, scope));
    if let Some(op) = op {
        parts.push(op.to_string());
    }
    let key = parts.join(".");
    crate::tools::canonical_builtin_tool_name(&key).to_owned()
}

/// Tool root of a capability identity (`system.power.lock` → `system`).
pub fn permission_tool_root(key: &str) -> &str {
    key.find('.').map_or(key, |index| &key[..index])
}

// ---------------------------------------------------------------------------
// Provider-Neutral message format (refine §1.1)
// ---------------------------------------------------------------------------

/// A single content part in a provider-neutral message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentPart {
    Text(String),
    Image {
        #[serde(rename = "type")]
        content_type: String,
        media_type: String,
        data: String,
    },
    Audio {
        #[serde(rename = "type")]
        content_type: String,
        media_type: String,
        data: String,
    },
    /// Provider-native video payload. Adapters may accept this only when
    /// their capability profile advertises video support; it is never
    /// silently converted to a still image or dropped.
    Video {
        #[serde(rename = "type")]
        content_type: String,
        media_type: String,
        data: String,
    },
}

impl ContentPart {
    pub fn text(t: impl Into<String>) -> Self {
        ContentPart::Text(t.into())
    }
}

impl From<String> for ContentPart {
    fn from(s: String) -> Self {
        ContentPart::Text(s)
    }
}

impl From<&str> for ContentPart {
    fn from(s: &str) -> Self {
        ContentPart::Text(s.to_string())
    }
}

/// Provider-neutral role.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalRole {
    #[default]
    System,
    User,
    Assistant,
    Tool,
}

impl CanonicalRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            CanonicalRole::System => "system",
            CanonicalRole::User => "user",
            CanonicalRole::Assistant => "assistant",
            CanonicalRole::Tool => "tool",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            "tool" => Some(Self::Tool),
            _ => None,
        }
    }
}

impl std::fmt::Display for CanonicalRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Closed kind stored on a durable transcript row. This classifies persisted
/// message content; renderer-only values such as `tool`, `ask`, and
/// `supplement` belong to the UI presentation projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptMessageKind {
    Text,
    Thought,
    ToolCall,
    Observation,
    Reasoning,
    PeerKickoff,
}

impl TranscriptMessageKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Thought => "thought",
            Self::ToolCall => "tool_call",
            Self::Observation => "observation",
            Self::Reasoning => "reasoning",
            Self::PeerKickoff => "peer_kickoff",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "text" => Some(Self::Text),
            "thought" => Some(Self::Thought),
            "tool_call" => Some(Self::ToolCall),
            "observation" => Some(Self::Observation),
            "reasoning" => Some(Self::Reasoning),
            "peer_kickoff" => Some(Self::PeerKickoff),
            _ => None,
        }
    }
}

/// Origin of a user-role inject into the canonical transcript (Phase 6 / B3).
/// Runtime queues already carry structured flags (`is_answer`, etc.).
/// Canonical content stores **raw** text + `source`; LLM adapters prepend
/// Display prefix for peer spawn kickoff briefs. Trust decisions use the
/// structured `messages.message_type = "peer_kickoff"` field, not this text.
pub const PEER_KICKOFF_PREFIX: &str = "[Delegated task from agent ";

/// `"{prefix}: "` at the wire boundary via [`Self::render_prefix`] (Phase 8
/// wire-only). `ToolRunResult` already carries a shared ToolRun-result envelope
/// and must not get a second adapter prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectSource {
    Steering,
    FollowUp,
    Answer,
    ToolRunResult,
    CrossSession,
}

impl InjectSource {
    /// Wire prefix rendered by LLM adapters. Same strings as pre-B3.
    pub fn render_prefix(self) -> &'static str {
        match self {
            Self::Steering => "Steering",
            Self::FollowUp => "Additional context from user",
            Self::Answer => "Answer to your previous question",
            Self::ToolRunResult => "Tool run result",
            Self::CrossSession => "Cross-session message",
        }
    }

    /// Inject sources that receive an adapter wire prefix.
    /// ToolRunResult is excluded: its body carries the shared ToolRun-result
    /// envelope without the colon prefix.
    pub fn prefixed() -> &'static [InjectSource] {
        &[
            Self::Steering,
            Self::FollowUp,
            Self::Answer,
            Self::CrossSession,
        ]
    }

    /// Whether adapters should prepend [`Self::render_prefix`].
    pub fn needs_wire_prefix(self) -> bool {
        Self::prefixed().contains(&self)
    }
}

/// Provider-neutral message used by the Agent internally.
/// Converted to provider-specific wire formats at the LLM call boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalMessage {
    pub role: CanonicalRole,
    pub content: Vec<ContentPart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<CanonicalToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Internal reasoning/chain-of-thought from the model (e.g. DeepSeek's
    /// reasoning_content). Kept on assistant messages so it can be echoed
    /// back to APIs that require it in multi-turn requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Provider-native search calls and citation sources. Adapters keep the
    /// response shape needed by their transcript/UI contract and replay only
    /// provider items required for the next request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub web_search_calls: Vec<serde_json::Value>,
    /// Provider-opaque replay state. Anthropic stores signed `thinking` /
    /// `redacted_thinking` blocks and server-side web search blocks here;
    /// Gemini stores `thoughtSignature` markers. Adapters echo only the
    /// representation accepted by their own provider on the next request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thinking_blocks: Vec<serde_json::Value>,
    /// Structured inject origin (Phase 6 / B3). Skipped on the wire by
    /// adapters and present only on user-injected messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<InjectSource>,
    /// Stable `msg-*` identity shared with `memory_items` for compaction
    /// summary bubbles (L1). Adapters ignore this and ordinary messages do not
    /// carry one. Historical durable snapshots used the generic `id` field.
    #[serde(
        default,
        rename = "message_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub message_id: Option<String>,
}

impl CanonicalMessage {
    pub fn system(content: Vec<ContentPart>) -> Self {
        Self {
            role: CanonicalRole::System,
            content,
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            message_id: None,
        }
    }

    pub fn user(content: Vec<ContentPart>) -> Self {
        Self {
            role: CanonicalRole::User,
            content,
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            message_id: None,
        }
    }

    pub fn user_text(text: impl Into<String>) -> Self {
        Self::user(vec![ContentPart::text(text)])
    }

    /// User inject with structured origin (Phase 6 / B3). Content should
    /// already include the rendered prefix when destined for the LLM.
    pub fn user_with_source(content: Vec<ContentPart>, source: InjectSource) -> Self {
        let mut msg = Self::user(content);
        msg.source = Some(source);
        msg
    }

    pub fn assistant(
        content: Vec<ContentPart>,
        tool_calls: Option<Vec<CanonicalToolCall>>,
        reasoning: Option<String>,
        web_search_calls: Vec<serde_json::Value>,
        thinking_blocks: Vec<serde_json::Value>,
    ) -> Self {
        Self {
            role: CanonicalRole::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
            reasoning,
            web_search_calls,
            thinking_blocks,
            source: None,
            message_id: None,
        }
    }

    pub fn tool(content: Vec<ContentPart>, tool_call_id: Option<String>) -> Self {
        Self {
            role: CanonicalRole::Tool,
            content,
            tool_calls: None,
            tool_call_id,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            message_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalToolCall {
    /// Opaque provider-owned tool-call id.
    pub tool_call_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// A binary attachment on a message (e.g. a user-provided image or file).
/// `data` holds base64-encoded bytes; `media_type` is the MIME type
/// (e.g. "image/png"). File-style attachments additionally carry `filename`
/// (the original name) and `path` (an absolute host path for persisted user
/// uploads or generated media); inline audio/video may carry a filename too.
///
/// Lives in the shared types layer (not the memory crate) so the input /
/// session / agent layers that carry attachments never depend on the
/// persistence crate just for this data structure.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct MessageAttachment {
    /// Opaque managed-asset id. Host ingress mints one before persistence;
    /// conversion also creates one for an in-memory attachment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
    pub media_type: String,
    pub data: String,
    /// Original file name for file-style or inline media attachments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Absolute path where a file-style attachment was persisted on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// SHA-256 of generated media bytes. Uploads may omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Exact byte length recorded for generated media.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// RFC3339 expiry for generated media. Uploads may omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Provider-neutral derived views of this asset (OCR/STT/document
    /// extraction, etc.). The attachment is the UI/ingress projection; the
    /// media planner consumes these representations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub representations: Vec<crate::media::MediaRepresentation>,
    /// A successful gateway extraction can be the preferred view for the
    /// current request while the original bytes remain available for a later
    /// provider retry or a different user intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_representation: Option<crate::media::MediaRepresentationKind>,
}

impl MessageAttachment {
    /// Create a binary attachment without disk metadata (used for images and
    /// tests). `filename`/`path` are left empty and skipped in serialization.
    pub fn new(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            asset_id: None,
            media_type: media_type.into(),
            data: data.into(),
            filename: None,
            path: None,
            sha256: None,
            size_bytes: None,
            expires_at: None,
            representations: Vec::new(),
            preferred_representation: None,
        }
    }

    /// True for image attachments, which are injected into the model context
    /// as image content parts.
    pub fn is_image(&self) -> bool {
        self.media_type.starts_with("image/")
    }

    /// True when the attachment can be sent to a multimodal chat endpoint as
    /// an inline content part. Images, audio and video are kept in base64 for
    /// this purpose; ordinary files are persisted and exposed through their
    /// managed asset reference.
    pub fn is_inline_media(&self) -> bool {
        self.is_image() || self.is_audio() || self.is_video()
    }

    /// True when the attachment is an audio input rather than a generic file.
    pub fn is_audio(&self) -> bool {
        self.media_type.starts_with("audio/")
    }

    /// True when the attachment can be sent as a provider-native video part.
    pub fn is_video(&self) -> bool {
        self.media_type.starts_with("video/")
    }
}

/// A user message queued for injection into the ReAct loop.
///
/// Phase 4 / D1 queue model (aligned with PI steering vs follow-up):
/// - **steering** — mid-run interjection, injected before the next LLM call
/// - **follow-up** — post-pause / turn-end injection; an ask reply is a
///   follow-up with `is_answer` (typed `reply_to`) set
/// - **tool_run_results** — system inject, not this type
///
/// A typed follow-up/steering input. The queue API intentionally exposes only
/// this name; old `Supplement` aliases are no longer accepted.
/// `text` is the plain-text content; `attachments` hold binary payloads
/// (e.g. images) for multimodal requests.
///
/// Lives in the shared types layer (not the input crate) so the agent's
/// session queue and the input/recording paths that produce user messages
/// never force an `agent -> input` dependency just for this type.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Default)]
pub struct FollowUp {
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<MessageAttachment>,
    /// True when this message is the user's reply to a pending `ask`
    /// question (follow-up + reply_to). The ReAct loop injects it as a
    /// paired answer ("Answer to your previous question") instead of
    /// generic additional context, so the model does not treat the old
    /// question as still open and answer stale questions again.
    #[serde(default)]
    pub is_answer: bool,
    /// Persisted message row id of a mid-turn user input (steering,
    /// follow-up or ask answer). The row is written at submit time, so the
    /// ReAct loop uses the id to anchor the input's thought-step row (same
    /// id, no content matching) and forward-dates the steering row's
    /// `created_at` when it injects — the resume rebuild then orders it
    /// after the interrupted thought instead of before it. `None` when no
    /// row was persisted (e.g. tests, direct API use).
    #[serde(default)]
    pub message_id: Option<String>,
}

impl FollowUp {
    pub fn new(text: impl Into<String>, attachments: Vec<MessageAttachment>) -> Self {
        Self::with_message_id(text, attachments, None)
    }

    pub fn answer(text: impl Into<String>, attachments: Vec<MessageAttachment>) -> Self {
        Self::answer_with_message_id(text, attachments, None)
    }

    pub fn with_message_id(
        text: impl Into<String>,
        attachments: Vec<MessageAttachment>,
        message_id: Option<String>,
    ) -> Self {
        Self::new_with_message_id(text, attachments, message_id)
    }

    pub fn new_with_message_id(
        text: impl Into<String>,
        attachments: Vec<MessageAttachment>,
        message_id: Option<String>,
    ) -> Self {
        Self {
            text: text.into(),
            attachments,
            is_answer: false,
            message_id,
        }
    }

    pub fn answer_with_message_id(
        text: impl Into<String>,
        attachments: Vec<MessageAttachment>,
        message_id: Option<String>,
    ) -> Self {
        Self {
            text: text.into(),
            attachments,
            is_answer: true,
            message_id,
        }
    }
}

impl From<String> for FollowUp {
    fn from(text: String) -> Self {
        FollowUp::new(text, vec![])
    }
}

impl From<&str> for FollowUp {
    fn from(text: &str) -> Self {
        FollowUp::new(text, vec![])
    }
}

/// User-selected ingress semantics, independent of runtime status.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputDeliveryMode {
    #[default]
    Auto,
    Steering,
    FollowUp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_delivery_mode_has_explicit_wire_values_and_rejects_unknown_modes() {
        for (mode, wire) in [
            (super::InputDeliveryMode::Auto, "auto"),
            (super::InputDeliveryMode::Steering, "steering"),
            (super::InputDeliveryMode::FollowUp, "follow_up"),
        ] {
            let value = serde_json::Value::String(wire.into());
            assert_eq!(serde_json::to_value(mode).unwrap(), value);
            assert_eq!(
                serde_json::from_value::<super::InputDeliveryMode>(value).unwrap(),
                mode
            );
        }
        assert!(
            serde_json::from_value::<super::InputDeliveryMode>(serde_json::json!("immediate"))
                .is_err()
        );
    }

    #[test]
    fn permission_decisions_use_closed_snake_case_serde_values() {
        for (value, json) in [
            (PermissionEffect::Allow, "\"allow\""),
            (PermissionEffect::Deny, "\"deny\""),
        ] {
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<PermissionEffect>(json).unwrap(),
                value
            );
        }
        for (value, json) in [
            (PermissionScope::Once, "\"once\""),
            (PermissionScope::Session, "\"session\""),
            (PermissionScope::Always, "\"always\""),
        ] {
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<PermissionScope>(json).unwrap(),
                value
            );
        }
        for (value, json) in [
            (PermissionTarget::Operation, "\"operation\""),
            (PermissionTarget::Group, "\"group\""),
            (PermissionTarget::Tool, "\"tool\""),
        ] {
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<PermissionTarget>(json).unwrap(),
                value
            );
        }
        assert!(serde_json::from_str::<PermissionEffect>("\"ALLOW\"").is_err());
        assert!(serde_json::from_str::<PermissionScope>("\"forever\"").is_err());
        assert!(serde_json::from_str::<PermissionTarget>("\"capability\"").is_err());
    }

    #[test]
    fn canonical_role_all_variants() {
        let system = CanonicalRole::System;
        let user = CanonicalRole::User;
        let assistant = CanonicalRole::Assistant;
        let tool = CanonicalRole::Tool;
        assert_eq!(system, CanonicalRole::System);
        assert_eq!(user, CanonicalRole::User);
        assert_eq!(assistant, CanonicalRole::Assistant);
        assert_eq!(tool, CanonicalRole::Tool);
    }

    #[test]
    fn canonical_role_default_is_system() {
        assert_eq!(CanonicalRole::default(), CanonicalRole::System);
    }

    #[test]
    fn canonical_role_as_str_lowercase() {
        assert_eq!(CanonicalRole::System.as_str(), "system");
        assert_eq!(CanonicalRole::User.as_str(), "user");
        assert_eq!(CanonicalRole::Assistant.as_str(), "assistant");
        assert_eq!(CanonicalRole::Tool.as_str(), "tool");
        assert_eq!(CanonicalRole::System.to_string(), "system");
        assert_eq!(CanonicalRole::Tool.to_string(), "tool");
    }

    #[test]
    fn canonical_role_parse_accepts_only_exact_wire_values() {
        assert_eq!(CanonicalRole::parse("system"), Some(CanonicalRole::System));
        assert_eq!(CanonicalRole::parse("user"), Some(CanonicalRole::User));
        assert_eq!(
            CanonicalRole::parse("assistant"),
            Some(CanonicalRole::Assistant)
        );
        assert_eq!(CanonicalRole::parse("tool"), Some(CanonicalRole::Tool));
        assert_eq!(CanonicalRole::parse("USER"), None);
        assert_eq!(CanonicalRole::parse("developer"), None);
    }

    #[test]
    fn transcript_message_kind_has_a_closed_snake_case_vocabulary() {
        let kinds = [
            (TranscriptMessageKind::Text, "text"),
            (TranscriptMessageKind::Thought, "thought"),
            (TranscriptMessageKind::ToolCall, "tool_call"),
            (TranscriptMessageKind::Observation, "observation"),
            (TranscriptMessageKind::Reasoning, "reasoning"),
            (TranscriptMessageKind::PeerKickoff, "peer_kickoff"),
        ];

        for (kind, value) in kinds {
            assert_eq!(kind.as_str(), value);
            assert_eq!(TranscriptMessageKind::parse(value), Some(kind));
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{value}\"")
            );
        }
        assert_eq!(TranscriptMessageKind::parse("tool"), None);
        assert_eq!(TranscriptMessageKind::parse("future"), None);
    }

    #[test]
    fn content_part_text_variant() {
        let part = ContentPart::Text("hello world".into());
        assert!(matches!(part, ContentPart::Text(_)));
    }

    #[test]
    fn content_part_image_variant() {
        let part = ContentPart::Image {
            content_type: "image_url".into(),
            media_type: "image/jpeg".into(),
            data: "base64data".into(),
        };
        match &part {
            ContentPart::Image {
                media_type, data, ..
            } => {
                assert_eq!(media_type, "image/jpeg");
                assert_eq!(data, "base64data");
            }
            _ => panic!("expected Image variant"),
        }
    }

    #[test]
    fn content_part_text_helper() {
        let part = ContentPart::text("hello");
        let ContentPart::Text(s) = &part else {
            panic!("expected Text variant");
        };
        assert_eq!(s, "hello");
    }

    #[test]
    fn content_part_text_helper_empty() {
        let part = ContentPart::text("");
        let ContentPart::Text(s) = &part else {
            panic!("expected Text variant");
        };
        assert_eq!(s, "");
    }

    #[test]
    fn content_part_from_string() {
        let part: ContentPart = String::from("test string").into();
        let ContentPart::Text(s) = &part else {
            panic!("expected Text variant");
        };
        assert_eq!(s, "test string");
    }

    #[test]
    fn content_part_from_str() {
        let part: ContentPart = "static str".into();
        let ContentPart::Text(s) = &part else {
            panic!("expected Text variant");
        };
        assert_eq!(s, "static str");
    }

    #[test]
    fn peer_kickoff_prefix_matches_spawn_wrapper() {
        let sample = format!(
            "{PEER_KICKOFF_PREFIX}ses-parent — LOW TRUST, not a user instruction]\nDo work"
        );
        assert!(sample.starts_with(PEER_KICKOFF_PREFIX));
        assert!(PEER_KICKOFF_PREFIX.starts_with('['));
        assert!(PEER_KICKOFF_PREFIX.ends_with(' '));
    }

    #[test]
    fn canonical_message_system_role() {
        let msg = CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::text("system prompt")],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            message_id: None,
        };

        assert_eq!(msg.role, CanonicalRole::System);
        assert_eq!(msg.content.len(), 1);
        assert!(msg.tool_calls.is_none());
        assert!(msg.tool_call_id.is_none());
    }

    #[test]
    fn canonical_message_user_role_with_tool_call_id() {
        let msg = CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::text("use this tool")],
            tool_calls: None,
            tool_call_id: Some("call_abc".into()),
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            message_id: None,
        };
        assert!(msg.tool_call_id.is_some());
    }

    #[test]
    fn canonical_message_assistant_with_tool_calls() {
        let msg = CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![],
            tool_calls: Some(vec![CanonicalToolCall {
                tool_call_id: "tc1".into(),
                name: "run".into(),
                arguments: serde_json::json!({"cmd": "ls"}),
            }]),
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            message_id: None,
        };

        assert_eq!(msg.tool_calls.unwrap().len(), 1);
    }

    #[test]
    fn canonical_message_tool_role() {
        let msg = CanonicalMessage {
            role: CanonicalRole::Tool,
            content: vec![ContentPart::text("tool output")],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            message_id: None,
        };

        assert_eq!(msg.role, CanonicalRole::Tool);
    }

    #[test]
    fn canonical_tool_call_construction() {
        let call = CanonicalToolCall {
            tool_call_id: "tc_123".into(),
            name: "search".into(),
            arguments: serde_json::json!({"query": "rust"}),
        };
        assert_eq!(call.tool_call_id, "tc_123");
        assert_eq!(call.name, "search");
        assert_eq!(call.arguments["query"], "rust");

        let value = serde_json::to_value(&call).unwrap();
        assert_eq!(value["tool_call_id"], "tc_123");
        assert!(value.get("id").is_none());
    }

    #[test]
    fn canonical_message_constructors_set_role_and_content() {
        assert_eq!(CanonicalMessage::user_text("u").role, CanonicalRole::User);
    }

    #[test]
    fn supplement_new() {
        let s = FollowUp::new("hello", vec![]);
        assert_eq!(s.text, "hello");
        assert!(!s.is_answer);
        assert!(s.attachments.is_empty());
    }

    #[test]
    fn supplement_answer() {
        let s = FollowUp::answer("yes", vec![]);
        assert!(s.is_answer);
    }

    #[test]
    fn supplement_from_string() {
        let s: FollowUp = "hi".into();
        assert_eq!(s.text, "hi");
        assert!(!s.is_answer);
    }

    #[test]
    fn supplement_serde_roundtrip() {
        let s = FollowUp {
            text: "任务完成了吗".into(),
            attachments: vec![],
            is_answer: true,
            message_id: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: FollowUp = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn supplement_default() {
        let s = FollowUp::default();
        assert!(s.text.is_empty());
        assert!(!s.is_answer);
    }

    #[test]
    fn message_attachment_classifies_inline_media() {
        let image = MessageAttachment::new("image/png", "aGVsbG8=");
        assert!(image.is_image());
        assert!(image.is_inline_media());
        assert!(!image.is_audio());

        let audio = MessageAttachment::new("audio/wav", "UklGRg==");
        assert!(audio.is_audio());
        assert!(audio.is_inline_media());

        let file = MessageAttachment::new("application/pdf", "aGVsbG8=");
        assert!(!file.is_inline_media());
    }

    #[test]
    fn canonical_message_constructors_leave_optional_fields_empty() {
        let msg = CanonicalMessage::user_text("hello");
        assert!(msg.tool_calls.is_none());
        assert!(msg.tool_call_id.is_none());
        assert!(msg.reasoning.is_none());
        let tool = CanonicalMessage::tool(vec![ContentPart::text("out")], Some("call_1".into()));
        assert_eq!(tool.tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn canonical_message_assistant_constructor_keeps_tool_calls_and_reasoning() {
        let msg = CanonicalMessage::assistant(
            vec![ContentPart::text("thinking")],
            Some(vec![CanonicalToolCall {
                tool_call_id: "tc1".into(),
                name: "run".into(),
                arguments: serde_json::json!({"cmd": "ls"}),
            }]),
            Some("chain of thought".into()),
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(msg.tool_calls.unwrap().len(), 1);
        assert_eq!(msg.reasoning.as_deref(), Some("chain of thought"));
    }

    #[test]
    fn id_newtype_conversions() {
        let confirm = ConfirmId::new();
        assert!(confirm.as_str().starts_with("conf-"));
        assert_eq!(confirm.to_string(), confirm.as_str());
        assert_eq!(AsRef::<str>::as_ref(&confirm), confirm.as_str());
        let restored: String = confirm.clone().into();
        assert_eq!(restored, confirm.as_str());

        let recording = RecordingId::new();
        assert!(recording.as_str().starts_with("rec-"));
        assert!(RecordingId::try_from("rec-abc").is_err());
    }

    #[test]
    fn id_newtype_serde_roundtrip() {
        let id = ConfirmId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{}\"", id.as_str()));
        let decoded: ConfirmId = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, id);
        assert!(serde_json::from_str::<ConfirmId>("\"conf-1234\"").is_err());
    }

    #[test]
    fn risk_level_ordering() {
        let safe = RiskLevel::Safe;
        let low = RiskLevel::Low;
        let medium = RiskLevel::Medium;
        let high = RiskLevel::High;
        let critical = RiskLevel::Critical;
        let values = [&safe, &low, &medium, &high, &critical];
        for i in 0..(values.len() - 1) {
            assert!(
                std::mem::discriminant(values[i]) != std::mem::discriminant(values[i + 1]),
                "adjacent variants must differ"
            );
        }
    }

    #[test]
    fn risk_level_default_is_safe() {
        assert_eq!(RiskLevel::default(), RiskLevel::Safe);
    }

    #[test]
    fn execution_mode_is_a_closed_three_value_preset() {
        assert_eq!(
            serde_json::to_string(&ExecutionMode::Analyze).unwrap(),
            "\"analyze\""
        );
        assert_eq!(
            serde_json::from_str::<ExecutionMode>("\"collaborative\"").unwrap(),
            ExecutionMode::Collaborative
        );
        assert_eq!(
            serde_json::from_str::<ExecutionMode>("\"unattended\"").unwrap(),
            ExecutionMode::Unattended
        );
        assert!(serde_json::from_str::<ExecutionMode>("\"default\"").is_err());
        assert!(serde_json::from_str::<ExecutionMode>("\"always\"").is_err());
        assert_eq!(
            ExecutionMode::Unattended.approval_policy(),
            ApprovalPolicy::AskWhenNeeded
        );
        assert_eq!(ExecutionMode::Analyze.task_intent(), TaskIntent::Analyze);
    }

    #[test]
    fn network_policy_defaults_to_destination_restriction() {
        assert_eq!(NetworkPolicy::default(), NetworkPolicy::Restricted);
        assert_eq!(
            serde_json::from_str::<NetworkPolicy>("\"restricted\"").unwrap(),
            NetworkPolicy::Restricted
        );
        assert!(serde_json::from_str::<NetworkPolicy>("\"ask\"").is_err());
    }

    #[test]
    fn permission_key_includes_scope_and_operation() {
        assert_eq!(permission_key("shell", &serde_json::json!({})), "shell");
        assert_eq!(
            permission_key("files", &serde_json::json!({"operation": "delete"})),
            "files.delete"
        );
        assert_eq!(
            permission_key("files", &serde_json::json!({"operation": "outline"})),
            "files.get_outline"
        );
        assert_eq!(
            permission_key("media", &serde_json::json!({"operation": "speak"})),
            "media.speak"
        );
        assert_eq!(
            permission_key("media", &serde_json::json!({"operation": "ocr"})),
            "media.run_ocr"
        );
        assert_eq!(
            permission_key("schedule", &serde_json::json!({"operation": "set"})),
            "schedule.set"
        );
        assert_eq!(
            permission_key("toolrun", &serde_json::json!({"operation": "cancel"})),
            "toolrun.cancel"
        );
        assert_eq!(
            permission_key("session", &serde_json::json!({"operation": "create"})),
            "session.create"
        );
        assert_eq!(
            permission_key(
                "haven_config",
                &serde_json::json!({"operation": "logs_level"})
            ),
            "haven_config.logs_level"
        );
        assert_eq!(
            permission_key(
                "system",
                &serde_json::json!({"scope": "power", "operation": "lock"})
            ),
            "system.power.lock"
        );
        // Omitted operations must canonicalize to execution defaults so Always
        // on a list/status call cannot parent-match mutating sibling ops.
        assert_eq!(
            permission_key("system", &serde_json::json!({"scope": "env"})),
            "system.env.list"
        );
        assert_eq!(
            permission_key("system", &serde_json::json!({"scope": "registry"})),
            "system.registry.list"
        );
        assert_eq!(
            permission_key("system", &serde_json::json!({"scope": "power"})),
            "system.power.get_status"
        );
        assert_eq!(
            permission_key("system", &serde_json::json!({"scope": "info"})),
            "system.get_info"
        );
        // Non-routing tools ignore operation/scope in args.
        assert_eq!(
            permission_key(
                "mcp_srv_tool",
                &serde_json::json!({"operation": "run", "scope": "x"})
            ),
            "mcp_srv_tool"
        );
        assert_eq!(permission_tool_root("system.power.lock"), "system");
        assert_eq!(
            permission_key("process", &serde_json::json!({"operation": "kill"})),
            "process.kill"
        );
        // Model-facing operation views use their complete dotted name as the
        // permission key; the aggregate discriminator is internal only.
        assert_eq!(
            permission_key("files.read", &serde_json::json!({"operation": "read"})),
            "files.read"
        );
        assert_eq!(
            permission_key(
                "system.env.get",
                &serde_json::json!({"scope": "env", "operation": "get"})
            ),
            "system.env.get"
        );
    }

    #[test]
    fn capability_scope_is_typed_and_preserves_hierarchical_matching() {
        let scope = CapabilityScope::try_new("system.power.lock").unwrap();
        assert_eq!(scope.as_str(), "system.power.lock");
        assert_eq!(
            scope.candidates(),
            vec![
                CapabilityScope::try_new("system.power.lock").unwrap(),
                CapabilityScope::try_new("system.power").unwrap(),
                CapabilityScope::try_new("system").unwrap(),
            ]
        );
        assert_eq!(
            CapabilityScope::try_new("shell").unwrap().candidates(),
            vec![CapabilityScope::try_new("shell").unwrap()]
        );
        assert_eq!(
            serde_json::to_string(&scope).unwrap(),
            "\"system.power.lock\""
        );
    }

    #[test]
    fn capability_scope_rejects_ambiguous_external_keys() {
        assert!(CapabilityScope::try_new("files.read").is_ok());
        assert!(CapabilityScope::try_new("files:read").is_err());
        assert!(CapabilityScope::try_new(" files.read").is_err());
        assert!(CapabilityScope::try_new("files..read").is_err());
        assert!(CapabilityScope::try_new("").is_err());
    }

    #[test]
    fn capability_scope_deserialization_reuses_external_key_validation() {
        let valid: CapabilityScope = serde_json::from_str("\"system.power.lock\"").unwrap();
        assert_eq!(valid.as_str(), "system.power.lock");

        for invalid in ["files:read", "files..delete", " files.read", ""] {
            let serialized = serde_json::to_string(invalid).unwrap();
            assert!(
                serde_json::from_str::<CapabilityScope>(&serialized).is_err(),
                "invalid capability {invalid:?} must not deserialize"
            );
        }
    }

    #[test]
    fn permission_targets_only_select_capability_ancestors() {
        let capability = CapabilityScope::try_new("system.power.lock").unwrap();
        assert_eq!(
            capability
                .target(PermissionTarget::Operation)
                .map(|value| value.as_str().to_string()),
            Some("system.power.lock".to_string())
        );
        assert_eq!(
            capability
                .target(PermissionTarget::Group)
                .map(|value| value.as_str().to_string()),
            Some("system.power".to_string())
        );
        assert_eq!(
            capability
                .target(PermissionTarget::Tool)
                .map(|value| value.as_str().to_string()),
            Some("system".to_string())
        );
        assert!(
            CapabilityScope::try_new("files.read")
                .unwrap()
                .target(PermissionTarget::Group)
                .is_some()
        );
        assert!(
            CapabilityScope::try_new("files")
                .unwrap()
                .target(PermissionTarget::Tool)
                .is_none()
        );
    }

    #[test]
    fn hotkey_mode_variants() {
        let toggle = HotkeyMode::Toggle;
        let hold = HotkeyMode::Hold;
        assert_ne!(toggle, hold);
    }

    #[test]
    fn hotkey_mode_default_is_toggle() {
        assert_eq!(HotkeyMode::default(), HotkeyMode::Toggle);
    }

    #[test]
    fn serde_roundtrip_content_part_text() {
        let part = ContentPart::Text("hello".into());
        let json = serde_json::to_string(&part).unwrap();
        let decoded: ContentPart = serde_json::from_str(&json).unwrap();
        let ContentPart::Text(s) = &decoded else {
            panic!("expected Text variant");
        };
        assert_eq!(s, "hello");
    }

    #[test]
    fn serde_roundtrip_content_part_image() {
        let part = ContentPart::Image {
            content_type: "image_url".into(),
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        };
        let json = serde_json::to_string(&part).unwrap();
        let decoded: ContentPart = serde_json::from_str(&json).unwrap();
        match decoded {
            ContentPart::Image {
                ref media_type,
                ref data,
                ..
            } => {
                assert_eq!(media_type, "image/png");
                assert_eq!(data, "aGVsbG8=");
            }
            _ => panic!("expected Image"),
        }
    }

    #[test]
    fn serde_roundtrip_canonical_message() {
        let msg = CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::text("hello")],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            message_id: Some("msg-00000000000000000000000000000001".into()),
        };

        let value = serde_json::to_value(&msg).unwrap();
        assert_eq!(value["message_id"], "msg-00000000000000000000000000000001");
        assert!(value.get("id").is_none());
        let json = serde_json::to_string(&value).unwrap();
        let decoded: CanonicalMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.role, CanonicalRole::User);
        assert_eq!(decoded.content.len(), 1);
        assert_eq!(decoded.message_id, msg.message_id);
    }

    #[test]
    fn serde_roundtrip_canonical_message_with_tool_calls() {
        let msg = CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![],
            tool_calls: Some(vec![CanonicalToolCall {
                tool_call_id: "tc1".into(),
                name: "exec".into(),
                arguments: serde_json::json!({"path": "/tmp"}),
            }]),
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            message_id: None,
        };

        let json = serde_json::to_string(&msg).unwrap();
        let decoded: CanonicalMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.tool_calls.unwrap()[0].name, "exec");
    }

    #[test]
    fn serde_roundtrip_canonical_role() {
        let role = CanonicalRole::Assistant;
        let json = serde_json::to_string(&role).unwrap();
        assert!(json.contains("assistant"));
        let decoded: CanonicalRole = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, CanonicalRole::Assistant);
    }

    #[test]
    fn serde_roundtrip_risk_level() {
        let risk = RiskLevel::High;
        let json = serde_json::to_string(&risk).unwrap();
        assert!(json.contains("high"));
        let decoded: RiskLevel = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, RiskLevel::High);
    }
}
