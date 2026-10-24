# Tauri IPC 契约目录

本目录记录稳定的 Tauri 命令与事件边界。Rust 端 wire payload 使用 `snake_case`；前端只在
`ui/src/lib/contracts/` 与 `ui/src/lib/events.ts` 的监听封装中转换为 `camelCase`。业务状态不得在
路由深处重复转换字段。

命令 boundary 与 security metadata 由 `crates/app-binary/src/commands/contracts.rs` 单点登记，IPC 生成器据此生成前端 registry 与下方文档表；检查器逐字段拒绝生成物 drift。`scripts/generate-ipc-contracts.ps1` 也从 Rust Tauri handler 参数和 Serde DTO 生成前端 request/response 类型，并为每个 command response 生成运行时 wire shape。事件 channel 与 Rust payload DTO 显式配对，生成器覆盖全部登记渠道并在集合漂移时失败。中心 `invoke` 和各事件 domain mapper 在语义投影前执行 shape 检查；具体 ID、闭合 enum、状态转移和跨字段约束仍由业务 contract 校验。CI 检查生成物 drift，并核对 handler、注册表和本文命令名集合。wire 字段仍保持扁平（例如 `{ sessionId }`），不额外包裹 `{ request: ... }`。
命令目录不提供固定版本号或旧命令别名。

## 全量命令登记

以下表格由 Rust command registry 生成，列出完整命令集合及人工审阅的边界、安全不变量。wire shape 由 Rust handler 与 Serde DTO 生成，并在运行时统一检查；响应中的 `Value` 仅用于明确的 provider、动态工具 schema 或工具输出扩展点。

