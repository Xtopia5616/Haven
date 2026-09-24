# ADR 0310：MemoryWorker 确定性维护通过 MemoryMaintenanceStore

- 状态：Accepted
- 日期：2026-09-25
- 范围：`MemoryWorker::run_memory_maintenance` 的确定性数据库维护操作
- 关联：[ADR 0308](0308-memory-worker-fact-extraction-state-store.md)、[ADR 0309](0309-memory-worker-fact-batch-through-store.md)

## 背景

ADR 0308、0309 已把普通事实抽取状态和 prepared fact batch 写入收口到 typed store，但
`MemoryWorker::run_memory_maintenance` 仍持有 raw `MemoryDatabase` 执行多个确定性清理 SQL。
这些 repository 调用属于记忆持久化边界；维护周期、步骤顺序、日志、告警、总计数和失败隔离仍
属于 Agent。

维护中的规则矛盾 keeper 是确定性 repository 操作；它与之后的 LLM 残余矛盾仲裁、谓词合并有
不同边界。embedding 孤儿清理与 embedding catch-up 也不同：前者是本地 SQL 清理，后者包含
provider 请求和索引维护策略。

## 决定

1. 在 `haven-memory` 新增 `MemoryMaintenanceStore`，只公开有名称的维护操作：
   `dedup_facts`、`delete_sensitive_facts`、`resolve_contradictions`、
   `flush_low_confidence`、`prune_orphaned_embeddings`、
   `cleanup_orphan_extraction_cursors` 和 `cleanup_orphan_source_refs`。
2. Store 的每个调用分别调度一个既有 Database repository 方法到 SQLite blocking pool，返回该
   方法的计数或原始结构化错误。操作之间不新增事务，也不改变 repository 已有的事务行为。
   没有创建泛化的 `run_blocking` facade。
3. `MemoryWorker` 仍按 dedup、敏感事实删除、规则矛盾 keeper、低置信度 flush、embedding prune、
   orphan cursor 清理、source ref 清理的顺序调度这些端口。每项失败仍记录原有级别的日志并继续
   后续项；只要存在失败，全部确定性步骤结束后仍返回带各步骤错误的聚合错误，不返回部分计数，
   且与此前相同，跳过后续 LLM 维护和 embedding catch-up。
4. 规则 keeper 在 alias merge 之后的第二次调用也改经 `MemoryMaintenanceStore`。这次调用仍是
   best-effort：失败只 warning 并按 0 计数，不并入首段聚合错误。
5. Store 的操作可选择接收 `CancellationToken` 并使用 `Database::run_blocking_cancellable`。
   MemoryRuntime 将周期调度 token 传给确定性维护段；取消阻止尚未开始的操作，并中断支持中断的
   SQLite 调用。Worker 在步骤边界停止后续维护。手动维护入口保持无外部取消 token 的行为。
6. `prune_orphaned_embeddings` 纳入 Store，因为它只做孤儿 vector/LSH 行清理；embedding
   generation、catch-up 与 LSH lagging 策略仍由 `MemoryService`/`MemoryEmbeddingStore` 所有。
   `cleanup_orphan_extraction_cursors` 也纳入：它会清除已删除 session 对应的普通/summary
   extraction cursor、节流 stamp 和 pending marker，以及 memory-event cursor；这属于周期清理
   SQL，不迁移 summary extraction 的 cursor 读取/推进或节流策略。
7. 以下路径继续保留 `MemoryDatabase`，作为后续独立切片：
   - LLM contradiction arbitration 的 `list_ambiguous_contradictions` 与 `demote_fact_ids`；
   - LLM predicate merge 的 `list_predicate_counts` 与 `rewrite_predicate`；
   - summary extraction 对 `fact_extraction_episode.{session_id}` cursor 的读取/推进，以及对共享
     `fact_extraction_last_run.{session_id}` throttle 的读取/stamp。
   `MemoryStore` 已持有 summary episode/outbox 的 durable enqueue 和 episode 输入读取；本 ADR
   不迁移 summary extraction 路径其余部分。Embedding catch-up 不经 `MemoryDatabase`，维持既有
   `MemoryService`/`MemoryEmbeddingStore` 边界。

## 影响与验证

- 无 schema、IPC、配置或依赖变化，无需重置数据。
- Store 测试覆盖维护计数、缺表错误传播、前项失败后可继续调用后续 typed operation，以及已取消
  token 不启动数据库工作。
- Agent 回归测试覆盖成功总计数、prune 失败后 cursor/source-ref 步骤仍执行并返回聚合错误，以及
  取消在首个数据库步骤前阻止维护。
- 验收命令：`cargo fmt --all -- --check`、`cargo test -p haven-memory --locked`、
  `cargo test -p haven-agent --locked`、`cargo check --workspace --locked`、
  `cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 替代方案

- 将所有清理 SQL 留在 Agent 并直接调用 `MemoryDatabase`：Agent 继续拥有 SQLite 调度，拒绝。
- 用一个多步骤 closure/通用 blocking facade 代替逐项 typed 操作：会把策略顺序和部分失败处理
  也挪到 store，且需要新增聚合 report；逐操作端口已能保留原有 best-effort/聚合错误行为，拒绝。
- 把 LLM 提案、summary cursor/throttle 或 embedding catch-up 同轮迁入：策略边界不同，超出本切片。

## 回滚

代码回退并恢复 `MemoryWorker` 对同一组 Database 方法的调用，删除 `MemoryMaintenanceStore`、
其测试和文档条目即可。无数据格式或 schema 迁移要求；回滚代码不要求重置数据库。
