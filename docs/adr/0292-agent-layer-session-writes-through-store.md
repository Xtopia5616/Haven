# ADR 0292：AgentLayer 会话写入通过 SessionStore

- 状态：Accepted
- 日期：2026-09-24
- 范围：AgentLayer 的首条消息失败清理与 peer session 标题写入
- 关联：[ADR 0260](0260-session-store-session-creation-port.md)、[ADR 0281](0281-session-title-write-session-store-port.md)、[ADR 0291](0291-session-deletion-through-session-store.md)

## 背景

AgentLayer 仍有三处 session 写路径直接以 `Database::run_blocking` 调度 SQLite：首条消息持久化失败后删除新建 session、写入 peer 显式标题，以及写入用于通知的 fallback 标题。AgentLayer 已可通过 `executor.session_store()` 取得对应的 typed port，这三处重复暴露了 blocking 调度边界。

AgentLayer 的 raw `Database` 字段仍被 `MemoryService`、`ReActEngine` 等其他职责使用。本次只迁移这三处写入，不扩大到读取、记忆服务、ReActEngine 或其他模块。

## 决策

1. 首条消息持久化失败时，通过已有 `SessionStore::delete_session` 尽力删除 durable session，忽略清理错误并返回原始消息错误。
2. peer 显式标题和无标题时的 fallback 标题通过已有 `SessionStore::update_session_title` 写入。只有持久化成功后才更新 executor 和局部 `SessionInfo`；显式标题仍在之后发布既有 `TitleUpdated` 事件，fallback 保持不发布该事件。
3. 标题写入失败仍记录各自既有 warning 并继续 inbox 注册和 peer session 注册流程；fallback 的生成方式、适用条件及 `SessionCreated` 行为不变。
4. `SessionStore` 在此只承接既有 Database 操作的 blocking-pool 调度。生命周期、错误降级、运行态更新和事件顺序仍由 AgentLayer 负责；不增加 store 字段、重复端口、generic trait 或 facade。AgentLayer 保留 raw `Database`，供 MemoryService、ReActEngine 等现存职责使用。
5. 为验证 peer 注册流程，生产入口继续使用默认 inbox；一个私有实现方法接收现有 `MessagingService`，Agent 回归测试使用临时 inbox 目录。

`layer.rs` 继续作为 AgentLayer composition/facade 文件承载本切片；拆分其余标题生成、peer spawn 和 reopen 职责不属于本次 session write 调度收口。

## 影响与验证

- 无 schema、IPC 或用户数据契约变化，无需重置数据库。
- Agent 回归测试覆盖显式标题写入成功/失败、fallback 标题成功/失败、写失败后 peer 仍注册、title event 只在显式标题写入成功后发布，以及首条消息失败后的 session 删除和原始错误传播。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-agent`、`cargo clippy --locked -p haven-agent -- -D warnings`；条件允许时运行 workspace check/clippy/test。

## 回滚

将这三处调用恢复为 AgentLayer 内的 `Database::run_blocking`，移除对应回归测试、peer messaging 测试 seam、本 ADR、索引与路线图记录。无需迁移或重置数据库。
