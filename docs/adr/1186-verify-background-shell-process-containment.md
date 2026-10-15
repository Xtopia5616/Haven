# ADR 1186：验证后台 Shell 关闭时的进程树回收

## 状态

已采纳并实施（2026-10-15）。

## 背景

后台 Shell 在 `ToolRunService` 托管任务中继续运行，Job Object guard 也随任务持有；服务关闭会取消 runner 并等待任务清理。ADR 1184 已验证前台 Shell 的取消路径，ADR 1185 已验证 SkillRunner。仍需验证后台 ToolRun 的实际关闭路径能终止 Shell 派生的后代。

## 决定

1. 增加 Windows adapter 级测试：后台 Shell 启动长驻子 PowerShell 并记录 PID；`ToolRunService::shutdown` 返回前必须结束该后代、清空受管任务并将 ToolRun 标记为 Cancelled。
2. 复用仅测试用的 `DescendantProcessMarker` 做 PID 等待与失败清理，不增加生产进程控制入口。
3. 此决定验证服务关闭的进程树生命周期，不改变后台任务的授权、持久化或用户可见语义，也不代表文件系统或网络隔离。
4. 无配置、IPC、数据库契约或持久数据变化，无需重置。

## 替代方案

- 只断言 ToolRun 状态变成 Cancelled：不足以证明派生进程也随 Job 关闭。
- 从测试中启动 `taskkill` 递归清理：拒绝。它依赖进程树发现行为且不能证明 Job Object 自身完成了清理。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked --offline -p haven-tools tool_run_service::tests::test_shutdown_terminates_background_shell_descendants -- --exact --nocapture`
- `cargo check --workspace --locked --offline`
- `cargo clippy --workspace --locked --offline -- -D warnings`
- `cargo test --workspace --locked --offline --quiet -- --test-threads=4`
- ADR index、crate dependency inventory 与 `git diff --check`。

## 回滚

删除后台 Shell 进程树测试和 ADR，并恢复路线图的未完成项。无需变更生产逻辑、配置或持久数据。
