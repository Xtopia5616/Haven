# ADR 0284：end_session 展示标题读取通过 SessionStore

- 状态：Accepted
- 日期：2026-09-24
- 范围：App `end_session` 的持久会话标题 fallback
- 关联：[ADR 0249](0249-session-store-session-record-reads.md)、[ADR 0277](0277-context-source-session-title-port.md)、[ADR 0283](0283-app-session-record-lookups-through-session-store.md)

## 背景

`end_session` 在异步 Tauri command 中先读 executor 内存会话；executor 没有该会话时，仍直接同步调用 `state.db.get_session`，以持久记录的 `title` 或 `input_text` 生成完成通知标题。这使 App command 保留了 raw Database 读取，并在 Tokio async worker 上执行 SQLite 查询。既有 `SessionStore::session_title` 只返回可选 `title`，不能表达 `input_text` fallback。

## 决策

1. 新增异步 `SessionStore::session_display_title(session_id)`，在 `Database::run_blocking` 中调用既有 `get_session`；记录存在时返回 `title.unwrap_or(input_text)`，记录缺失时返回 `None`，查询错误原样返回。
2. `end_session` 继续优先采用 executor 会话的 `title` 或 `input`。仅在 executor 没有该会话时，才调用上述端口。缺失记录和查询错误继续产生空标题；查询错误记录已清理的 warning，并继续结束会话。
3. 标题解析仍发生在 executor `end_session` 之前。`end_session` 调用、错误映射、completed 通知及其顺序、IPC 契约和通知文本均不变。
4. 保留同步 `SessionStore::session_record` 等现有调用方；不修改 schema、持久化数据、Memory facts 或 AgentLayer。

端口使用现有 `run_blocking` 取消边界：调用方 future 被丢弃时，已启动的 blocking 查询可能继续执行。

## 影响与验证

- `end_session` 不再直接访问 `state.db`；持久标题读取由 `ApplicationRuntime` 注入的 `SessionStore` 承接。
- Memory 测试覆盖有标题、无标题时使用 `input_text`、以及记录缺失。App helper 测试覆盖 executor 优先级、持久 fallback 和查询失败降级。
- 验证通过：`cargo fmt --all -- --check`、`cargo test --locked -p haven-memory session_store_`（18 项）、`cargo test --locked -p haven-app-binary`（144 项）、`cargo clippy --locked -p haven-app-binary -- -D warnings`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、`corepack pnpm --dir ui run check`（0 错误/警告）、`corepack pnpm --dir ui run test:run`（99 个文件、710 项）和 `corepack pnpm --dir ui run build`。

## 回滚

恢复 `end_session` 通过 `state.db.get_session` 的 fallback，删除 `SessionStore::session_display_title` 及其测试，并移除此 ADR、索引和路线图记录。无 schema 或数据重置要求。
