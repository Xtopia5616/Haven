# ADR-0201：统一事实存储与记忆后台编排命名

- Status: Accepted
- Date: 2026-09-22
- Owners: Haven maintainers

## Context

记忆事实的 SQLite 表名是 `memory_edges`，Rust 领域对象、仓库和 Tauri/UI
契约却使用 `Fact` / `facts`；FTS 还使用独立的 `edge` 域名。Agent 的后台事实
编排实现已经是 `MemoryWorker`，但 `InferenceEngine` 仍以兼容类型别名导出，
内部组合字段也继续使用 `inference`。这些名称描述同一条事实记忆链路，却让
调用方无法判断哪些是当前正式入口。

## Decision

1. 事实存储的正式名称统一为 `facts`：SQLite 表、索引、FTS 域和仓库边界都
   使用 `fact` / `facts`；`Fact`、`FactGraph`、`fact_query` 和现有 `fact-*`
   ID 规范保持不变。SPO 图谱仍是实现结构，但不再作为持久化表的公开命名。
2. 后台事实编排的唯一正式入口是 `MemoryWorker`。删除 `inference.rs`、
   `InferenceEngine` 导出和测试别名，并将 Agent 组合字段、Hook 注入点统一为
   `memory_worker`。
3. 这是破坏性契约变更。schema 从 v25 升至 v26；不提供
   `memory_edges` → `facts` 的运行时迁移，也不保留 `InferenceEngine` 源码别名。
   旧数据库按发布与重置说明删除后重新创建。

## Alternatives

- 将 `Fact` 全部改为 `MemoryEdge`：会把当前稳定的 Tauri/UI、`fact-*` ID 和
  用户可见事实语义改成图实现术语，扩大了不必要的跨端契约变化。
- 保留表名并只更新注释：不能消除 schema、FTS 与领域名称分裂，也会继续保留
  已过期的 Agent 兼容入口。

## Consequences

- 记忆存储、FTS 和 embedding owner 查询只有一套事实表名；旧表不会被打开或
  自动复制。
- 外部调用方必须使用 `MemoryWorker`；旧 `InferenceEngine` 代码需要源码迁移。
- 用户现有数据库中的会话、事实、任务和用量在升级时一并按 reset contract
  清除；配置文件可以按发布说明单独保留。

## Verification

- `cargo test --locked -p haven-memory`
- `cargo test --locked -p haven-agent`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --locked -- -D warnings`
