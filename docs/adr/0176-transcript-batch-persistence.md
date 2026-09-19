# ADR 0176：Transcript 事件与投影批量持久化

- Status: Accepted
- Date: 2026-09-19
- Owners: Haven maintainers

## 背景

`SessionEventStore::append_batch` 已能把多个 durable event 放进一次 SQLite
事务，但 live `apply_transcript` 仍会为事件、消息和步骤投影分别取得 blocking
连接。ToolCall 的 action step 因此可能与对应 event 分属不同提交，增加写放大，
也让 live emit 的顺序难以审查。

## 决定

- 新增 `TranscriptBatchWriter`，通过 `SessionEventStore::append_transcript_batch`
  在一个有界事务中按“事件 → messages/session_steps 投影”写入。ToolCall 的
  pending action rows、thought anchor、thought/reasoning/ask 消息都可以随其事件
  一起提交；同一 ToolBatch 的 ToolResult 现在也先按 assistant call 顺序聚合，
  再以一次 transcript 事务提交。ToolResult 的执行完成状态仍由工具执行边界
  负责，保留取消和 `unknown` outcome 语义。
- Transcript batch 同时受事件数、投影行数和总可变文本字节数限制；事件 JSON、消息
  内容以及 action 的工具输入都计入同一个 4 MiB 上限，避免少量超大工具结果绕过
  行数上限并放大 SQLite 事务。
- Tool-owned LLM usage 先在 Agent 层归一化为写入 DTO，再由
  `persist_llm_call_batch_and_refresh_session_usage` 在一次事务中插入全部明细并
  重建聚合行；UI usage 事件仍在提交后按原顺序逐条发出。这样 64 个工具调用不会
  因明细行或 transcript 行分别产生 64 次事务。
- 事务提交后才发送 SessionEventStore live broadcast；Agent 随后才发 Action、
  Observation、Thought、Supplement 等权威 UI 事件并更新内存 canonical。事务失败
  不得修改 canonical，也不得产生 live UI 事件；事件 authority 可在 resume 时修复
  物化投影。
- snapshot 仍是提交后的 checkpoint/cache，branch point 仍独立追加。两者不与
  transcript SQL 事务强行合并：snapshot 序列化需要已经更新的 canonical，而
  branch cutoff 依赖当前 `last_msg_at` 和 event cursor。inbox claim 继续在 transcript
  投影与 snapshot durable 后才 ack。
- PartialStore 为已知无 active partial 的 session 保留有界进程内集合；首次未知
  session 仍执行一次防御性 DELETE，后续 discard 走 fast path。checkpoint、promote
  或失败的 DELETE 会清除/保留相应状态，不改变跨进程 stale partial 的清理语义。
  promote/discard 必须持有同一个 session lock 直到 `run_blocking` 数据库操作完成；
  只有在没有等待者时才回收空闲 lock entry，避免回收窗口创建第二把锁并并发
  checkpoint。

## 替代方案与影响

逐条保留 `run_blocking` 调用最简单，但在多工具批次上会重复 checkout、事务和
SQLite lock 等待。把 snapshot 也塞进同一事务会要求在 DB 事务内持有 ReAct state
或反序列化前置状态，容易产生 canonical 落后 checkpoint；因此不采用。

本 ADR 不引入后台无界队列或 writer worker。每个批次由当前 ReAct 调用同步等待，
事务大小受现有 tool/context 批次上限约束，取消不会在提交后撤销已 durable 的
事件；之后仍按 rollback/unknown outcome 协议处理。

## 验证与回滚

- Memory 单测覆盖批次 event/live broadcast、message/step 投影以及投影失败时的
  整体 rollback，并覆盖事件与 projection payload 超过 4 MiB 时事务尚未开启即
  被拒绝。
- Agent/Memory 单测覆盖多个 ToolResult 的顺序投影与多条 tool usage 的批量写入。
- Agent transcript、resume/rollback 与 PartialStore 测试覆盖 canonical 顺序、
  stable identity、投影恢复和无 partial discard fast path。
- 回滚代码与本 ADR 即可恢复逐条写入；不需要数据库 schema reset，因为事件和投影
  的 wire/schema 未改变。
