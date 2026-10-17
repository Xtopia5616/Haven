# ADR 1304：由私有模块拥有 transcript append 与 projection

状态：已接受并实施（2026-10-17）。

## 背景

`SessionStore` 已把非 event-core 操作族迁入私有模块，但 `session_events.rs` 仍混合 durable event model、domain/seed append、transcript commit 和 message/step projection。生产调用应继续面向同一个 `SessionStore` facade；transcript event 与物化行还必须在同一 SQLite transaction 内提交。

## 决定

1. 将 `SessionEvent`、`SessionEventInput`、`SessionEventPage` 及持久行映射移入私有 `event_model`，由 facade 重新导出原公开类型。
2. 将同步/异步 raw append、domain event append、transcript append 和 seed 写入移入私有 `event_append`。现有验证、blocking-pool 边界、序号分配和 post-commit broadcast 保持原样。
3. 将 `SessionCommitted` intent、容量与身份校验、`append_batch_in_transaction`、transcript commit 和 `Database::write_session_projections` 移入私有 `transcript_commit`。`SessionStore` 继续通过稳定 facade 暴露原 commit API；共享事务 helper 只在 Memory repository 内可见。
4. 保持 transcript commit 顺序：先在 `BEGIN IMMEDIATE` 中追加 event，再写 message/step projection 和 cursor；所有操作成功后 `COMMIT`，之后才失效 message cache 并广播事件。任何 projection 错误都回滚 append。
5. 保持 `SessionStore` 作为唯一数据库 owner。fork、rollback、usage 等已有操作继续调用同一个 append-in-transaction helper，不复制序号分配或事件写路径。

## 替代方案

- 将 event append 与 transcript projection 变成两个 store 或两笔事务：拒绝。projection 失败会留下无对应物化状态的 durable event。
- 暴露新的子模块给 Agent/App：拒绝。职责整理不改变 crate/API 边界，原 facade 是唯一跨层入口。
- 重写 event payload、持久格式或 broadcast 顺序：拒绝。本切片只迁移实现，不改变事件契约。

## 影响与恢复

schema、event payload、sequence、projection clocks、IPC 与用户可见行为均未变化，不需要数据库重置或数据迁移。回退时恢复旧私有模块布局即可。

## 验收

- `cargo test --locked -p haven-memory`：477 passed，2 ignored。
- `cargo clippy --locked -p haven-memory -- -D warnings` 通过。
- `cargo check --workspace --locked` 通过，含 Agent 与 App consumers。
- `cargo fmt --all -- --check` 通过。
- 既有 projection rollback、commit ordering、capacity limit、post-COMMIT broadcast、pending input acknowledgement 和 stable facade 测试全部通过。
