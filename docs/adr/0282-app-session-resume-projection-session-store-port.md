# ADR 0282：App 会话恢复 read model 通过 SessionStore

- 状态：Accepted
- 日期：2026-09-24
- 范围：App `get_session_for_resume` 与 `get_last_conversation` 的恢复响应读取
- 关联：[ADR 0279](0279-app-history-session-store-ports.md)、[ADR 0280](0280-fresh-run-conversation-window-session-store-port.md)

## 背景

`resume_response_for_session` 在异步 Tauri 命令路径中同步读取 messages、steps、
session usage、LLM usage 和 active domain events。这让命令层持有 SQLite 细节，且
同步查询占用 Tokio async worker。恢复响应 DTO 与 interaction event 解码仍属于 App
边界；底层各查询已有稳定结果类型和数据库实现。

## 决策

`SessionStore` 提供异步 `session_resume_projection(session_id)` typed port，返回
cloneable `SessionResumeProjection`，包含现有 `Message`、`SessionStep`、
`SessionUsage`、`LlmCallUsage` 和 active `SessionEvent` 集合。端口在一个
`Database::run_blocking` closure 中，依次调用既有 messages、steps、session usage、
LLM usage 和 active domain event 查询。查询顺序、各自错误和结果保持不变；不新增
事务或跨查询一致快照承诺。dropping 调用方 future 不保证中断已启动的 blocking 查询。

App 仅将投影映射到既有 `SessionResumeResponse`，继续在 App 解码 active interaction
事件并生成 renderer projection。IPC 字段与 JSON 保持不变。当时两个 command 保留原有
session record 查询；这些入口随后由 [ADR 0283](0283-app-session-record-lookups-through-session-store.md)
迁移到异步 SessionStore ports，并保留 `get_session_for_resume` 的
`Session not found: {id}` 语义。

## 影响与验证

- Resume read-model SQLite work 在 Tokio blocking pool 执行，App 不再逐项直接读取
  messages、steps、usage 或 active domain events。
- Memory 测试将 typed projection 与原 Database/SessionStore 查询结果比较；App 测试
  固定恢复响应既有 6 个 IPC 顶层字段。
- 验收：`cargo fmt --all -- --check`、haven-memory focused test、
  `cargo test --locked -p haven-app-binary`、`cargo check --workspace --locked` 和
  `cargo clippy --workspace --locked -- -D warnings`。

## 回滚

可恢复 App 中的逐项读取并删除 `SessionResumeProjection` 与
`SessionStore::session_resume_projection`。本切片不改 schema、持久化数据或 IPC DTO，
无需数据重置。
