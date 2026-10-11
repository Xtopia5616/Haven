# ADR 0993：统一 SessionStep 投影身份字段

## 状态

已采纳并实施。

## 背景

Memory 的 `SessionStep` 是 `session_steps` 执行投影，供 resume command、Agent transcript replay 与记忆提取等消费者使用。其 Rust/Serde 字段曾叫泛名 `id`，UI 只能通过所属 `steps` 集合推断它标识执行步骤。补充输入的执行行可以复用 `msg-*`，所以字段表达的是步骤行身份，不限制其值只能来自 `step-*` 空间。

## 决定

1. `SessionStep.id` 统一改为 `SessionStep.step_id`，生成的 resume wire contract 与 UI identity join 同步使用 `step_id`。
2. resume parser 只接受 canonical `step_id`；`step-*` 和按规范复用的 `msg-*` 都有效，不接受旧泛名 `id`。
3. SQLite 物理列 `session_steps.id`、读写 SQL、`SessionEvent` durable payload 和 `SessionStepStatus` 生命周期语义保持不变。
4. Agent runtime `SessionInfo`/`StepInfo` 属于独立的进程内 list contract，本 ADR 不改变其字段。

## 替代方案

- 保留 `id`，让调用方依赖集合/类型上下文解释：没有让实体身份在字段名中可见。
- 只允许 `step-*`：会拒绝补充输入投影合法复用的 `msg-*` 身份。
- 同时输出 `id` 和 `step_id`：会产生两个可依赖字段并保留已退役泛名。

## 影响与验证

- Rust `SessionStep`、其消费者、resume UI parser/identity join、测试 fixture 与生成的 TypeScript contract 使用 `step_id`。
- Rust 序列化测试断言 wire shape 包含 `step_id` 且没有 `id`；UI parser 测试拒绝旧字段，并覆盖 `msg-*` supplement step。
- 不涉及数据库 schema、durable event payload、用户配置或迁移/重置行为。Rust 与 UI command response 随应用一起发布。
- 验证包括 workspace Rust tests 与 Clippy、UI 类型检查/测试/build、IPC 契约和 ADR 索引检查，以及格式/diff 检查。

## 回滚

Rust `SessionStep`、generated contract、UI parser/consumer 应作为一个版本原子恢复为 `id`。数据库与持久事件无需回滚；回滚会重新引入依赖上下文推断步骤实体的字段名。
