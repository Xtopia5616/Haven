# ADR 1426：为 RunStart Hook 增加运行来源

## 状态

已实现并通过门禁。

## 背景

`run_start` 已覆盖 Agent 的恢复、继续和新输入运行，但 payload 没有区分来源。Hook 脚本只能通过额外事件或外部状态猜测触发原因。

## 决定

- 在现有 `run_start` payload 中加入 `run_source`，使用封闭的字符串枚举：`new_input`、`continue`、`recovery`。
- 由待运行队列携带进程内来源元数据，并沿 dispatcher、session actor、ReAct run 传给 Hook。新会话和新输入标记为 `new_input`；显式继续、确认恢复及回滚重跑标记为 `continue`；启动时重载的 Pending 会话标记为 `recovery`。
- `run_source` 只在 `run_start` payload 中出现，其他 Hook payload 省略此可选字段。
- 不新增 lifecycle event，也不把来源写入 `session_events`、数据库或配置。

## 替代方案

- 新增 `run_recovery`、`run_continue` 等事件：拒绝；已有 `run_start` 的时序和身份字段足够，来源作为一个字段即可避免事件数量增长。
- 由 Hook 按 session 状态、历史事件或进程状态推断：拒绝；状态描述生命周期位置，不可靠地表示本次运行的触发原因。
- 持久化 run 来源：拒绝；来源只服务于当前 `run_start` 通知，不参与恢复决策和状态转换。

## 影响与迁移

不修改数据库 schema、session event、IPC 或用户配置，无需重置数据。新增的 `run_source` 是 JSON Hook payload 字段；非 `run_start` payload 会省略它。

## 验证

已通过 `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 和 `cargo test --workspace --locked`。全量 UI 检查、测试和生产构建也通过。