<!-- BEGIN GENERATED COMMAND SECURITY METADATA -->
| 命令 | 边界 | 安全不变量 |
|---|---|---|
| `add_fact` | mutate | credential-like predicates and values rejected |
| `add_mcp_server` | execute | AuthorizationPort; typed native admin operation validates and persists config |
| `cancel_chatgpt_plan_login` | mutate | cancels one in-memory OAuth attempt and aborts its loopback callback listener |
| `cancel_mcp_feature_input` | execute | explicit UI cancellation of one pending selected resource or prompt interaction |
| `cancel_recording` | execute | cancel clears the in-flight recording id |
| `cancel_schedule` | mutate | canonical schedule and owner-session IDs; owner-scoped cancellation |
| `cancel_tool_run` | mutate | only a canonical toolrun ID can cancel background tool execution |
| `check_llm_connection` | read | status and non-sensitive reason only; no endpoint or provider payload |
| `check_shell_available` | read | availability boolean only |
| `clear_facts` | mutate | delete all saved long-term facts and invalidate derived caches |
| `clear_tool_run_history` | mutate | delete terminal task history; preserve live work and undelivered results |
| `complete_chatgpt_plan_login` | mutate | validates OAuth state, issuer, audience, nonce and signature; stores token and granted-scope state only in secure storage |
| `continue_session` | mutate | replay durable session_events and truncate partial projection |
| `count_session_history` | read | read-only aggregate |
| `count_session_history_search` | read | parameterized read-only search |
| `delete_all_sessions` | mutate | clears persisted sessions and session trust |
| `delete_fact` | mutate | delete one fact by id |
| `delete_session` | mutate | delete by session id and release runtime state |
| `disable_autostart` | execute | managed autostart entry only |
| `discard_staged_chatgpt_plan_credential` | mutate | deletes only the exact uncommitted ChatGPT plan credential reference abandoned by the settings draft |
| `discard_staged_credentials` | mutate | deletes staged values not committed by a Settings save |
| `discover_models` | execute | http(s) endpoint; typed auth scheme for an explicitly entered key; stored keys require a matching configured endpoint |
| `enable_autostart` | execute | release build only |
| `end_session` | mutate | explicit user termination |
| `execute_skill` | execute | AuthorizationPort; confirmation queues direct calls and errors are renderer-safe |
| `fork_session_from_message` | mutate | canonical parent and user message ids; creates an independent re-keyed transcript prefix |
| `get_api_key_status` | read | boolean presence only; credentials excluded |
| `get_bootstrap_status` | read | status enum only |
| `get_latest_session_for_resume` | read | most recent persisted session only |
| `get_lifecycle_hooks_settings` | read | Hook executable config and exact trusted roots; diagnostics omit commands, payloads, and output |
| `get_log_info` | read | path is optional; no environment details |
| `get_mcp_prompt` | execute | explicit user-selected prompt and declared arguments; enabled live target binding; bounded untrusted messages |
| `get_performance_metrics` | read | bounded content-free backend counters plus renderer stream counters |
| `get_session_event_sequence_watermark` | read | canonical session id; returns only its durable event high-water mark |
| `get_session_execution_mode` | read | canonical session id; reads its durable execution mode |
| `get_session_execution_workspace` | read | canonical session id; reads its durable workspace binding |
| `get_session_for_resume` | read | session-scoped persisted projection |
| `get_session_lineage` | read | parent and direct child sessions of the selected session only |
| `get_settings` | read | config response redacts credentials and MCP environment values |
| `get_settings_runtime_apply_status` | read | per-runtime-target config versions and sanitized apply status only |
| `interrupt_session` | mutate | pauses the selected active session without deleting it |
| `is_autostart_enabled` | read | state boolean only |
| `list_builtin_tool_manifests` | read | builtin manifest projection; schemas are dynamic extension data |
| `list_fact_proposals` | read | bounded pending-only queue; credential-like values filtered |
| `list_facts` | read | read-only fact projection |
| `list_failed_transcriptions` | read | returns bounded app-owned retry metadata without audio bytes or transcript text |
| `list_mcp_prompts` | execute | user-driven prompt discovery; enabled live target binding; no model callable surface |
| `list_mcp_resource_templates` | execute | user-driven template discovery; enabled live target binding; bounded opaque URI templates |
| `list_mcp_resources` | execute | explicit host discovery; enabled live target binding; bounded opaque resource metadata |
| `list_mcp_servers` | read | snapshot only; env values redacted; invocation remains gated |
| `list_runtime_sessions` | read | resident nonterminal runtime sessions only |
| `list_schedules` | read | safe projection excludes dynamic arguments and continuation prompt |
| `list_session_history` | read | read-only session projection |
| `list_session_permissions` | read | typed session grants with exact session, capability, target, and effect |
| `list_skills` | read | metadata projection |
| `list_tool_run_history` | read | optional session filter; limit capped at 200; internal tool args excluded |
| `list_tool_runs` | read | projected task fields only |
| `log_frontend_error` | mutate | fixed renderer failure category only; error text and payload are not mirrored |
| `open_external` | execute | AuthorizationPort; queues AppCommand confirmation; http(s) or validated absolute local path only |
| `open_skills_dir` | execute | AuthorizationPort; queues AppCommand confirmation; configured skills root only |
| `pick_execution_workspace_directory` | read | user-selected local directory candidate only; validates but grants no file access |
| `read_log_tail` | read | bounded tail; file logging must be enabled |
| `read_mcp_resource` | execute | explicit UI selection; enabled live target binding; opaque URI; bounded untrusted content |
| `reapply_saved_settings` | mutate | reapplies only pending live targets from the latest saved config snapshot under the shared config apply gate |
| `reassign_staged_chatgpt_plan_credential` | mutate | moves one exact staged ChatGPT credential reference to another ChatGPT plan provider name without exposing its secret |
| `reassign_staged_provider_credential` | mutate | moves one exact staged provider API key reference to another provider name without exposing its secret |
| `recall_memory` | read | bounded and credential-filtered recall |
| `reconnect_mcp_server` | execute | AuthorizationPort; typed native operation reconnects one existing configured server after final version check |
| `refresh_mcp_servers` | execute | AuthorizationPort; one batch over persisted config diff and its affected targets; no renderer process arguments |
| `refresh_skills` | execute | configured skills root scan |
| `remove_mcp_server` | execute | AuthorizationPort; typed native admin operation removes client and config |
| `reopen_session` | mutate | session id selects persisted session |
| `reset_permissions` | mutate | clears permanent rules only; keeps session grants and selected default policy |
| `reset_session_permissions` | mutate | clears durable session grants only; keeps permanent rules |
| `reset_tool_circuits` | mutate | clears local circuit state only |
| `resolve_confirmation` | mutate | owner/request id selects one registry; receipt, effect, scope, target and expiry are revalidated; retryable failures keep pending |
| `respond_mcp_feature_input` | execute | explicit user response to a bounded form request; enabled live target binding; schema-validated answers |
| `retry_failed_transcription` | execute | canonical file identity selects only an app-owned retained recording and reuses its capture-time session target |
| `review_fact_proposal` | mutate | canonical fprop ID; one-shot transactional accept/reject |
| `revoke_permission` | mutate | non-empty exact permanent key; session grants are retained |
| `revoke_session_permission` | mutate | non-empty session id and capability; removes one session grant |
| `rollback_session` | mutate | event cursor and projection clock rollback |
| `run_memory_maintenance` | mutate | maintenance path owns purge and embedding cleanup |
| `save_lifecycle_hooks` | mutate | typed validated Hook config; execution remains subject to the active security policy |
| `search_session_history_filtered` | read | bounded page and date-filtered projection |
| `set_hotkey_capture_active` | mutate | transient renderer key-capture state only; not persisted |
| `set_reasoning_effort` | mutate | request kind selects the assigned model; validated before config save |
| `set_recording_target_session` | mutate | validated transient session target for the next app-owned capture; monotonic revision rejects stale renderer updates |
| `set_session_execution_mode` | mutate | canonical session id; binds one execution preset to that session |
| `set_session_execution_workspace` | mutate | canonical session id; local directory is validated and bound to that session |
| `set_skill_enabled` | mutate | AuthorizationPort; typed native admin operation persists the toggle |
| `set_tool_enabled` | mutate | AuthorizationPort; typed native admin operation persists the toggle |
| `set_web_search` | mutate | request kind selects the assigned model; provider capability checked before config save |
| `stage_independent_search_credential` | mutate | writes a model's Alibaba AI Search key to secure storage and returns only an opaque reference |
| `stage_ocr_credential` | mutate | writes OCR secret to secure storage and returns only an opaque reference |
| `stage_provider_credential` | mutate | writes provider secret to secure storage and returns only an opaque reference |
| `start_chatgpt_plan_login` | execute | starts a state- and PKCE-bound loopback OAuth callback and returns only the official authorization URL |
| `start_recording` | execute | input pipeline owns capture lifecycle |
| `stop_recording` | execute | capture stops before asynchronous transcription |
| `submit_transcript` | execute | attachment limits and file persistence are enforced |
| `subscribe_mcp_resource` | execute | explicit UI selection from current resource list; enabled live target binding; bounded acknowledged subscription |
| `switch_model` | mutate | requestKind selects the route; modelConfigId is an assigned, capability-compatible Haven ModelConfig identity, not a Provider service model ID |
| `toggle_mcp_server` | execute | AuthorizationPort; typed native admin operation connects before enabling |
| `trust_hook_workspace` | mutate | native picker only; canonical local directory; grants project Hook config trust only |
| `unsubscribe_mcp_resource` | execute | explicit UI cancellation; enabled live target binding; cancels only the selected resource stream |
| `untrust_hook_workspace` | mutate | removes only an exact root already present in the trusted Hook list |
| `update_mcp_server` | execute | AuthorizationPort; typed native admin operation validates and reconnects safely |
| `update_session_title` | mutate | trimmed non-empty title only |
| `update_settings` | mutate | rejects inline secrets; shared loader preserves credential refs, tool sections, and permission rules |
<!-- END GENERATED COMMAND SECURITY METADATA -->

