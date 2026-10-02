# Tauri IPC 契约目录

本目录记录稳定的 Tauri 命令与事件边界。Rust 端 wire payload 使用 `snake_case`；前端只在
`ui/src/lib/contracts/` 与 `ui/src/lib/events.ts` 的监听封装中转换为 `camelCase`。业务状态不得在
路由深处重复转换字段。

版本 1 的全量命令目录由 `crates/app-binary/src/commands/contracts.rs` 唯一登记；
前端目录 `ui/src/lib/contracts/commands.ts` 只维护经审阅的边界与安全说明；`scripts/generate-ipc-contracts.ps1` 从 Rust Tauri handler 参数和 Serde DTO 生成前端 request/response 类型。CI 检查生成物 drift，并核对 handler、注册表和本文命令名集合。wire 字段仍保持扁平（例如 `{ sessionId }`），不额外包裹 `{ request: ... }`。
版本 1 不保留旧字段或旧事件别名。

## 全量命令登记（v1）

以下表格列出完整命令集合及人工审阅的边界、安全不变量。wire shape 由 Rust handler 与 Serde DTO 生成；响应中的 `Value` 仅用于明确的 provider、动态工具 schema 或工具输出扩展点。

| 命令 | 边界 | 安全不变量 |
|---|---|---|
| `list_actions` | read | 仅任务投影字段 |
| `cancel_action` | mutate | kind 枚举校验 |
| `list_action_history` | read | limit ≤ 200 |
| `delete_action` | mutate | 按 id 删除单条任务 |
| `open_external` | execute | 仅 http(s) 或校验后的本地绝对路径 |
| `get_history` | read | 只读会话投影 |
| `count_history` | read | 只读聚合 |
| `search_history_paginated` | read | 参数化查询 |
| `count_history_search` | read | 参数化查询 |
| `search_history` | read | 参数化查询 |
| `search_history_filtered` | read | 分页和日期边界 |
| `export_history` | read | 仅导出持久化历史 |
| `get_log_info` | read | 不返回环境详情 |
| `read_log_tail` | read | 尾部长度受限 |
| `log_frontend_error` | mutate | 脱敏后写入后端日志 |
| `get_performance_metrics` | read | 仅返回有界、无内容的后端与渲染器计数 |
| `list_mcp_tools` | read | 快照不执行工具，env 值统一遮蔽 |
| `reconnect_mcp` | execute | 只能选择已配置客户端 |
| `refresh_mcp_servers` | execute | renderer 触发的配置客户端 reconcile；不接收进程参数 |
| `mcp_tool_call` | execute | 适配器调用经过 AuthorizationEngine |
| `add_mcp_server` | execute | 共享 self 操作校验并持久化 |
| `update_mcp_server` | execute | 共享 self 操作安全重连 |
| `remove_mcp_server` | execute | 共享 self 操作删除 |
| `toggle_mcp_server` | execute | 启用前先连接 |
| `run_memory_maintenance` | mutate | 维护路径统一清理 |
| `recall_memory` | read | limit 受限且凭据过滤 |
| `list_facts` | read | 只读事实投影 |
| `add_fact` | mutate | 拒绝凭据样式内容 |
| `delete_fact` | mutate | 按 fact id 删除 |
| `get_api_key_status` | read | 只返回 presence，不返回凭据 |
| `check_llm_connection` | read | 返回状态与非敏感原因分类，不返回 endpoint 或 provider 响应 |
| `discover_models` | execute | 仅 HTTP(S) endpoint；显式输入 key 使用 provider preset 的鉴权方案；已保存 key 仅发送到名称与地址均匹配的 provider；keyless 预设可显式跳过鉴权；代理沿用对应 Provider 设置 |
| `discover_all_models` | execute | 只查询已配置 provider，并沿用各自代理设置 |
| `switch_model` | mutate | `role` 接收模型 ID 或 `RequestKind`，先校验再保存 |
| `set_reasoning_effort` | mutate | `role` 接收模型 ID 或 `RequestKind`，先校验再保存 |
| `set_web_search` | mutate | provider capability 先校验 |
| `get_recording_state` | read | 只返回采集状态 |
| `set_hotkey_capture_active` | mutate | 仅控制快捷键录入期间的临时抑制状态 |
| `start_recording` | execute | 采集生命周期由 input 管线控制 |
| `stop_recording` | execute | 先停止采集再异步转写 |
| `cancel_recording` | execute | 清除 in-flight recording id |
| `process_transcript` | execute | 附件限制和文件持久化校验 |
| `reopen_session` | mutate | session id 选择持久化会话 |
| `get_sessions` | read | 活跃会话投影 |
| `end_session` | mutate | 仅显式结束 |
| `interrupt_session` | mutate | 停止当前输出但保留会话，可继续 |
| `resolve_confirmation` | mutate | effect/scope/target 后端校验，deny 优先 |
| `update_session_title` | mutate | trim 后不得为空 |
| `delete_session` | mutate | 删除并释放运行态 |
| `clear_history` | mutate | 同时清除会话授权 |
| `rollback_session` | mutate | event cursor 与 projection clock 一起回退 |
| `continue_session` | mutate | 从错误 snapshot 恢复 |
| `get_session_for_resume` | read | 会话范围投影 |
| `get_last_conversation` | read | 只取最近持久化会话 |
| `get_settings` | read | 响应遮蔽凭据和 MCP 环境变量 |
| `stage_provider_credential` | mutate | provider 密钥只写入安全凭据存储，返回不透明引用 |
| `stage_ocr_credential` | mutate | OCR 密钥只写入安全凭据存储，返回不透明引用 |
| `discard_staged_credentials` | mutate | 删除未由 Settings 保存提交的暂存凭据 |
| `get_bootstrap_status` | read | 仅状态枚举 |
| `update_settings` | mutate | shared loader 保留遮蔽密钥和工具段 |
| `list_permissions` | read | 只返回 key/effect |
| `revoke_permission` | mutate | 非空 key，原子保存 |
| `reset_permissions` | mutate | 清除永久/会话规则，保留当前默认策略 |
| `check_shell_available` | read | 只返回 available |
| `enable_autostart` | execute | 仅 release 构建 |
| `disable_autostart` | execute | 只能删除受管条目 |
| `is_autostart_enabled` | read | 只返回状态布尔值 |
| `list_skills` | read | 仅元数据投影 |
| `refresh_skills` | execute | 只扫描配置 skills root |
| `set_skill_enabled` | mutate | 共享 self 操作持久化切换 |
| `set_tool_enabled` | mutate | 共享 self 操作持久化切换 |
| `open_skills_dir` | execute | 只能打开配置 skills root |
| `execute_skill` | execute | 限定 skill 名并经过 AuthorizationEngine |
| `get_tools` | read | 返回全量 `ToolManifest[]`；UI 只在边界把 manifest 转为 camelCase |
| `reset_tool_circuits` | mutate | 只清本地 circuit 状态 |

