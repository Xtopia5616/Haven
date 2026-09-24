# ADR 0309：MemoryWorker 事实批量写入通过 MemoryFactStore

- 状态：Accepted
- 日期：2026-09-25
- 范围：`MemoryWorker::persist_fact_batch`、`MemoryFactStore::persist_inferred_batch`
- 关联：[ADR 0307](0307-memory-worker-known-facts-through-memory-fact-store.md)、[ADR 0308](0308-memory-worker-fact-extraction-state-store.md)

## 背景

ADR 0308 将普通 session 事实抽取的读取状态和 transcript 投影收口到专用 Store，但 Worker 仍在 `persist_fact_batch` 中持有事实 SQLite 操作：整批存在性查询、逐条 `upsert_fact_with_durability` 和 `FactSourceRef` 持久化。Agent 因而仍需获得 `MemoryDatabase` 才能完成事实抽取。

事实候选的模型解析与信任策略属于 Agent；SQLite blocking 调度、存在性快照、图谱 upsert 和来源引用写入属于 Memory。该批操作应由一个窄的 typed Store 完成，并在任一事实写入失败时回滚整批，避免失败重试重复强化先前已部分写入的事实。

## 决定

1. `MemoryFactStore` 新增 `persist_inferred_batch(Vec<MemoryFactWrite>, new_fact_confidence_floor)`。`MemoryFactWrite` 只承载 Agent 已完成规范化、清洗和白名单过滤的字段、置信度、标签、来源引用、durability，以及 Agent 从规范谓词策略得出的 single-valued 标记。Store 不解析 LLM 输出，也不清洗/标准化候选。
2. Agent 继续拥有空值与敏感值过滤、置信度和 durability clamp、字段长度、谓词标准化、标签白名单、来源消息解析，以及 `0.55` 新事实下限的选择。下限作为明确参数传给 Store；Memory 根据同一事务里的存在性快照执行该门槛：已存在的相同事实继续强化，已存在 single-valued 谓词对允许低于下限的更新。
3. Store 在一个 `Database::run_blocking` closure 中执行一次 `BEGIN IMMEDIATE` 事务：批量读取候选 subject 的已有 triples/pairs，然后按输入顺序调用图谱写入逻辑。强化、单值纠正、用户事实优先、source ref 和 durability 语义保持不变；返回值仍表示是否有事实被插入、强化或纠正。空批次返回 `false`。
4. 任意事实写入失败都会回滚整批以及同事务创建的图谱节点。错误仍包含失败事实的 subject/predicate/object 上下文并传播给 Worker，所以 extraction cursor 不推进，durable outbox marker 仍按既有流程保留。
5. `MemoryWorker` 仍保留 `MemoryDatabase`，仅用于明确尚未迁移的维护 SQL、矛盾候选/裁决与谓词合并、compaction-summary episode cursor，以及 summary extraction 路径对共享节流状态的读写。Embedding catch-up 继续由 `MemoryService` 的 `MemoryEmbeddingStore` 负责。本 ADR 不迁移全量维护，也不宣称 Worker 已无 raw DB。

## 影响与验证

- 无 schema、IPC、配置或依赖变化，无需重置数据。
- `MemoryFactStore` 的内存数据库测试覆盖空批次、既有事实强化、source ref/durability/tag 保留以及后续事实写入失败时的整批回滚。
- `MemoryWorker` 回归测试覆盖 Agent 置信度下限的边界值、既有 fact 强化、single-valued 更新例外、候选标签清洗和空批次。
- 验收命令：`cargo fmt --all -- --check`、`cargo test -p haven-memory --locked`、`cargo test -p haven-agent --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 替代方案

- 由 Agent 继续直接调用 `run_blocking` 与 fact repository API：会继续让 Agent 承担 SQLite 调度、批量存在性查询和写入顺序，拒绝。
- 将阈值、敏感/空值处理、字段清洗或谓词归一化移入 Memory：会把模型输出信任策略移出 Agent，拒绝。
- 同轮迁移维护、矛盾裁决、谓词合并、summary cursor 和 embedding：这些拥有不同策略边界，超出本切片范围，拒绝。

## 回滚

恢复 Agent 中的批量存在性查询与逐条 upsert，删除 `MemoryFactWrite`/`persist_inferred_batch`、相应测试和文档条目即可。无数据格式或 schema 迁移要求；回滚代码不会要求重置数据库。
