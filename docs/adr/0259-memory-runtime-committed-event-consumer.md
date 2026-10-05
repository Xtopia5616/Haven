# ADR 0259：MemoryRuntime 消费已提交会话事件

> 当前 app/Agent 所有权与启动入口由 [ADR 0367](0367-memory-runtime-application-ownership.md) 修订：事件处理语义不变，MemoryStartup 由 ApplicationRuntime 持有并负责 prepare/live task 注册；本文旧的 AgentLayer start wiring 仅是当时实现记录。

- 状态：Accepted（MemoryRuntime 核心实现及 2026-10-06 有界 durable outbox/session recovery follow-up 均已完成）
- 日期：2026-09-24
- 范围：`haven-agent` 记忆后台触发、`haven-memory::SessionStore` 事件订阅与内部 `kv_store`
- 关联：[ADR 0107](0107-durable-memory-extraction-outbox.md)、[ADR 0207](0207-session-store-replay-boundaries-and-durable-ui-sequences.md)、[ADR 0247](0247-memory-worker-inference-port.md)

## 背景与现状

`SessionStore` 的 `subscribe_from(session_id, after_sequence)` 先订阅共享 broadcast，再从 SQLite 读取该 session 更大的 sequence；回放和 live 可能重叠，调用方必须按 `(session_id, sequence)` 去重。broadcast 容量为 256，lag 之后必须从持久化 sequence 重放。`read_from`/`subscribe_from` 返回 append-only 原始行；其中也包括后来被 `timeline_rollback` 覆盖的历史。当前实现位置为 `crates/memory/src/repositories/session_events.rs` 的 `SessionStore::{subscribe,subscribe_from,read_from,read_active,latest_sequence}`。

现有事件含义不可混用：`transcript` 承载已提交的 ReAct transcript/compaction 记录；`usage_recorded` 与 `usage_discarded` 是用量账本事件；`recovery_persistence` 是恢复控制标记。它们共用每个 session 的 sequence，但 usage/recovery 不构成记忆输入。`read_active` 负责解释 rollback 与 `compact_summary` 根；单纯 `read_from` 不做 active timeline 过滤。事件类型定义见 `crates/memory/src/repositories/session_events.rs`。

`AgentLayer::new` 当前创建 `MemoryWorker`，并通过 `InferCallback` 将 `DefaultHooks::before_step` 的间隔触发和 `DefaultHooks::on_pause` 的 bypass 触发直接转给 `MemoryWorker::enqueue_infer`（`crates/agent/src/layer.rs`、`crates/agent/src/react/hooks.rs`、`hook_policy.rs`）。`on_pause` 对 `TurnEnd`、`Ask`、`Confirm`、`Budget`、`External` 都请求 `bypass_throttle=true`。ReAct transcript 先由 `apply_transcript` 提交到 `SessionStore`，再发布 committed UI 并更新进程内投影（`crates/agent/src/react/transcript.rs`）；因此记忆触发与 transcript commit 当前不是同一事务。

`MemoryWorker` 的 `fact_extraction_pending.{session_id}` durable outbox、`fact_extraction.{session_id}` 用户消息 cursor、`fact_extraction_last_run.{session_id}` 节流时间戳由 `crates/agent/src/memory_worker.rs` 与 `crates/memory/src/repositories/kv_store.rs` 使用。ADR 0107 规定 pending marker 只有成功后才清除；当前容量切片将 retry attempt/deadline 也持久化到 marker，并以 64 项 keyset 页扫描。抽取从当前 messages/steps 投影构造窗口，失败不推进用户消息 cursor；空结果是成功。`fact_extraction_episode.{session_id}` 是另一个 compaction-summary cursor；summary episode 与待抽取 marker 在同一事务提交，facts 与 summaries 共用有界 scanner 和持久退避。

应用在 `crates/app-binary/src/app_state.rs` 装配 `AgentLayer`，并用 application runtime 的 child cancellation token 每 6 小时调用 `run_memory_maintenance`；`AgentLayer::start_inner` 再启动 session dispatcher 与生命周期订阅。`SessionStore::new(db)` 会新建 broadcast sender；只有对同一 store 实例做 `clone()` 才共享 live stream。MemoryRuntime 必须和 ReAct writer 使用同一个 SessionStore 实例，不能只共享同一个 `Database`。

