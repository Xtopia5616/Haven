# Tauri IPC 契约目录

本目录记录稳定的 Tauri 命令与事件边界。Rust 端 wire payload 使用 `snake_case`；前端只在
`ui/src/lib/contracts/` 与 `ui/src/lib/events.ts` 的监听封装中转换为 `camelCase`。业务状态不得在
路由深处重复转换字段。

已登记会话与任务域；其它域迁移到命名 DTO 后按同一格式追加。版本 1 不保留旧字段或旧事件别名。

## 会话命令（v1）

| 命令 | 请求 | 响应 | 说明 |
|---|---|---|---|
| `get_sessions` | 无 | `SessionListResponse` | 运行中会话列表 |
| `get_session_for_resume` | `{ session_id }` | `SessionResumeResponse` | 加载持久化会话、消息、步骤与用量 |
| `get_last_conversation` | 无 | `Option<SessionResumeResponse>` | 应用启动时恢复最近会话 |
| `reopen_session` / `continue_session` / `end_session` | `{ session_id }` | `()` | 生命周期控制 |
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
