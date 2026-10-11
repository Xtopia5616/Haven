# 0939：移除 Agent crate 对 Memory 类型的重复导出

## 状态

已接受并实现（2026-10-11）。

## 背景

`SessionStore`、`SessionEvent` 和 `SessionEventInput` 由 `haven-memory` 定义并持有。Agent crate root 曾将这三个类型再次导出，但 Agent 内部和 App/Tools 生产代码均直接通过 `haven_memory` 使用它们；唯一外部调用点是 App 测试中的 `SessionStore` 导入。Agent 没有为这些 Memory 类型增加 Agent 语义或适配。

## 决定

- 删除 Agent crate root 对三个 Memory 类型的 re-export。
- App 测试直接从 `haven_memory` 导入 `SessionStore`，与其持久化 owner 一致。
- 保留 Agent 的 session orchestration 与 ports API；不改变 Memory 类型定义和实现。

## 影响

这是未发布 workspace Rust API 的破坏性收口，只移除无独立语义的源码导入路径。运行时行为、序列化、事件、数据库和 IPC 契约均不变，无需数据重置。

## 验证

- 静态搜索确认仓内没有 `haven_agent` 的 Memory 类型 re-export 消费者。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引及 `git diff --check`。

## 替代方案

- 保留 Agent root aliases 作为便利入口：拒绝。Memory 已是这些类型唯一 owner，App 已直接依赖 Memory；二次导出不提供 Agent capability 或稳定适配。

## 回滚

若 Agent 以后拥有独立的 durable-session port，应以 Agent 自有窄接口表达该能力并由 Memory adapter 实现，不恢复这些类型的并行 re-export。
