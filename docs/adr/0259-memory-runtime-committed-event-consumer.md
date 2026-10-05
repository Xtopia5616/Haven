# ADR 0259：MemoryRuntime 消费已提交会话事件

> 当前 app/Agent 所有权与启动入口由 [ADR 0367](0367-memory-runtime-application-ownership.md) 修订：事件处理语义不变，MemoryStartup 由 ApplicationRuntime 持有并负责 prepare/live task 注册；本文旧的 AgentLayer start wiring 仅是当时实现记录。

- 状态：Accepted（设计已采纳；实现未开始）
- 日期：2026-09-24
- 范围：`haven-agent` 记忆后台触发、`haven-memory::SessionStore` 事件订阅与内部 `kv_store`
- 关联：[ADR 0107](0107-durable-memory-extraction-outbox.md)、[ADR 0207](0207-session-store-replay-boundaries-and-durable-ui-sequences.md)、[ADR 0247](0247-memory-worker-inference-port.md)

## 背景与现状

`SessionStore` 的 `subscribe_from(session_id, after_sequence)` 先订阅共享 broadcast，再从 SQLite 读取该 session 更大的 sequence；回放和 live 可能重叠，调用方必须按 `(session_id, sequence)` 去重。broadcast 容量为 256，lag 之后必须从持久化 sequence 重放。`read_from`/`subscribe_from` 返回 append-only 原始行；其中也包括后来被 `timeline_rollback` 覆盖的历史。当前实现位置为 `crates/memory/src/repositories/session_events.rs` 的 `SessionStore::{subscribe,subscribe_from,read_from,read_active,latest_sequence}`。

现有事件含义不可混用：`transcript` 承载已提交的 ReAct transcript/compaction 记录；`usage_recorded` 与 `usage_discarded` 是用量账本事件；`recovery_persistence` 是恢复控制标记。它们共用每个 session 的 sequence，但 usage/recovery 不构成记忆输入。`read_active` 负责解释 rollback 与 `compact_summary` 根；单纯 `read_from` 不做 active timeline 过滤。事件类型定义见 `crates/memory/src/repositories/session_events.rs`。

`AgentLayer::new` 当前创建 `MemoryWorker`，并通过 `InferCallback` 将 `DefaultHooks::before_step` 的间隔触发和 `DefaultHooks::on_pause` 的 bypass 触发直接转给 `MemoryWorker::enqueue_infer`（`crates/agent/src/layer.rs`、`crates/agent/src/react/hooks.rs`、`hook_policy.rs`）。`on_pause` 对 `TurnEnd`、`Ask`、`Confirm`、`Budget`、`External` 都请求 `bypass_throttle=true`。ReAct transcript 先由 `apply_transcript` 提交到 `SessionStore`，再发布 committed UI 并更新进程内投影（`crates/agent/src/react/transcript.rs`）；因此记忆触发与 transcript commit 当前不是同一事务。

`MemoryWorker` 的 `fact_extraction_pending.{session_id}` durable outbox、内存 coalescing、`fact_extraction.{session_id}` 用户消息 cursor、`fact_extraction_last_run.{session_id}` 节流时间戳由 `crates/agent/src/memory_worker.rs` 与 `crates/memory/src/repositories/kv_store.rs` 使用。ADR 0107 规定 pending marker 只有成功后才清除；失败或进程退出会留给后续 enqueue/重启恢复。抽取从当前 messages/steps 投影构造窗口，失败不推进用户消息 cursor；空结果是成功。`fact_extraction_episode.{session_id}` 是另一个 compaction-summary cursor。Summary 目前由 `persist_compaction_summary` 写 episode 后直接调用 `enqueue_summary_extract`，采用独立、最多 8 次的进程内重试，不属于 durable fact-extraction outbox。

