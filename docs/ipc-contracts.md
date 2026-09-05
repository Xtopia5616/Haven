# Tauri IPC 契约目录

本目录记录稳定的 Tauri 命令与事件边界。Rust 端 wire payload 使用 `snake_case`；前端只在
`ui/src/lib/contracts/` 与 `ui/src/lib/events.ts` 的监听封装中转换为 `camelCase`。业务状态不得在
路由深处重复转换字段。

版本 1 的全量命令目录由 `crates/app-binary/src/commands/contracts.rs` 唯一登记；
前端镜像位于 `ui/src/lib/contracts/commands.ts`，CI 会校验两者与
`generate_handler!`、本文目录的命令集合完全一致。请求 DTO 名称描述稳定 schema，
实际 Tauri wire 仍保持扁平字段（例如 `{ sessionId }`），不额外包裹 `{ request: ... }`。
版本 1 不保留旧字段或旧事件别名。

## 全量命令登记（v1）

以下表格是机器可校验的完整目录；响应中的 `Value` 只允许出现在明确的 provider、
动态工具 schema 或工具输出扩展点，不能作为稳定业务外壳。

| 命令 | 请求 DTO | 响应 | 边界 | 安全不变量 |
|---|---|---|---|---|
| `list_actions` | `-` | `ActionEvent[]` | read | 仅任务投影字段 |
| `cancel_action` | `CancelActionRequest` | `bool` | mutate | kind 枚举校验 |
| `list_action_history` | `ListActionHistoryRequest` | `ActionEvent[]` | read | limit ≤ 200 |
| `delete_action` | `DeleteActionRequest` | `bool` | mutate | 按 id 删除单条任务 |
| `open_external` | `OpenExternalRequest` | `()` | execute | 仅 http(s) 或校验后的本地绝对路径 |
| `get_history` | `HistoryPageRequest` | `Session[]` | read | 只读会话投影 |
| `count_history` | `-` | `i64` | read | 只读聚合 |
| `search_history_paginated` | `HistorySearchPageRequest` | `Session[]` | read | 参数化查询 |
| `count_history_search` | `HistorySearchRequest` | `i64` | read | 参数化查询 |
| `search_history` | `HistorySearchRequest` | `Session[]` | read | 参数化查询 |
| `search_history_filtered` | `HistoryFilterRequest` | `Session[]` | read | 分页和日期边界 |
| `export_history` | `HistoryExportRequest` | `string` | read | 仅导出持久化历史 |
| `get_log_info` | `-` | `LogInfo` | read | 不返回环境详情 |
| `read_log_tail` | `ReadLogTailRequest` | `LogTail` | read | 尾部长度受限 |
| `list_mcp_tools` | `-` | `McpServerSnapshot[]` | read | 快照不执行工具，env 值统一遮蔽 |
| `reconnect_mcp` | `McpNameRequest` | `()` | execute | 只能选择已配置客户端 |
| `refresh_mcp_servers` | `-` | `McpRefreshResult` | execute | 只重 reconcile 配置客户端 |
| `mcp_tool_call` | `McpToolCallRequest` | `McpToolCallResponse` | execute | 适配器调用经过 SafetyGateway |
| `add_mcp_server` | `McpServerConfig` | `()` | execute | 共享 self 操作校验并持久化 |
| `update_mcp_server` | `UpdateMcpServerRequest` | `()` | execute | 共享 self 操作安全重连 |
| `remove_mcp_server` | `McpNameRequest` | `()` | execute | 共享 self 操作删除 |
| `toggle_mcp_server` | `ToggleMcpServerRequest` | `()` | execute | 启用前先连接 |
| `run_memory_maintenance` | `-` | `u64` | mutate | 维护路径统一清理 |
| `recall_memory` | `RecallMemoryRequest` | `MemoryRecallItem[]` | read | limit 受限且凭据过滤 |
| `list_facts` | `ListFactsRequest` | `Fact[]` | read | 只读事实投影 |
| `add_fact` | `AddFactRequest` | `Fact` | mutate | 拒绝凭据样式内容 |
| `delete_fact` | `DeleteFactRequest` | `()` | mutate | 按 fact id 删除 |
| `get_api_key_status` | `-` | `ApiKeyStatus` | read | 只返回 presence，不返回凭据 |
| `check_llm_connection` | `-` | `string` | read | 只返回状态 |
| `discover_models` | `DiscoverModelsRequest` | `ModelInfo[]` | execute | endpoint 与已保存 key 主机匹配 |
| `discover_all_models` | `-` | `Record<string, ModelInfo[]>` | execute | 只查询已配置 provider |
| `switch_model` | `SwitchModelRequest` | `()` | mutate | role 先校验再保存 |
| `set_reasoning_effort` | `SetReasoningEffortRequest` | `()` | mutate | role 先校验再保存 |
| `set_web_search` | `SetWebSearchRequest` | `()` | mutate | provider capability 先校验 |
| `get_recording_state` | `-` | `RecordingState` | read | 只返回采集状态 |
| `start_recording` | `-` | `()` | execute | 采集生命周期由 input 管线控制 |
| `stop_recording` | `-` | `string` | execute | 先停止采集再异步转写 |
| `cancel_recording` | `-` | `()` | execute | 清除 in-flight recording id |
| `process_transcript` | `ProcessTranscriptRequest` | `ProcessResult` | execute | 附件限制和文件持久化校验 |
| `reopen_session` | `SessionIdRequest` | `()` | mutate | session id 选择持久化会话 |
| `get_sessions` | `-` | `SessionListResponse` | read | 活跃会话投影 |
| `end_session` | `SessionIdRequest` | `()` | mutate | 仅显式结束 |
| `interrupt_session` | `SessionIdRequest` | `()` | mutate | 停止当前输出但保留会话，可继续 |
| `resolve_confirmation` | `ResolveConfirmationRequest` | `()` | mutate | effect/scope 后端校验，deny 优先 |
| `update_session_title` | `UpdateSessionTitleRequest` | `()` | mutate | trim 后不得为空 |
| `delete_session` | `SessionIdRequest` | `()` | mutate | 删除并释放运行态 |
| `clear_history` | `-` | `u64` | mutate | 同时清除会话授权 |
| `rollback_session` | `RollbackSessionRequest` | `()` | mutate | event cursor 与 projection clock 一起回退 |
| `continue_session` | `SessionIdRequest` | `()` | mutate | 从错误 snapshot 恢复 |
| `get_session_for_resume` | `SessionIdRequest` | `SessionResumeResponse` | read | 会话范围投影 |
| `get_last_conversation` | `-` | `Option<SessionResumeResponse>` | read | 只取最近持久化会话 |
| `get_settings` | `-` | `Settings` | read | 响应遮蔽凭据 |
| `get_bootstrap_status` | `-` | `string` | read | 仅状态枚举 |
| `update_settings` | `Settings` | `()` | mutate | shared loader 保留遮蔽密钥和工具段 |
| `list_permissions` | `-` | `StoredPermission[]` | read | 只返回 key/effect |
| `revoke_permission` | `RevokePermissionRequest` | `()` | mutate | 非空 key，原子保存 |
| `check_shell_available` | `CheckShellAvailableRequest` | `ShellAvailability` | read | 只返回 available |
| `enable_autostart` | `-` | `()` | execute | 仅 release 构建 |
| `disable_autostart` | `-` | `()` | execute | 只能删除受管条目 |
| `is_autostart_enabled` | `-` | `bool` | read | 只返回状态布尔值 |
| `list_skills` | `-` | `SkillInfo[]` | read | 仅元数据投影 |
| `refresh_skills` | `-` | `()` | execute | 只扫描配置 skills root |
| `set_skill_enabled` | `SetEnabledRequest` | `()` | mutate | 共享 self 操作持久化切换 |
| `set_tool_enabled` | `SetEnabledRequest` | `()` | mutate | 共享 self 操作持久化切换 |
| `open_skills_dir` | `-` | `string` | execute | 只能打开配置 skills root |
| `execute_skill` | `ExecuteSkillRequest` | `SkillExecutionResponse` | execute | 限定 skill 名并经过 SafetyGateway |
| `get_tools` | `-` | `ToolListResponse` | read | 固定工具字段，schema 才是动态扩展 |
| `reset_tool_circuits` | `-` | `()` | mutate | 只清本地 circuit 状态 |

