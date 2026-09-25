# ADR 0353：Scheduled admission 保留运行中的 action

- 状态：Implemented
- 日期：2026-09-25
- 范围：`haven-tools::ActionService::set` 的 scheduled 内存 entry 清理
- 关联：[ADR 0332](0332-action-claim-lease-core.md)、[ADR 0334](0334-action-terminal-persistence-retry-policy.md)、[ADR 0338](0338-action-tail-output-policy-and-snapshot.md)、[ADR 0352](0352-action-lifecycle-transition-core-audit.md)

## 背景

ADR 0352 审计发现，`ActionService::set` 通过 `Scheduled && !Waiting` 回收 scheduled registry entry。该条件会同时移除 `Running` 和 terminal row，尽管注释描述的只是 terminal cleanup。AgentLayer 收到 fire 后执行 scheduled tool/session work，再调用 `complete_scheduled` 或 `fail_scheduled`；这些入口需要先从 registry 取得 live row。若期间另一次 `set` 移除了 Running row，terminal callback 会返回 `false`，durable row 留在 `running`，直到下次启动 cleanup。

## 决定

1. Scheduled admission 只回收 `ActionState::is_terminal()` 的 entry。Waiting 和 Running entry 保留；pending capacity 仍只按 Waiting 计数。
2. Running entry 在后续 admission 后仍由既有 completion、failure、cancel 与 terminal retry 路径查找。终态 entry 仍在下一次 admission 时从内存 board 回收，durable history 不删除。
3. No-consumer fire 若成功 rollback，仍回到 Waiting 并重装 timer；rollback 失败时 pending-fire recovery 仍保留 Running fire，额外 admission 不得移除其内存 row，使迟到 receiver 的 terminal callback 继续可用。
4. Restart 语义不变：只恢复 durable Waiting scheduled row；durable Running scheduled row 由 `restore_after_restart` 标记 failed，不 replay。Watch dependency 仍是进程内关系。
5. 不改变 status/state graph、ActionStore CAS/outbox、lease、timer、terminal retry、事件顺序、IPC、schema、ID 或 background action 行为；不增加 Job 状态、owner token、timeout、续租或重试语义。

## 验证

- Tools 回归覆盖 admission 后 Running row 仍可完成或取消、terminal entry 在后续 admission 回收且 durable history 保留、no-consumer rollback 失败后的 late receiver 完成，以及 restart 将遗留 Running row 标为 failed 且不重放。
- 验收命令：`cargo fmt --all`、`cargo test --locked -p haven-tools`（含 `mcp_integration`）、`cargo test --locked -p haven-agent`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`。

## 影响与回滚

无需 schema、IPC、配置或用户数据迁移。回滚时恢复 admission 清理条件并删除对应回归测试、ADR 与架构/路线图记录；回滚会重新引入 Running entry 可能被后续 admission 移除的问题。
