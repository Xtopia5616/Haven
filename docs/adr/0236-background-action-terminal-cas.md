# ADR 0236：后台任务终态写入采用 first-wins CAS

- 状态：已采纳（2026-09-24）
- 范围：`haven-memory` 后台 action 终态持久化与 completion outbox
- 关联：[ADR 0180](0180-action-completion-outbox-and-stream-overflow-pump.md)、[ADR 0229](0229-scheduled-action-state-dedup.md)

## 背景

后台任务的完成回调可能重复、迟到，或在进程恢复后到达。此前按 action id 更新终态时没有要求当前状态仍为
`running`，迟到结果可能覆盖已完成、失败、取消或恢复失败的行；completion outbox 仍保留第一次快照，造成持久状态不一致。

## 决定

1. 后台 action 只允许一次 `running → completed/failed` 条件更新；重复或迟到写入返回未转换，不覆盖已有终态。
2. 后台取消使用独立的 `running → cancelled` CAS，不能制造 completion outbox。
3. action 行和 completion outbox 必须在同一事务中由胜出的状态转换写入；outbox 保留第一次成功快照。
4. scheduled action 的 waiting/running claim、无消费者回退、恢复和 ack 语义保持现状，不与 messaging claim/ack 合并。

## 影响与验证

不改 schema、IPC、ActionEvent 或 outbox ack 契约。回归测试覆盖重复终态、重启后迟到完成和 outbox/action 快照一致性。

验证：`cargo fmt --all -- --check`、`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-tools`、
`cargo clippy --workspace --locked -- -D warnings` 的 memory/tools 目标。

## 回滚

恢复终态更新 SQL 与 outbox 写入前的旧路径；不涉及 schema 或用户数据格式。