## 会话命令（v1）

| 命令 | 请求 | 响应 | 说明 |
|---|---|---|---|
| `get_sessions` | 无 | `SessionListResponse` | 运行中会话列表 |
| `get_session_for_resume` | `{ session_id }` | `SessionResumeResponse` | 加载持久化会话、消息、步骤与用量 |
| `get_last_conversation` | 无 | `Option<SessionResumeResponse>` | 应用启动时恢复最近会话 |
| `reopen_session` / `continue_session` / `end_session` / `interrupt_session` | `{ session_id }` | `()` | 生命周期控制；中断保留会话 |
| `rollback_session` | `{ session_id, target_step, pause, target_message_id }` | `()` | 按事件游标与投影时钟回滚 |
| `update_session_title` | `{ session_id, title }` | `()` | 保存并广播新标题 |
| `delete_session` / `clear_history` | `{ session_id }` / 无 | `()` / 删除数量 | 删除后广播 `session:deleted` |
| `resolve_confirmation` | `{ step_id, confirmed, trust_session?, effect?, scope? }` | `()` | 仅确认流程使用；权限决策由后端校验 |

Tauri 接收前端参数时采用其自动 camelCase → Rust snake_case 映射；页面调用处使用 camelCase。

## 会话事件（v1）

后端唯一名称常量与 DTO 位于 `crates/app-binary/src/events.rs`。Agent 事件由
`TauriEmitter` 映射，命令直接发送的事件也必须使用同一常量与 DTO。前端唯一登记表在
`ui/src/lib/contracts/session.ts`；`sessionEventListeners` / `registerSessionListener` 负责字段转换。

