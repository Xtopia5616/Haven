# ADR 0475：统一事实敏感信息检测与清理规则

## 状态

已采纳并实施（2026-10-05）。

## 背景

Memory 的 Rust 敏感信息检测用于事实写入和 recall 过滤；`FactMaintenance::delete_sensitive_facts` 则用独立 SQL 列表永久清理匹配的行。两处规则需要保持一致，但 SQL 使用 `LIKE 'ghp_%'`、`LIKE 'npm_%'`、`LIKE 'dop_v1_%'` 等模式时，`_` 会匹配任意单字符，而 Rust 检测器要求字面下划线。于是例如 `ghpXordinary` 不会被 Rust 判为敏感，却会被定期维护当成凭据删除。这是数据清理的误删风险，不是 crate 体积或行数问题。

## 决定

1. 新增 Memory repository 私有 `fact_security` 策略模块，集中保存敏感 predicate 关键词、凭据 object 前缀和 marker，并提供 Rust detector 与清理 SQL predicate。
2. 批量删除仍由 SQLite 单条 `DELETE` 执行；SQL 条件从相同规则列表生成。前缀使用 `substr` 精确比较，不用带通配符的 `LIKE`。JWT、PEM 和 credential URL 规则也由共享常量驱动；URL 只在 `://` 后出现 `@` 时匹配，保持清理路径原有的精确条件。
3. `facts` 模块继续作为既有公开门面重导出 `is_sensitive_predicate`、`is_sensitive_object` 和 `is_sensitive_text`；不增加跨 crate API、SQLite UDF、schema 或新的敏感信息状态 owner。
4. 用数据库回归覆盖 Rust detector 与 purge 的正向规则，以及含相似但不匹配下划线前缀的普通值，确保清理既删除敏感值，也保留近似普通值。

## 替代方案

- 逐个给现有 `LIKE` 模式加 `ESCAPE` 可以修正已知通配符，但仍保留两份规则列表，未来可能继续漂移。
- 逐行加载后在 Rust 过滤和删除会改变批量清理路径并增加扫描/写入成本。
- SQLite UDF 可重用 Rust detector，但会给连接注册和 SQL 环境增加不必要的运行时依赖。

## 影响与验收

清理范围现在与 detector 的凭据前缀规则一致，避免 `LIKE` 通配符扩大永久删除范围。没有 schema、配置、IPC 或 reset 变更；本次代码只修正后续清理行为，已被旧清理删除的事实无法由该代码恢复。回滚会重新引入 SQL/Rust 规则漂移和误删风险。

验证：

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `git diff --check`

2026-10-05：workspace 测试通过（Memory 387 passed、2 ignored；其余 workspace crate 均无失败），严格 Clippy 与格式检查通过。