`list_facts` / `add_fact` 返回 App-owned `MemoryFactResponse`，以 `fact_id` 表示事实身份；其中 `source_ref` 使用
`MemoryFactSourceRef`。事实的 `source` 使用 generated `MemoryFactSource`；App mapper 严格解析
repository source 值后显式投影字段，repository 新增字段不会自动进入 IPC payload。

## 会话命令（v1）

| 命令 | 边界 | 说明 |
|---|---|---|
| `list_runtime_sessions` | read | 当前进程中驻留且未终结的会话列表；Rust `session_id` / `input_text` / `waiting_reason` 在 renderer 边界映射为 `sessionId` / `inputText` / `waitingReason`；响应仅包含会话切换器所需字段 |
| `get_session_lineage` | read | 当前会话的父会话和直接子会话 |
| `get_session_for_resume` | read | 恢复指定会话所需的持久化投影，包含当前工作文档、其活动 sequence 与同一事件快照的 append-only `event_sequence_watermark` |
| `get_session_event_sequence_watermark` | read | 只读取指定会话 append-only `session_events` 的最大 sequence，用于检测 renderer 漏收的提交事件 |
| `get_latest_session_for_resume` | read | 最近持久化会话及恢复投影（含当前工作文档与事件水位） |
| `reopen_session` / `continue_session` / `end_session` / `interrupt_session` | mutate | 会话生命周期控制；中断保留会话 |
| `rollback_session` | mutate | 回滚分支并同步截断事件和投影 |
| `fork_session_from_message` | mutate | 按精确用户消息身份创建独立子会话；从消息处重放，可能再次触发工具 |
| `update_session_title` | mutate | 更新非空标题 |
| `delete_session` / `delete_all_sessions` | mutate | 删除一条或全部持久会话，并广播 `session:lifecycle(deleted)` |
| `resolve_confirmation` | mutate | 输入 `{ owner, requestId, effect: PermissionEffectInput, scope: PermissionScopeInput, target: PermissionTargetInput, resourceTarget?: PermissionResourceTargetInput }`；Tauri 只接受 Rust permission enum 的规范 snake_case 值。持久 Allow 会从 pending receipt 的 typed 声明计算资源范围；调用方不能提交路径、origin 或任意 scope。owner 只选择唯一 pending registry，AppCommand 不经过 Agent executor；期限由 owner 按登记的 `expires_at` 仲裁，不接受 renderer 的超时决定。结果为 `resolved`、`expired` 或 `stale`；解码/命令错误表示可重试失败，pending UI 保留 |

