# 跨层输出契约清单

> 审计日期：2026-09-28
> 范围：生产代码中有跨 crate 消费者的存储端口、服务/trait、工具与事件边界，以及全部已注册 Tauri 命令成功响应的静态签名。
> 背景：ADR 0383 完成了 Memory typed-store 构造和 Action status/list projection 的阶段范围验收；它没有完成全仓输出审计。

## 审计方法与判定

审计从 crate 根导出和公开 port/store/service 开始，跟踪有生产调用证据的消费者；检查返回类型和别名中的 struct、enum、`String`、标量、`Option`、`Vec`、map、tuple、`serde_json::Value`，再追到实际 Serde、工具输出、事件或 Tauri 边界。Tauri 命令部分以 Rust command registry、handler 签名、前端 contract registry 和校验脚本交叉核对。分类不根据搜索到 `Value` 与否决定。

清单使用以下结论：

- **typed**：命名类型或明确标量在内部边界保持类型化；若直接作为 Tauri/tool DTO，还可能有领域类型变更带动 wire 形状变化的风险。
- **dynamic JSON**：输出本身允许调用方、配置路径、provider 或协议决定 JSON 形状；记录动态数据 owner 和最终序列化边界。
- **local-only**：本地 Rust 生命周期、缓存、路径或 worker 数据，不进入外部序列化边界。
- **further review**：当前形状和 owner 已分类，但有稳定语义经 tuple、字符串、`Value` 或领域对象传播，值得独立收口。

## 跨 crate 输出边界

