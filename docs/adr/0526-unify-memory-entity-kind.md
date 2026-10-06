# ADR 0526：统一 Memory 实体类型

## 状态

已完成（2026-10-06）。

## 背景

Recall 的 `MemoryKind` 与 embedding lifecycle 的 `MemoryEmbeddingEntity` 都只表达 `Fact`、`Episode`，并分别映射到相同的 embedding `entity_type` 字符串。新增记忆实体时，这两组变体和映射需要重复维护。

## 决定

1. `haven_memory::MemoryEntityKind` 是 Fact/Episode 记忆实体域的唯一 Rust 类型，供 `MemoryQuery` 和 embedding lifecycle 请求、结果共用。
2. 它只在 SQLite embedding 边界映射到既有 `entity_kind` 字符串；FTS 继续使用独立的存储词汇，并在 FTS 查询中显式保留 `ITEM` ↔ `EPISODE` 映射。
3. 不保留两个旧枚举的兼容别名；workspace 调用方统一迁移到共享类型。

## 影响

- 变体集合及其字符串映射只有一个 Rust 定义，新增实体时不用维护两份同构枚举。
- SQLite `entity_type` 值、`memory_fts` 词汇、schema、序列化和 recall 行为不变；无需重置数据库或缓存。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo test --workspace --locked`

## 回滚

恢复 recall 与 embedding lifecycle 各自的枚举及其 workspace 调用点即可。没有持久化或配置迁移。
