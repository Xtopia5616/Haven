# ADR 0217：continue_session 失败步骤投影截断边界

- 状态：已采纳（架构降复杂度路线图阶段 2 首个最小切片）
- 日期：2026-09-24
- 范围：`haven-memory` 的 `SessionStore` 与 `haven-agent` 的 `continue_session`
- 关联：[架构降复杂度重构路线图阶段 2](../architecture-refactor-roadmap.md#阶段-2恢复回滚和事件投影边界再收口p0)、[ADR 0206](0206-session-store-usage-events-and-atomic-rollback.md)、[ADR 0207](0207-session-store-replay-boundaries-and-durable-ui-sequences.md)

## 背景

`continue_session` 只应在最新 recovery marker 的 phase 为 `committed` 时清理失败步骤留下的投影。原调用先独立读取 branch point 的 `last_msg_at`，随后再启动投影截断事务；两次读取之间，active branch point 或投影可能发生变化。截断本身还需要同步收集被删除的 usage ID、写入 `usage_discarded` 事件并重建 `session_usage`。

## 决定

1. `SessionStore::truncate_projection_after_step(session_id, step_number)` 是按失败步骤截断投影的唯一入口。Agent 不读取 projection cutoff，也不组合单独的读取与删除 API。
2. 入口在一个 `BEGIN IMMEDIATE` 事务、同一 SQLite 连接上读取 active branch point 并解析其 `last_msg_at`，再按现有 exclusive 规则删除 cutoff 之后的 `messages`、`session_steps` 与 `llm_usage` 行。
3. 同一事务先收集将删除的 usage ID，在删除后为每个 ID 追加 `usage_discarded`，并重建 `session_usage`。事务提交后失效 message cache，再发布补偿事件。没有匹配 branch point 或其 `last_msg_at` 为空时安全 no-op。
4. `continue_session` 保留“最新 recovery marker 必须 committed 才授权截断”的判定。marker 缺失或失败时继续保留历史；partial discard、usage sidecar 失效、interaction 清理和 Pending 状态流转不变。
5. 此入口只使用投影时间戳 `last_msg_at` 和现有 exclusive 删除规则；不改变 append-only `session_events` 的 event cursor、rollback marker 或其他事件语义，也不从 event cursor 推导投影 cutoff。它不改变 schema。

## 替代方案

- 保留 Agent 先读 cutoff、再调用截断：会让 branch point 解析与删除落在不同事务边界，不能保证它们观察同一状态。
- 让 Agent 直接操作 `Database` 删除投影：会把多表删除、usage 补偿和汇总重建从 `SessionStore` 的持久化所有权中拆开。
- 用 rollback marker 表示 continue 截断：会改变 active event timeline 与 rollback 语义，而本切片只修复物化投影。

## 影响与验证

- 持久化职责收口在 `SessionStore`，失败恢复入口不再拥有 branch point 到 projection cutoff 的解析知识。
- 数据表、事件格式、schema 版本、event cursor 和 rollback 主路径均不变；无需重置用户数据。
- 回归覆盖 committed branch point 的 exclusive 删除、usage 补偿事件、无 branch point/无 `last_msg_at` 的 no-op，以及无 recovery marker 时保留历史。
- 验证命令：

  ```text
  cargo fmt -p haven-memory -p haven-agent
  cargo check --locked -p haven-memory -p haven-agent
  cargo test --locked -p haven-memory --lib
  cargo test --locked -p haven-agent --lib
  cargo clippy --locked -p haven-memory -p haven-agent -- -D warnings
  git diff --check
  ```

## 回滚

恢复 `continue_session` 的原调用，并移除按步骤截断入口、对应回归测试和本 ADR/索引项即可。该切片没有 schema 或用户数据格式变化，回滚无需数据库重置。