| 生产者 → 消费者 | 输出及稳定性 | JSON / Serde 边界与 owner | 结论 |
|---|---|---|---|
| Memory `SessionStore` → Agent resume/context/runtime、App commands、Tools Admin | 会话记录、标题上下文、窗口文本、resume projection、usage、事件页和游标分别使用 `Session`、`SessionMessageText`、`SessionResumeMedia`、`SessionResumeProjection`、`SessionUsage`、`SessionEventPage`、`SessionCursor` 等具名结果；历史/搜索返回 `Vec<Session>`，计数返回 `i64`，标题/记录按需返回 `Option<String>` 或 `Option<Session>`。生产调用见 `crates/memory/src/repositories/session_events.rs`、Agent `resume.rs`/`layer.rs` 和 App `commands/session.rs`/`commands/history.rs`。 | Agent 消费 Rust 类型并负责 replay/resume 编排。App 将 resume projection 映射到命令 DTO；history 命令目前把领域 `Session` 直接交给 Tauri。Memory 持久化和事件契约由 Memory owner，IPC mapping 由 App owner。 | **typed**；直接将 `Session` 序列化到 IPC 有 DTO 漂移风险，列入后续审查。 |
| Memory `SessionStore` replay → Agent | `load_replay_state[_async]` 返回 `Option<SessionReplayState>`；事件 append/read 返回 `SessionEvent`、`Option<SessionEvent>`、`Vec<SessionEvent>`；ingress 和 replay 辅助读取返回 `Vec<Message>`、`Vec<String>`、游标/序号。`StoredBranchPoint = (SessionEvent, usize, u32, Option<String>)` 经公开别名承载 event、cursor、step number、last message timestamp。 | 内部使用具名 Rust/Serde 模型，无 Tauri 序列化。Durable event `payload: String` 是 Agent `TranscriptRecord` 的版本化 JSON 持久化边界：Memory owner 序号、schema version 和存储；Agent owner payload 语义及 replay。 | 主体 **typed/versioned**；branch-point tuple 有稳定语义但位置型，列为 **further review**。 |
| Memory stores → Agent `memory_worker` / `MemoryIndex` | `MemoryStore` pending extraction 返回 `Vec<(String, bool)>` 和 `Vec<(String, String)>`；episode text 返回 `Option<String>`；extraction cursor 返回 `Option<String>`；其他 enqueue/advance/clear 操作返回 `()`。Recall、embedding、maintenance 读取使用 `MemoryHit`、`MemoryRecall`、`PendingMemoryEmbedding`、`MemoryEmbeddingSaveReport`、`ContradictionCandidate`、`PredicateCount` 等具名类型，另有 `Vec<String>`、`Vec<usize>` 和计数。 | 这些是 Agent worker/index 使用的内部 Rust 数据，无 JSON 边界。Memory owner 持久化读取结果和过滤；Agent owner extraction/vector/maintenance policy。 | 具名结果 **typed**；两类 extraction tuple 按位置表达稳定业务字段，列为 **further review**。worker cursor、episode text 和 unit ack 属 **local-only**。 |
| Memory fact/repository ports → Tools `MemoryTool`、App memory commands、Agent `MemoryService` | `FactStore` 返回 `Vec<Fact>`、`Fact`、`MemoryRecall`、`u64` 或 `()`；`MemoryRecallStore` 返回 `Vec<MemoryHit>`/`MemoryRecall`；embedding 与维护端口返回具名行、`Vec<String>`/`Vec<usize>` 和计数。 | App 命令经 Tauri 序列化，工具经 `ToolResult.output` 投影到工具 JSON；Memory 管存储/可见性语义，App/Tools 分别拥有外部投影。`Fact` 是领域/存储类型，直接用于 IPC 或工具输出有 DTO 漂移风险。 | **typed**，并标记仓储实体复用为 wire DTO 的风险。没有生产跨 crate 消费证据的 `FactPresence` tuple/map alias 暂列 **local-only / caller 待核实**。 |
| Memory `ActionStore` → Tools `ActionService` | list/get/claim 返回 `Vec<ActionRow>`、`Option<ActionRow>`、`Option<ActionCompletionOutboxRow>`；ack/delete/lifecycle 返回 `bool`、`usize` 或 `()`。`ActionCompletionOutboxRow.status_json: Value` 在构造处字段固定，Tools/Agent 按完成状态字段读取。 | store row 在 Rust 内部流转；Memory owner durable rows/outbox，Tools owner job lifecycle。`status_json` 在 durable row 与 Agent 消费间仍为 JSON string/value data，尚无专用状态 DTO。 | 主体 **typed**；后台完成状态 JSON 形状固定且有消费者依赖，列为 **further review**。 |
| Agent services / ports → App adapters、Tools、event bridge | `AgentLayer` 提供 `MetricsSnapshot`、`LlmConnectionReport`、`MemoryRecall`、`u64`/`usize` 计数和 `()` ack；tool/catalog/prompt/auth ports 返回 `PromptCatalogVersions`、`PromptCatalogContent`、`ToolCatalogSnapshot`、`RiskLevel`、`AuthorizationRequest`、`Vec<ToolRegistration>`、`ToolResult`、`bool`/`()`, observation `String`。 | 多数为内部 Rust ports，由 Agent 持有执行语义、App 组合层持有 adapters。`ToolResult.output: Value` 是异构工具结果边界。`AgentEvent` 经 App `event_bridge` 映射后才是 Tauri event DTO；Agent event 自身的 Serde 不是 wire contract。 | 外层 **typed**；工具输出和 `WebSearch.result: Option<Value>` 等扩展字段 **dynamic JSON**。Agent/App 分别持有 event 语义与 IPC 映射。 |
| Tools catalog/manager → Agent adapters、App、tool wire | 输出包括 `ToolCatalogSnapshot`、`Vec<ToolDef>`、`Vec<McpServerConfig>`、运行时/上下文具名 DTO、`RiskLevel`/`AuthorizationRequest`、`Option<ToolBox>`、`ToolResult`、`u64` 版本和 `(u64, u64)` 版本 tuple。`build_mcp_index`/session schema list 返回 `Vec<Value>`。 | 工具 schema 最终序列化到 provider/tool catalog；prompt index 在 Tools→App adapter→Agent 内部流转，固定含 `name/description/tool_names/tool_count`，不属于 MCP 远端 schema。Tools owner catalog/execution；App owner Tauri projection。 | Tool、runtime 外层 **typed**；Tool input/output 和 schema **dynamic JSON**；固定 prompt index `Vec<Value>` 是 **further review**。version tuple 是 **local-only cache state**。 |
| Tools `ActionService` → Agent、App event mapper、tool wire | `board -> Vec<ActionView>`、session list/status/scheduled list 返回 `ActionListView`/`ActionStatusView` 等命名投影；cancel/delete 返回 `bool`/`Result<bool>`，restore 返回计数或内部组合结果。 | ADR 0383 已将稳定 status/list projection 类型化。App/tool owner 在各自输出边界序列化。`ScheduledActionView.tool_args: Option<Value>` 是动态工具参数。`EventSink = Fn(String, Value)` 的事件名/部分字段有约定，但签名未约束 payload；Tauri 由 App 事件桥承载。 | status/list **typed**；tool args **dynamic JSON**；Action `EventSink` 是 **further review**。Tools 拥有 Action projection，Tools/App 需明确 sink payload owner。 |
| `ToolExecutionPort` / `ToolResult` → Agent ReAct、tool/provider output | 执行输入含 `Value`；结果 envelope 的 success/outcome/error/retry/usage 等有命名类型，`output: Value` 是逐工具异构结果。`ToolDef.input_schema: Value` 和 operation schema 同样动态。 | `TypedToolOperation::Output: Serialize` 由 `TypedToolAdapter` 序列化进 `ToolResult.output`；LLM/provider 或 Tauri 再序列化工具结果。Tools 拥有执行 envelope 和 adapter；Agent 拥有调用/观察。 | **dynamic JSON boundary**；内部已具名 envelope，不把异构 payload 强行改为统一 DTO。 |
| LLM client/router → Agent、Tools、App、Memory | completion 返回 `LlmResponse`，stream 返回具名 chunks，embedding 返回 `Embedding` 或 `Vec<f32>`，transcription/OCR/TTS/image generation 返回具名媒体类型或 `Vec<u8>`，health 返回 `()`，connection status 返回 `LlmConnectionReport`。 | 内部 provider-neutral ports 不直接作为 IPC DTO。`haven-llm` 拥有 provider wire mapping 与规范化；Agent/Tools/App 拥有调用或 UI projection。`CanonicalToolCall.arguments`、JSON schema、provider thinking/web-search extras 和原始 payload 保留 JSON。Memory usage 的 `cache_diagnostics: Option<Value>` 是另一存储边界。 | 主要 **typed**；provider/tool extension payload **dynamic JSON**；usage diagnostic 的持久化映射列为 **further review**。 |
| MCP client/manager → Tools、App | 结果包括 `Vec<McpToolInfo>`、`McpServerSnapshot`/其向量、`McpReconcile`、`McpCallOutput`、状态/诊断具名类型、`Vec<String>` server names 和 `Result<()>` ack。 | MCP JSON-RPC 自己序列化远端请求/响应；Tauri snapshot 由 App 过滤/投影；Tools adapter 把 call output 映射到 `ToolResult`。`McpToolInfo.input_schema: Value`、`McpCallOutput.output: Value` 是远端 schema/content。MCP owner protocol/client，Tools owner adapter，App owner UI DTO。 | 快照/状态外层 **typed**；远端 schema/tool content **dynamic JSON**。`McpReconcile` 的全字段投影和敏感 URL/config 输出需 **further review**。 |
| SkillsEngine → Tools、App | `list -> Vec<SkillInfo>`、`get -> Option<SkillInfo>`、enabled filter `Option<Vec<String>>`、resolved root `PathBuf`、folder signature `Vec<(PathBuf, SystemTime, u64)>`；execution 返回 `ToolResult`。 | `SkillInfo` 经 Tauri 序列化；领域 `Skill`/manifest、root 和 watcher signature 是本地对象。Skills 拥有元数据，Tools 拥有执行/catalog，App 拥有 IPC mapping。 | `SkillInfo` **typed**，但领域类型直接做 IPC DTO 有漂移风险；路径/watcher **local-only**；script/stdout tool payload **dynamic JSON**。 |
| Common config/types → App、Agent、Tools、LLM | `new_id -> String`；config snapshot/settings 和 `ConfigUpdate<T>` 是具名类型；subscriber 返回 typed receiver；media planner 返回 `MediaPlan`/`MediaInput`。ID newtypes 按字符串 Serde。Common config 同时由 LLM/Agent/Tools/App 消费。 | Common 拥有共享 schema/ID 语义和配置持久化；各消费 crate 拥有自己的外部投影。动态配置路径只在 Admin `config_get` 处输出。Canonical tool arguments、JSON content、structured media extension 和 tool schemas 保留 JSON。 | 主要 **typed**；ID 字符串格式固定但未用 newtype 表达；provider/tool/config extension JSON 是 **dynamic boundary**。 |
| Input pipeline → App recording commands | VAD/state 使用 `VadState`/`RecordingState`；capture/transcription 返回 `RecordingResult`/文本；WAV encode 返回 `Vec<u8>`，start/cancel/shutdown 返回 `()`。 | Input owns capture lifecycle; App maps results to command/event DTO at Tauri edge. Bytes/text are operation payloads, not shared stable DTOs. | 内部 **typed/local**，Tauri mapping owner 为 App。 |

