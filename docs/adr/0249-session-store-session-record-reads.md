# ADR 0249：SessionStore 会话记录读取端口

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent::SessionSupervisor` 的按 ID 加载与启动 pending session 恢复
- 关联：[ADR 0207](0207-session-store-replay-boundaries-and-durable-ui-sequences.md)、[ADR 0238](0238-session-recovery-read-ports.md)

## 背景

`SessionStore` 已承载恢复读取，但 `SessionSupervisor::ensure_session_loaded_locked` 仍直接调用
`Database::get_session`，`load_pending_sessions` 则直接拼接通用
`search_sessions_filtered(None, Some("pending"), None, None, -1, 0)`。启动 dispatcher
本身不访问数据库，而是调用 `load_pending_sessions`；因此读取边界集中在该生命周期方法中。

pending 查询的现有语义是只筛选 `pending`，不限制条数（`limit = -1`）、从头读取
（`offset = 0`），并按 `created_at DESC` 排序。按 ID 查询对缺失记录返回 `None`，由调用方
转换成原有 not-found 错误。读取和 actor 安装之间没有共享事务。

## 决定

1. `SessionStore::session_record(session_id)` 提供按 ID 的 typed 读取，返回
   `Result<Option<Session>>`，保留缺失记录语义。
2. `SessionStore::pending_session_records()` 提供无分页的 pending 列表，封装既有状态过滤、
   `limit = -1`、`offset = 0` 与 `created_at DESC` 顺序；通用搜索参数不暴露到 Agent。
3. Agent 通过这两个方法读取。pending 结果仍按返回顺序逐条跳过 closing session、安装 actor、
   入队；全部处理后仅在有新 actor 时唤醒 dispatcher。dispatcher 保留调用
   `load_pending_sessions` 的启动时序。
4. 使用既有 `Session` persistence record，不增加重复 DTO，不创建跨读取事务，也不改变 schema、
   IPC 或消息/附件恢复查询。

## 验证

`haven-memory` 内存数据库测试覆盖缺失 ID、全部 pending 结果的降序排列，以及非 pending 状态
被排除。另运行 `cargo fmt --all -- --check`、haven-memory/agent 测试和 workspace strict Clippy。

## 回滚

恢复 Agent 对 `Database::get_session` 和 `search_sessions_filtered` 的原调用，并删除
`SessionStore` 两个只读方法、对应测试与本文索引/路线图记录。没有持久化格式或用户数据变化。