Tauri 接收前端参数时采用其自动 camelCase → Rust snake_case 映射；页面调用处使用 camelCase。

## 会话生命周期事件（v2）

所有会话生命周期变化共用 `session:lifecycle` 和一个带 `type` 判别字段的 Rust enum。
Agent 事件由 `TauriEmitter` 映射，命令直接发送的事件也必须使用同一常量与 DTO。聊天页、根布局
和记忆视图各自订阅同一事件；唯一字段转换位于 `ui/src/lib/contracts/session.ts`，由
`sessionEventListeners` / `registerSessionLifecycleListener` 承担。

| 事件 | Rust DTO（wire） | 生产者 | 消费者 | 顺序、幂等与敏感字段 |
|---|---|---|---|---|
| `session:lifecycle` | `SessionLifecycleEvent`, `type`: `created`, `updated`, `completed`, `error`, `title_updated`, `deleted` | Agent 生命周期事件、`update_session_title`、删除命令 | 聊天页、根布局、记忆视图 | 只有一个事件事实和一个 payload。`updated.status` 限于 `pending`/`running`/`paused`；终态分别使用 `completed` 与 `error`，同 payload 必须携带已净化的 `reason` 或 `error`，不再发 secondary event，也不需要 `occurrence_id`。`waiting_reason` 仅用于 paused；`run_end_kind=turn_completed` 标记正常 ReAct 轮次结束，会话仍为可继续的 paused；其它暂停不带该字段。完成后同 run 不再有流式事件。创建在首个流式事件前；标题按 `session_id` 覆盖；`deleted.session_id = null` 表示全量清空。不得发送原始输入、摘要、凭据或完整 provider 响应。 |

