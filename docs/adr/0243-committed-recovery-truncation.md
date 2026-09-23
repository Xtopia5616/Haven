# ADR 0243：在 SessionStore 事务内应用 committed recovery 截断

- 状态：已采纳（2026-09-24）
- 范围：`continue_session` 的 recovery marker 判定与 projection 截断
- 关联：[ADR 0217](0217-continue-projection-truncation.md)、[ADR 0238](0238-session-recovery-read-ports.md)

## 背景

`continue_session` 原先先从全历史读取最新 recovery persistence marker，在 Agent 中解析 `phase`，再按 marker 的 `step_number` 调用 SessionStore 截断 projection。marker 与截断各自使用独立数据库操作；两次操作之间可能出现新事件或 branch point，Agent 也因此承担了恢复协议的 JSON 决策。

## 决定与事实所有权

1. SessionStore 提供 `truncate_projection_after_latest_committed_recovery`。它在一次 `BEGIN IMMEDIATE` 事务内按 `session_events.sequence` 查询全历史中最新的 recovery marker，判定 phase，解析 active branch point 和 exclusive projection cutoff，并执行截断。
2. 只有最新 marker 的 JSON 字段 `phase` 是字符串 `committed` 时才继续。更高 sequence 的 `failed`、缺失字段、非字符串 phase 或无效结构均覆盖更早的 committed marker，并安全 no-op。
3. 截断复用同一存储事务内的现有实现：读取将被删除的 usage ID，删除 `messages`、`session_steps`、`llm_usage` 投影，重建 `session_usage`，并为被删 usage 追加 `usage_discarded` 补偿事件。
4. Agent 的 `continue_session` 只调用这一项存储操作。生命周期 join、partial discard、interaction 清理、Pending 状态流转和 usage cache invalidation 的相对顺序保持原样。
5. 成功提交后仍先失效 message cache，再按顺序发布补偿事件。没有 marker、phase 不匹配、没有对应 active branch point 或没有 `last_msg_at` 时不改 projection、不失效缓存、不发布事件。marker 没有 `step_number` 时沿用旧行为，以 0 查找 branch point。

## 必须保持的不变量

- recovery marker 的新旧只由 append-only 全历史 sequence 决定，不能从 active replay 推断。较新的 failed 或格式不符合预期的 marker 必须阻止较早 committed marker 授权删除。
- branch point 必须来自当前 active timeline，且只使用 marker 对应步骤的 `last_msg_at`；删除边界为该时间之后，不含边界时间本身。
- marker 查询、phase 判断、cutoff 解析、三类 projection 删除、usage 补偿和 `session_usage` 重建在同一个 SQLite 写事务内完成。
- 操作不移动 transcript cursor，不追加 timeline rollback marker，也不改 lifecycle join、partial generation、interaction 或 Pending 状态处理。
- projection 事务提交后才失效缓存并发布 durable usage 补偿事件。

## 明确不做

本次不重写 `load_replay_state`、resume 重建、active timeline、compaction root、transcript cursor 或 rollback epoch；不改变事件 schema、数据库版本和 UI 发布协议。更大范围的 replay 与崩溃窗口验证仍按阶段 2 的独立工作项推进。

## 替代方案与影响

保留 Agent 先读取 marker、随后调用截断接口，会继续留下两个存储操作之间的竞态窗口。只从 active replay 中取 marker 会让 rollback 隐藏的 failed marker 失去否决权，因此均不采用。

该边界把 recovery protocol 的持久化事实和 projection 写入决策归给 SessionStore；Agent 不再解析 marker payload。数据库表结构和用户数据格式不变。

## 验证与回滚

回归测试覆盖 committed marker 截断及 usage 补偿、较新的 failed/格式错误 marker 覆盖早期 committed marker、无 marker/branch point/cutoff 时保留 projection。另运行 Rust 格式检查、`haven-memory` 与 `haven-agent` 测试，以及 workspace 严格 Clippy。

回滚时恢复 Agent 的两次调用路径并移除此 ADR；本决策不修改 schema，不要求重置数据库。