## 决定

1. 在 `haven-agent` 增加 app-lifetime `MemoryRuntime`，作为唯一的 session-event consumer 和记忆 job 调度入口。它消费持久化事件，按 session 合并工作并调用现有 `MemoryWorker` 执行事实抽取；不把 provider、事实准入或 recall 逻辑搬进事件存储 crate。
2. ReAct 只发出 typed `SessionCommitted` 触发意图；该意图作为新增 `memory_trigger` event 写入 `session_events`，payload 至少含 `trigger_kind`、`bypass_throttle`、`run_id`、`step_number` 和必要的 pause reason，不含 transcript 文本。原有 step interval 产生 `bypass_throttle=false`，原有所有 `on_pause` 原因产生 `true`。MemoryRuntime 只从 committed event 做工作，不接收 `MemoryWorker` 闭包。
3. `memory_trigger` 是调度事实，不代表新 transcript 内容。正常 turn 的最终 transcript/event boundary 必须先提交，再提交对应 trigger；可在 pause boundary 的 SessionStore 写批次中一并追加 trigger，保证 marker 自身 durable。Consumer 不根据普通 `transcript` 行、session UI 状态或 `usage_recorded` 推测 turn 已结束。终止 `Completed`/`Error`/`Cancelled` 路径不额外合成 bypass；保持当前 `on_pause` 触发边界。`compact_summary` 的 episode 写入/抽取保持现有独立流程，直到后续阶段引入 durable episode job。
4. 新增独立进度 key `memory_event_cursor.{session_id}`，值为最后成功处理的 session event sequence。它只用于事件回放/去重，不能复用 `fact_extraction.{session_id}`（用户 message ID）、`SessionCursor.event_cursor`、`event_sequence` 或 `last_msg_at`。Rust/DB key 仍采用 `domain.key`，清理 session、清空历史和 orphan cleanup 必须同步清理该 key。
5. 对每个 session 严格按递增 sequence 处理。回放与 live 重叠、重复广播或重启重放时，`sequence <= memory_event_cursor` 直接跳过；收到大于 `cursor + 1` 的事件时，先从 durable store 补读缺口。仅 `memory_trigger` 入队：先将现有 `fact_extraction_pending.{session_id}` 写入 durable outbox，再推进 `memory_event_cursor`。无关事件仅推进 event cursor。`memory_trigger` 事件 cursor 与 message cursor 是两个独立时钟。
6. 处理 `memory_trigger` 时只写既有 pending outbox，再唤醒 worker。outbox 仍是一 session 一个 job，marker generation 使用触发 event sequence；较新的 trigger 替换较旧 generation，`false` 可被同 session 的 `true` 升级且不可降级；成功完成后只确认 worker 实际读取的 generation 和 bypass 值。这样同值新 trigger 也不能被旧 job 清除。若 marker 仍在，重复 enqueue 同一 sequence 合并为同一 generation；若 marker 已确认但 event cursor 尚未 checkpoint，event replay 会重新创建该 generation 的 marker，抽取 message cursor 会让 worker 安全完成空窗口并再次确认。MemoryRuntime 的 event cursor 不是 job 成功凭据，pending outbox 才是未完成事实。
7. `SessionStore` live broadcast 只负责低延迟唤醒，不是 durable 队列。启动先用一条 SQLite statement 为当前已存在且缺 cursor 的 sessions 建立 cutover baseline，再订阅 live broadcast，然后由 MemoryWorker 扫 durable outbox、由 MemoryRuntime 按 `memory_event_cursor` 分页恢复事件。baseline 与 subscribe 间的事件由 append-only event store 补齐；该窗口新建的 session 没有 baseline，会从 sequence 0 恢复。完成恢复后才开放 dispatcher 的 pending-session recovery/接受新 turn。live 和 replay 通过 sequence 合并；`Lagged`、连接关闭或读取失败时不得快进 cursor，应重放后再继续。事件 replay 每页最多 256 条；session ID recovery 每页最多 64 个。
8. 新部署对 baseline statement 执行时尚无 `memory_event_cursor` 的既存 session 采用一次性 cutover baseline：以 statement snapshot 中的 `latest_sequence` 初始化，不全量重放旧 transcript；已存在的 `fact_extraction_pending` 仍由 durable scanner 恢复。baseline 必须发生在 subscribe 之前。新 session 若在 baseline 后创建，其缺省 cursor 为 0 并从 durable event store 恢复。此选择避免升级时对所有历史会话意外触发 LLM 回填；它无法修复旧版在 cutover 前“transcript 已提交但 outbox 尚未写入”的历史 crash window，该限制须保留为迁移风险，不能声称已补偿。
9. rollback marker、`branch_point`、`usage_recorded`、`usage_discarded`、`recovery_persistence`、interaction 事件及其他控制事件不触发事实抽取，但仍参与 sequence 前进和缺口重放。重放输入以当前物化的 messages/steps 为准，不从 event payload 重建事实窗口；因此 rollback 后不会把已截断投影重新喂给 worker。已经持久化的事实不由本 ADR 撤销，事实来源回滚/撤销属于单独数据语义决策。
10. MemoryRuntime 与 MemoryWorker 共用有限的并发与容量边界：保留 `MemoryWorker` FastChat semaphore（1）、transcript/fact/context 限额、embedding batch 限额及 prompt prefetch slots（2）；事件消费按 session 合并，不缓存 transcript payload，不为每条事件创建永久任务。durable outbox 以 marker 为唯一 backlog 来源，fact/summary 各最多物化 64 个短 descriptor，worker 同时最多执行一个 job。失败不改变 ReAct turn 结果，不忙循环；失败抽取保留 marker 与原 message cursor，attempt 和绝对 due time 写回 marker。一般 retryable 失败按 1–30 秒 capped exponential delay 自动重试；summary 的 throttle/Retryable 结果继续尊重其显式 requested wait（可超过 30 秒）。某类退避不阻塞另一类 ready job。throttle 导致 `false` 时不确认 marker；pause 的 `true` bypass 规则保持。
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
- fact marker 的 event-sequence generation 是本 ADR 的补充实现决策；保留旧 `0`/`1` 值读取，不改 schema/version。除此之外 Phase 7.1 不改抽取准入、message cursor、节流间隔或失败重试节奏；summary episode job/cursor 与提取准入也不在该初始切片内，后由本 ADR 的 2026-10-06 follow-up 纳入有界扫描。MemoryService/MemoryReader recall、MemoryRecallPort、embedding/index identity、PromptBuilder MEMORY fence 与工具输出仍不在本切片内。
- 不动 ActionService、Job/action schema 与 UI projection；不将 messaging 合并进 Job；不改 IPC、前端或 unrelated Database 端口。

