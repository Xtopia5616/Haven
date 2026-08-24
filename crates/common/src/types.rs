use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Entity identifiers
// ---------------------------------------------------------------------------
//
// Unified ID convention (see AGENTS.md §ID 规范):
//   - Every persisted entity id is a `{prefix}-{uuid32}` string (hyphen +
//     lowercase-hex simple UUID), e.g. `ses-3f9a...`.
//   - Prefixes: `ses-` (sessions), `msg-` (messages and memory episodes —
//     memory_episodes shares the message id space), `step-` (session_steps),
//     `fact-` (facts), `act-` (actions — unified background actions and
//     scheduled actions), `usage-` (llm_usage);
//     `conf-` (safety-gateway confirmations), `rec-` (voice recording
//     sessions), `file-` (temporary files), `call-` (locally synthesized
//     tool-call ids when the provider sends an empty one) are in-process
//     only and never persisted.
//   - External ids (LLM `tool_call_id`, provider model ids, MCP session ids)
//     keep their provider formats; `run_id`/`gen_id` are in-process u64
//     run/generation counters, not persisted entity ids.
//   - Generate ids with `new_id(prefix)` — never build them by hand.
//   - Rust/DB/event fields use snake_case `xxx_id`; the frontend maps to
//     camelCase `xxxId` at the boundary.

/// Generate an entity id in the canonical `{prefix}-{uuid32}` format.
/// Every persisted entity id must come from here (see the module docs above),
/// then be converted into its newtype with `.into()`.
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

/// Defines an entity-id newtype: `pub struct $name(pub String)` with the
/// standard derives plus the conversions/accessors the rest of the codebase
/// relies on. Serializes as the plain `{prefix}-{uuid32}` string on the wire,
/// so the frontend and the DB (which store ids as text) are unaffected.
macro_rules! id_newtype {
    ($(#[$attr:meta])* $name:ident) => {
        $(#[$attr])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl $name {
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

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
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
    /// (`conf-{uuid32}`). Ephemeral: lives only in the `confirm:requested`
    /// event payload and the executor's pending-wait map.
    ConfirmId
}

id_newtype! {
    /// Unique identifier for a voice-recording session (`rec-{uuid32}`).
    /// Ephemeral: one id per recording, generated at `recording:started` and
    /// shared by the `transcription:result`/`transcription:error` events of the
    /// same recording (held in `AppState.recording_session` between the two).
    SessionId
}

/// MCP transport type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportType {
    #[default]
    #[serde(alias = "Stdio")]
    Stdio,
    #[serde(alias = "Http")]
    Http,
}

impl McpTransportType {
    pub fn as_str(&self) -> &'static str {
        match self {
            McpTransportType::Stdio => "stdio",
            McpTransportType::Http => "http",
        }
    }
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

/// How the safety gateway decides when to prompt the user.
///
/// Legacy config value `"always"` deserializes as [`ConfirmationMode::Ask`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationMode {
    /// Ask when `risk >= min_risk_level` (default).
    #[default]
    #[serde(alias = "always")]
    Ask,
    /// Ask for every non-`Safe` operation.
    Paranoid,
    /// Auto-approve everything except permanent/session denies and disabled ops.
    Autopilot,
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
    /// Remainder of the owning session.
    Session,
    /// Persisted across restarts (`SecurityConfig.permissions`).
    Always,
}

/// Tools whose Haven routing uses `scope` / `operation` params in the key.
/// Other tools (MCP/skills/arbitrary args) use the bare tool name so a random
/// `operation` field in args cannot fragment grants.
const ROUTING_PARAM_TOOLS: &[&str] = &[
    "files",
    "process",
    "window",
    "system",
    "clipboard",
    "input",
    "audio",
    "memory",
    "messaging",
    "haven",
    "scheduled_action",
];

/// Default `operation` when a routing tool omits it — must match execution
/// defaults so Always grants cannot land on a bare `tool:scope` parent key
/// that later auto-approves mutating sibling ops (e.g. `system:env` → set).
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

/// Build a permission key from tool name + optional routing params.
///
/// Examples: `shell`, `files:delete`, `system:power:lock`.
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
    parts.join(":")
}

