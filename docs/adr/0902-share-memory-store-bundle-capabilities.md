# 0902：从单一 MemoryStores 装配共享 typed capabilities

## 状态

已接受并实现（2026-10-10）。

## 背景

`MemoryPersistence` 同时提供 `memory_fact_store()` 和 `memory_stores()`。App composition root 先通过前者为命令及 Admin surfaces 构造一份 `MemoryFactStore`，再通过后者为 `MemoryService` 构造另一份 `facts` store。它们当前只是共享 `Arc<Database>` 的独立 wrapper，没有独立业务状态，但两个构造入口允许同一能力在组合根内被重复装配；若 typed store 后续获得局部缓存或生命周期状态，两份 handle 会产生隐蔽分歧。

`MemoryStores` 已是 Memory crate 面向 Agent/App composition 的 typed capability bundle，且 `facts` 可克隆。App 无需再从数据库 owner 请求第二份 fact store。

## 决定

- `AppState` 只调用一次 `MemoryPersistence::memory_stores()`，并从 bundle 克隆 `facts` handle 供 App 命令、Admin 与 Runtime 使用。
- 同一 bundle 传给唯一 `MemoryService`；Agent 和 App 获得的是同一 typed capability 的克隆，不再由 `MemoryPersistence` 分别构造。
- 删除 `MemoryPersistence::memory_fact_store()`。数据库 owner 不暴露按单个 store 重复装配的平行入口。
- 保留 `session_store()`：每次调用有意创建各自的 live-event channel。保留 `tool_run_store()`：它是独立消费能力，不是 `MemoryStores` 中的成员。

## 影响

- App 命令、Admin、Agent MemoryService 仍通过相同 `MemoryFactStore` API 访问同一数据库；查询、写入、安全和事务语义不变。
- `MemoryPersistence` 的生产 Rust API 收敛为单一 Memory bundle、SessionStore 和 ToolRunStore 三类有意区分的构造能力，不保留旧入口兼容层。
- 数据库 schema、持久字段、IPC、事件与发布重置契约不变。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`（通过；Agent 619、App 235、Memory 408、Tools 805、MCP integration 7 passed；其它 crate 通过，含既有 ignored tests）
- `cargo clippy --workspace --locked -- -D warnings`

## 替代方案

- 保留两个入口并约定调用方自行复用：拒绝，因为类型/API 不能保证单一装配。
- 将所有 stores 改为共享可变容器或新增通用 store factory：拒绝，因为 `MemoryStores` 已表达所需 capability 集合；扩大通用构造面无必要。
