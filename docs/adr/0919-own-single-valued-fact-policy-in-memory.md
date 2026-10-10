# 0919：由 Memory 决定单值谓词的事实更新策略

## 状态

已接受并实现（2026-10-11）。

## 背景

规范事实谓词、单值谓词集合和事实图 upsert 均由 Memory 持有。但 Agent 构造公开 `MemoryFactWrite` 时还要计算并携带 `is_single_valued_predicate`；`FactGraph` 在批量写入时信任这个调用方布尔值，决定一个低于新事实置信度下限的已有 `(subject, predicate)` 更新是否可入库。规则实际上已有唯一 Memory helper，却被 root 导出供 Agent 再次做决策。错误或其它调用方传入不一致布尔值会改变低置信度候选的接纳结果。

## 决定

- 删除 `MemoryFactWrite::is_single_valued_predicate`。FactGraph 在 Memory 内部基于规范谓词自身的 `is_single_valued_predicate` 策略判断已有 pair 的低置信度更新例外。
- 将 `is_single_valued_predicate` 收为 Memory crate 内部 helper，并从 `haven_memory` 根 API 删除。Agent 只提交已完成字段清洗的 `MemoryFactWrite` 与显式新事实置信度下限，不复制 Memory 谓词策略。
- 保留 Agent 拥有的 LLM 字段清洗、候选置信度与 durability clamp、标签过滤、来源解析以及新事实阈值选择。Memory 的单值/多值语义和事实图更新策略不转移到 Agent。
- 为 MemoryFactStore 添加批量写入回归，验证低置信度的既有 `works_at` 单值 pair 更新仍通过、低置信度的 `likes` 多值新对象仍被阈值过滤。

本 ADR 修订 ADR 0309 中由 Agent 计算并传递 single-valued 标记的实现决定；事实抽取字段清洗与事务原子性决定保持有效。

## 影响

当前 Agent 已按同一个 Memory helper 计算该标记，所以工作区生产行为保持一致；变化是规则现在由真正 owner 计算，调用方无法用 DTO 字段覆盖它。外部 Rust API 有意删除公开 helper 和 DTO 字段，不保留兼容别名。

不改变数据库 schema、已存事实、抽取阈值、来源/标签、IPC、安全策略或事务边界，无需数据重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`
- 全仓确认 Agent 不再引用 `is_single_valued_predicate`，根 API 和 `MemoryFactWrite` 均不再暴露调用方策略标记。

## 替代方案

- 保留调用方布尔值并要求 Agent 总是正确计算：拒绝，因为正确性依赖重复维护的跨 crate 策略调用，Memory 已拥有规则本身。
- 将单值谓词表复制到 Agent 或 Common：拒绝，因为这会产生第二份权威事实图策略。
- 让置信度过滤移入 Agent：拒绝，因为批量写入时对既有事实 pair 的存在性快照由 Memory 在同一 SQLite 事务中读取，阈值例外必须与该快照共同判定。

## 回滚

可恢复公开 helper、`MemoryFactWrite` 标记与 Agent 预计算；无持久化恢复步骤。
