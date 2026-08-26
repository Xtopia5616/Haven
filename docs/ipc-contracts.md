# Tauri IPC 契约目录

本目录记录稳定的 Tauri 命令与事件边界。Rust 端 wire payload 使用 `snake_case`；前端只在
`ui/src/lib/contracts/` 与 `ui/src/lib/events.ts` 的监听封装中转换为 `camelCase`。业务状态不得在
路由深处重复转换字段。

当前先登记会话域；其它域迁移到命名 DTO 后按同一格式追加。版本 1 不保留旧字段或旧事件别名。

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
