# ADR 0266：Compaction Summary Fact Extraction 的 durable job

- 状态：Accepted
- 日期：2026-09-24
- 范围：`haven-memory` episode persistence、`haven-agent` summary extraction worker
- 关联：[ADR 0259](0259-memory-runtime-committed-event-consumer.md)、[ADR 0261](0261-memory-event-cursor-and-bounded-replay.md)、[ADR 0265](0265-memory-trigger-pause-producer.md)

## 背景

`CompactSummary` 原先先写 `memory_items`，再通过 `tokio::spawn` 启动有界的进程内重试。
进程退出、runtime 尚未启动或重试耗尽都会使 summary fact extraction 丢失；episode 行本身
仍然存在，但没有可恢复的工作事实。仅把 marker 写在第二个调用中也会保留 episode 与 job
之间的崩溃窗口。

## 决策

### 独立的 per-episode durable marker

每个需要抽取的 summary 使用独立 key：

`fact_extraction_episode_pending.{session_id}.{episode_id}`

marker 的 value 是 `session_id`，`episode_id` 保留在 key 中。它不复用按 session
coalesce 的 `fact_extraction_pending.{session_id}`，也不推进用户消息事实抽取 cursor。
每个 episode 可以独立确认，删除 session 时与 session 一起清理。

### episode 与 marker 同事务提交

compaction producer 使用 `Database::add_episode_with_pending_extraction`：在同一个
SQLite `BEGIN IMMEDIATE` 事务内插入（或确认已存在的相同）`episode_summary` 行，并按摘要准入
策略 upsert marker。重复 replay 必须匹配原 session、kind 和 content；匹配时可安全重新创建
缺失 marker。提交后才唤醒 live worker，因此不会出现 live projection 已看到 job 但 durable
写入失败的假状态。

摘要过短时仍持久化 episode，但不创建 extraction marker；这保持原有摘要抽取准入行为。

### durable marker 是恢复权威

`MemoryWorker::summary_outbox` 只是低延迟 live wake-up projection。启动恢复从 durable marker
重建它，`MemoryRuntime::prepare_start` 的 readiness barrier 仍先于 dispatcher recovery。
worker 根据 `episode_id` 从 `memory_items` 读取正文，不在 job marker 中复制摘要内容。

抽取成功后清除对应 episode marker；数据库读取失败、provider retry/throttle、应用取消或进程
退出都保留 marker，供后续唤醒或启动恢复。缺失的 episode 被视为 orphan job 并清除 marker，
避免永久重试；session 删除事务会同时删除相关 marker。

现有共享 throttle 和 episode cursor 语义保持不变。summary job 不进入 `ActionService`，因为
它不是用户可见后台/定时任务，ActionService 的状态、通知和 UI projection 会引入不必要的
生命周期耦合。通用 Job 的 claim/lease/retry/cancel 生命周期另留后续设计。

本切片关闭的是 episode 写入到 durable job marker 的窗口；`CompactSummary` 事件本身在调用
producer 之前的 recovery 语义仍由既有 event-boundary 流程负责，不在本 ADR 中扩展事件协议。

## 验证

- KV 测试覆盖多个 episode marker 的独立 enqueue/ack 与 session cleanup。
- episode repository 测试覆盖同事务写入、重复 replay 幂等和 marker 重建。
- worker 测试覆盖 durable enqueue 后 live projection 的存在。
- 通过 haven-memory / haven-agent focused tests、workspace tests、fmt、Clippy 和 staged diff 检查。