## 待实现决策 / 风险

- 需要在代码实现前确认 typed `memory_trigger` event payload 的版本策略，以及它与最后一批 transcript projection 是否能由 `SessionStore` 在一个事务中追加；如果不能原子追加，须用故障注入测试证明 trigger 本身提交后即可恢复，并明确 transcript commit 到 trigger append 之间的残余窗口。
- 旧数据切换采用 latest-sequence baseline，不回填；这会保留一次性旧版本 crash window。若要求无损跨版本补偿，需先设计有界 backfill 和费用上限，不能偷偷全量调用 FastChat。
- 现有 `subscribe_from` 的全量 replay 不适合长离线窗口；Phase 7.1 已通过 durable replay page API、固定页大小和 live/replay sequence 合并落实。
- 有界 outbox follow-up 已将 fact/summary retry 状态写回 marker，并采用有限页自动重试；如未来需要 dead-letter、人工重放或审计历史，仍需另立存储与容量决定。
- `MemoryInferencePort` 未接收 cancellation token；必须确认 future drop 足以停止在途 Router 请求，并证明取消不会清理 durable marker。
- rollback 不撤销已写 facts；若需要按来源对 facts 做回滚删除/修正，另立数据契约 ADR。

## Phase 7.1 原始验收范围

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

本补充（generation/CAS ack）不代替 §10 既有的容量要求。2026-10-05 复核时仍需有界队列和分页恢复；随后经 roadmap 步骤 0 准入，具体契约与实现记录见下方 2026-10-06 follow-up。

