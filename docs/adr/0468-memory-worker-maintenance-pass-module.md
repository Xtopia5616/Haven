# ADR 0468：隔离 MemoryWorker 定期维护 pass

## 状态

已完成（2026-10-05；同日补正构造器依赖边界）。Outbox 内部状态归属由后续 [ADR 0518](0518-memory-worker-outbox-owner.md) 修订；本 ADR 的 maintenance pass 边界不变。

## 背景

`MemoryWorker` 同时编排 fact/summary extraction、durable outbox、prompt prefetch、embedding catch-up 与全量 maintenance。定期维护本身是一条完整流程：确定性事实清理与失败汇总、可选 LLM predicate merge、merge 后 deterministic contradiction keeper、残余冲突 LLM 仲裁和 embedding 索引追赶。把该流程与实时提取/outbox lifecycle 放在同一文件，令两类任务的依赖和失败策略难以分开审查。

数据库端的确定性维护已由 `haven-memory::MemoryMaintenanceStore` 持有；LLM proposal gate 与 DTO 已由 `fact_inference` 持有；`MemoryRuntime` 持有周期调度与取消/join。此次只隔离 Agent 层 pass 编排，不重新分配这些 owner。

## 决定

1. 新建 `memory_worker/maintenance.rs` 私有模块，以 `MemoryMaintenancePass<'a>` 表示一次 pass；只借用现有 `MemoryMaintenanceStore`、`MemoryInferencePort`、共享 inference `Semaphore` 和 `MemoryService`。不复制 store、inference、并发限制或运行状态。
2. `MemoryWorker::run_memory_maintenance` 与 `run_memory_maintenance_cancellable` 保持原入口并委托给 pass。MemoryRuntime 的六小时 schedule、手动命令入口、普通 fact/summary inference 与 `MemoryWorker` durable outbox 字段/队列均留在原 owner。
3. 保持 deterministic 操作顺序：dedup → sensitive purge → rule contradiction keeper → low-confidence flush（0.3）→ orphan embedding prune → orphan extraction cursor cleanup → orphan source-ref cleanup。确定性步骤失败时继续剩余清理，完成后聚合错误；有聚合错误就跳过全部后续 LLM 与索引追赶阶段，且不返回部分计数。
4. 全部确定性步骤成功后保持：LLM predicate merge → 仅当 merge 行数大于零时再次 rule keeper → residual contradiction LLM arbitration → embedding catch-up → lagging LSH rebuild。单个 predicate rewrite 和 LLM/list/parse/gate/demote 仍按当前 best-effort 策略处理；不改变返回计数口径。
5. 精确保留现有取消检查点。确定性 store 操作继续接收同一 token 并在阶段边界检查；LLM 和 embedding 调用中不新增中断语义，post-merge keeper 继续接收 token，embedding 与 LSH 两次调用间不新增检查。需要改变取消响应时另立行为决策。
6. outbox worker 的 marker 写入、恢复、drain、ack、失败重排及 shutdown/cancel 语义不迁入 pass，持久 marker 仍是恢复权威。第 3 项现有的 orphan extraction cursor 清理仍属于 maintenance store 步骤，保留其清理孤儿 marker 的既有行为。proposal gates、数据库 SQL 和 schema 均不变。

## 替代方案

- 只把方法移至同 crate 文件但仍由全量 `MemoryWorker` 隐式提供依赖：拒绝。文件变小但 maintenance 依赖面仍不显式；pass 仅接收它实际需要的四个现有 capability。
- 将 outbox、提取和 prompt-prefetch 一并迁入 maintenance owner：拒绝。它们有独立的 durable marker 与恢复生命周期。
- 新增 trait/fake store 框架或调整持久化 API 来记录调用次序：拒绝。现有真实 SQLite fixture 可验证关键步骤顺序，不扩大生产抽象。

## 影响与验证

该变化限于 `haven-agent` 私有模块，不修改 schema、IPC 或持久化合同，无需重置用户数据。现有维护回归需保持总计数、确定性失败后的继续清理与错误聚合、预取消短路、逐项 predicate rewrite 计数、contradiction gate 后写入、FastChat 未配置时跳过；增加一项真实 DB 回归固定 keeper 必须先于低置信度清理。outbox marker/ack 恢复测试继续在原 worker 测试区运行。

`MemoryMaintenancePass` 只借用现有四个依赖，worker 入口和调用顺序未变；新增 36 小时事实 fixture 验证 keeper 先于 low-confidence flush，防止将新事实宽限期或两天 demote 年龄上限混入断言。验证：`cargo fmt --all -- --check`、`cargo test --locked -p haven-agent`（569 passed、1 ignored；手动性能 profile 另有 2 ignored）、`cargo clippy --locked -p haven-agent -- -D warnings` 及 `git diff --check` 均通过，既有 outbox marker/ack 回归仍在原测试区执行。

## 构造器依赖边界补正（2026-10-05）

实施复核发现，首次实现的 `MemoryMaintenancePass::new(&MemoryWorker)` 虽只保存四项 capability，但 pass 子模块仍可访问整个 worker 的父模块私有字段。这不符合决定 1 和“显式传入四项依赖”的替代方案约束。现将构造器签名改为分别接收 `&MemoryMaintenanceStore`、`&dyn MemoryInferencePort`、`&Semaphore` 与 `&MemoryService`；worker 的普通/可取消入口及维护测试都显式传入这四项。pass 不能再通过构造参数取得 `MemoryWorker`，维护顺序、取消点、结果和 durable outbox 均不变。

补正验收：`cargo fmt --all -- --check`、`cargo test --locked -p haven-agent`、`cargo clippy --locked -p haven-agent -- -D warnings`、`git diff --check`。

## 回滚

将 pass 实现移回 `memory_worker.rs` 并恢复 worker 内部调用即可；入口、状态和数据格式未变，无数据迁移或用户状态重置。