应用在 `crates/app-binary/src/app_state.rs` 装配 `AgentLayer`，并用 application runtime 的 child cancellation token 每 6 小时调用 `run_memory_maintenance`；`AgentLayer::start_inner` 再启动 session dispatcher 与生命周期订阅。`SessionStore::new(db)` 会新建 broadcast sender；只有对同一 store 实例做 `clone()` 才共享 live stream。MemoryRuntime 必须和 ReAct writer 使用同一个 SessionStore 实例，不能只共享同一个 `Database`。

## 决定

1. 在 `haven-agent` 增加 app-lifetime `MemoryRuntime`，作为唯一的 session-event consumer 和记忆 job 调度入口。它消费持久化事件，按 session 合并工作并调用现有 `MemoryWorker` 执行事实抽取；不把 provider、事实准入或 recall 逻辑搬进事件存储 crate。
2. ReAct 只发出 typed `SessionCommitted` 触发意图；该意图作为新增 `memory_trigger` event 写入 `session_events`，payload 至少含 `trigger_kind`、`bypass_throttle`、`run_id`、`step_number` 和必要的 pause reason，不含 transcript 文本。原有 step interval 产生 `bypass_throttle=false`，原有所有 `on_pause` 原因产生 `true`。MemoryRuntime 只从 committed event 做工作，不接收 `MemoryWorker` 闭包。
3. `memory_trigger` 是调度事实，不代表新 transcript 内容。正常 turn 的最终 transcript/event boundary 必须先提交，再提交对应 trigger；可在 pause boundary 的 SessionStore 写批次中一并追加 trigger，保证 marker 自身 durable。Consumer 不根据普通 `transcript` 行、session UI 状态或 `usage_recorded` 推测 turn 已结束。终止 `Completed`/`Error`/`Cancelled` 路径不额外合成 bypass；保持当前 `on_pause` 触发边界。`compact_summary` 的 episode 写入/抽取保持现有独立流程，直到后续阶段引入 durable episode job。
4. 新增独立进度 key `memory_event_cursor.{session_id}`，值为最后成功处理的 session event sequence。它只用于事件回放/去重，不能复用 `fact_extraction.{session_id}`（用户 message ID）、`SessionCursor.event_cursor`、`event_sequence` 或 `last_msg_at`。Rust/DB key 仍采用 `domain.key`，清理 session、清空历史和 orphan cleanup 必须同步清理该 key。
5. 对每个 session 严格按递增 sequence 处理。回放与 live 重叠、重复广播或重启重放时，`sequence <= memory_event_cursor` 直接跳过；收到大于 `cursor + 1` 的事件时，先从 durable store 补读缺口。仅 `memory_trigger` 入队：先将现有 `fact_extraction_pending.{session_id}` 写入 durable outbox，再推进 `memory_event_cursor`。无关事件仅推进 event cursor。`memory_trigger` 事件 cursor 与 message cursor 是两个独立时钟。
6. 处理 `memory_trigger` 时只写既有 pending outbox，再唤醒 worker。outbox 仍是一 session 一个 job，marker generation 使用触发 event sequence；较新的 trigger 替换较旧 generation，`false` 可被同 session 的 `true` 升级且不可降级；成功完成后只确认 worker 实际读取的 generation 和 bypass 值。这样同值新 trigger 也不能被旧 job 清除。若 marker 仍在，重复 enqueue 同一 sequence 合并为同一 generation；若 marker 已确认但 event cursor 尚未 checkpoint，event replay 会重新创建该 generation 的 marker，抽取 message cursor 会让 worker 安全完成空窗口并再次确认。MemoryRuntime 的 event cursor 不是 job 成功凭据，pending outbox 才是未完成事实。
7. `SessionStore` live broadcast 只负责低延迟唤醒，不是 durable 队列。启动先订阅，再恢复 durable pending outbox 和 session event checkpoints；随后按 `memory_event_cursor` 重放，完成恢复后才开放 dispatcher 的 pending-session recovery/接受新 turn。live 和 replay 通过 sequence 合并；`Lagged`、连接关闭或读取失败时不得快进 cursor，应重放后再继续。回放需分页并限制一次在内存中的事件/任务数量；现有 `subscribe_from` 返回全量 replay，Phase 7.1 应提供 bounded replay API 或等效的有界读取，不把长时间离线 session 的全量历史一次载入内存。
8. 新部署对尚无 `memory_event_cursor` 的既存 session 采用一次性 cutover baseline：以当前 `latest_sequence` 初始化，不全量重放旧 transcript；已存在的 `fact_extraction_pending` 仍按 ADR 0107 恢复。新 session 的初始 cursor 必须为 0 并在首个 durable event 前或同一事务内落库。此选择避免升级时对所有历史会话意外触发 LLM 回填；它无法修复旧版在 cutover 前“transcript 已提交但 outbox 尚未写入”的历史 crash window，该限制须保留为迁移风险，不能声称已补偿。
9. rollback marker、`branch_point`、`usage_recorded`、`usage_discarded`、`recovery_persistence`、interaction 事件及其他控制事件不触发事实抽取，但仍参与 sequence 前进和缺口重放。重放输入以当前物化的 messages/steps 为准，不从 event payload 重建事实窗口；因此 rollback 后不会把已截断投影重新喂给 worker。已经持久化的事实不由本 ADR 撤销，事实来源回滚/撤销属于单独数据语义决策。
10. MemoryRuntime 与 MemoryWorker 共用有限的并发与容量边界：保留 `MemoryWorker` FastChat semaphore（1）、transcript/fact/context 限额、embedding batch 限额及 prompt prefetch slots（2）；事件消费按 session 合并，不缓存 transcript payload，不为每条事件创建永久任务。超过内存队列上限时只留 durable outbox，由 worker 分页恢复。失败不改变 ReAct turn 结果，不忙循环；失败的抽取保留 pending marker 和原 message cursor，按当前策略等待新 enqueue 或下次启动恢复。throttle 导致 `false` 时不确认 marker；pause 的 `true` bypass 规则保持。
11. application cancellation 关闭 event consumer 并停止创建新 job；不得因取消而推进事件 cursor 或清除 pending marker。in-flight FastChat 被取消时按未完成处理。当前 `MemoryInferencePort::fast_chat` 没有显式 cancellation 参数，Phase 7.1 必须在 runtime/worker 边界验证取消 future 后不会 ack；若底层 provider 不响应 future drop，再单独提出 cooperative cancellation 端口决策。`SessionCleanup` 继续清理 prompt-prefetch/dirty state；它不能清除 durable pending job，除非 session 删除事务已同步清理对应 key。
12. 职责拆分：`MemoryRuntime` 负责事件订阅、sequence checkpoint、恢复、coalescing、outbox 派发、任务取消和后续维护调度；`MemoryWorker` 在第一步仍是推理/事实维护 executor，保留 ADR 0247 的 `MemoryInferencePort`。`MemoryService` 继续拥有 recall、prompt candidates/cache、embedding identity 与向量检索；工具读取仍走 `MemoryRecallPort`，PromptBuilder 继续通过 `MemoryService` 读取。若后续确需 `MemoryReader`，它只能是 Agent 到 `MemoryService` 的窄只读 port/适配器，不能合并进 MemoryRuntime 或复制过滤/排序/缓存规则；Phase 7.1 不新增同义 trait，也不改变 recall 查询结果。

