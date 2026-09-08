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

1. `enqueue_infer` 在内存 coalescing 的同时写入
   `fact_extraction_pending.{session_id}`。值 `1` 表示 bypass throttle，值 `0`
   表示普通抽取；同一 session 的 bypass 标记只能升级不能降级。
2. outbox worker 启动时恢复仍属于现存 session 的 pending markers。成功处理后
   才删除 marker；处理失败、数据库异常或进程中途退出时保留 marker，下一次
   enqueue 或进程启动可以继续处理。cursor 保证崩溃重放是幂等的。
3. marker、用户消息 cursor、节流时间戳和 summary cursor 都属于 session-scoped
   internal state；单个会话删除、批量历史清理和 orphan maintenance 必须一起清理。
4. 不新增 schema 表。队列使用已有的内部 `kv_store`，并通过 typed database
   methods 读写，禁止让 Agent 直接拼 SQL。

## 后果

- 进程崩溃不再直接丢弃已入队的事实抽取任务；最坏情况是安全重放，而不是漏记忆。
- enqueue 增加一次短同步 SQLite 写入；内存队列仍负责 worker 的快速 coalescing。
- 当前 worker 对失败任务保留 marker，后续通过新的 enqueue 或重启恢复；未来如需
  独立重试计数、退避和 dead-letter，应升级为专用 job 表并定义容量上限。
- `kv_store` 中的 session-scoped key 不再是零散约定，新增状态必须同步加入删除和
  orphan cleanup 路径。

## 验证

- `haven-memory` 测试覆盖 marker 的 bypass coalescing、live-session restore、
  orphan cleanup 和三种会话删除路径。
- `haven-agent` 测试覆盖抽取成功/失败/节流时 cursor 行为；构造 Agent 不再注入
  伪造的默认姓名事实。

## 回滚 / 重置

无需数据库 schema reset。若回滚到不识别 pending marker 的旧二进制，旧版本会
把该 key 当作无关内部状态；重新运行当前版本或执行 memory maintenance 会清理
不存在 session 的 marker。
