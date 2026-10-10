# 0890：通过 Memory crate 根 facade 暴露跨 crate 契约

## 状态

已接受并实现（2026-10-10）。

## 背景

`haven-memory` 已有 crate 根 re-export，但下游代码仍混用 `haven_memory::Type`、`haven_memory::recall::Type`、`haven_memory::repositories::domain::Type` 和 `haven_memory::embeddings::Type`。这些路径让消费方依赖内部文件布局，也把 repository 实现和跨 crate 领域契约暴露在同一命名空间。

调用图显示，Agent、Tools 和 App 需要的是事实、消息、步骤、用量、outbox marker、事件 DTO 与纯领域策略；它们不需要选择或调用 `repositories`、`recall`、`embeddings` 模块。生产 persistence 行为由 typed stores 提供，实际实现模块只由 `haven-memory` 自己使用。

## 决定

- 将 `repositories`、`recall`、`embeddings` 与既有 `db`、`schema` 一样设为 crate-private 实现模块。
- 把仓内真实跨 crate 消费的领域 DTO、持久化端口、事实安全/规范化策略和 recall 请求/响应类型统一从 `haven_memory` 根导出。
- 将 Agent、Tools、App 和性能测试中的公开子模块路径迁移到 crate 根；不提供旧路径 alias。
- 保留 `Database` 根级构造能力供组合根创建数据库和装配 stores。其剩余领域级公开操作仍需继续按实际消费者审查。

## 影响

- 下游不再依赖 Memory 的源码目录结构；新增或移动内部 repository 文件不会改变调用方的导入路径。
- 跨 crate 的 `Database` 访问仍仅用于组合根构造与 typed store 装配；领域操作保持现有行为和事务边界。
- 不改变 IPC、配置、数据库 schema 或持久内容，无需数据重置。

## 验证

- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo fmt --all -- --check`

## 替代方案

- 保留可公开寻址的 `repositories::*` 并只约定下游不要使用：拒绝，因为约定无法阻止生产 crate 直接依赖实现模块。
- 为每个历史 module path 保留 re-export alias：拒绝，测试版不保留无期限兼容层，且会继续暴露内部布局。
