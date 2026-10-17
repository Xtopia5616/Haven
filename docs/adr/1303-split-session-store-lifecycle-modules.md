# ADR 1303：拆分 SessionStore lifecycle persistence 模块

状态：已接受并实施（2026-10-17）。

## 背景

`repositories/session_events.rs` 同时承载会话 fork、ingress seed、pending input、recovery partial/terminal result、tool-step persistence，以及 event append、transcript projection、replay 和 rollback。前一组操作有独立职责，但仍应使用同一个 `SessionStore` facade 和 Memory 数据库 owner。

## 决定

1. 将 fork、ingress/recovery、pending input 和 tool-step API 分别移入私有 `forks`、`ingress`、`pending_inputs` 与 `tool_steps` 模块。各模块仍以 `impl SessionStore` 实现对应方法；调用者继续使用原 `SessionStore` facade。
2. 将 ingress message-row helper 留在 `ingress` 模块。它只在父模块单测中开放 `pub(super)`，供现有 ID 幂等与冲突断言直接验证；不会成为 crate 或外部 API。
3. 保留原有 SQLite 事务、验证顺序、阻塞池执行、取消行为、post-commit event/cache 发布和恢复语义。本切片只调整私有代码归属，不新增第二个 store、恢复来源或投影路径。
4. 既有 facade 测试继续覆盖这些方法，包括 fork、tool-step identity/outcome、ingress media/ID 校验、recovery partial 幂等、message-row 冲突和 pending marker 读取。其余 event append、transcript projection、replay/read 与 rollback/usage 实现留在 facade，按后续子切片迁移。

## 替代方案

- 为每一组职责创建独立 store：拒绝。会复制数据库 owner，并模糊 event/projection 的事务边界。
- 只按行数拆文件：拒绝。模块按生产操作族分组，后续事务核心仍按 append/projection、replay/read、rollback/usage 的一致性边界拆分。
- 在本切片重写持久化操作或测试夹具：拒绝。无行为变化的迁移应保留已验证的语义和故障断言。

## 影响与恢复

数据库 schema、事件格式、消息身份、IPC 和用户可见行为均未变化，不需要数据库重置或数据迁移。回退时恢复原私有文件布局即可；外部消费者不受影响。

## 验收

- `cargo test --locked -p haven-memory`：477 passed，2 ignored。
- `cargo test --workspace --locked` 通过。
- `cargo clippy --workspace --locked -- -D warnings` 通过。
- `cargo fmt --all -- --check` 通过。
- 测试仍通过 `SessionStore` facade 验证既有失败与恢复行为；本切片没有更改这些路径的逻辑。