## AdminServices 输出逐项清单

Admin 操作最终由 `TypedToolAdapter` 或 `AdminSurfaces.execute` 序列化到通用 `ToolResult.output`。服务层不应因此先把所有固定形状擦成 `Value`。最终工具 payload 仍是动态 wire JSON；配置读取和 provider/tool 内容等动态结果继续保留 JSON。

| `AdminServices` producer | 当前输出分支 | 稳定性、序列化边界与 owner | 判定 |
|---|---|---|---|
| `config_get` | 无路径时为已脱敏 `Settings`；路径存在时为任意配置子树；缺失路径走 error。 | `ConfigService` 拥有配置；`AdminServices` 拥有敏感字段遮蔽；Admin tool output 边界输出 JSON。 | **保留动态 JSON**，任意 path 是设计能力。 |
| `logs_level` | `{level, saved, version}`。 | LogLevel、版本值固定；Admin 只在 tool output 边界序列化。 | **typed DTO**。 |
| `tool_set` | `{name, enabled, saved, note}`。 | 固定 ack；工具启停策略由 Admin/Tools 持有。 | **typed DTO**。 |
| `diagnostics_status` | config/settings 或 config_error；可选 model-health map；tools count/names；MCP 或 mcp_error；skills；session counts 或 unavailable；log path。 | 外层字段固定。settings 经 mask；模型 map key 来自 `RequestKind::ALL`；诊断文本受 sanitizer；session read/count 可分别部分失败。Admin/Tools 持有聚合与安全过滤。 | **typed DTO**，保留可选字段和部分失败分支。 |
| `logs_tail` | 成功 `{path,total_lines,lines}`；读取失败 `{path,error}`；行文本过滤敏感 marker 并截断。 | 文件路径/日志自由文本是动态 scalar；形状由 Admin 持有，最终输出在 tool edge。 | **typed DTO + string payload**，保留失败分支与 sanitizer。 |
| `sessions` / `errors` | `{sessions:[rows...]}` 或 `{errors:[rows...]}`；无 SessionStore 时 `{unavailable:true}`；成功允许空数组。Session rows 含 id/status/title/input_chars/created_at/updated_at；error rows 含 id/title/input_chars/created_at。 | Admin 从 SessionStore 投影，不泄露 input_text；列表顺序/limit 由 SessionStore 和 Admin 共同决定。 | **typed DTO**，测试空值、unavailable、过滤和顺序。 |
| `skills_list` | `{skills:[{name,enabled,description,root}]}`；允许空列表。 | SkillsEngine 拥有元数据；绝对 root 是既有 wire 输出，保留现有形状。 | **typed DTO**；回归不得扩出 script 内容。 |
| `skill_set` / `skill_create` | toggle `{name,enabled,saved,note}`；create `{name,created,root,has_script}`。 | name/root 自由字符串；create 的 root 是既有绝对路径输出。durable config failure 及回滚日志由 Admin/Skills 持有。 | **typed DTO**，保留字段和值。 |
| `mcp_status` | 数组行 `{name,enabled,connected,tools,last_error,diagnostic}`；diagnostic 不可用时当前 wire 是 `null`。 | `McpServerConfig`/McpManager 拥有运行状态；诊断/error 经 sanitizer；数组顺序来自当前 config map。 | **typed DTO**；保留 `diagnostic: null`。 |
| `mcp_connect` / `mcp_disconnect` / `mcp_reconnect` | `{name,connected}`。 | 固定 ack；MCP 管连接、Admin 拥有 config gate；reconnect error 区分 preflight 和副作用失败。 | **typed DTO**。 |
| `mcp_add` | 新增 ack `{name,enabled,saved,connected}`，自动连接失败时另含 `warning`；重复名称可走 config-update ack。 | 连接失败可能发生在 config durable save 之后；warning 需 sanitized，不能把 command/env 值输出。AdminServices 组装，tool edge 序列化。 | **typed DTO enum/可选字段**，覆盖 duplicate/partial failure。 |
| `mcp_update` / `mcp_toggle` | `{name,enabled,saved,connected}`。 | 固定结果；连接或持久化失败保持现有副作用错误语义和 rollback 策略。 | **typed DTO**。 |
| `mcp_remove` | `{name,removed,connected}`。 | 固定 ack，MCP config/client owner 为 AdminServices/McpManager。 | **typed DTO**。 |
| `mcp_reload` | `{reloaded,connected:[{name,connected:true}|{name,connected:false,error}]}`。 | 每台 server 独立失败，错误经 sanitizer；既有成功行无 error 字段，失败行才有。 | **typed DTO enum**，保留 partial failure 和字段省略。 |
| `mcp_refresh` | `{added,removed,updated,failed}`，每项是 server name 字符串。 | ToolResult generic output 后供 App command 构造 `McpRefreshResult`；确认完成时 `commands/mod.rs` 从 output 读取 failed 并与已授权 plan 交叉过滤。Tools/Admin 负责输出；App 负责 Tauri DTO 与 authorized-name filter。 | **typed service result → tool JSON boundary**；保留 App 当前 plan filter 回归。 |

