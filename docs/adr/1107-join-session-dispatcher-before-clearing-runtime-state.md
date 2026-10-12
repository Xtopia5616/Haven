# ADR 1107：清理会话运行态前停止并 join dispatcher

## 状态

已接受（2026-10-13）

## 背景

Session dispatcher 持有每个 session run wrapper 的 `JoinSet`。wrapper 在 `actor.finish_run()` 发布 `running=false` 后仍要执行 `reconcile_run_exit()`；因此仅等待 actor run bit 会早于 terminal cleanup。旧关闭顺序可能在 reconciliation 获得 lifecycle gate 前清除 actor registry，且 dispatcher 退出时会丢弃未处理的 terminal-cleanup retry。`finish_ended_session` 还会 detached 启动 Messaging inbox 注销。

## 决定

- `SessionSupervisor` 为 dispatcher 创建独立 child cancellation token，并持有 dispatcher handle。`join_dispatcher()` 负责停止 dispatcher，等待其 loop、run wrappers 和关停期间排队的 terminal-cleanup retry；等待最多 30 秒，超时后记录并 abort。
- `clear_session_runtime_state_for_shutdown()` 先阻断新 lifecycle admission、取消 actor 并等待 run bit，然后在不持有 lifecycle gate 的状态下 stop/join dispatcher，最后才取得 gate 并清理 actor registry。即使 quiesce 返回错误，也先尝试 join dispatcher。
- dispatcher 在 join run wrappers 后对剩余 retry queue 做一次最终处理；仍失败的项目通过 warning 计数显式记录。
- Messaging inbox 注销通过 `spawn_blocking` 执行，并由 `finish_ended_session` await；不再留下 detached cleanup task。

## 影响

运行时关闭期间，dispatcher reconciliation 和 inbox 注销仍在其依赖服务关闭前完成。shutdown 多等待至多 30 秒后才 abort dispatcher。没有数据库 schema、配置或 IPC shape 变化，无需重置持久数据。

## 验证

- `cargo test --locked -p haven-agent session::tests -- --nocapture`：94 passed，0 failed，0 ignored（session 子集）。
- `cargo check --locked -p haven-agent -p haven-app-binary`。
- 工作区完整测试、check、严格 Clippy、IPC 与 ADR 索引检查在本切片集成后执行。

## 回滚

不得在清空 actor registry 或关闭 ToolRun/MCP 之前丢弃 dispatcher handle。若调整关闭顺序，必须保留“停止 run admission → actor quiesce → wrapper/retry join → 清 registry → 关闭依赖资源”的所有权顺序。
