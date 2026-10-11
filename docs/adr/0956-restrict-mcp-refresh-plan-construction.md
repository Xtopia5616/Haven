# 0956：限制 MCP refresh plan 的下游构造

## 状态

已接受并实现（2026-10-11）。

## 背景

`McpRefreshPlan` 与 `McpRefreshTarget` 是 MCP runtime 从当前 manager reconciliation 生成、供 App 确认/授权流程读取的 Tools-owned projection。生产路径只由 `McpRuntimePort::plan_refresh` 生成；App 使用字段筛选授权后的失败目标。App 只有一处单元测试 fixture 通过 struct literal 构造该计划，没有生产代码在 App 或 Agent 侧创建计划。

公开字段和 `McpRefreshPlan::from_reconcile` 让下游可以伪造 Tools producer-owned refresh plan，也使 DTO 结构调整要求 App fixture 同步改写。

## 决定

- 将 `McpRefreshPlan` 和 `McpRefreshTarget` 标为 `non_exhaustive`，保留下游字段读取能力并禁止 struct literal 构造。
- 将 `from_reconcile` 收回为 Tools crate 内部操作。
- 为确有需要的 App projection 单测提供 `test-support` 下的 `McpRefreshPlan::new_for_test`；构造器接收 name/action pairs，由 owner DTO 内部装配 targets。
- 保持 `McpRefreshAction` 作为读取和测试所需的语义枚举，refresh 计划内容、授权、确认与失败通知行为不变。

## 影响

Tools 继续唯一负责从 MCP reconciliation 导出 refresh plan；App 仍可读取计划并按 Connect/Reconnect/Disconnect 决定确认、授权和结果投影。Tauri IPC、MCP config、持久化、网络策略和安全语义均不变，无需数据重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`
- `git diff --cached --check`

## 回滚

若未来 App 需要作为生产 refresh-plan producer，应先确认其对 MCP reconciliation 的权威来源；保持当前由 Tools 生成计划的事实边界，并通过具名 Tools operation 接收输入，不恢复通用跨 crate struct literal。
