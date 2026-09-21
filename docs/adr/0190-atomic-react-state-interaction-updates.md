# 0190：交互状态更新与输出快照原子合并

## 背景

`sessions.react_state` 是可丢失的 ReAct checkpoint/cache。流式输出会持续写入包含最新事件和分支点的快照；Ask/Confirm 生命周期也需要把 `interactions` 写入同一快照。旧路径先读取快照、修改 `interactions`，再整块写回，两个异步写入交错时会用旧内容覆盖较新的输出快照。

## 决定

新增 `Database::update_react_state_interactions_json`。该操作先取得 `BEGIN IMMEDIATE` 写事务，再读取当前压缩快照，只替换 `interactions` 字段，并在同一事务中刷新快照与 checkpoint 高水位元数据。交互状态持久化不再在 agent 层执行“读—改—写”组合。

## 替代方案

- 为 agent 层增加快照写入锁：无法覆盖其它 `Database` 调用方，且扩大锁的生命周期。
- 为整块快照引入乐观 CAS 和重试：交互更新仍需重新读取/合并，复杂度高于数据库内单事务局部更新。

## 影响与回滚

只改变 checkpoint/cache 的写入路径，不改变 `session_events` 权威事件流、数据库 schema 或 IPC 契约。回滚代码时需同步恢复 agent 的交互快照写入调用；已有快照无需迁移。

## 验证

- 内存数据库测试确认最新输出事件不会被交互更新覆盖。
- 测试非法交互数组与缺失 checkpoint 的失败边界。
- 运行 `cargo test --locked -p haven-memory` 与 `cargo test --locked -p haven-agent`。
