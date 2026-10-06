# ADR 0544：移除 SessionEventStore 过渡别名

## 状态

已采纳并实施；Rust workspace 门禁通过。

## 背景

`haven_memory::SessionEventStore` 是 `SessionStore` 的公开类型别名，声明处已说明它是“只消费 append-only event API”的 transitional 名称，并要求新的 owner 使用 `SessionStore`。实际类型并未收窄能力，两者具有完全相同的构造函数和方法。当前生产代码已全部使用 `SessionStore`；遗留引用仅在测试、Agent/Memory re-export 和历史 ADR 中。

## 决定

1. 删除 `SessionEventStore` 类型别名及 Memory、Agent crate root 中的 re-export。
2. 将残留测试引用改为 `SessionStore`，让代码、测试和当前 public API 使用唯一类型名。
3. 保留历史 ADR 中的旧名称，避免改写历史决策文本。

## 替代方案

- 保留 alias 并标 deprecated：拒绝。没有生产消费者需要迁移，deprecated public synonym 仍使读者误以为存在不同边界。
- 将 `SessionStore` 改回 `SessionEventStore`：拒绝。当前持久化 owner 同时负责 event log 与 session 读写/投影边界，`SessionStore` 更准确，且已被生产调用统一采用。
- 将 store 拆成事件与会话两个 owner：拒绝。本次没有重复事务 owner 的证据；append、投影、rollback 和 post-commit broadcast 仍由同一 `SessionStore` 保持原子性。

## 影响与验证

- 移除 crate public alias 属于 Rust API 清理；workspace 内所有调用与测试已迁移。SQLite schema、持久化、事务、IPC、事件 payload 和运行行为不变。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked -- --test-threads=1`、`scripts/check-adr-index.ps1` 与 `git diff --check`。

## 回滚

恢复 `pub type SessionEventStore = SessionStore` 及两处 crate root re-export。无需数据库、IPC 或用户数据迁移。
