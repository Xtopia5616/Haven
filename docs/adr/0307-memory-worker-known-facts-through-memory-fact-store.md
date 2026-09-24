# ADR 0307：MemoryWorker 已知事实读取通过 MemoryFactStore

- 状态：Accepted
- 日期：2026-09-25
- 范围：`MemoryService`、`MemoryWorker::load_known_facts` 与 `MemoryFactStore`
- 关联：[ADR 0285](0285-app-memory-fact-store.md)、[ADR 0302](0302-tools-memory-fact-store-port.md)、[ADR 0304](0304-agent-memory-recall-store-port.md)

## 背景

`MemoryWorker::load_known_facts` 仍从 `MemoryDatabase` 直接取得 `Database`，并在 Agent 调用 `run_blocking(db.list_facts())`，随后过滤敏感事实并应用 `max_known_facts`。这让事实查询的 blocking 调度、可见性策略和有效置信度顺序仍跨越 Agent/Memory 边界。已有 `MemoryFactStore` 管理事实查询和写入，但没有提供适用于抽取 prompt 的有界事实列表。

## 决定

1. `MemoryFactStore` 增加异步 `list_recent_visible_facts_limited(limit)`。SQLite blocking 调度在 `haven-memory` 内完成，复用 `Database::list_facts` 的有效置信度与稳定顺序，在过滤敏感事实后应用 limit；返回现有 `Fact` 类型。零 limit 返回空结果。
2. `MemoryService` 在构造时创建并持有 `MemoryFactStore`，通过窄 accessor 暴露其 clone。`MemoryWorker` 构造时接收并持有该 store；生产装配从共享 `MemoryService` 取得 handle，不另行创建 store。
3. `load_known_facts` 仅通过该有界端口读取 facts。Agent 继续负责 prompt 行格式、非 user subject 前缀、字段 sanitize、百分比显示和失败降级；warning 文本与读取失败时的空上下文行为保持不变。
4. 本切片只迁移 known-facts prompt 查询。事实写入、KV cursor/throttle、维护扫描、embedding 路径及 MemoryWorker 的 `MemoryDatabase` 兼容句柄仍留在原处；不添加泛型 facade。

## 影响与验证

- 无 schema、IPC、配置或依赖变化，无需重置数据。
- `MemoryFactStore` 测试覆盖零/正 limit、有效置信度顺序、敏感行先过滤再截断。
- Agent 回归测试验证 subject 前缀与 prompt 文本格式、可见结果的排序与数量限制，以及旧敏感事实不会进入抽取上下文。
- 验收命令：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-agent`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 替代方案

- 只在 Agent 包一层异步 helper 并保留 `MemoryDatabase`：仍由 Agent 调度 SQL 且拥有敏感事实过滤，拒绝。
- 在 MemoryFactStore 中先按数据库原始 confidence 截断，再过滤敏感行：会让敏感行消耗 prompt slots，导致可见结果数量和旧行为变化，拒绝。
- 扩展 MemoryWorker 全部 Database 路径：扩大了本轮边界，事实写入、维护和 KV 的行为独立于 prompt context 读取，留待后续切片。

## 回滚

将 `load_known_facts` 恢复为原有 `MemoryDatabase` blocking 查询和 Agent 侧过滤/截断，移除新端口、注入字段与测试，并回退本 ADR、README 索引及架构/路线图记录即可。无数据格式或迁移要求。
