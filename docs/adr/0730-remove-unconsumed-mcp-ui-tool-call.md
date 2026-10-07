# ADR 0730：移除无消费者的 MCP UI 直调命令

## 状态

已采纳并实施。

## 背景

`mcp_tool_call` 是注册到 Tauri 的 UI 直调入口，但全仓 UI 没有调用它，也没有 wrapper；MCP 设置页只维护服务器配置、连接状态和发现到的工具清单。`list_mcp_servers` 等管理入口有 `ToolsView` 消费者。

Agent 侧的真实 MCP 执行链由 `McpToolAdapter` 将 tool call 路由到 `McpManager::call_tool`，并经过 `AgentExecutor` 的授权与执行生命周期。该链与 UI 命令共享 MCP client manager，但不依赖这条命令。

孤立 handler 自己重复了外部能力授权，并尝试复用 AppCommand confirmation：自动批准后返回 `{success, output, error}`；需要确认时排队，确认后又通过 `UiConfirmationAction::Mcp` 重放调用，却丢弃结果；授权拒绝返回命令错误，client 调用错误也返回错误。它不写持久数据，也没有可恢复的独立任务或输出投递 owner。`UiConfirmationAction::Mcp` 除该 handler 外没有生产创建者；少量测试只借用它构造通用 AppCommand confirmation。

## 决定

- 删除 `mcp_tool_call` Tauri handler、handler 注册和 command contract 项；不保留旧 command alias。
- 删除专属 `McpToolCallResponse` wire DTO 和 `UiConfirmationAction::Mcp` 及其执行分支。
- 通用 AppCommand confirmation、`execute_skill` 的真实 UI 调用、MCP server 管理命令与 Agent MCP tool adapter 保持各自 owner。
- 将确认路由测试改为使用仍在生产使用的 Skill action，保留 request ID、owner 校验、过期和重试行为覆盖。
- 若未来需要 UI 直接运行 MCP tool，须由具体交互页面提出新契约，并明确结果呈现、取消、授权和失败恢复；不为未实现的预期保留当前入口。

## 替代方案

- 保留命令等待未来页面：拒绝。现状没有生产消费者，测试/登记/安全说明是同一孤立路径的旁证，不能证明实际 UI 能力；确认后丢弃返回值也不构成完整用户流程。
- 复用 Agent MCP 执行器：拒绝。本轮没有 UI 直调功能需求；未来页面应先定义完整结果与生命周期，再选择可复用的授权/执行边界。
- 删除 MCP server 管理或 Agent adapter：拒绝。前者由设置页消费，后者是 Agent 执行 MCP 能力的生产链。

## 影响与验证

Tauri 命令目录从 81 项减为 80 项，generated TypeScript command contract 随 Rust handler 更新。该变化删除未被当前 UI 使用的 IPC surface，不改变 MCP 协议、MCP server 配置、Agent tool call、数据库或配置持久格式；不需要数据迁移或重置。没有兼容别名。

验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`；`corepack pnpm run check`、`corepack pnpm run test:run`（124 files / 990 tests）、`corepack pnpm run build`；`scripts/check-ipc-contracts.ps1`（80 commands）、`scripts/check-ipc-events.ps1`（35 channels）、`scripts/check-adr-index.ps1`（713 records）与 `git diff --check`。

## 回滚

如恢复 UI 直调功能，应在真实 renderer caller 与可见结果/失败流程准备完成后新增明确契约；不恢复当前无消费者命令或其结果被丢弃的确认分支。
