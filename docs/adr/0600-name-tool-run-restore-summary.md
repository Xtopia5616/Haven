# ADR 0600：命名 ToolRun restore summary

## 状态

已采纳并实施。

## 背景

Agent startup 调用 Tools 的 `ToolRunService::restore`，它依次 fire 逾期 scheduled runs，并把前进程遗留 running rows 标记为 failed。该方法原返回 `(scheduled, interrupted)` 两个 usize，由 AgentLayer 按位置解构后记录日志；第二项可覆盖 background 与 scheduled 两种 kind，原日志却称为 interrupted background ToolRun。

## 决定

1. 跨 crate 结果改为 `ToolRunRestoreSummary { overdue_scheduled_runs, interrupted_runs_marked_failed }` 并从 `haven_tools` crate root 导出。第一项准确计数进入 overdue fire 路径的行，不承诺各行 fire 都成功提交。
2. AgentLayer 按字段读取两类恢复计数，并将第二项日志改为不限定 kind 的 ToolRun 文案。
3. `restore_after_restart` 与 `restore_pending` 的各自标量返回保持不变，它们仅报告单阶段计数。

## 替代方案

- 保留 tuple 并调整 Agent 局部变量名：拒绝，Tools→Agent 公共 crate API 仍要求消费者记住顺序。
- 把两种计数合为一个恢复总数：拒绝，逾期定时 fire 和未完成 ToolRun 失败清理表示不同恢复动作，观测和日志意义不同。

## 影响与验证

- 变更 Tools→Agent Rust crate API 返回类型，不改数据库行、ToolRun kind/status、due-time、恢复顺序或自动 replay 策略。
- 命名路线图仍保持 Active；IPC/UI、其他 crate API、配置持久名继续逐域审计。
- 验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、ADR 索引及 staged diff 检查。

## 回滚

恢复 `(scheduled, interrupted)` 并还原 AgentLayer tuple 解构与旧日志表述；无持久化迁移。
