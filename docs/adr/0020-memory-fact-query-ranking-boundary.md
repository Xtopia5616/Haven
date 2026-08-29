# ADR 0020：Memory 事实查询与排序边界

## 背景

`repositories/facts.rs` 在事实图谱写入拆分后仍同时承载事实行映射、列表/批量
读取、FTS/LIKE 搜索、标签查询、缓存读取和有效置信度排序。查询行为与维护、
冲突处理继续混在同一文件中，修改查询列或排序规则时容易扩大审查范围。

## 决定

1. 新增内部模块 `repositories/fact_query.rs`，集中负责事实读取 SQL、行映射、
   FTS/LIKE 查询、标签查询、查询缓存和有效置信度排序。
2. `facts.rs` 继续保留 `Fact` 数据类型、谓词/敏感信息策略、公开
   `Database` 写入外观以及维护/冲突处理；`fact_graph.rs` 继续是唯一事实图谱
   写入实现。
3. 通过 `Database` 的既有方法名和签名保持对调用方兼容；只迁移实现位置，
   不修改查询结果的去重、排序、限制、缓存失效或 FTS/LIKE 降级语义。
4. `fact_query.rs` 依赖事实类型与纯谓词策略，但不引入 Agent、LLM、UI 或
   schema/migration 依赖；查询层只负责持久化读取与排序，不做业务推理。

## 替代方案

- 继续把查询实现留在 `facts.rs`：短期改动少，但会继续扩大事实热点文件，
  拒绝。
- 为查询引入新的公开 repository trait：会扩大 API 和生命周期传播面；当前
  只建立内部模块边界，拒绝。
- 在 SQL 中直接固化有效置信度排序：有效置信度包含时间衰减、耐久度和纯 Rust
  规则，SQL 与 Rust 双写会产生两个排序真源，拒绝。

## 影响

这是内部代码重组，不修改数据库 schema、版本、数据内容、ID 格式、缓存键或
对外 `Database` API，不需要用户重置。后续事实查询变更应优先在
`fact_query.rs` 评估；事实写入仍必须通过 `FactGraph`，维护策略仍在
`facts.rs`。

## 验证

```text
cargo fmt --all -- --check
cargo check --locked -p haven-memory
cargo test --locked -p haven-memory --lib -- --test-threads=1
cargo clippy --locked -p haven-memory --lib -- -D warnings
```

既有 Memory 查询、FTS/LIKE 降级、限制、标签精确匹配、缓存和维护测试继续
覆盖原行为；新增查询模块单元测试覆盖有效置信度相同情况下按最近观测时间的
稳定排序。

## 回滚与重置

代码回滚时删除 `fact_query.rs`，把其 `Database` 查询实现、行映射和排序逻辑
恢复到 `facts.rs`，并恢复模块登记与文档。本次不改变 schema 或持久化格式，
不需要数据库、配置或缓存重置。