| 事件 | Rust DTO（wire） | 生产者 | 消费者 | 顺序、幂等与敏感字段 |
|---|---|---|---|---|
| `session:created` | `SessionLifecycleEvent { session_id, status, title }` | Agent 创建会话 | 聊天页、根布局、记忆视图 | 在该会话首个流式事件前；按 `session_id` 幂等合并。`title` 可为 `null`，不得发送原始输入或摘要。 |
| `session:updated` | `SessionLifecycleEvent` | Agent 状态变迁；完成/错误的副发 | 聊天页、根布局、记忆视图 | 状态顺序通常为 pending → running → paused/completed/error；消费方按最后状态归并，允许重复。只含展示标题。 |
| `session:completed` | `SessionLifecycleEvent` | Agent 完成 / 用户结束 | 聊天页、根布局、记忆视图 | 终态，随后无同 run 的流式事件；会同时副发 `session:updated`，消费者必须幂等。 |
| `session:error` | `SessionErrorEvent { session_id, error }` | Agent 执行失败 | 聊天页、根布局、记忆视图 | 终态并副发 `session:updated(error)`；`error` 是面向用户的已净化错误，不得带密钥、完整命令输出或原始 provider 响应。 |
| `session:title-updated` | `SessionTitleUpdatedEvent { session_id, title }` | Agent 自动标题 / `update_session_title` | 聊天页、记忆视图 | 可在任意非删除状态后出现；按 `session_id` 覆盖标题，重复安全。 |
| `session:deleted` | `SessionDeletedEvent { session_id: Option<String> }` | `delete_session` / `clear_history` | 根布局 | `session_id = null` 表示全量清空；删除后不期待该会话的终态事件。payload 不含会话内容。 |

前端内部对应为 `sessionId`、`targetMessageId` 等 camelCase 字段；只允许监听边界进行转换。

## 任务命令（v1）

| 命令 | 请求 | 响应 | 说明 |
|---|---|---|---|
| `list_actions` | 无 | `ActionEvent[]` | 运行中后台任务、待触发定时任务和尚在内存板上的终态任务。 |
| `cancel_action` | `{ action_id, kind }` | `bool` | `kind` 仅为 `background` 或 `scheduled`；未知值由 Tauri 反序列化拒绝。 |
| `list_action_history` | `{ kind?, limit? }` | `ActionEvent[]` | 持久化历史；定时任务仅返回已触发记录，`limit` 最大为 200。 |
| `delete_action` | `{ action_id }` | `bool` | 删除一条已持久化的任务历史。 |

`ActionEvent` 是任务面板的唯一公开记录：`{ id, kind, status?, session_id?, started_at?,
finished_at?, due_at?, title?, body?, mode?, command?, output?, error?, error_reason?,
exit_code?, preview? }`。它不包含动态 `tool_args`、续接 `prompt`、`tool_name` 或本地
`log_path`；这些是执行内部字段，不能作为跨端契约或泄漏到 UI。

## 任务事件（v1）

后端常量与 DTO 位于 `crates/app-binary/src/events.rs`。`haven-tools` 可以维持内部状态 JSON，
但 app shell 必须在 emit 前投影为 `ActionEvent`；前端唯一登记表是
`ui/src/lib/contracts/action.ts`，`actionEventListeners` 负责 snake_case → camelCase。