## Tauri 命令成功响应（71 个）

命令成功类型由 Tauri IPC 序列化；失败为 `Result<T,String>` 的 error 字符串并拒绝前端 invoke。下面按 Rust handler 返回的 `T` 分组，命令名来自 Rust 与 TS command registry。

| Rust 成功响应类型 | 命令 | 形状分类与 owner |
|---|---|---|
| `Vec<ActionEvent>` | `list_actions`, `list_action_history` | 固定 UI projection；Tools owner lifecycle，App events/commands 负责 IPC DTO。 |
| `bool` | `cancel_action`, `delete_action`, `is_autostart_enabled` | 标量 ack/state；owner 分别为 ActionService 或 App autostart adapter。 |
| `()` | `open_external`, `log_frontend_error`, `reconnect_mcp`, `add_mcp_server`, `update_mcp_server`, `remove_mcp_server`, `toggle_mcp_server`, `delete_fact`, `switch_model`, `set_reasoning_effort`, `set_web_search`, `start_recording`, `cancel_recording`, `reopen_session`, `end_session`, `interrupt_session`, `resolve_confirmation`, `update_session_title`, `delete_session`, `rollback_session`, `continue_session`, `update_settings`, `revoke_permission`, `reset_permissions`, `enable_autostart`, `disable_autostart`, `refresh_skills`, `set_skill_enabled`, `set_tool_enabled`, `reset_tool_circuits` | 成功时无 payload；mutation owner 是相应 App/Tools/Memory service，IPC 错误走 String。 |
| `Vec<Session>` | `get_history`, `search_history_paginated`, `search_history`, `search_history_filtered` | 形状可序列化但直接暴露存储/领域类型；历史 wire owner 为 App，**DTO 漂移风险**。 |
| `i64` | `count_history`, `count_history_search` | 固定标量计数；SessionStore producer，App handler/Tauri edge。 |
| `String` | `export_history`, `stop_recording`, `get_bootstrap_status`, `open_skills_dir` | 混合语义：导出/转写/路径是文本 payload；bootstrap 只有 Loading/Ready 两态但目前作为 String 暴露，列为轻量 typed enum 审查。 |
| `LogInfo`, `LogTail`, `MetricsSnapshot` | `get_log_info`, `read_log_tail`, `get_performance_metrics` | 命名响应类型；日志路径/文本是自由数据，Metrics 为受限 counters；App owns Tauri serialization。 |
| `Vec<McpServerSnapshot>`, `McpRefreshResult`, `McpToolCallResponse` | `list_mcp_tools`, `refresh_mcp_servers`, `mcp_tool_call` | 外层 MCP snapshots/result DTO typed；`input_schema` 和 tool output 可保留远端动态 JSON；Mcp crate owns protocol fields，App owns renderer-safe projection。 |
| `u64` | `run_memory_maintenance`, `clear_history` | 固定计数；Agent/Memory owns maintenance/deletion，App owns command mapping。 |
| `Vec<MemoryRecallItem>`, `Vec<Fact>`, `Fact` | `recall_memory`, `list_facts`, `add_fact` | Named result; Fact direct domain type has DTO drift risk; App command and Memory own filtering/write semantics. |
| `ApiKeyStatus`, `LlmConnectionReport`, `Vec<ModelInfo>`, `BTreeMap<String, Vec<ModelInfo>>` | `get_api_key_status`, `check_llm_connection`, `discover_models`, `discover_all_models` | Outer DTOs typed; provider names form a dynamic map key set derived from configuration; LLM owns provider lookup, App owns command contract. |
| `RecordingState`, `ProcessResult` | `get_recording_state`, `process_transcript` | Named outputs; ProcessResult is a closed Rust enum but frontend consumes through `invoke: Promise<any>` and has legacy string handling, so add response contract review. |
| `SessionListResponse`, `SessionResumeResponse`, `Option<SessionResumeResponse>` | `get_sessions`, `get_session_for_resume`, `get_last_conversation` | Typed session projection, serialized at Tauri edge; App owns response mapping. |
| `Settings`, `Vec<StoredPermission>`, `ShellAvailability` | `get_settings`, `list_permissions`, `check_shell_available` | Named result types; Settings carries broad/open configuration shape but credentials are masked; App/Common own config projection and persistence. |
| `Vec<SkillInfo>`, `SkillExecutionResponse`, `ToolListResponse` | `list_skills`, `execute_skill`, `get_tools` | Fixed envelope; skill/tool execution and schemas may contain dynamic extension fields; Skills/Tools own values, App owns IPC projection. |