/// Tool root of a permission key (`system:power:lock` → `system`).
pub fn permission_tool_root(key: &str) -> &str {
    key.split_once(':').map(|(root, _)| root).unwrap_or(key)
}

/// Ancestor keys for grant matching: exact key first, then parents.
///
/// `files:delete` → `["files:delete", "files"]`
/// `system:power:lock` → `["system:power:lock", "system:power", "system"]`
pub fn permission_key_candidates(key: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut end = key.len();
    loop {
        out.push(&key[..end]);
        match key[..end].rfind(':') {
            Some(i) => end = i,
            None => break,
        }
    }
    out
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
}

impl std::fmt::Display for CanonicalRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Origin of a user-role inject into the canonical transcript (Phase 6 / B3).
/// Runtime queues already carry structured flags (`is_answer`, etc.).
/// Canonical content stores **raw** text + `source`; LLM adapters prepend
/// Legacy / content fallback detector for peer spawn kickoff briefs.
/// Primary signal is `messages.message_type = "peer_kickoff"`; this prefix
/// covers older rows and in-memory bubbles that only have the wrapper text.
/// Keep in sync with `ui/src/lib/peerKickoff.ts`.
pub const PEER_KICKOFF_PREFIX: &str = "[Delegated task from agent ";

/// `"{prefix}: "` at the wire boundary via [`Self::render_prefix`] (Phase 8
/// wire-only). `ActionResult` is producer-labelled and must not get a second
/// adapter prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectSource {
    Steering,
    FollowUp,
    Answer,
    ActionResult,
    CrossSession,
}

impl InjectSource {
    /// Wire prefix rendered by LLM adapters. Same strings as pre-B3.
    pub fn render_prefix(self) -> &'static str {
        match self {
            Self::Steering => "Steering",
            Self::FollowUp => "Additional context from user",
            Self::Answer => "Answer to your previous question",
            Self::ActionResult => "Background action result",
            Self::CrossSession => "Cross-session message",
        }
    }

    /// Inject sources that receive an adapter wire prefix.
    /// ActionResult is excluded: its body is producer-labelled
    /// (`[Background action result]…`) without the colon prefix.
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

    /// Prefixes used when matching a raw DB user message against a historically
    /// prefixed display string (rollback). Derived from [`Self::render_prefix`]
    /// so the two cannot drift.
    pub fn match_prefixes() -> Vec<String> {
        Self::prefixed()
            .iter()
            .map(|s| format!("{}: ", s.render_prefix()))
            .collect()
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
    /// Raw `web_search_call` output items produced by the provider's built-in
    /// web search tool. Carried on assistant messages so they are passed back
    /// verbatim in the next request's input (the server restores the search
    /// context from them). Never parsed or rewritten.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub web_search_calls: Vec<serde_json::Value>,
    /// Raw Anthropic `thinking` content blocks (`{"type":"thinking",
    /// "thinking":…, "signature":…}`). Carried verbatim so tool-use turns can
    /// echo them back: Anthropic validates thinking blocks against their
    /// signature and 400s a follow-up request that omits or rewrites them.
    /// The Anthropic adapter appends an internal `__layout` marker entry that
    /// records each block's original position; the adapter strips it before
    /// the echo. Other consumers treat the list as opaque.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thinking_blocks: Vec<serde_json::Value>,
    /// Structured inject origin (Phase 6 / B3). Skipped on the wire by
    /// adapters; optional so legacy snapshots deserialize cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<InjectSource>,
    /// Stable `msg-*` identity shared with `memory_episodes` for compaction
    /// summary bubbles (L1). Adapters ignore this; optional for legacy
    /// snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
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
            id: None,
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
            id: None,
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
            id: None,
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
            id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

impl CanonicalToolCall {
    /// Serialize the canonical argument object to the wire JSON string every
    /// provider expects. Centralized so the encode policy (and the `{}`
    /// fallback for a null/missing object) lives in one place instead of
    /// being duplicated per adapter.
    pub fn args_to_wire(&self) -> String {
        serde_json::to_string(&self.arguments).unwrap_or_else(|_| "{}".to_string())
    }

    /// Parse a provider's wire arguments JSON string back into a canonical
    /// value (finished-stream path).
    ///
    /// - Empty / whitespace → `{}` (tools with no args).
    /// - Valid JSON → parsed value.
    /// - Structural truncation repair (missing `}` / `]` / value after `:`) →
    ///   repaired object — only safe after the provider finished cleanly.
    /// - Mid-string cut or unrepairable → `Null` (ReAct retries).
    pub fn from_wire_args(args: &str) -> serde_json::Value {
        match Self::parse_wire_args(args) {
            WireArgsParse::Empty => serde_json::json!({}),
            WireArgsParse::Valid(v) | WireArgsParse::Repaired(v) => v,
            WireArgsParse::Incomplete => serde_json::Value::Null,
        }
    }

    /// Classify wire argument JSON without losing Empty vs Valid vs Repaired.
    pub fn parse_wire_args(args: &str) -> WireArgsParse {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            return WireArgsParse::Empty;
        }
        if let Ok(v) = serde_json::from_str(trimmed) {
            return WireArgsParse::Valid(v);
        }
        match repair_truncated_json(trimmed) {
            Some(RepairOutcome {
                value,
                closed_open_string: false,
            }) => WireArgsParse::Repaired(value),
            _ => WireArgsParse::Incomplete,
        }
    }

    /// True when an in-flight tool call must not be flushed without a provider
    /// finish signal: name present but args still empty, structurally repaired
    /// only, or mid-string / unrepairable. Valid complete JSON is fine.
    pub fn stream_tool_args_unfinished(name: &str, args: &str) -> bool {
        if name.is_empty() {
            return false;
        }
        match Self::parse_wire_args(args) {
            WireArgsParse::Valid(_) => false,
            WireArgsParse::Empty
            | WireArgsParse::Repaired(_)
            | WireArgsParse::Incomplete => true,
        }
    }
}

