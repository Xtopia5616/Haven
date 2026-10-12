# ADR 1081：将无序 Thought 发布限定为恢复 partial

## 状态

已接受（2026-10-13）

## 背景

X12 要求普通 ReAct Thought 通过 transcript event 与消息/步骤投影提交，并由 `CommittedUiPublisher` 发布。错误恢复期间尚未完成的 thought partial 是已记录的例外：它作为 recovery partial 持久化，由 branch point、recovery marker 与 `last_msg_at` 控制继续/回滚，并可发布临时 UI 事件。原 `emit_thought_from` 名称过于通用，且相邻注释错误地描述成 UI 先于投影。

## 决定

- 将入口收窄为 Agent crate 内部的 `emit_recovery_partial_thought_from`，并让名称明确表达 X12 例外语义。
- 恢复消息 partial 已写入后，先持久化共享 message ID 的 thought step，再发布无 `event_sequence` 的临时 Thought 事件。
- 移除无独立调用者的 `persist_thought_step` 泛型 helper。普通 ReAct Thought 继续经 `apply_transcript` 与 `CommittedUiPublisher`。
- 在 ADR 0336 所述 error partial 契约下保留该行为，不新增 transcript event 或持久化格式。

## 影响

运行行为只在错误恢复路径保留现有顺序；普通 ReAct 路径没有变化。Agent crate 外不再可调用此恢复 helper。

## 验证

- Agent recovery/event boundary tests。
- workspace locked tests、strict Clippy 与格式检查。

## 回滚

恢复旧 helper 名称前必须同时恢复其唯一 recovery 调用点、可见性与注释；不得把此入口重新用于普通 ReAct Thought。
