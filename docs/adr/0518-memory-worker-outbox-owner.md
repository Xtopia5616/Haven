# ADR 0518：隔离 MemoryWorker durable outbox owner

## 状态

已完成（2026-10-06）。

## 背景

`MemoryWorker` 同时拥有 fact/summary inference、durable extraction outbox、prompt prefetch 与 MEMORY-fence dirty 状态。它们生命周期不同：outbox 以 durable marker 为唯一 backlog，负责 bounded page scan、公平调度、退避、条件 ack 和取消恢复；prefetch 是 best-effort、按 session 去重并受双槽限制；inference 负责事实准入、cursor 与原子持久化。

该边界在近几周多次共同修改：`22f6cdf`/`3100c33` 扩展 durable summary/retry，`8247578`/`c32340f` 修改 prompt prefetch，`0e4ff8c`/`880ec96` 抽离 maintenance pass，`5aa659e` 增加 generation-safe ack，`f72e5fe` 完成有界 durable scan。最后一个切片在 `memory_worker.rs` 中增加 659 行、删除 365 行。文件规模只触发审查；准入依据是独立恢复生命周期长期与 prompt/inference 状态共处同一协调对象，导致变更持续集中在同一 owner 文件。

[ADR 0468](0468-memory-worker-maintenance-pass-module.md) 已把全量 maintenance 提取为显式依赖的 pass，并保留 outbox 与 inference 在 `MemoryWorker`。ADR 0259 的有界 outbox follow-up 已完成，现有恢复契约稳定，适合进一步隔离 durable lifecycle。本文修订 ADR 0468 关于 outbox 状态归属的决定，并细化 ADR 0259 决定 1、7、10、12 中的内部所有权：MemoryRuntime 仍独占事件消费、sequence recovery 与 trigger coalescing；MemoryWorker 保留组合入口；durable marker 扫描、dispatch、retry/ack、scanner 生命周期与取消恢复属于 MemoryOutbox。该修订不改变 MemoryRuntime 的事件契约、enqueue 顺序、durable backlog 或 maintenance 顺序。

## 决定

1. 在 `haven-agent::memory_worker` 下增加私有 `MemoryOutbox` owner。它独占 `MemoryStore` 读写入口、scanner 唤醒与 lifecycle 状态、取消 token、worker-start 状态、fact/summary 页面轮转、marker retry/ack 与 poison marker 修复。
2. `MemoryWorker` 保留现有 Agent/MemoryRuntime 组合入口，持有 `Arc<MemoryOutbox>`，并转发 durable enqueue、summary wake、scanner start 与 shutdown。MemoryRuntime 不创建第二个 scanner；durable marker 仍是唯一 backlog 权威。
3. Outbox 通过窄的 crate-private `MemoryExtractionHandler` 调用普通 fact、pause fact 和 summary inference。Outbox 不保存 `MemoryWorker`，不读取其 prompt-prefetch/dirty/throttle 字段；不得以全量 worker 参数或 broad trait 作为快捷访问方式。Inference cursor、事实准入、facts 写入与 MEMORY-fence dirty 更新仍由 `MemoryWorker` 现有 inference owner 完成。
4. Prompt prefetch 的 per-session cancellation、双槽上限、cache-success dirty 语义继续由 `MemoryWorker` 持有。`MemoryWorker::shutdown` 同时停止 Outbox 并取消 prefetch；拆分不能产生未受管的 detached worker。
5. 保持 ADR 0107/0259 全部 durable 语义：每类页面至多 64 个 descriptor，high-water/keyset 分页，fact/summary 公平调度，marker key/value CAS retry/ack，持久退避，坏 marker 修复，restart recovery，取消不误 ack。不得增加进程内 backlog、SQL owner、schema/version、IPC、用户配置或新的公共 crate/API。
6. 将 outbox 专属状态、局部状态/退避测试归属到 `memory_worker/outbox.rs`；需要真实 `MemoryWorker` inference handler、SQLite marker 与 scanner 共同装配的恢复/ack/cancellation 场景，作为 Worker↔Outbox 合同测试留在 `memory_worker.rs`。inference/prefetch 测试留在其真实 owner。

## 拒绝的替代方案

- 只把 `impl MemoryWorker` 方法搬到 sibling 文件：拒绝。它改变文件位置，却仍让 durable coordinator 隐式访问全部 Worker 状态。
- 让 `MemoryOutbox` 长期保存 `Arc<MemoryWorker>`：拒绝。会建立反向依赖，并允许 outbox 读取与其无关的 prompt/inference 状态。
- 创建第二个内存队列、MemoryRuntime scanner 或通用 Job framework：拒绝。会复制 backlog/恢复 owner，并扩大持久化状态面。
- 按 crate 体量拆分 `haven-agent`：拒绝。当前没有独立 crate 消费者或依赖方向收益证据。

## 验收与停止条件

- Outbox 状态字段与实现落在 `MemoryOutbox`；`MemoryWorker` 只组合并转发必要入口；Outbox 使用窄 inference handler，不持有完整 Worker。局部重试/状态规则测试随 owner 放在 `outbox.rs`，Worker 测试只保留跨 owner 的真实装配行为。
- 现有 outbox 回归覆盖 page/high-water/keyset、公平性、retry/ack CAS、坏 marker、取消/重启恢复和多页容量；prefetch 回归覆盖去重、session/shutdown cancellation、双槽和 cache-success dirty；fact cursor 与 atomic write 回归保持通过。
- 保持无 schema/API/IPC 变化和无需数据重置。必须通过 `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、crate dependency inventory、ADR index 与 `git diff --check`。
- 若 outbox 仍需访问全量 Worker、需要第二 backlog/恢复来源、改变 marker 行为，或收益只剩代码搬移，则停止并将候选退回 Deferred。

## 回滚

恢复 `MemoryWorker` 内的 outbox 状态与调用实现，移除私有 `MemoryOutbox`/handler 模块；durable marker 格式、表结构、schema 和外部 API 均不变，无需用户数据重置。

## 实施与验证结果（2026-10-06）

- 新增私有 `memory_worker/outbox.rs`；`MemoryOutbox` 独占 durable `MemoryStore` 读写、分页 scanner、通知、retry/ack 与 lifecycle gate。`MemoryWorker` 作为组合 facade 转发 durable enqueue、summary wake、start 和 shutdown。
- `MemoryExtractionHandler` 只暴露 ordinary fact、pause fact 与 summary inference 三个调用；仅 scanner task 临时持有 trait object，Outbox 不持有 `MemoryWorker`。事实 cursor/准入/原子写、prompt prefetch 与 MEMORY-fence dirty 状态继续由 Worker inference owner 管理。
- Outbox 与 prompt prefetch 共享同一 root cancellation token；scanner startup 与 shutdown 仍由同一 lifecycle mutex 线性化。Durable marker、key/value CAS、64 项分页、公平调度、重试退避与取消恢复行为未变。
- 局部 retry/backoff 单测归入 `outbox.rs`；真实 inference handler、SQLite markers 与 scanner 装配的 ack/recovery/cancellation 情景继续作为 Worker↔Outbox 合同测试留在 `memory_worker.rs`。无 schema、IPC、公共 crate/API 或重置范围变化。
- 通过：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、crate dependency inventory、ADR index 与 `git diff --check`。Agent 单测 599 passed、1 ignored；workspace 测试命令整体成功。