前端内部对应为 `sessionId`、`targetMessageId` 等 camelCase 字段；只允许监听边界进行转换。

## 任务命令（v1）

| 命令 | 边界 | 说明 |
|---|---|---|
| `list_tool_runs` | read | 当前任务投影 |
| `cancel_tool_run` | mutate | 取消指定任务 |
| `list_tool_run_history` | read | 有界终态历史，可按 session 过滤 |

`ToolRunEvent` 是后台任务面板的唯一公开记录：`{ tool_run_id, kind: 'background', tool_name, status?, session_id?, source_step_id?, started_at?, finished_at?, output?, error?, error_reason?, preview? }`。`source_step_id` 将由 Agent ToolCall 启动的后台工具调用关联回来源步骤。状态只能是 `running`、`completed`、`failed`、`cancelled`。ToolRun 不携带 Schedule 字段、Shell 命令或解释器、退出码、动态 `tool_args`、续接 `prompt` 或本地 `log_path`；目标工具名是必填字段。

## 任务事件（v1）

后端常量与 DTO 位于 `crates/app-binary/src/events.rs`。`haven-tools` 可以维持内部状态 JSON，
但 app shell 必须在 emit 前投影为 `ToolRunEvent`；前端唯一登记表是
`ui/src/lib/contracts/toolRun.ts`，`toolRunEventListeners` 负责 snake_case → camelCase。

| 事件 | Rust DTO（wire） | 生产者 | 消费者 | 顺序、幂等与敏感字段 |
|---|---|---|---|---|
| `tool_run:created` | `ToolRunEvent` | 后台工具调用创建 | 根布局 `toolRunStore` | 在任务对用户可见前发送；按 `tool_run_id` 覆盖合并，重复安全。 |
| `tool_run:updated` | `ToolRunEvent` | 后台任务关联会话 | 根布局 `toolRunStore` | 按 `tool_run_id` 合并，重复安全。 |
| `tool_run:finished` | `ToolRunEvent` | 后台工具调用终态 | 根布局与聊天页 | 后台按 `tool_run_id` 合并；事件丢失时由 `list_tool_runs` / `list_tool_run_history` reconciliation 修复。 |

wire 身份字段为 `tool_run_id`；contracts mapper 在边界将其映射为页面 DTO 的 `toolRunId`。
页面只读取 camelCase DTO，不直接读取 snake_case wire 字段。前台流式输出仍使用独立的
`agent:tool_output`；Schedule 通过自身 DTO 与命令管理，不进入 ToolRun 事件。

## 设置诊断命令（v1）

| 命令 | 边界 | 说明 |
|---|---|---|
| `get_log_info` | read | 日志状态与路径信息 |
| `read_log_tail` | read | 有界日志尾部 |
| `log_frontend_error` | mutate | 记录 `operation_failure` / `unhandled_ui_error` / `unhandled_rejection` 与固定描述，不镜像错误详情 |
| `check_shell_available` | read | `shell` 使用 generated `ShellChoiceInput` 查询已配置 shell 是否可用 |

上述命令的 Rust 响应均为命名 DTO，前端由 `ui/src/lib/contracts/settings.ts` 在消费前校验。
日志内容仍按日志查看器用途返回，不能复用于普通错误提示或其它 IPC 事件。