`commands/contracts.rs` and `ui/src/lib/contracts/commands.ts` inventory request/response names, while `check-ipc-contracts.ps1` checks registry/handler/docs names and count and selected field/type groups. It does not generically compare all 71 Rust handler return signatures against response labels. `invoke` currently exposes `Promise<any>`. Events are a separate 40-channel output path: Rust DTO registration plus App `event_bridge` mapping and TS event mappers are the owners. The mapped event DTOs are the wire contract; `AgentEvent` alone is not.

## 明确保留的动态 JSON 边界

这些 `Value` 或动态 map 是有 owner 的扩展载荷，不是 typed-output 验收的清零目标：

- Admin `config_get` 任意配置 path；`AdminServices` 拥有 masking，ConfigService 拥有配置树，tool output 边界负责最终 JSON。
- Tool arguments、逐工具 `ToolResult.output`、tool input schema / JSON Schema、Skill/MCP 动态工具注册；Tools/operation adapter owns wire projection。
- MCP `input_schema`、`tools/call` arguments/results 和 JSON-RPC payload；MCP crate owns protocol serialization。
- LLM provider raw extensions（thinking、web-search、opaque provider payload）和动态工具 arguments；LLM adapter owns provider wire mapping。
- Action `tool_args`、Agent WebSearch result 等上游 tool/provider 扩展字段；Tools/Agent own semantics，App mapper controls Tauri projection。
- 按配置 provider 名索引的模型结果 map；Model config/provider inventory owns keys，App owns Tauri result type。