## 分步落地

### Phase 7.1：durable event intake 与现有抽取 outbox（本 ADR 的首个代码切片）

代码 Agent 按以下顺序实施，每步独立可测并提交：

1. 为 `SessionStore` 增加共享实例注入、session ID 恢复枚举、有界 sequence replay，以及 `memory_event_cursor`/现有 pending outbox 的 typed 方法；完善 key 清理。没有 schema 变更。
2. 增加 `MemoryRuntime`；用一个 cancellation-aware consumer 先接 live，再按 durable checkpoint 分页 replay；过滤/去重事件、outbox-first 写入、cursor-after-enqueue、lag 恢复均由它单独拥有。
3. 在 Agent composition root 创建一个 `SessionStore`，将 clone 注入 `ReActEngine` 与 `MemoryRuntime`。Runtime 在 dispatcher recovery 前启动；app lifetime cancellation 传入 runtime。`app_state.rs` 现有六小时 maintenance scheduler 在本切片继续调用既有入口。
4. 将 `DefaultHooks` 的 interval/pause 触发改为追加 typed `SessionCommitted`/`memory_trigger`，删除直接捕获 `MemoryWorker` 的 `InferCallback`。必须保留所有 pause reason 的 bypass，且触发记录在 transcript/event boundary 成功后可恢复。`MemoryPatchHandle` 仍可读 MemoryWorker 的 dirty/prompt-patch 状态，作为后续独立解耦工作，不与 trigger 改造捆绑。
5. `MemoryWorker` 只做现有 outbox job executor；将 durable enqueue/ack 暴露为可等待、可报告结果的窄方法，让 Runtime 在 outbox durable commit 后才推进 event cursor。加入失败、重放、重复事件、pause bypass 升级、cancellation 与 bounded replay 的测试。

