# 0910：按抽取 outbox 职责命名 Memory store 并封装 store bundle

## 状态

已接受并实现（2026-10-11）。

## 背景

Memory 根导出的 `MemoryStore` 实际只负责 compaction episode 与 extraction marker 的事务写入、outbox marker 的 bounded read/retry/ack，以及 episode text 读取。其泛化名称容易被理解为整个 Memory persistence API；Agent 的 `MemoryService`、`MemoryWorker` 和 `ReActEngine` 也沿用 `memory_store` 字段名。

Agent 另有 `MemoryOutbox` runtime owner，负责 durable marker 的扫描、调度、重试/ack 决策和生命周期。当前命名没有在类型上明确“SQL store 操作”与“Agent outbox 协调”这两层边界。

`MemoryStores` 的唯一生产构造 owner 已是 `MemoryPersistence`，但 bundle 的 store 字段仍为 `pub`。下游可以从多个 bundle 抽取 store 后拼出混合实例，绕过 ADR 0902 的单一装配约束；App 也直接读取 `facts` 字段。

## 决定

- 将 `MemoryStore` / `memory_store.rs` 统一更名为 `MemoryOutboxStore` / `memory_outbox_store.rs`。Agent 的 service、worker 与 ReAct 字段/accessor 使用 `outbox_store` 命名。
- 保持职责分离：`MemoryOutboxStore` 只提供 durable persistence 操作；Agent `MemoryOutbox` 保持 scanner、调度、retry/ack 决策与生命周期 owner，不把这些策略搬入 Memory，也不建立反向调用。
- 将 `MemoryStores` 所有 store 字段设为私有，通过 `outbox_store()`、`fact_store()`、`fact_extraction_store()`、`maintenance_store()`、`recall_store()` 与 `embedding_store()` 返回共享 handle 的 clone。生产构造仍只由 `MemoryPersistence::memory_stores()` 提供；Agent 与 App 不能自行拼装 bundle。`test-support` 下的 Database fixture conversion 仅供测试。
- 更新当前架构和输出契约文档；历史 ADR 保留决策时的旧符号名以供检索。

## 影响

Rust 源码 API 有意破坏性改名，不提供兼容别名。持久化操作、episode/outbox 事务、分页、重试/ack 结果、schema、IPC 与用户可见行为均不改变；无需数据重置。回滚时恢复类型/模块名和 bundle 字段可见性，无数据恢复步骤。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`

## 替代方案

- 只改注释继续使用泛化的 `MemoryStore`：拒绝，因为类型和 Agent 字段仍不能表明它只持久化 extraction outbox。
- 将 durable store 改名为 `MemoryOutbox`：拒绝，因为同名会混淆 Agent 中已存在的协调 owner；`Store` 后缀明确它是 SQL capability。
- 为兼容保留 `MemoryStore` alias 或 bundle 公共字段：拒绝，因为用户要求不做向下兼容，且这些入口会继续允许依赖过宽名称或重组混合 store bundle。
