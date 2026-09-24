# ADR 0312：MemoryWorker 摘要抽取状态通过 MemoryFactExtractionStore

- 状态：Accepted
- 日期：2026-09-25
- 范围：`MemoryWorker::infer_facts_from_summary` 的 summary cursor 与共享抽取节流状态
- 关联：[ADR 0308](0308-memory-worker-fact-extraction-state-store.md)、[ADR 0309](0309-memory-worker-fact-batch-through-store.md)、[ADR 0310](0310-memory-worker-deterministic-maintenance-store.md)、[ADR 0311](0311-memory-worker-llm-maintenance-store.md)

## 背景

ADR 0308–0311 已将普通 session 抽取状态、事实批量写入和确定性/LLM maintenance persistence 收口到 typed stores。`MemoryWorker` 仍通过 `MemoryDatabase` 直接读取和推进 `fact_extraction_episode.{session_id}`，并对普通抽取共享的 `fact_extraction_last_run.{session_id}` throttle 执行读取和 stamp。这是 MemoryWorker 最后一条生产 raw Database 路径。

游标读写属于抽取状态持久化；是否节流、何时 stamp、模型调用与游标推进时机仍属于 Worker 策略。MemoryService 继续负责组合 typed stores 与 embedding index。

## 决定

1. 扩展既有 `MemoryFactExtractionStore`，提供明确区分普通/摘要状态的 `ordinary_extraction_cursor`、`advance_ordinary_extraction_cursor`、`summary_extraction_cursor`、`advance_summary_extraction_cursor`，以及两种抽取共用的 `shared_extraction_last_attempt_timestamp` 和 `stamp_shared_extraction_attempt`。普通 transcript 读取命名为 `load_ordinary_transcript`。不新增通用 KV facade。
2. 保持 key 格式不变：普通游标 `fact_extraction.{session_id}`、摘要 episode 游标 `fact_extraction_episode.{session_id}`、共享节流戳 `fact_extraction_last_run.{session_id}`。不改变数据表示或排序。
3. 摘要路径继续先读 episode cursor；episode 已处理时直接完成并跳过模型。节流仅在配置间隔大于零时执行，沿用读取、解析 RFC3339、计算剩余等待和模型调用前 stamp 的顺序。任何游标/节流持久化错误仍返回 `Retryable { wait_secs: 1 }`；stamp 失败时不调用模型，后续重试仍可执行。
4. 摘要 cursor 只在模型返回有效结果且事实持久化成功，或模型返回有效空结果后推进。模型、事实批量写入或 cursor 更新失败都不提前确认 episode；现有节流行为（包括成功 stamp 后模型失败仍保留 stamp）不变。
5. 删除 `MemoryWorker` 的 `MemoryDatabase` 字段和 `MemoryService::database_handle`/wrapper。生产 `MemoryWorker` 只持有 typed store capability；`MemoryService` 可私有持有 backing `Database`，用于构造 stores 与 embedding index，不对 Agent Worker 暴露 raw handle。`MemoryWorker::new` 仅为单元测试便捷构造器，不编译进生产代码。
6. 摘要 episode/outbox 的 durable enqueue 与输入读取仍由 `MemoryStore` 承接；embedding catch-up/LSH 维护仍由 `MemoryService` 的 `MemoryEmbeddingStore` 承接。Agent 继续拥有窗口、模型调用、失败策略和维护调度。

## 影响、验证与重置

- 不改 schema、KV key、IPC、配置或用户数据格式，无需重置数据库。
- Store 测试覆盖 summary cursor 往返、ordinary/summary cursor 隔离、共享 throttle 往返和缺失 kv_store 错误传播。
- Worker 测试覆盖重复 episode 跳过、事实写入失败时 cursor 不推进且成功重试后推进，以及 throttle stamp 写失败不调用模型并可重试。
- 生产代码扫描确认 `MemoryWorker` 不再持有或调用 raw Database；`MemoryService` 只保留私有 backing handle 和 typed store/index 构造用途。
- 验收命令：`cargo fmt --all -- --check`、`cargo test -p haven-memory --locked`、`cargo test -p haven-agent --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 替代方案

- 让 Agent Worker 继续直接调用 Database 的 `get_kv`/`set_kv`：会让最后一段状态持久化继续绕过已建立的 Store 边界，拒绝。
- 新建通用 KV Store 并暴露任意 key：会失去普通/摘要抽取状态的领域语义，也扩大不受约束的持久化接口，拒绝。
- 把节流判断或摘要抽取调度迁入 Memory Store：会把 Agent 策略挪入持久化 crate，拒绝。

## 回滚

回滚本提交即可恢复 summary cursor/throttle 的旧调用；删除本 ADR 与索引/路线图更新，并恢复 `MemoryDatabase` wrapper。无数据迁移或重置要求。