Phase 7.1 的代码写集限于：`crates/memory/src/repositories/session_events.rs`、`kv_store.rs`、`sessions.rs` 及必要的导出；`crates/agent/src/memory_runtime.rs`（新文件）、`lib.rs`、`layer.rs`、`memory_worker.rs`、`memory_inference.rs`、`react/hooks.rs`、`react/hook_policy.rs`、`react/mod.rs`、`react/event_boundary.rs`、`react/transcript.rs`/`turn_end.rs` 中实际承载触发边界的文件；相应单元/集成测试。只有当现有 composition/start 顺序不能满足启动先于 dispatcher 时，才改 `crates/app-binary/src/app_state.rs`。实现 Agent 必须先核对 `apply_transcript`/pause effect 顺序，再锁定最小 Rust 写集；不得为了路径列表机械触碰所有文件。

### 后续阶段

- Phase 7.2：为 `CompactSummary` episode 抽取定义 durable job/outbox、重启恢复和 cancellation；然后移除 `persist_compaction_summary` 到 `enqueue_summary_extract` 的直接 callback。保留 episode cursor 与现有 summary 提示/准入策略。
- Phase 7.3：评估把定期 maintenance/index catch-up 生命周期移至 MemoryRuntime，以及是否需要 MemoryReader 只读 port；只有职责和行为测试证明能减少重复边界时才执行。
- 后续另行评估 Job 生命周期统一；不能把本 ADR 当作 ActionService/定时任务统一的授权或设计。

## 禁止范围

- 不改数据库 schema/version，不批量迁移或删除用户数据；不改变现有 `fact_extraction_pending` 编码/合并契约、事实准入策略、消息 cursor 语义、节流间隔或失败后的重试节奏。
- 不把 event sequence、message ID cursor、projection `event_cursor`、`last_msg_at` 合并；不按 transcript 文本做去重。
- 不在 `SessionStore`/`haven-memory` 放入 LLM、事实抽取规则、prompt、Agent/ReAct 类型；不让 MemoryRuntime 读取 event payload 来重建 canonical transcript。
- fact marker 的 event-sequence generation 是本 ADR 的补充实现决策；保留旧 `0`/`1` 值读取，不改 schema/version。除此之外 Phase 7.1 不改抽取准入、message cursor、节流间隔或失败重试节奏；summary episode job、summary cursor/最多 8 次重试、MemoryService/MemoryReader recall、MemoryRecallPort、embedding/index identity、PromptBuilder MEMORY fence 与工具输出均不在本切片内。
- 不动 ActionService、Job/action schema 与 UI projection；不将 messaging 合并进 Job；不改 IPC、前端或 unrelated Database 端口。

## 待实现决策 / 风险

