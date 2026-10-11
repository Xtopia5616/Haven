# 0953：限制 ToolRun sidecar event payload 的下游构造

## 状态

已接受并实现（2026-10-11）。

## 背景

与 `ToolRunLifecyclePayload` 相同，`ToolRunSessionAttachedPayload` 与 `ToolRunOutputPayload` 由 Tools 产生、App 投影为 Tauri DTO。两个 struct 曾公开所有字段且可在任意下游 crate 直接构造；仓内生产构造点均属于 Tools，App 的使用是只读投影和测试 fixture。

## 决定

- 两个 producer-owned payload 标记为 `non_exhaustive`，关闭下游 struct literal 构造。
- 仅在 `test-support` feature 下提供 `new_for_test` fixture 构造器；App 事件投影测试改用该入口。
- 同步将受影响的 App ToolRun event fixture 改为规范 `toolrun-*`、`ses-*`、`step-*` identities。事件字段、顺序与 Tauri payload 不变。

## 影响

Tools 仍拥有 session attachment 与 bounded output preview 的生产事件；App 仍能读取并投影 DTO。普通生产依赖不再暴露任意构造入口；持久化、IPC 与输出限制语义不变，无需数据重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `git diff --check`

## 回滚

新增生产者应通过 Tools owner 操作发布事件；新的跨 crate 测试依赖应使用非默认 `test-support`，不要恢复公开 struct literal 构造路径。
