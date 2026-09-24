# ADR 0311：MemoryWorker 的 LLM 维护持久化通过 MemoryMaintenanceStore

- 状态：Accepted
- 日期：2026-09-25
- 范围：`MemoryWorker` 的 LLM 矛盾仲裁与 predicate merge 的数据库读写边界
- 关联：[ADR 0310](0310-memory-worker-deterministic-maintenance-store.md)

## 背景

ADR 0310 将确定性维护 SQL 收口到 `MemoryMaintenanceStore`，但 `MemoryWorker` 仍用
`MemoryDatabase` 执行 LLM 矛盾仲裁的候选读取/事实 demote，以及 predicate merge 的计数读取/重写。
数据库调度属于 Memory 持久化边界；模型门禁、上下文、解析、安全 gate、日志和失败策略属于 Agent。

## 决定

1. 扩展既有 `MemoryMaintenanceStore`，由 `MemoryService` 构造并向 Worker 提供共享实例，增加
   `list_ambiguous_contradictions`、`demote_fact_ids`、`list_predicate_counts` 和
   `rewrite_predicate`。谓词计数以 `PredicateCount { predicate, row_count }` DTO 返回；矛盾候选
   使用已有 `ContradictionCandidate` / `Fact` 类型。Store 只执行 typed 查询/写入、DTO 映射和
   blocking-pool 调度。
2. Agent 继续拥有 fast-chat 配置判断、提示词和上下文构造、LLM 调用、serde 解析、候选可见性过滤、
   提案数量限制、安全 gate、日志、并发 semaphore 和失败降级。Store 不接收模型提案，也不实现
   维护顺序或策略。
3. 保持现有错误语义：矛盾列表读取失败 warning 并跳过仲裁；门控后的 demote 失败仍按 0 计数；
   predicate count 读取失败 warning 并跳过合并；单项 predicate rewrite 失败 warning 后继续下一项，
   成功行数逐项累加。LLM 维护失败不进入确定性步骤的聚合错误。
4. 每个 store 方法分别进行一个 blocking 调用。demote 和每次 predicate rewrite 复用各自现有
   Database repository 写入语义；不同提案之间不建立新事务或合并原子性。predicate rewrite 仍由
   repository 在其单项写入边界内更新并去重。
5. `MemoryWorker` 的 raw `MemoryDatabase` 生产字段保留给 summary extraction：读取/推进
   `fact_extraction_episode.{session_id}` cursor，以及读取/stamp 与普通抽取共享的
   `fact_extraction_last_run.{session_id}` throttle。它不再承接 LLM maintenance SQL。
   embedding catch-up 继续通过 `MemoryService` 的 `MemoryEmbeddingStore`。

## 影响、验证与重置

- 不改变 schema、kv key、IPC、配置或用户数据格式，无需重置数据库。
- Store 测试覆盖候选和 `PredicateCount` DTO 往返、缺表错误传播、空 demote 返回 0。
- Worker 测试覆盖多个独立 predicate rewrite 的累计计数、矛盾提案经 gate 后 demote，以及
  FastChat 未配置时不调用模型；既有提案 gate 单测继续覆盖置信度、keeper、过期事实与用户事实保护。
- 验收命令：`cargo fmt --all -- --check`、`cargo test -p haven-memory --locked`、
  `cargo test -p haven-agent --locked`、`cargo check --workspace --locked`、
  `cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。
- 回滚本提交即可恢复旧边界；无数据迁移或重置要求。

## 替代方案

- 将配置、prompt、提案过滤和 LLM 策略一并放入 store：会使 Memory 持久化 crate 拥有 Agent 策略，拒绝。
- 将每批 predicate 提案包进一个 store closure/事务：会改变单项失败后继续处理的边界，拒绝。
- 同轮迁移 summary extraction KV：cursor 与共享 throttle 有独立恢复/节流语义，留给后续切片。