/// Result of parsing provider tool-call argument JSON.
#[derive(Debug, Clone, PartialEq)]
pub enum WireArgsParse {
    /// No arguments text yet (or whitespace-only).
    Empty,
    /// Parsed without repair.
    Valid(serde_json::Value),
    /// Parsed only after structural truncation repair (missing closers / value).
    Repaired(serde_json::Value),
    /// Mid-string cut or still invalid after repair.
    Incomplete,
}

struct RepairOutcome {
    value: serde_json::Value,
    /// True when the input ended inside a JSON string — the closed value is
    /// a guess and must not be treated as complete arguments.
    closed_open_string: bool,
}

/// Best-effort repair for tool-call argument JSON cut off mid-stream
/// (cancel, idle timeout, or `finish_reason=length` while arguments were
/// still being generated). Closes an open string, completes a bare object
/// key with `:null`, fills a missing value with `null`, strips a trailing
/// comma, and closes unmatched `{` / `[`.
fn repair_truncated_json(input: &str) -> Option<RepairOutcome> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Expect {
        ObjectKey,
        ObjectColon,
        ObjectValue,
        ArrayValue,
        CommaOrClose,
    }

    let mut out = String::with_capacity(input.len() + 16);
    let mut stack: Vec<char> = Vec::new();
    let mut expect = Expect::ObjectValue;
    let mut in_string = false;
    let mut escape = false;
    let mut closed_open_string = false;

    for ch in input.chars() {
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
                expect = if expect == Expect::ObjectKey {
                    Expect::ObjectColon
                } else {
                    Expect::CommaOrClose
                };
            }
            continue;
        }
        if ch.is_whitespace() {
            out.push(ch);
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            '{' => {
                stack.push('{');
                expect = Expect::ObjectKey;
                out.push(ch);
            }
            '[' => {
                stack.push('[');
                expect = Expect::ArrayValue;
                out.push(ch);
            }
            '}' | ']' => {
                let open = if ch == '}' { '{' } else { '[' };
                if stack.last() != Some(&open) {
                    return None;
                }
                stack.pop();
                out.push(ch);
                expect = Expect::CommaOrClose;
            }
            ':' => {
                expect = Expect::ObjectValue;
                out.push(ch);
            }
            ',' => {
                expect = if stack.last() == Some(&'[') {
                    Expect::ArrayValue
                } else {
                    Expect::ObjectKey
                };
                out.push(ch);
            }
            _ => {
                out.push(ch);
                expect = Expect::CommaOrClose;
            }
        }
    }

    if in_string {
        closed_open_string = true;
        if escape {
            out.pop();
        }
        out.push('"');
        expect = if expect == Expect::ObjectKey {
            Expect::ObjectColon
        } else {
            Expect::CommaOrClose
        };
    }

    // Strip trailing comma BEFORE filling missing values — otherwise
    // `["a",` becomes `["a",null]`.
    {
        let trimmed_len = out.trim_end().len();
        out.truncate(trimmed_len);
        if out.ends_with(',') {
            out.pop();
            expect = Expect::CommaOrClose;
        }
    }

    match expect {
        Expect::ObjectColon => out.push_str(":null"),
        Expect::ObjectValue | Expect::ArrayValue => out.push_str("null"),
        Expect::ObjectKey | Expect::CommaOrClose => {}
    }

    while let Some(open) = stack.pop() {
        out.push(if open == '{' { '}' } else { ']' });
    }

    let value = serde_json::from_str(&out).ok()?;
    Some(RepairOutcome {
        value,
        closed_open_string,
    })
}