## 模型设置命令（v1）

| 命令 | 边界 | 说明 |
|---|---|---|
| `get_api_key_status` | read | 凭据存在性，不返回密钥 |

`ApiKeyStatus.models` 的 key 是用户配置的 model id，`providers` 的 key 是用户配置的
provider 名称；两者都是明确扩展点，value 始终为布尔值。其它状态字段由 Rust DTO
和前端 parser 固定。

## 录音与转写命令、事件（v1）

| 命令 | 边界 | 说明 |
|---|---|---|
| `set_hotkey_capture_active` | mutate | 快捷键录入期间抑制录音触发，结束录入时恢复。 |
| `start_recording` / `stop_recording` / `cancel_recording` | execute | 采集与转写生命周期由下列事件报告。 |
| `submit_transcript` | execute | 提交文本与附件进入会话；语音输入可携带 `recordingId`，仅用于关联该次录音，不作为会话身份 |

Rust DTO 定义在 `crates/app-binary/src/events.rs`，前端唯一转换边界是
`ui/src/lib/contracts/recording.ts` 与 `recordingEventListeners`。`rec-*` 为单次录音 ID，
只用于关联录音与转写事件，不能当作会话 ID 使用。

| 事件 | Rust DTO（wire） | 生产者 | 消费者 | 顺序、幂等与敏感字段 |
|---|---|---|---|---|
| `recording:started` | `RecordingEvent { is_recording, recording_id }` | 按钮、热键 | 根布局录音浮层 | 每段录音最多一次；先于 stopped/transcription。仅携带 `rec-*`。 |
| `recording:stopped` | `RecordingEvent { is_recording, recording_id, reason?, duration_ms? }` | 输入管线 | 根布局录音浮层 | Producer 必须带闭集 `reason`；UI mapper 缺失或未知时丢弃事件。在采集停止后；可由取消结束，不保证随后有转写。 |
| `recording:vad_status` | `VadStatusEvent { signal: VadSignalDto, state: VadStateDto }` | 输入管线 | 根布局录音浮层 | 高频、可丢失；闭枚举值由 Rust 生成到 UI，mapper 校验 event ID 和 payload；消费者只保留最后状态（ADR 1018）。 |
| `recording:error` | `RecordingErrorEvent { recording_id, error }` | 录音命令/热键 | 根布局 | 终态；错误为已净化用户文案，不含设备路径或 provider 原始响应。 |
| `transcription:started` | `TranscriptionStartedEvent { recording_id }` | 输入管线 | 根布局 | 在网络转写前，和对应 `rec-*` 关联。 |
| `transcription:result` | `TranscriptionResultEvent { recording_id, text, duration_ms, confidence? }` | 输入管线 | 根布局→会话输入 | `text` 与非负整数 `duration_ms` 必填，mapper 遇到错误类型丢弃事件；空 `text` / 零时长表示静音或过短录音。文本仅交给会话输入，不写日志或其它状态。 |
| `transcription:error` | `TranscriptionErrorEvent { recording_id, error }` | 输入管线 | 根布局 | 终态；和 result 互斥，错误不得含凭据或原始响应。 |

## 应用壳层与 Agent 事件（v1）

以下事件与录音、会话、任务事件共同组成 v1 的完整事件目录。Rust 名称常量和
DTO 位于 `crates/app-binary/src/events.rs`；前端镜像分别位于
`ui/src/lib/contracts/app.ts`、`ui/src/lib/contracts/agent.ts`，只能通过
`appEventListeners` / `agentEventListeners` 进入路由。`scripts/check-ipc-events.ps1`
会比较两侧的全部 37 个 channel，防止新增事件只改一侧。