### 2026-10-06 后续切片契约与实现：有界 durable outbox 扫描（已完成）

步骤 0 复核确认 §7、§10 的容量要求尚未落地：两个 worker `HashMap`、durable restore 的全量 `Vec`、整表 drain 快照、按任务 ID 增长的 retry map，以及 Runtime 的全量 session ID 枚举都可能随 backlog 增长。没有观察到生产 RSS/backlog 事故；本切片依据是既有 ADR 的明确容量合同，不是未经测量的性能优化。

实现前固定以下边界：

1. durable `kv_store` markers 是唯一 backlog 权威；worker 不再按 session/episode 保留全量内存队列。Fact 与 summary 各只持有一个最多 64 项的页面，并且最多一个活跃 job。SQLite 页面按稳定完整 marker key 做 keyset 查询，`LIMIT=64`，不使用 OFFSET；每个扫描 pass 先读取各自全部 marker 的最大 key 作为 high-water，再扫描 `key > cursor AND key <= high_water ORDER BY key`。每页处理后释放，再读下一页。high-water 后的新 key 不延长本 pass；游标之前新增或更新的同 key marker 在下一 pass 从头扫描发现。marker 写入只发 coalesced wake；并发删除不会令下一页 offset 漂移。命名空间使用 BINARY key 范围，不能使用未转义的 LIKE 前缀（其中 `_` 是通配符）。坏 marker 按原始 key 推进页游标并记录错误，避免卡住后续 key。若新的 fact trigger 遇到坏 value，入队事务会把原 value 移到 `fact_extraction_pending_poison.<session_id>`，再写入该 event 的有效 generation，确保 event cursor 可前进；每个 session 保留最近一个原始坏值。Summary retry metadata 损坏时，仅当 composite key 与 `memory_items` 中 episode owner 一致，scanner 才把原值移到 `fact_extraction_episode_pending_poison.<session_id>.<episode_id>` 并恢复一个立即 runnable marker；owner/key 不一致时保留原 marker 并记录错误，不猜测归属。session orphan cleanup 同时清理对应 poison key。
2. Facts 与 summaries 每次各持有一个有限页面；页面内按单 job 交替处理，防止大量 fact 持续压住 summary，单类暂时没有 ready job 时另一类继续。失败任务的 attempt 与绝对 `next_attempt_at_ms` 持久化在现有 marker value，不用按 session/episode 增长的 retry map；一般 retryable 失败使用 1–30 秒 capped exponential policy，summary 的 throttle/Retryable outcome 仍尊重显式 requested wait（可超过 30 秒）。扫描会记住本 pass 最早的 due 时间，并在该 deadline、新 marker 通知或取消时唤醒；某类 backoff 不阻塞另一类 ready job。ack/retry update 都以完整 marker key+value 做 CAS；新 event sequence 会重置该 fact marker 的 due/attempt，重放同一 sequence 保留它们；bypass 从 false 升为 true 时重置退避，以免暂停触发被延迟。
3. Fact marker 新 value 形状为 `<event_sequence>:<bypass>:<attempt>:<next_attempt_at_ms>`；保留旧 `0`/`1` 和上一代 `<sequence>:<bypass>` 读取，均解释为 attempt 0、立即可运行。Summary marker 新 value 形状为 `<session_id>:<attempt>:<next_attempt_at_ms>`；旧单独 `<session_id>` 同样立即可运行。只有同 marker 的条件 ack/重试更新能修改该行。无需 schema/version 变化；回滚到不识别新 marker value 的旧二进制仍按 ADR 0107 的开发库重置说明处理。
4. Runtime 的 session event replay 继续使用现有最多 256 条页；session ID recovery 改为最多 64 个 ID 的 keyset 页。启动时缺失 `memory_event_cursor` 的 session 不能逐页异步 baseline，否则分页期间新建的随机 UUID session 可能被误当作旧 session。改由订阅前的一条 SQLite `INSERT … SELECT` 语句在单一 statement snapshot 上为当时已存在且缺失 cursor 的 sessions 初始化到各自 latest sequence；Rust 不物化 ID 列表。baseline 与 subscribe 之间发生的 event 由后续 durable replay 补齐；该次 statement 之后新建的 session 保持缺 cursor 并从 0 恢复。随后 bounded session-ID pages 只负责事件 recovery，不再做启动 baseline。
5. 最坏同时持有 64 个 fact descriptors、64 个 summary descriptors、一个活跃 job、一个 64 项 session-ID page 与一页至多 256 个 session events；retry 状态不随任务数量占用进程内存。现有 transcript、prompt、embedding、FastChat semaphore 限额保持。SQLite page API 强制 `1..=64`（session ID 页同样 `1..=64`）。
6. Startup 只由 MemoryWorker 开始 durable outbox scan；MemoryRuntime 不做第二次 hydrate。取消、读页失败、推理失败、retry 状态写失败或 ack 失败都不能清除尚未完成的 marker 或推进错误 event cursor。SQLite CAS commit 是 retry/ack 的线性化点：取消生效前已成功提交的 ack 视为已完成；未提交的工作保留 marker。`Ok(false)` 表示 marker 已变化/删除，按 stale descriptor 处理并重新扫描。失败页读取可安全从当前 pass 重试；取消/重启后扫描从首 key 开始。页面中的旧 generation 允许完成，但精确 ack 失败后新 marker 会在下一 pass 读取。