| 事件 | Rust DTO（wire） | 生产者 | 消费者 | 顺序、幂等与敏感字段 |
|---|---|---|---|---|
| `action:created` | `ActionEvent` | 后台任务创建 / 定时任务建立 | 根布局 `actionStore` | 在任务对用户可见前发送；按 `id` 覆盖合并，重复安全。 |
| `action:updated` | `ActionEvent` | 后台任务关联会话 / 定时任务取消 | 根布局 `actionStore` | 后台任务只更新关联字段；定时任务 `status = cancelled`，消费者移除待触发条目。 |
| `action:output` | `ActionEvent` | 后台任务输出尾部变化 | 根布局 `actionStore` | 仅后台任务；可丢失、可重复，按 `id` 最后写入。输出已受后端尾部上限约束。 |
| `action:finished` | `ActionEvent` | 后台任务终态 / 定时任务触发 | 根布局与聊天页 | 终态后不再期待同一任务的 `output`；后台按 `id` 合并，定时任务从待触发列表移除。 |

前端内部字段为 `sessionId`、`startedAt`、`errorReason` 等 camelCase；页面不得读取
`action_id` 或其它工具内部 JSON 字段。

## 设置诊断命令（v1）

| 命令 | 请求 | 响应 | 说明 |
|---|---|---|---|
| `get_log_info` | 无 | `LogInfo` | 返回文件日志开关、级别和当前日志路径；路径可能为 `null`。 |
| `read_log_tail` | `{ max_lines? }` | `LogTail` | 返回当前日志文件路径和受上限约束的尾部文本。文件日志关闭或文件不存在时返回命令错误。 |
| `check_shell_available` | `{ shell }` | `ShellAvailability` | 返回指定 shell 是否可用；不会返回 PATH 或进程探测细节。 |

上述命令的 Rust 响应均为命名 DTO，前端由 `ui/src/lib/contracts/settings.ts` 在消费前校验。
日志内容仍按日志查看器用途返回，不能复用于普通错误提示或其它 IPC 事件。

## 模型设置命令（v1）

| 命令 | 请求 | 响应 | 说明 |
|---|---|---|---|
| `get_api_key_status` | 无 | `ApiKeyStatus` | 返回各固定模型槽位、媒体能力和已配置 provider 的布尔状态；不返回任何凭据。 |

`ApiKeyStatus.providers` 的 key 是用户配置的 provider 名称这一明确扩展点，其 value 始终为
布尔值；其它状态字段由 Rust DTO 和前端 parser 固定。

## 录音与转写命令、事件（v1）

| 命令 | 请求 | 响应 | 说明 |
|---|---|---|---|
| `get_recording_state` | 无 | `RecordingState { is_recording, is_toggle }` | 当前采集状态；不会暴露设备或 provider 细节。 |
| `start_recording` / `stop_recording` / `cancel_recording` | 无 | `()` | 采集与转写生命周期由下列事件报告。 |
| `process_transcript` | `{ text, session_id? }` | `()` | 将已确认的纯文本提交为会话输入。 |

Rust DTO 定义在 `crates/app-binary/src/events.rs`，前端唯一转换边界是
`ui/src/lib/contracts/recording.ts` 与 `recordingEventListeners`。`rec-*` 为单次录音 ID，
只用于关联录音与转写事件，不能当作会话 ID 使用。

| 事件 | Rust DTO（wire） | 生产者 | 消费者 | 顺序、幂等与敏感字段 |
|---|---|---|---|---|
| `recording:started` | `RecordingEvent { is_recording, session_id }` | 按钮、热键 | 根布局录音浮层 | 每段录音最多一次；先于 stopped/transcription。仅携带 `rec-*`。 |
| `recording:stopped` | `RecordingEvent { is_recording, reason?, duration_ms? }` | 输入管线 | 根布局录音浮层 | 在采集停止后；可由取消结束，不保证随后有转写。 |
| `recording:vad_status` | `VadStatusEvent { signal, state }` | 输入管线 | 根布局录音浮层 | 高频、可丢失；消费者只保留最后状态。 |
| `recording:error` | `RecordingErrorEvent { session_id, error }` | 录音命令/热键 | 根布局 | 终态；错误为已净化用户文案，不含设备路径或 provider 原始响应。 |
| `transcription:started` | `TranscriptionStartedEvent { session_id }` | 输入管线 | 根布局 | 在网络转写前，和对应 `rec-*` 关联。 |
| `transcription:result` | `TranscriptionResultEvent { session_id, text, duration_ms, confidence? }` | 输入管线 | 根布局→会话输入 | 每段录音一个终态结果；空 `text` 表示静音/过短录音。文本仅交给会话输入，不写日志或其它状态。 |
| `transcription:error` | `TranscriptionErrorEvent { session_id, error }` | 输入管线 | 根布局 | 终态；和 result 互斥，错误不得含凭据或原始响应。 |

## 应用壳层与 Agent 事件（v1）

