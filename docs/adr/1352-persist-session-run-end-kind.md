# ADR 1352：持久化会话运行结束类型

## 状态

已采纳并实施。

## 背景

ADR 1336 将 `run_end_kind` 限定为当前进程的生命周期提示。正常 ReAct `TurnEnd` 会让可继续的 Session 保持 `paused`，历史恢复只能看到 `paused`，因此无法继续显示“本轮已完成”，也无法与用户打断或交互等待区分。

## 决定

- 在 `sessions.run_end_kind` 持久化 `SessionRunEndKind`，目前唯一值为 `turn_completed`；它只允许和 `paused` 状态同时存在。
- Session Actor 在同一状态更新中写入状态与结束类型。新的 `pending` / `running` 运行以及其它暂停或终态会清除旧结束类型。
- 会话历史、恢复详情和运行时会话摘要暴露这个闭合类型；UI 按类型恢复完成提示，不从自由文本、等待原因或 transcript 推断。
- schema 从 v53 升至 v54。旧数据库仍按测试版策略拒绝打开，升级需要按 `docs/release-and-reset.md` 重置数据库。
- 本 ADR supersedes [ADR 1336](1336-distinguish-completed-turn-from-paused-session.md) 中“只用于 live run-end presentation”的决定；`run_end_kind` 的 producer 仍只认 ReAct `TurnEnd`。

## 替代方案

- 根据 `waiting_reason=user_input` 推断完成：拒绝，关闭恢复后的暂停也可能等待用户输入。
- 比较 `run_end_reason` 文本或 transcript 内容推断：拒绝，自由文本不是生命周期类型契约。
- 保持 live-only：拒绝，无法在历史恢复或应用重启后呈现相同的结束状态。

## 影响与验证

变更覆盖 SQLite sessions、Agent Session Actor、Session history/runtime IPC DTO 和 UI 恢复路径。`SessionRunEndKind` 仍是 `SessionStatus` 的附属结果，不会把可继续会话改成终态。

- IPC 契约已通过生成器更新；`git diff --check` 通过。
- 2026-10-22 使用固定工具链复验：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`pnpm run check` 与 `pnpm run test:run`（165 个文件、1408 项通过）均通过。

## 回滚

移除持久化字段、Actor 写入路径及 UI 恢复读取，并恢复 ADR 1336 的 live-only 语义。v54 数据库不兼容旧二进制；回滚前需备份并按发布说明重置数据库。