替代方案是把内存 `HashMap` 截断到固定容量并在超额时由额外 overflow map/计数重排。该方案仍需要第二套待办状态与 admission/restore 协调，故选择直接从 durable markers 分页。任务 ID 已按 AGENTS.md 统一格式生成；分页只搬运固定数量的短标识，不保留 transcript/prompt 内容。

回归测试必须证明：markers/session 数量超过多个页面时单页/总页内存计数不超界；marker durable-first 且 overflow 最终执行；页间删除不跳过后续 key、游标前插入或同 key 更新能被下一 pass 找到、high-water 后插入不延长当前 pass；fact/summary 混合持续到达时轮转，某类 backoff 不阻塞另一类；legacy marker 可读取，新事件重置 retry 而同 sequence replay 不重置；取消与重启恢复保留 marker；baseline 与 subscribe 之间创建的新 session 不会被 baseline 跳过。

实施与验证结果（2026-10-06）：

- `MemoryWorker` 移除了生产环境中按 session/episode 增长的 outbox/retry map，改为从 durable marker 扫描；最多同时保留每类一个 64 项 descriptor page 与一个活跃 inference job。事实/摘要逐 job 轮转，失败 attempt/deadline 条件写回 marker。
- `MemoryRuntime` 在 subscribe 前以单条 SQLite statement baseline 旧 session cursor，再订阅、启动唯一 scanner 并按 64 个 session ID 一页恢复。事件 replay 仍保持现有 256 行上限。
- 坏 fact value 不再阻断新的 trigger 入队/cursor checkpoint；原值保存在 session-scoped poison key。可确认 owner 的坏 summary retry value 被隔离后恢复成可运行 job；无法确认归属的行仍保留供诊断。orphan cleanup 覆盖两类 poison key。
- 回归覆盖 marker 页高水位与 keyset 并发变化、坏 fact marker 的 event cursor 恢复、summary marker 修复、容量上限、旧格式读取、退避重置/保留、跨页 drain、公平性、取消重启、session 跨页恢复与 baseline 后/subscribe 前新增 session 的真实交错。
- 通过：`cargo fmt --all --check`、`cargo test --locked -p haven-memory`（399 passed, 2 ignored）、`cargo test --locked -p haven-agent`（599 passed, 1 ignored）、`cargo test --workspace --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、ADR 索引与 crate 依赖边界脚本，以及 `git diff --check`。
- 本 follow-up 无 UI、IPC、schema 或用户数据契约变更；没有数据库重置要求。Windows 安装包验收继续作为独立 Gate。