## 会话命令（v1）

| 命令 | 边界 | 说明 |
|---|---|---|
| `get_sessions` | read | 活跃会话列表 |
| `get_session_for_resume` | read | 恢复指定会话所需的持久化投影 |
| `get_last_conversation` | read | 最近持久化会话 |
| `reopen_session` / `continue_session` / `end_session` / `interrupt_session` | mutate | 会话生命周期控制；中断保留会话 |
| `rollback_session` | mutate | 回滚分支并同步截断事件和投影 |
| `update_session_title` | mutate | 更新非空标题 |
| `delete_session` / `clear_history` | mutate | 删除会话或清空历史，并广播 `session:deleted` |
| `resolve_confirmation` | mutate | 解决一个待确认请求 |

Tauri 接收前端参数时采用其自动 camelCase → Rust snake_case 映射；页面调用处使用 camelCase。

## 会话事件（v1）

后端唯一名称常量与 DTO 位于 `crates/app-binary/src/events.rs`。Agent 事件由
`TauriEmitter` 映射，命令直接发送的事件也必须使用同一常量与 DTO。前端唯一登记表在
`ui/src/lib/contracts/session.ts`；`sessionEventListeners` / `registerSessionListener` 负责字段转换。

| 事件 | Rust DTO（wire） | 生产者 | 消费者 | 顺序、幂等与敏感字段 |
|---|---|---|---|---|
| `session:created` | `SessionLifecycleEvent { session_id, status, waiting_reason?, title, reason? }` | Agent 创建会话 | 聊天页、根布局、记忆视图 | 在该会话首个流式事件前；按 `session_id` 幂等合并。`title` 可为 `null`，非终态不发送 `reason`；不得发送原始输入或摘要。 |
| `session:updated` | `SessionLifecycleEvent { session_id, status, waiting_reason?, title, reason?, occurrence_id? }` | Agent 状态变迁；完成/错误的副发 | 聊天页、根布局、记忆视图 | `status` 只能是 `pending`、`running`、`paused`、`completed`、`error`；`waiting_reason` 只在 `paused` 时有值，消费方不得再组合 interaction/action 状态推断等待原因；允许重复。终态副发携带同一 `reason`。仅与主终态 channel 配对的副发携带共同 `occurrence_id`；独立终态更新不带该字段。 |
| `session:completed` | `SessionLifecycleEvent` | Agent 完成 / 用户结束 | 聊天页、根布局、记忆视图 | 终态，`waiting_reason` 缺省，`reason` 必填且为已净化的用户可见结束原因；随后无同 run 的流式事件；会同时副发 `session:updated`，两个 payload 携带相同的短期 `occurrence_id`。 |
| `session:error` | `SessionErrorEvent { session_id, error, occurrence_id? }` | Agent 执行失败 | 聊天页、根布局、记忆视图 | 终态并副发 `session:updated(error)`；两者携带相同的短期 `occurrence_id`。`error` 是面向用户的已净化错误，不得带密钥、完整命令输出或原始 provider 响应。 |
| `session:title-updated` | `SessionTitleUpdatedEvent { session_id, title }` | Agent 自动标题 / `update_session_title` | 聊天页、记忆视图 | 可在任意非删除状态后出现；按 `session_id` 覆盖标题，重复安全。 |
| `session:deleted` | `SessionDeletedEvent { session_id: Option<String> }` | `delete_session` / `clear_history` | 根布局 | `session_id = null` 表示全量清空；删除后不期待该会话的终态事件。payload 不含会话内容。 |

