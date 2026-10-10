# 0892：在 Memory 内部打开数据库并装配 typed stores

## 状态

已接受并实现（2026-10-10）。

## 背景

`Database` 虽位于私有 `db` module，仍从 `haven_memory` crate 根公开；App composition root 直接打开它并逐个调用 `SessionStore::new`、Memory store constructors 和 `ToolRunStore::new`。这让任意下游 crate 都能绕开 typed store，调用挂在 `Database` 上的领域方法，也让 App 依赖 Memory 的 repository 装配清单。

生产调用图显示，App 需要的能力是两份各自拥有 live-event channel 的 `SessionStore`、一份共享的 fact store、`MemoryService` 所需的 store bundle，以及 `ToolRunStore`。业务调用已经通过这些 typed stores 完成；原始 `Database` 主要承担连接与构造职责。测试则确实需要显式 raw SQL 和直接 DB fixture API。

## 决定

- 在 Memory 中增加 `MemoryPersistence`：它私有持有 `Arc<Database>`，负责打开当前数据库并创建 Session、fact、Agent memory 和 ToolRun typed stores。每次 `session_store()` 都创建独立的 live-event channel，共用同一数据库。
- 将 `MemoryStores` bundle 放在 Memory crate，由 Agent `MemoryService` 直接接收；删除 Agent 自己重复定义的 `MemoryServiceStores` 装配 DTO。
- 默认生产构建不从 crate 根导出 `Database`；各 store 的数据库构造器只对 Memory crate 可见。跨 crate 的生产代码通过 `MemoryPersistence` 获得 typed stores。
- `Database` 与 raw 构造/fixture API 只通过非默认 `test-support` feature 暴露，供测试使用。它不构成生产依赖注入入口。

## 影响

- App 不再打开 Database 或知道 Memory repository 的构造顺序；业务调用只能取得各自所需的 typed store。
- 默认构建中，下游无法命名 `Database`，因此也无法直接调用其公开领域方法、缓存 helper 或 SQL 连接。
- 保留既有数据库 schema、事务、缓存失效和 session event 广播语义；不需要数据重置。
- 测试可显式启用 `test-support` 继续使用 direct DB fixture API；生产依赖不启用该 feature。

## 验证

- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo fmt --all -- --check`

## 替代方案

- 仅约定 App 以外不要调用 `Database`：拒绝，因为任意下游仍能绕过 typed stores，约定无法由编译器保证。
- 让 App 继续逐个创建 store：拒绝，因为 repository 装配属于 Memory 的数据层职责；App 只应选择并组合应用能力。
