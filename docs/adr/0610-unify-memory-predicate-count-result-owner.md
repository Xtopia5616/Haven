# ADR 0610：统一 Memory predicate count 结果 owner

## 状态

已采纳并实施。

## 背景

Facts repository 的 `list_predicate_counts` 原返回 `Vec<(String, u64)>`。异步 `MemoryMaintenanceStore` 已有公开 `PredicateCount { predicate, row_count }`，但要先从 Database 收到 tuple 再逐项重建同一对象；Agent maintenance 最终消费的也是具名字段。

## 决定

1. 将 `PredicateCount` 的唯一类型定义移到 facts domain repository。
2. `FactMaintenance`、`Database` 与 `MemoryMaintenanceStore` 全链路直接返回该类型，删除 store 层 tuple 转换。
3. 保持 `haven_memory::PredicateCount` 及 `repositories::memory_maintenance_store::PredicateCount` 的公开导出路径，并保留 `predicate` / `row_count` serde 字段。

## 替代方案

- 保留 Database tuple 并在 store 层重建 `PredicateCount`：拒绝，同一领域结果仍在仓储与 service 边界之间丢失类型身份。
- 把类型放进 maintenance orchestration：拒绝，predicate/count 是 facts 查询结果，由 facts repository 持有；store 负责异步调度。

## 影响与验证

- Database Rust 方法返回类型从 tuple 改为 `PredicateCount`；Agent 已按已有字段消费。类型序列化 shape、SQL 排序与计数行为不变。
- 命名路线图 §5.7 继续保持 Active；其他 Memory repository tuples 与全仓命名仍待逐项审计。
- 验证：`cargo fmt --all -- --check`、`cargo check --locked -p haven-memory`、`cargo clippy --locked -p haven-memory -- -D warnings`、`cargo test --locked -p haven-memory`、ADR 索引及 staged diff 检查。

## 回滚

将 Database 与 FactMaintenance 返回类型恢复为 `Vec<(String, u64)>`，并在 MemoryMaintenanceStore 重建 `PredicateCount`；serde 数据不需要迁移。