以下事件与录音、会话、任务事件共同组成 v1 的完整事件目录。Rust 名称常量和
DTO 位于 `crates/app-binary/src/events.rs`；前端镜像分别位于
`ui/src/lib/contracts/app.ts`、`ui/src/lib/contracts/agent.ts`，只能通过
`appEventListeners` / `agentEventListeners` 进入路由。`scripts/check-ipc-events.ps1`
会比较两侧的全部 40 个 channel，防止新增事件只改一侧。

| 事件 | Rust DTO（wire） | 消费者 | 顺序、幂等与敏感字段 |
|---|---|---|---|
| `app:bootstrap` | `AppBootstrapEvent { status }` | 根布局 | `loading → ready`；状态枚举，不含初始化错误细节。 |
| `tray:status_changed` | `TrayStatusChangedEvent { status, tooltip }` | 根布局 | 最新状态覆盖；tooltip 为固定用户文案。 |
| `mute:changed` | `MuteChangedEvent { muted }` | 根布局、设置页 | 最新值覆盖；只含布尔状态。 |
| `mcp:status_change` | `McpStatusChangedEvent { name, status }` | 工具视图、根布局 | 按 server name 合并；`Offline.error` 为净化错误，不含 env。 |
| `skills:status_change` | `SkillsStatusChangedEvent { op }` | 技能视图 | refresh 通知可丢失，消费者重新读取受管 skills root。 |
| `confirm:requested` | `ConfirmationRequestedEvent { step_id, invocation_step_id, action_index, tool_call_id, tool_name, risk_level, session_id, params, permission_key }` | 聊天页 | `step_id` 是确认请求 ID，用于 resolve；ReAct 工具另带稳定的 `invocation_step_id + action_index + tool_call_id`，定时/后台动作的 invocation identity 为空；必须先由后端 SafetyGateway 创建，决策仍由后端校验。 |
| `hotkey:conflict` / `hotkey:rebind` | `HotkeyConflictEvent` / `HotkeyRebindEvent` | 根布局、设置页 | 仅报告绑定状态；不执行 renderer 传入的快捷键。 |
| `llm:config_changed` | `()` | 设置页、模型页 | 无 payload；通知页面重新读取脱敏配置。 |
| `agent:thought` | `AgentThoughtEvent` | 聊天页 | 按 `session_id + run_id + step_number` 归并；文本不得重复写入普通日志。 |
| `agent:action` | `AgentActionEvent` | 聊天页 | `input` 是工具参数动态扩展点；其余执行身份固定，`silent` 由后端计算。 |
| `agent:observation` | `AgentObservationEvent` | 聊天页 | 与 action 的 `step_id` / `tool_call_id` 关联；工具输出按后端门禁净化。 |
| `agent:stream_stalled` | `AgentStreamStalledEvent` | 根布局、聊天页 | 状态提示可重复；不得携带 provider 原始响应。 |
| `agent:thought_chunk` / `agent:reasoning_chunk` | `Agent*ChunkEvent` | 聊天页 | 通过 `seq` 排序，丢失 chunk 时由完整消息投影兜底。 |
| `agent:stream_reset` | `AgentStreamResetEvent` | 聊天页 | 与 chunk 共用后端有序队列；先清空对应 live thought/reasoning，再接受新尝试；不回滚 durable transcript。 |
| `agent:web_search` | `AgentWebSearchEvent` | 聊天页 | `result` 是 provider 动态扩展点；错误和结果按阶段更新。 |
| `agent:supplement` | `AgentSupplementEvent` | 聊天页 | 按 run/step 顺序消费；只发送补充上下文，不发送快照内部对象。 |
| `agent:compaction` | `AgentCompactionEvent { summary, tokens_before, tokens_after, degraded, episode_id? }` | 聊天页 | 按事件顺序消费；`degraded=true` 表示摘要请求未完成、使用了 `[older context omitted]`，UI 必须提示较早内容已省略；不发送快照内部对象。 |
| `agent:usage` | `AgentUsageEvent` | 用量面板、聊天页 | 固定 token/cost 字段；`cache_diagnostics` 仅为 provider 诊断扩展点。 |
| `agent:tool_output` | `AgentToolOutputEvent` | 聊天页 | UI-only 的有界输出通道；未知 channel 或畸形 payload 直接丢弃并记录。 |
| `notification:show` | `AgentNotificationEvent` | 根布局 | 纯文本 toast/系统通知；不承载密钥、完整命令输出或原始 provider 错误。 |

前端业务代码只使用 camelCase（例如 `sessionId`、`stepNumber`、`toolCallId`）；
`serde_json::Value` 只保留在上表明确标注的动态字段。新增事件必须同时更新 Rust
常量/DTO、前端 contract、消费者、本文以及目录校验脚本。
