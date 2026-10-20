# ADR 1336：区分已完成轮次与暂停会话

## 状态

Accepted — 2026-10-20

## 背景

ReAct 的正常 `TurnEnd` 会把可继续的 Session 状态设为 `paused`。生命周期事件此前只包含 `paused` 和等待用户输入的原因，聊天页因此把成功结束的轮次显示成普通等待状态，没有及时展示结束提示，也无法区分正常完成与用户打断。

## 决定

- Session lifecycle update 增加可选闭合字段 `run_end_kind`，目前 `turn_completed` 只由 ReAct `TurnEnd` 产生。
- Agent、App event bridge 与生成的 UI IPC contract 原样传递该类型化结果；UI 收到后立即展示成功完成提示。
- 会话实体仍保持 `paused`，可继续接收后续消息。用户打断、Ask、确认与步骤预算暂停继续使用各自现有状态和提示。
- 该结果只用于 live run-end presentation；不改变持久化会话状态或数据库 schema，无需重置用户数据。

## 替代方案

- 将每个 `waiting_reason=user_input` 的暂停都当作正常完成：拒绝，运行取消或关闭后的恢复状态也可能等待用户输入。
- 通过比较自由文本 `reason` 推断完成类型：拒绝，文案不应成为生命周期判别契约。

## 影响与验证

新增字段是可选的 Tauri event contract；旧事件形状继续映射为 `null`。回归覆盖 Rust enum 序列化、App event 投影、UI lifecycle reducer 和提示文案。

- 通过：生成的 IPC contract `--check`、`scripts/check-ipc-events.ps1`、`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`corepack pnpm run check`、生产构建、相关 Vitest（99 项）。`skill_runner` 曾在首次并行运行中间歇失败；单独重跑和第二次完整 workspace 测试均通过。
- 全量 UI 测试中有 9 项位于同一工作树其他未提交视图改动的失败（`ModelSettings` 6 项、`SettingsView` 1 项、`SessionHeader` 2 项）；本次相关 UI 测试全部通过。

## 回滚

移除 `run_end_kind` 并恢复普通 paused 状态投影，回退 UI 的成功提示和 label。没有持久化数据需要回滚。