前端内部对应为 `sessionId`、`targetMessageId` 等 camelCase 字段；只允许监听边界进行转换。

## 任务命令（v1）

| 命令 | 边界 | 说明 |
|---|---|---|
| `list_actions` | read | 当前任务投影 |
| `cancel_action` | mutate | 取消指定任务 |
| `list_action_history` | read | 有界终态历史 |
| `delete_action` | mutate | 删除指定历史任务 |

`ActionEvent` 是任务面板的唯一公开记录：`{ id, kind, status?, session_id?, started_at?,
finished_at?, due_at?, title?, body?, mode?, command?, output?, error?, error_reason?,
exit_code?, preview? }`。`status` 只能是 `waiting`、`running`、`completed`、`failed`、
`cancelled`；`kind` 才区分 `background` 与 `scheduled`。它不包含动态 `tool_args`、续接 `prompt`、`tool_name` 或本地
`log_path`；这些是执行内部字段，不能作为跨端契约或泄漏到 UI。

## 任务事件（v1）

后端常量与 DTO 位于 `crates/app-binary/src/events.rs`。`haven-tools` 可以维持内部状态 JSON，
但 app shell 必须在 emit 前投影为 `ActionEvent`；前端唯一登记表是
`ui/src/lib/contracts/action.ts`，`actionEventListeners` 负责 snake_case → camelCase。

| 事件 | Rust DTO（wire） | 生产者 | 消费者 | 顺序、幂等与敏感字段 |
|---|---|---|---|---|
| `action:created` | `ActionEvent` | 后台任务创建 / 定时任务建立 | 根布局 `actionStore` | 在任务对用户可见前发送；按 `id` 覆盖合并，重复安全。 |
| `action:updated` | `ActionEvent` | 后台任务关联会话 / 定时任务触发或回退 | 根布局 `actionStore` | 按 `id` 合并；定时任务用它表达 `waiting ↔ running` 的 live 状态，重复安全。 |
| `action:output` | `ActionEvent` | 后台任务输出尾部变化 | 根布局 `actionStore` | 仅后台任务；可丢失、可重复，按 `id` 最后写入。输出已受后端尾部上限约束。 |
| `action:finished` | `ActionEvent` | 后台任务终态 / 定时任务完成、失败或取消 | 根布局与聊天页 | 终态后不再期待同一任务的 `output`；后台按 `id` 合并，定时任务从 live 列表移除。事件丢失时由 `list_actions` reconciliation 修复。 |

前端内部字段为 `sessionId`、`startedAt`、`errorReason` 等 camelCase；页面不得读取
`action_id` 或其它工具内部 JSON 字段。

## 设置诊断命令（v1）

