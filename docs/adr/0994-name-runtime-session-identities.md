# ADR 0994：统一 runtime Session 与 Step 身份字段

## 状态

已采纳并实施。

## 背景

`list_runtime_sessions` 返回当前进程驻留会话的 Agent `SessionInfo`，其中会话及步骤身份曾使用泛名 `id`。该 DTO 由 Rust 序列化为 Tauri response，UI parser 只能依赖对象上下文推断实体。生命周期事件本身已经由 App bridge 显式投影成 `session_id`；UI reducer 的 `SessionSummary` 则在 wire mapper 后仍用 `id`，不符合跨层 `snake_case` → renderer `camelCase` 的实体命名约定。

## 决定

1. Agent runtime list 的 `SessionInfo.id` 改为 `session_id`，`StepInfo.id` 改为 `step_id`；UI runtime parser 只接受这两个 canonical 字段和值前缀。
2. UI `SessionSummary` 使用 `sessionId`，runtime list、history 与 lineage mapper 均在边界投影该字段；reducer、switcher 与视图按实体名访问它。
3. `SessionCreated` lifecycle event 继续由 App bridge 映射为 `session_id`，payload/status/title 行为不变。SQLite `sessions.id`、Memory `Session.id` 和 Agent 内部 actor handle 的字段不属于本 runtime list DTO 决定范围。
4. resume 使用的 `SessionRecordDto.session_id`、`SessionStep.step_id` 与其 `msg-*` supplement 规则不变；本决定不改变持久事件或数据库 schema。

## 替代方案

- 保留通用 `id` 并依赖 DTO 上下文：继续让 Rust 与 UI consumer 从字段名之外推断实体身份。
- 同时输出旧 `id` 与新实体字段：会保留双重可依赖身份，并把旧 shape 带入生成契约。
- 通过改变 lifecycle event payload 收敛：事件已有显式 `session_id` projection，更新它不会修复 runtime list 契约。

## 影响与验证

- Generated `SessionInfo`/`StepInfo`、UI runtime identity parser、`SessionSummary` 投影与 reducer 消费者改用实体字段；parser 拒绝旧 `id`。
- App lifecycle event projection 仍单独输出 `session_id`，没有 durable event、配置、数据库或迁移/重置变化。
- Rust workspace tests/Clippy、UI 类型检查/测试/build、IPC contract 与 ADR 索引检查、格式/diff 检查验证该切片。

## 回滚

Rust DTO、generated contracts、UI runtime parser 和 SessionSummary mapper/reducer 必须作为一个版本恢复为 `id`。无需数据库或事件回滚；回滚会重新引入 runtime identity 的泛名字段。
