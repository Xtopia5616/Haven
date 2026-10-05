# ADR 0107：事实抽取使用可恢复的持久化 outbox

- 状态：Accepted
- 日期：2026-09-08
- 范围：`haven-agent` 事实抽取调度、`haven-memory` 内部 `kv_store`

## 背景

事实抽取原先只有进程内 `HashMap<session_id, bypass_throttle>`。会话结束后如果
进程在 worker 执行前崩溃，已经触发但尚未开始的抽取会静默丢失；这会让事实记忆
与用户实际完成的会话不一致。抽取 cursor 本身是持久化的，因此恢复同一 job
不会重复写入已处理窗口。

## 决定

1. `enqueue_infer` 将触发写入 `fact_extraction_pending.{session_id}`。marker 值编码
   event sequence、bypass、retry attempt 和绝对 `next_attempt_at_ms`；较新 event
   sequence 取代较旧 generation 并重置退避，同 sequence replay 保留退避；bypass
   只能升级，升级为 true 时重置退避。旧值 `0`/`1` 和 `<sequence>:<bypass>` 仍可读，
   并分别按 generation zero 或立即可运行的初始 attempt 处理。
2. outbox worker 以 marker 为 backlog 唯一来源，使用最多 64 项的 keyset 页恢复仍
   属于现存 session 的 pending markers。成功处理后只可用本 job 读取的完整 marker
   key/value 做条件删除；较新 generation 即使 bool 相同也必须保留。处理失败、
   数据库异常或进程中途退出时保留 marker；retry attempt/deadline 持久化后自动重试，
   重启从 durable marker 恢复。message cursor 保证已经提交的 transcript 窗口
   不会重复应用；若 marker 已确认而 event cursor 尚未 checkpoint，event replay
   可以重新创建同一 generation 的 marker，worker 会因 message cursor 没有新窗口而
   安全完成并再次确认。
3. marker、用户消息 cursor、节流时间戳和 summary cursor 都属于 session-scoped
   internal state；单个会话删除、批量历史清理和 orphan maintenance 必须一起清理。
4. 不新增 schema 表。队列使用已有的内部 `kv_store`，并通过 typed database
   methods 读写，禁止让 Agent 直接拼 SQL。

## 后果

- 进程崩溃不再直接丢弃已入队的事实抽取任务；最坏情况是安全重放，而不是漏记忆。
- enqueue 增加一次短同步 SQLite 写入；worker 只保留有限页和一个活跃任务，不按
  backlog 大小增长内存队列。
- memory event cursor 和 pending marker 共同保护两个不同进度：前者确认 trigger
  已调度，marker generation 确认对应抽取是否完成，旧抽取不能越过新 trigger。
- worker 对失败任务保留 marker，并在同一 marker value 持久化 1–30 秒指数退避；
  若未来需要 dead-letter、人工重放或审计历史，应另行设计专用 job 表及容量上限。
- `kv_store` 中的 session-scoped key 不再是零散约定，新增状态必须同步加入删除和
  orphan cleanup 路径。

## 验证

- `haven-memory` 测试覆盖 generation 条件确认、同 bypass 重入、旧 bool marker
  读取、live-session restore、orphan cleanup 和三种会话删除路径。
- `haven-agent` 测试覆盖抽取成功/失败/节流时 cursor 行为；构造 Agent 不再注入
  伪造的默认姓名事实。

## 回滚 / 重置

无需数据库 schema reset。读路径兼容旧 `0`/`1` 与上一代 generation marker；新版本
第一次 enqueue 会将其升级为扩展 marker。若回滚到不识别扩展 pending marker
值的旧二进制，旧版本会把任何非 `1` 值误作普通任务，不能正确恢复 bypass；应在
回滚前关闭 app 并按当前数据重置流程处理开发数据库。重新运行当前版本或执行
memory maintenance 会清理不存在 session 的 marker。
