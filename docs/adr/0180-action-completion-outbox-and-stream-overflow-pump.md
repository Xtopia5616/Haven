# ADR 0180：Action completion durable outbox 与流式 overflow pump

## 状态

已接受（2026-09-20）

## 背景

后台 action 的终态结果先写入 `actions`，再通过进程内 broadcast 投递给
Agent。broadcast lag 或消费者暂时不存在时，稳定 `action_result_id` 只能避免
重复，不能找回从未投递的结果；如果结果刚入会话 actor 队列，会话随后立即终态，
队列清理也会让结果丢失。

LLM provider 的同步 streaming callback 还会在 web-search 事件 channel 满载时为
每个事件创建一个独立的异步发送任务。这些任务没有统一 FIFO，可能互相超车并在
flush 时形成不可控的等待堆积。

## 决策

1. `action_completion_outbox` 是后台终态结果的 durable delivery 记录。终态 action
   更新与 outbox 入队在同一 SQLite 事务中；reconcile 会从已有终态 action rows
   补建缺失记录，覆盖进程在两步之间崩溃的窗口。
2. Agent consumer 使用短 lease claim 从 outbox 恢复结果。只有 transcript event
   与 message projection 成功提交后才 acknowledge；actor 入队不算 delivery。这样
   “入队后会话立即终态”会在下一次 reconcile 中转为终态历史投影，仍复用同一个
   `action_result_id`/message id。不存在的 session 记录为已处理并保留 action 审计行。
3. web-search 保留容量为 256 的同步 fast path；一旦满载，后续事件全部进入单一
   FIFO overflow pump，pump 是唯一 async sender。overflow 上限为 1024；同一
   session/step/run/call/action 的待发送状态更新可 coalesce，互不相同的事件按输入
   顺序发送；队列仍满时计数并丢弃，而不是创建无界 task。

## 替代方案

- 只依赖 `action_result_id` 去重：无法恢复未进入 Agent 的 broadcast 事件，也无法
  覆盖终态清空 actor queue 的竞态。
- 只在 action service 内存中保留 pending completion：进程重启仍会丢失结果。
- 为每个 web-search overflow 事件 spawn 一个 sender：没有发送顺序或容量上限。
- 让 callback 直接 await channel：会把同步 provider callback 反压到网络读取路径。

## 影响、验证与回滚

新增 schema version 23；旧数据库按项目既有 reset boundary 重置，无运行时迁移。
outbox 行随 action 删除级联，delivery claim 是进程无关的短时间 lease。

验证覆盖：缺失 broadcast 的 durable reconcile、ack 前保持 pending、终态队列竞态
的直接历史投影、channel 满载下 overflow 顺序，以及 workspace Rust/UI 门禁。
回滚代码并按发布说明重置 schema 23 数据库即可。