/// A binary attachment on a message (e.g. a user-provided image or file).
/// `data` holds base64-encoded bytes; `media_type` is the MIME type
/// (e.g. "image/png"). Non-image attachments (user-uploaded files)
/// additionally carry `filename` (the original name) and `path` (absolute
/// path on disk, set after the backend persists the bytes so the agent can
/// read them with the file tool).
///
/// Lives in the shared types layer (not the memory crate) so the input /
/// session / agent layers that carry attachments never depend on the
/// persistence crate just for this data structure.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct MessageAttachment {
    pub media_type: String,
    pub data: String,
    /// Original file name for non-image attachments (e.g. "report.pdf").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Absolute path where a non-image attachment was persisted on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl MessageAttachment {
    /// Create a binary attachment without disk metadata (used for images and
    /// tests). `filename`/`path` are left empty and skipped in serialization.
    pub fn new(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            media_type: media_type.into(),
            data: data.into(),
            filename: None,
            path: None,
        }
    }

    /// True for vision-capable attachments (images), which are injected into
    /// the model context as image content parts. Everything else is a file
    /// attachment the agent reads from `path` via the file tool.
    pub fn is_image(&self) -> bool {
        self.media_type.starts_with("image/")
    }
}

/// A user message queued for injection into the ReAct loop.
///
/// Phase 4 / D1 queue model (aligned with PI steering vs follow-up):
/// - **steering** — mid-run interjection, injected before the next LLM call
/// - **follow-up** — post-pause / turn-end injection; an ask reply is a
///   follow-up with `is_answer` (typed `reply_to`) set
/// - **action_results** — system inject, not this type
///
/// `Supplement` is the historical name; [`FollowUp`] is the Phase 4 alias.
/// `text` is the plain-text content; `attachments` hold binary payloads
/// (e.g. images) for multimodal requests.
///
/// Lives in the shared types layer (not the input crate) so the agent's
/// session queue and the input/recording paths that produce user messages
/// never force an `agent -> input` dependency just for this type.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Default)]
pub struct Supplement {
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
    /// `created_at` when it injects — the review rebuild then orders it
    /// after the interrupted thought instead of before it. `None` when no
    /// row was persisted (e.g. tests, direct API use).
    #[serde(default)]
    pub message_id: Option<String>,
}

/// Phase 4 / D1 name for a post-pause user inject (PI `followUp`).
pub type FollowUp = Supplement;

