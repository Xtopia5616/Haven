# 0906：将 SQLite embedding row 限制在 Memory 与测试 API 内

## 状态

已接受并实现（2026-10-11）。

## 背景

`EmbeddedText` 是 Memory SQLite embedding 表的行投影。工作区生产消费者只使用 `MemoryEmbeddingStore` 的待嵌入/保存操作和 `MemoryRecallStore` 返回的 `MemoryHit`，没有消费者读取或构造 `EmbeddedText`。该类型却从 `haven_memory` 根无条件导出，且 `MemoryHit::from_embedding` 允许下游绕过 Memory 自己的 recall projection。

raw `Database` 只在 `test-support` feature 下提供给跨 crate 测试；其公开 embedding fixture 方法需要返回 `EmbeddedText`，因此测试目标仍需要该类型名。

## 决定

- 将 `EmbeddedText` 的 crate-root re-export 限定为 `test-support` feature。Memory 内部实现仍可使用该 SQLite 行投影。
- 将 `MemoryHit::from_embedding` 收回 crate 内部；生产调用方通过 Memory-owned recall store 获取语义化 `MemoryHit`。
- Memory 生产依赖仍只使用 crate 根暴露的 typed stores 与 domain DTO；`repositories`、`embeddings`、`db` 和 `schema` 实现模块保持私有。

## 影响

- 默认生产构建不再暴露 SQLite embedding 行结构，跨层 retrieval contract 维持在 `MemoryHit`。
- 启用 Memory `test-support` 的测试仍可通过 raw `Database` 检查 embedding 表，并取得其返回值类型。
- 不改变 embedding 数据、schema、索引行为、recall 排序或 IPC shape。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`

## 替代方案

- 将 `EmbeddedText` 留在生产 crate-root API、只约定下游不要使用：拒绝，因为 SQLite 行结构仍会成为可依赖的跨 crate contract。
- 删除 `EmbeddedText`：拒绝，因为 Memory `test-support` 的 raw Database embedding fixture 仍需具名返回类型。
- 让 App 或 Agent 自行转换 embedding rows：拒绝，因为 Memory owner 已提供语义 recall 和 embedding lifecycle store，转移该转换会再次暴露持久化形状。