| 事件 | Rust DTO（wire） | 消费者 | 顺序、幂等与敏感字段 |
|---|---|---|---|
| `app:bootstrap` | `AppBootstrapEvent { status }` | 根布局 | `loading → ready`；状态枚举，不含初始化错误细节。 |
| `tray:status_changed` | `TrayStatusChangedEvent { status, tooltip }` | 根布局 | 最新状态覆盖；tooltip 为固定用户文案。 |
| `mute:changed` | `MuteChangedEvent { muted }` | 根布局、设置页 | 最新值覆盖；只含布尔状态。 |
| `mcp:status_change` | `McpStatusChangedEvent { name, status }` | 工具视图、根布局 | 按 server name 合并；`Offline.error` 为净化错误，不含 env。 |
| `skills:status_change` | `SkillsStatusChangedEvent { op }` | 技能视图 | refresh 通知可丢失，消费者重新读取受管 skills root。 |
| `interaction:requested` | `InteractionRequestedEvent { request_id, owner, session_id?, kind: InteractionKind, status: InteractionStatus, options?, tool_name?, risk_level?, summary?, permission_key?, invocation_step_id?, tool_index?, tool_call_id?, created_at, expires_at? }` | 聊天页 | `kind/status` 使用 Agent Rust enum 并生成前端类型和值集合；`owner` 仍是显式运行时路由元数据：`session { session_id }`、`schedule_run { schedule_run_id }` 或 `app_command`；不包含 continuation。除 `request_id`、`owner`、`kind`、`status`、`created_at` 外，以上列表中带 `?` 的字段在 wire 上可省略；renderer 再按 `kind`/`owner` 校验展示必需字段（ADR 1078）。`session_id` 仅表示可选真实关联上下文；Session owner 必须与其相同，AppCommand 不带该上下文，ScheduleRun 可不关联会话。resume 投影明确为 Session owner。Ask 正文只存在 transcript，事件按 `request_id` 关联状态/选项，不重复携带正文或占位 prompt。confirm 原始参数、receipt 不跨边界，renderer 只收到安全摘要，决策仍由后端校验；前端映射为 `requestId` 后按请求身份幂等覆盖。durable clear event 使用 `request_ids`，replay 兼容旧持久 payload 的 `id` / `ids`，UI 不接受旧字段（ADR 0996）。 |
| `hotkey:conflict` / `hotkey:rebind` | `HotkeyConflictEvent` / `HotkeyRebindEvent` | 根布局、设置页 | 仅报告绑定状态；不执行 renderer 传入的快捷键。 |
| `llm:config_changed` | `()` | 设置页、模型页 | 无 payload；通知页面重新读取脱敏配置。 |
| `agent:thought` | `AgentThoughtEvent` | 聊天页 | 按 `message_id` 归并；可选 `event_sequence` 是已提交的 `session_events.sequence`，缺失表示该事件未关联 durable event row。文本不得重复写入普通日志。 |
| `agent:tool_call` | `AgentToolCallEvent` | 聊天页 | `input` 是工具参数动态扩展点；其余执行身份固定，`silent` 由后端计算。同一 `event_sequence` 可以对应多个 `step_id`，前端按 `(event_sequence, step_id)` 去重。 |
| `agent:observation` | `AgentObservationEvent` | 聊天页 | 与 ToolCall 的 `step_id` / `tool_call_id` 关联；`idempotency` / `operation_scope` 及 result envelope 的 `outcome` / `error_class` / `idempotency` / `retryability` 使用 generated Common enum；`idempotency` 由 operation policy 提供，不信任动态输出同名字段；工具输出按后端门禁净化。去重键是 `(event_sequence, step_id)`（ADR 0743、0744）。 |
| `agent:stream_stalled` | `AgentStreamStalledEvent` | 根布局、聊天页 | 状态提示可重复；不得携带 provider 原始响应。 |
| `agent:thought_chunk` / `agent:reasoning_chunk` | `Agent*ChunkEvent` | 聊天页 | 只使用 chunk `seq`，不分配 durable `event_sequence`。丢失 chunk 时由完整消息投影兜底。 |
| `agent:stream_reset` | `AgentStreamResetEvent` | 聊天页 | 与 chunk 共用后端有序队列；先清空对应 live thought/reasoning，再接受新尝试；不回滚 durable transcript。 |
| `agent:media_plan` | `AgentMediaPlanEvent { session_id, step_number, run_id, request_kind, strategy, projections, notices, event_sequence? }` | 聊天页媒体计划卡 | `request_kind` 字段承载 `RequestKind`。ingress 计划携带 `event_sequence`；请求准备阶段的计划没有 durable 行，`event_sequence` 为空。按 `session_id + step_number + run_id + request_kind` 归并；不携带原始媒体 bytes。 |
| `agent:web_search` | `AgentWebSearchEvent { session_id, phase, step_number, run_id, call_id?, call_index?, action?, result? }` | 聊天页 | `result` 是 provider 动态扩展点；错误和结果按阶段更新。不占用 durable `event_sequence`。`call_id` 原样保留 provider ID；聚合完成项缺少非空 provider ID 时，Agent 使用该响应列表内的零基 `call_index`，UI 通过独立 key namespace 保持结果卡身份（ADR 1127）。 |
| `agent:supplement` | `AgentSupplementEvent` | 聊天页 | 按 `(event_sequence, supplement_id)` 去重；只发送补充上下文，不发送快照内部对象。 |
| `agent:compaction` | `AgentCompactionEvent { summary, tokens_before, tokens_after, degraded, episode_id?, event_sequence? }` | 聊天页 | `event_sequence` 对应该条压缩摘要的 durable sequence。`degraded=true` 表示摘要请求未完成、使用了 `[older context omitted]`，UI 必须提示较早内容已省略；不发送快照内部对象。 |
| `agent:usage` | `AgentUsageEvent` | 用量面板、聊天页 | 不占用 durable `event_sequence`，live 事件自带累计值。固定 token/cost/cache/context 字段；`request_kind` 字段承载 `RequestKind`；`call_kind` 与 `cache_accounting` 使用 Common 生成枚举；`cache_diagnostics` 使用 Common 的 typed `CacheDiagnostics`，其中策略、结果和来源均为闭合枚举，前端 mapper 校验完整字段；缓存率由每次调用的 accounting 合同计算，未知口径不得猜测。 |
| `agent:tool_output` | `AgentToolOutputEvent` | 聊天页 | UI-only 的有界输出通道；未知 channel 或畸形 payload 直接丢弃并记录。 |
| `agent:working_document` | `AgentWorkingDocumentEvent { session_id, markdown, event_sequence }` | 聊天页 | 仅发布已追加的当前 Session 工作文档；按 durable sequence 忽略旧更新，resume snapshot、当前值 sequence 与 append-only `event_sequence_watermark` 一起处理分支回退和延迟事件。Markdown 使用 HTML 禁用的 renderer；内容是过程笔记，不是执行证明或授权，不写普通日志或跨 Session Memory。 |
| `notification:show` | `AgentNotificationEvent` | 根布局 | 通用纯文本通知可省略 session 关联；ToolRun completion 元数据使用生成的 `ToolRunKindDto` / `ToolRunCompletionStatusDto` 与 `AgentNotificationKind`；后台任务完成通知必须带真实 `session_id` 和终态。正文不承载密钥、完整工具输出或原始 provider 错误。 |

前端业务代码只使用 camelCase（例如 `sessionId`、`stepNumber`、`toolCallId`）；Rust-owned
Settings snapshot 是明确例外：Settings parser/editor 与保存 payload 保留 Rust 的 snake_case
字段，详见[命名规范](naming.md)的跨层映射表。
`serde_json::Value` 只保留在上表明确标注的动态字段。新增事件必须同时更新 Rust
常量/DTO、前端 contract、消费者、本文以及目录校验脚本。
