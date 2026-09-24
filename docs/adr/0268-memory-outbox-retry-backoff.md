# ADR 0268：MemoryWorker durable outbox 的逐 job 退避

- 状态：Accepted
- 日期：2026-09-24
- 范围：普通事实抽取与 compaction-summary 抽取的进程内重试调度
- 关联：[ADR 0266](0266-summary-fact-extraction-durable-job.md)、[ADR 0267](0267-memory-runtime-maintenance-schedule.md)

## 背景

durable marker 已经保证普通事实抽取和 summary 抽取不会因进程退出而丢失，但 worker 对
普通抽取失败没有同进程重试，summary 的 episode 读取失败也会立即重新入队。provider 不可用、
数据库暂时失败或共享 throttle 较长时，这会造成重试热循环或把恢复完全推迟到下一次进程启动。

## 决策

在 `MemoryWorker` 的共享 outbox drain loop 中按 job key 维护进程内 attempt 次数：

- 普通事实 job 以 `session_id` 为 key；summary job 以 `episode_id` 为 key；
- 每次 transient failure、throttle、episode 读取失败或 durable marker ack 失败都保留 marker，
  重新投递 live projection，并使用 1、2、4、8、16、30 秒的本地指数退避；
- summary 自身返回的 throttle 等待时间优先于较短的本地退避，确保不绕过共享 throttle；
- 成功完成并确认 marker 后清除 attempt 状态；进程重启时从 durable marker 恢复，attempt 从零开始；
- 缺失 episode 仍按 orphan policy 清除 marker，只有清除失败才重试。

这只是 MemoryWorker 内部的重试调度，不创建通用 Job 状态机，不复用 `ActionService` 的
lease/UI/cancel 语义，也不改变 facts cursor、summary cursor 或 durable marker 的权威性。完整
worker shutdown/cancellation 传播另留切片，避免把应用生命周期和本轮退避改动混在一起。

## 验证

- 退避单元测试覆盖指数序列、30 秒上限和 throttle wait 保留。
- 既有 durable marker、summary recovery、facts cursor 与 workspace 测试继续通过。
- 通过 fmt、严格 Clippy 和 staged diff 检查。
