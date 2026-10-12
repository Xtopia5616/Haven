# ADR 1075：让 Rust helper 名称标明操作与身份

## 状态

已接受（2026-10-13）

## 背景

部分 Agent、Memory 和 Tools helper 的短名称弱化了实际工作或实体归属：恢复待处理 session 的函数叫 `load`，Actor 只清理瞬态状态的命令叫 `clear_runtime`，明确写入用户事实的 repository helper 没有 `fact` 词义，后台工具存储桥接只叫 `run`。ToolRun helper 参数虽传递 `tool_run_id`，却叫 `id`；共享日志文件 helper 还把 shell 文件名标成 tracing 的 `tool_run_id`。

## 决定

- Agent 将持久 Pending session 安装到 runtime actor 并排队的动作叫 `restore_pending_sessions`；返回计数只包括新安装的 actor，既有 actor 的重新入队不计入。
- Agent 的 `get_runtime_session` 明确读取非终态 runtime actor；Actor mailbox 的 `clear_transient_state` 明确只清除瞬态队列、交互、子会话标志和消息状态。
- Memory repository helper 叫 `set_user_fact`；维护存储内只负责进入 SQLite blocking pool 的 helper 叫 `run_blocking_operation`。
- ToolRun helper 参数使用 `tool_run_id`；共享 output-log helper 的文件名参数叫 `file_stem`，相应 tracing field 叫 `output_log_stem`，不再把非实体文件名误标为 ToolRun identity。

## 替代方案

- 保留短名并依赖调用上下文：不采纳，因为跨模块调用和 tracing 字段会丢失操作或实体来源。
- 把 Actor 局部清理改为完整 Session runtime 清理：不采纳，因为生命周期和 active run 仍由 Supervisor 管理，Actor 命令只清 transient state。

## 影响

Rust 内部和 crate API 方法名变化；没有改变数据库、event、IPC、ToolRun 生命周期或文件路径行为。待处理 session 恢复返回的计数定义现在写明。无需重置数据。

## 验证

- Agent、Memory、Tools workspace tests 与 Clippy 通过。
- `cargo fmt --all --check` 通过。

## 回滚与数据重置

不涉及持久化或 wire contract，无需重置。若回滚名称，应统一恢复声明、调用方、测试与文档，不能重新将 output-log stem 标记为 ToolRun ID。