## 具名 owner 的后续审查项

下列输出已记录生产者、消费者和当前序列化边界；后续可以按独立切片决定是否补 DTO，不阻止本清单标明其 owner：

1. Memory extraction/replay positional tuples；Memory owns durable representation，Agent owns interpretation。
2. `ActionCompletionOutboxRow.status_json`；Memory owns persisted record production，Tools/Agent own completion interpretation。
3. 固定字段 MCP prompt index 的 `Vec<Value>`；Tools produces、Agent prompt port consumes。
4. Action `EventSink(String, Value)` payload owner/shape；Tools event producer 与 App Tauri mapper 共同维护。
5. `LlmCallUsage.cache_diagnostics: Option<Value>` durable serialization；LLM produces diagnostic, Memory persists it.
6. Tauri 直接返回的 `Session`/`Fact`/`SkillInfo` domain types，`ProcessResult` TS shape，bootstrap status String enum，provider-keyed maps 与少量 command families；App/owning domain must keep response shape synchronized.
7. MCP `McpReconcile` full field projection and renderer-safe config fields; MCP produces, App owns IPC projection.

## 审计边界

本清单按有证据的 production cross-crate ports/stores/services 与已注册 IPC 输出分类；没有把 `rg` 命中当成生产调用证据，也没有逐项列出所有 crate-private helper、未消费 public helper、测试接口、本地缓存/watch tuple。Tauri 的 71 个命令按静态成功签名归类，但 contract checker 只做全量命令名/计数以及部分响应字段校验；未重放 71 个命令的全部运行时分支。LLM/provider wire 内部类型和少数 Common/Tools public helpers 也未逐个列出。未展开的方法面归其定义 crate 所有，并属于 future audit；本清单不声称是 workspace 所有 Rust `pub fn` 的字面穷举。

该边界让输出分类可复核：对每个已确认生产输出记录其稳定性、消费者、序列化位置、owner，以及 typed/dynamic/local/further-review 结论。动态 JSON 的存在本身不阻止验收；仍需逐项维护本表中的 owner 和动态边界说明。
