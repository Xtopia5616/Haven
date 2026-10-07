# ADR 0613：命名 ToolRun event projection

## 状态

已采纳并实施。

## 背景

`project_tool_run_event` 将每个 Tools lifecycle event 转换为 App 所有的 Tauri channel 和 `ToolRunEvent` payload。原 `(&'static str, ToolRunEvent)` tuple 被 Tauri emitter 与测试消费；channel identity 和 event body 是同一次投影的两个独立部分。

## 决定

1. App adapter 返回 `ToolRunEventProjection { channel, payload }`。
2. `AppHandle::emit` 与契约测试按字段读取结果。
3. 保留 lifecycle 到 channel 的映射、payload 投影与序列化行为。

## 替代方案

- 保留 tuple 并只给局部变量命名：拒绝，projection 契约仍要求调用方依赖返回顺序。
- 把 channel 加进 `ToolRunEvent`：拒绝，payload 是稳定 event body，channel 选择属于 Tauri transport adapter。

## 影响与验证

- 仅改变 App 内部 event adapter 结果类型；event channel 名称与序列化 payload 不变。
- 命名路线图 §5.7 继续保持 Active；更广的 IPC command 与 event 命名审计仍未完成。
- 验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、ADR 索引及 staged diff 检查。

## 回滚

恢复 `(&'static str, ToolRunEvent)` 及 emitter 和测试中的 tuple 解构；无需 event wire 迁移。