impl Supplement {
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

impl From<String> for Supplement {
    fn from(text: String) -> Self {
        Supplement::new(text, vec![])
    }
}

impl From<&str> for Supplement {
    fn from(text: &str) -> Self {
        Supplement::new(text, vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn from_wire_args_empty_becomes_object() {
        assert_eq!(CanonicalToolCall::from_wire_args(""), serde_json::json!({}));
        assert_eq!(CanonicalToolCall::from_wire_args("  "), serde_json::json!({}));
        assert_eq!(
            CanonicalToolCall::parse_wire_args(""),
            WireArgsParse::Empty
        );
    }

    #[test]
    fn from_wire_args_repairs_structural_truncation_only() {
        // Mid-string cut: value is unknown → Null / incomplete (retry).
        assert_eq!(
            CanonicalToolCall::from_wire_args(r#"{"path":"/tmp/fo"#),
            serde_json::Value::Null
        );
        assert_eq!(
            CanonicalToolCall::parse_wire_args(r#"{"path":"/tmp/fo"#),
            WireArgsParse::Incomplete
        );
        assert_eq!(
            CanonicalToolCall::from_wire_args(r#"{"a":1,"b"#),
            serde_json::Value::Null
        );

        // Structural only: missing closers / missing value after `:`.
        let v3 = CanonicalToolCall::from_wire_args(r#"{"a":"#);
        assert!(v3["a"].is_null());
        assert!(matches!(
            CanonicalToolCall::parse_wire_args(r#"{"a":"#),
            WireArgsParse::Repaired(_)
        ));

        let v4 = CanonicalToolCall::from_wire_args(r#"{"items":[1,2"#);
        assert_eq!(v4["items"], serde_json::json!([1, 2]));

        // Trailing comma must not invent a null element.
        let v4c = CanonicalToolCall::from_wire_args(r#"{"items":[1,2,"#);
        assert_eq!(v4c["items"], serde_json::json!([1, 2]));
        assert!(matches!(
            CanonicalToolCall::parse_wire_args(r#"{"items":[1,2,"#),
            WireArgsParse::Repaired(_)
        ));

        let v5 = CanonicalToolCall::from_wire_args(r#"{"path":"/tmp/foo","mode":"r""#);
        assert_eq!(v5["path"], "/tmp/foo");
        assert_eq!(v5["mode"], "r");
    }

    #[test]
    fn stream_tool_args_unfinished_blocks_empty_and_repaired() {
        assert!(!CanonicalToolCall::stream_tool_args_unfinished("", ""));
        assert!(CanonicalToolCall::stream_tool_args_unfinished("files", ""));
        assert!(CanonicalToolCall::stream_tool_args_unfinished(
            "files",
            r#"{"path":"#
        ));
        assert!(CanonicalToolCall::stream_tool_args_unfinished(
            "files",
            r#"{"path":"/tm"#
        ));
        assert!(!CanonicalToolCall::stream_tool_args_unfinished(
            "files",
            r#"{"path":"/tmp"}"#
        ));
    }

    #[test]
    fn from_wire_args_unrepairable_stays_null_and_incomplete() {
        // Truncated literal cannot be closed into valid JSON.
        assert_eq!(
            CanonicalToolCall::from_wire_args(r#"{"ok":tru"#),
            serde_json::Value::Null
        );
        assert_eq!(
            CanonicalToolCall::parse_wire_args(r#"{"ok":tru"#),
            WireArgsParse::Incomplete
        );
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
    fn inject_source_match_prefixes_derive_from_render() {
        let prefixes = InjectSource::match_prefixes();
        assert_eq!(prefixes.len(), InjectSource::prefixed().len());
        for source in InjectSource::prefixed() {
            let expected = format!("{}: ", source.render_prefix());
            assert!(
                prefixes.contains(&expected),
                "missing match prefix for {source:?}: {expected}"
            );
        }
        assert!(
            !prefixes
                .iter()
                .any(|p| p.starts_with("Background action result:")),
            "ActionResult bodies are not colon-prefixed"
        );
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
            id: None,
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
            id: None,
        };
        assert!(msg.tool_call_id.is_some());
    }

    #[test]
    fn canonical_message_assistant_with_tool_calls() {
        let msg = CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![],
            tool_calls: Some(vec![CanonicalToolCall {
                id: "tc1".into(),
                name: "run".into(),
                arguments: serde_json::json!({"cmd": "ls"}),
            }]),
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
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
            id: None,
        };

        assert_eq!(msg.role, CanonicalRole::Tool);
    }

    #[test]
    fn canonical_tool_call_construction() {
        let call = CanonicalToolCall {
            id: "tc_123".into(),
            name: "search".into(),
            arguments: serde_json::json!({"query": "rust"}),
        };
        assert_eq!(call.id, "tc_123");
        assert_eq!(call.name, "search");
        assert_eq!(call.arguments["query"], "rust");
    }

    #[test]
    fn canonical_message_constructors_set_role_and_content() {
        assert_eq!(CanonicalMessage::user_text("u").role, CanonicalRole::User);
    }

    #[test]
    fn supplement_new() {
        let s = Supplement::new("hello", vec![]);
        assert_eq!(s.text, "hello");
        assert!(!s.is_answer);
        assert!(s.attachments.is_empty());
    }

    #[test]
    fn supplement_answer() {
        let s = Supplement::answer("yes", vec![]);
        assert!(s.is_answer);
    }

    #[test]
    fn supplement_from_string() {
        let s: Supplement = "hi".into();
        assert_eq!(s.text, "hi");
        assert!(!s.is_answer);
    }

    #[test]
    fn supplement_serde_roundtrip() {
        let s = Supplement {
            text: "任务完成了吗".into(),
            attachments: vec![],
            is_answer: true,
            message_id: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Supplement = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn supplement_default() {
        let s = Supplement::default();
        assert!(s.text.is_empty());
        assert!(!s.is_answer);
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
                id: "tc1".into(),
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
        let confirm: ConfirmId = new_id("conf").into();
        assert!(confirm.as_str().starts_with("conf-"));
        assert_eq!(confirm.to_string(), confirm.0);
        assert_eq!(AsRef::<str>::as_ref(&confirm), confirm.as_str());
        let restored: String = confirm.clone().into();
        assert_eq!(restored, confirm.0);
        let from_str: SessionId = "rec-abc".into();
        assert_eq!(from_str.0, "rec-abc");
    }

    #[test]
    fn id_newtype_serde_roundtrip() {
        let id: ConfirmId = "conf-1234".into();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"conf-1234\"");
        let decoded: ConfirmId = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, id);
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
    fn confirmation_mode_legacy_always_deserializes_as_ask() {
        let mode: ConfirmationMode = serde_json::from_str("\"always\"").unwrap();
        assert_eq!(mode, ConfirmationMode::Ask);
        let mode: ConfirmationMode = serde_json::from_str("\"ask\"").unwrap();
        assert_eq!(mode, ConfirmationMode::Ask);
    }

    #[test]
    fn permission_key_includes_scope_and_operation() {
        assert_eq!(permission_key("shell", &serde_json::json!({})), "shell");
        assert_eq!(
            permission_key("files", &serde_json::json!({"operation": "delete"})),
            "files:delete"
        );
        assert_eq!(
            permission_key(
                "system",
                &serde_json::json!({"scope": "power", "operation": "lock"})
            ),
            "system:power:lock"
        );
        // Omitted operations must canonicalize to execution defaults so Always
        // on a list/status call cannot parent-match mutating sibling ops.
        assert_eq!(
            permission_key("system", &serde_json::json!({"scope": "env"})),
            "system:env:list"
        );
        assert_eq!(
            permission_key("system", &serde_json::json!({"scope": "registry"})),
            "system:registry:list"
        );
        assert_eq!(
            permission_key("system", &serde_json::json!({"scope": "power"})),
            "system:power:status"
        );
        assert_eq!(
            permission_key("system", &serde_json::json!({"scope": "info"})),
            "system:info"
        );
        // Non-routing tools ignore operation/scope in args.
        assert_eq!(
            permission_key(
                "mcp_srv_tool",
                &serde_json::json!({"operation": "run", "scope": "x"})
            ),
            "mcp_srv_tool"
        );
        assert_eq!(permission_tool_root("system:power:lock"), "system");
    }

    #[test]
    fn permission_key_candidates_walk_parents() {
        assert_eq!(
            permission_key_candidates("system:power:lock"),
            vec!["system:power:lock", "system:power", "system"]
        );
        assert_eq!(permission_key_candidates("shell"), vec!["shell"]);
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
            id: None,
        };

        let json = serde_json::to_string(&msg).unwrap();
        let decoded: CanonicalMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.role, CanonicalRole::User);
        assert_eq!(decoded.content.len(), 1);
    }

    #[test]
    fn serde_roundtrip_canonical_message_with_tool_calls() {
        let msg = CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![],
            tool_calls: Some(vec![CanonicalToolCall {
                id: "tc1".into(),
                name: "exec".into(),
                arguments: serde_json::json!({"path": "/tmp"}),
            }]),
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
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