| 命令 | 边界 | 说明 |
|---|---|---|
| `get_log_info` | read | 日志状态与路径信息 |
| `read_log_tail` | read | 有界日志尾部 |
| `log_frontend_error` | mutate | 记录已净化的前端错误 |
| `check_shell_available` | read | 查询 shell 是否可用 |

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
| `get_recording_state` | read | 当前采集状态 |
| `set_hotkey_capture_active` | mutate | 快捷键录入期间抑制录音触发，结束录入时恢复。 |
| `start_recording` / `stop_recording` / `cancel_recording` | execute | 采集与转写生命周期由下列事件报告。 |
| `process_transcript` | execute | 提交文本与附件进入会话 |

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
| `interaction:requested` | `InteractionRequestedEvent { id, session_id, kind, status, prompt, options, tool_name, risk_level, summary, permission_key, invocation_step_id, action_index, tool_call_id, created_at, expires_at }` | 聊天页 | ask、confirm、scheduled confirm 共用 request id 和生命周期；confirm 的原始参数、receipt 不跨边界，renderer 只收到安全摘要，决策仍由后端校验；前端按 `id` 幂等覆盖并交给统一 `interactionStore`。 |
| `hotkey:conflict` / `hotkey:rebind` | `HotkeyConflictEvent` / `HotkeyRebindEvent` | 根布局、设置页 | 仅报告绑定状态；不执行 renderer 传入的快捷键。 |
| `llm:config_changed` | `()` | 设置页、模型页 | 无 payload；通知页面重新读取脱敏配置。 |
| `agent:thought` | `AgentThoughtEvent` | 聊天页 | 按 `message_id` 归并；可选 `event_seq` 是已提交的 `session_events.sequence`，缺失表示没有 durable 行的 snap。文本不得重复写入普通日志。 |
| `agent:action` | `AgentActionEvent` | 聊天页 | `input` 是工具参数动态扩展点；其余执行身份固定，`silent` 由后端计算。同一 `event_seq` 可以对应多个 `step_id`，前端按 `(event_seq, step_id)` 去重。 |
| `agent:observation` | `AgentObservationEvent` | 聊天页 | 与 action 的 `step_id` / `tool_call_id` 关联；工具输出按后端门禁净化。去重键是 `(event_seq, step_id)`。 |
| `agent:stream_stalled` | `AgentStreamStalledEvent` | 根布局、聊天页 | 状态提示可重复；不得携带 provider 原始响应。 |
| `agent:thought_chunk` / `agent:reasoning_chunk` | `Agent*ChunkEvent` | 聊天页 | 只使用 chunk `seq`，不分配 durable `event_seq`。丢失 chunk 时由完整消息投影兜底。 |
| `agent:stream_reset` | `AgentStreamResetEvent` | 聊天页 | 与 chunk 共用后端有序队列；先清空对应 live thought/reasoning，再接受新尝试；不回滚 durable transcript。 |
| `agent:media_plan` | `AgentMediaPlanEvent { session_id, step_number, run_id, role, strategy, projections, notices, event_seq? }` | 聊天页媒体计划卡 | `role` 字段承载 `RequestKind` 字符串。ingress 计划携带 `event_seq`；请求准备阶段的计划没有 durable 行，`event_seq` 为空。按 `session_id + step_number + run_id + role` 归并；不携带原始媒体 bytes。 |
| `agent:web_search` | `AgentWebSearchEvent` | 聊天页 | `result` 是 provider 动态扩展点；错误和结果按阶段更新。不占用 durable `event_seq`。 |
| `agent:supplement` | `AgentSupplementEvent` | 聊天页 | 按 `(event_seq, supplement_id)` 去重；只发送补充上下文，不发送快照内部对象。 |
| `agent:compaction` | `AgentCompactionEvent { summary, tokens_before, tokens_after, degraded, episode_id?, event_seq? }` | 聊天页 | `event_seq` 对应该条压缩摘要的 durable sequence。`degraded=true` 表示摘要请求未完成、使用了 `[older context omitted]`，UI 必须提示较早内容已省略；不发送快照内部对象。 |
| `agent:usage` | `AgentUsageEvent` | 用量面板、聊天页 | 不占用 durable `event_seq`，live 事件自带累计值。固定 token/cost/cache/context 字段；`role` 字段承载 `RequestKind` 字符串；`call_kind=agent` 为 Agent 主循环，`call_kind=media` 为工具拥有的媒体推理，`call_kind=tool` 为其它工具内部 LLM 调用，后二者均不更新主循环累计统计；`cache_diagnostics` 仅为 provider 诊断扩展点；缓存率由每次调用的 accounting 合同计算，未知口径不得猜测。 |
| `agent:tool_output` | `AgentToolOutputEvent` | 聊天页 | UI-only 的有界输出通道；未知 channel 或畸形 payload 直接丢弃并记录。 |
| `notification:show` | `AgentNotificationEvent` | 根布局 | 纯文本 toast/系统通知；不承载密钥、完整命令输出或原始 provider 错误。 |

前端业务代码只使用 camelCase（例如 `sessionId`、`stepNumber`、`toolCallId`）；
`serde_json::Value` 只保留在上表明确标注的动态字段。新增事件必须同时更新 Rust
常量/DTO、前端 contract、消费者、本文以及目录校验脚本。
