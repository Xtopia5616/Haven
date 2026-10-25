# ADR 1412：MCP 与 Skill 外部调用逐次确认

## 状态

已实施。

## 背景

MCP server 与 Skill 脚本可能在远端或子进程内产生本地授权器无法完整观察的副作用。它们当前以高风险 `ExternalEffect` 运行；Collaborative 模式会询问，但已保存的 Always grant 可以让后续调用静默执行。用户需要能依赖“外部副作用每次经过审批”的策略。

## 决定

1. `OperationPolicy::external` 对 MCP 与 Skill 调用统一使用 `ConfirmationRequirement::Required`。每次调用都必须取得新的用户确认，永久 Allow 和会话 Allow 不能跳过确认。
2. `Collaborative` 模式会发起确认；`Unattended` 模式无法询问，因此返回阻止。Analyze 仍按其只读边界阻止外部效果。
3. 显式禁用项、永久/会话 Deny、网络拒绝、WorkspaceWrite 下无法约束的宿主进程，以及文件拒绝/保护路径仍在确认之前硬阻止。确认不能覆盖这些策略。
4. 当前 Haven 没有按 Shell 命令文本匹配的独立 `prompt` / `forbidden` 规则。统一授权契约已有的 `Required` 表示每次询问，禁用项和 Deny 表示硬拒绝；不得把风险提示或模型指令当作命令匹配规则。
5. WorkspaceWrite 下的文件范围扩展继续使用现有一次性 `ResourceBoundaryExtension` 确认；Shell 的 MXC 拒绝重试按 [ADR 1401](1401-shell-mxc-structured-denial-confirmation.md) 处理。显式拒绝路径、只读路径和工具级限制不会被一次性确认绕过。

## 替代方案

- 继续允许外部调用复用 Always grant：拒绝，持久授权会使之后的远端或脚本副作用不再经过当前用户确认。
- 把 `Restricted` 网络策略或 WorkspaceWrite 的进程限制改成确认例外：拒绝，用户同意不等于 Haven 能验证目的地或约束宿主进程。
- 根据外部工具描述或 schema 推断其安全性：拒绝，这些信息由外部提供，不能降低授权要求。

## 影响与回滚

MCP 与 Skill 的已保存 Allow 不再免除每次调用的确认；显式 Deny 与执行边界仍优先。没有配置字段、数据库 schema、IPC 或持久授权格式变化，无需重置数据。若要回滚，恢复 `OperationPolicy::external` 原有的 `SecurityPolicy` 确认级别，并移除此 ADR 中逐次确认约定。

## 验证

- `cargo fmt --all -- --check` 与 `git diff --check` 通过。
- `cargo test --locked -p haven-tools confirmation` 通过（8 passed，1 个依赖 MXC 真机的测试 ignored）；覆盖 MCP 精确 Always grant 仍要求新确认，以及 Required/deny 优先级。
- `cargo test --workspace --locked` 与 `cargo clippy --workspace --locked -- -D warnings` 通过。
- `corepack pnpm --dir ui run check` 通过；`corepack pnpm --dir ui run test:run` 通过（1502 tests）。
- `pwsh -NoProfile -File scripts/check-adr-index.ps1` 通过（1309 个唯一编号记录，链接有效）。本次没有 IPC 契约变化。
