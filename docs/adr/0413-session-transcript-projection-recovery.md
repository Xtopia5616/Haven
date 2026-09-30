# 0413：Thought 原子投影与 Compaction 步骤恢复

> 状态：已采纳
> 日期：2026-09-30

## 背景

`Thought` 事件与 assistant 消息在一次 `SessionCommitted` 事务中写入，但对应的 `session_steps` 行此前在提交后单独写入。两次写入之间崩溃或步骤写入失败会留下缺失执行投影的 durable event。Compaction 又会用摘要根替换活动 transcript；摘要 payload 未保留其步骤号，resume 解码时也没有利用 `session_events.step_number`，因此摘要根可能把恢复步骤降到 1。

## 决定

- 把 Thought 的共享 ID `ThoughtStep` 投影加入对应的 `SessionCommitted`。事件、消息和步骤均在同一 SQLite 事务提交；任一投影失败时整笔事务回滚，提交后再发布 UI 事件。
- 在 `CompactSummary` durable payload 中保存 compaction 所在的 `step_number`，并在 resume 时用它推导起始步骤。
- 对旧摘要 payload，从同一 session event 行的 `step_number` 恢复步骤号；新写入同时保存摘要 payload 和事件行元数据。

## 替代方案

可以在 resume 时扫描事件并补齐缺失步骤，但这会把原本可在提交边界保证的不变量改成恢复时修复，还要处理写入幂等和事件/投影并发。Thought 步骤属于同一提交的物化投影，因此选择事务原子性。

## 影响

- `TranscriptRecord::CompactSummary` 增加步骤字段；serde 对缺失字段默认处理，读取器再使用已有事件行元数据修复旧记录。
- 不修改数据库 schema，无需重置数据库或迁移数据。
- 恢复以摘要根步骤作为当前恢复步骤；若摘要根后有工具结果，仍按现有规则推进到下一步。

## 验证与回滚

- Agent 回归测试覆盖 Thought 步骤冲突时事件和消息一并回滚、compaction 根持久化步骤号、旧摘要从事件行元数据恢复。
- 运行 `cargo test --locked -p haven-agent` 与 `cargo clippy --locked -p haven-agent -- -D warnings`。
- 回滚代码提交即可；不需要数据重置。旧记录仍可由后续版本读取。