- 需要在代码实现前确认 typed `memory_trigger` event payload 的版本策略，以及它与最后一批 transcript projection 是否能由 `SessionStore` 在一个事务中追加；如果不能原子追加，须用故障注入测试证明 trigger 本身提交后即可恢复，并明确 transcript commit 到 trigger append 之间的残余窗口。
- 旧数据切换采用 latest-sequence baseline，不回填；这会保留一次性旧版本 crash window。若要求无损跨版本补偿，需先设计有界 backfill 和费用上限，不能偷偷全量调用 FastChat。
- 现有 `subscribe_from` 的全量 replay 不适合长离线窗口；Phase 7.1 必须选定分页 API、页大小/最大批次和处理期间的 live/replay 合并规则。
- 当前 worker 失败 marker 留待下次新 enqueue/进程重启，summary retry 为有界内存重试。Phase 7.1 只保留这些策略；更主动的 retry/backoff/dead-letter 需要另立决定。
- `MemoryInferencePort` 未接收 cancellation token；必须确认 future drop 足以停止在途 Router 请求，并证明取消不会清理 durable marker。
- rollback 不撤销已写 facts；若需要按来源对 facts 做回滚删除/修正，另立数据契约 ADR。

## 验收门禁（下一位代码 Agent）

行为测试至少覆盖：

- 同一 `SessionStore` clone 上 subscribe/replay 的重叠只 enqueue 一次；忽略其他 session 和非 `memory_trigger` 事件；sequence gap、broadcast lag 与 restart 均能从持久化 cursor 补读。
- outbox 写失败时 event cursor 不前进；marker 已写、cursor 未写时重放不丢工作；job 成功后才清 marker；失败/限流/取消保持 marker 与原 message cursor。
- 正常 interval 仍为 `bypass=false`；`TurnEnd`、`Ask`、`Confirm`、`Budget`、`External` 都为 `true`；同时到达的 `true` 不被已运行的 `false` job 清除。
- `TRANSCRIPT` 触发边界、`USAGE`/`RECOVERY` 忽略规则、rollback 后只从当前物化投影抽取、session 删除时 cursor/outbox cleanup，以及 app cancellation 不确认任务。
- 有界 replay、单 worker 并发限额、长 transcript 和多 session backlog 不产生无界内存队列；记忆失败不改变 ReAct turn 结果；MemoryService recall 的现有过滤、排序、缓存与 tool/prompt 调用测试保持通过。

必须运行：

```sh
cargo fmt --all -- --check
cargo test --locked -p haven-memory
cargo test --locked -p haven-agent
cargo test --workspace --locked
cargo clippy --workspace --locked -- -D warnings
```

### 2026-10-05 实现复核补充：fact marker generation

原始 bool-only marker 有一个可复现的确认竞态：worker 读入某代任务并在模型请求中等待时，Runtime 可为同一 session 的新 `memory_trigger` 写入相同 bool 值并推进 `memory_event_cursor`；旧 worker 只按 bool 删除 marker 后，若进程退出，新 trigger 已无法由 event replay 恢复。实现将 marker 值扩展为 `<event_sequence>:<bypass>`，ack 使用读取到的这两个字段做条件删除。旧 `0`/`1` 值按 generation 0 读取；新 enqueue 保留 bypass 单调升级并取 event sequence 的较大值。只要 marker 仍在，对同一事件的 replay 会合并到同一 generation；marker 已 ack 但 event cursor 尚未 checkpoint 时，replay 可以重新创建该 marker，message cursor 会让重放任务安全完成空窗口并再次 ack。该变化只影响内部 `kv_store` 值，不改变 schema/version；回滚到只识别 `0`/`1` 的旧二进制前需按开发数据库重置流程处理，见 ADR 0107。

本补充不代替 §10 既有的容量要求。当前 `MemoryWorker` 内存队列与 pending marker restore 仍需后续独立切片实现有界队列和分页恢复；完成 generation/CAS 切片后重新经过 roadmap 步骤 0 准入。
