# ADR 0534：Memory 查询缓存使用 Cache 角色命名

## 状态

已采纳并实施；Memory crate 门禁通过（2026-10-06）。

## 背景

`haven_memory::cache::QueryCacheStore` 这个 crate-private 类型的名称使用了 `Store`，但它不读写 SQLite，也不持久化业务状态。它由 `Database` 持有，只实现进程内的有界查询结果缓存：TTL、LRU 淘汰、按域/按键 generation 失效，以及防止过期并发查询覆盖新结果。Memory 的 `*Store` 类型则是真正的持久化访问边界。

## 决定

1. 将 `QueryCacheStore` 改为 `QueryResultCache`，明确它是可丢弃、可重建的性能缓存。
2. 保持 `Database` 对缓存的唯一持有关系和现有 cache invalidation 时机；SQLite 查询、事务和 durable state 仍由 Database/repository 路径拥有。
3. 不合并 `QueryResultCache` 与 Memory 持久 `*Store`，也不更改缓存容量、TTL、generation 或失效语义。

## 替代方案

- 保留 `Store` 后缀：拒绝。它与同 crate 的持久 SQLite store API 表达相同角色名，但生命周期和权威性不同。
- 将 cache 拆成独立服务或移动到每个 repository：拒绝。Database 当前集中负责失效时机，改变 owner 不属于名称修正且没有重复逻辑证据。

## 影响与验证

- 更新 Memory crate 内的类型引用及当前架构/路线图说明；该类型为 crate-private。
- 不改变 query 输出、失效时机、持久 schema、IPC、事件或用户数据，不需要数据重置。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --locked -p haven-memory`、`cargo clippy --locked -p haven-memory -- -D warnings`、`cargo test --locked -p haven-memory -- --test-threads=1`（401 passed，2 个手工性能测试 ignored）。测试使用执行前不存在的隔离 `APPDATA` 根目录 `target/audit-runtime-data-0534/AppData/Roaming`。

## 回滚

恢复类型名 `QueryCacheStore` 及 `Database` 中的引用，并同步恢复本 ADR 和当前架构/路线图说明。无持久化回滚或数据重置。
