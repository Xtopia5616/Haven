# ADR 0703：分离 MCP server root 与 Tool renderer

## 状态

已采纳并实施。

## 背景

MCP tool manifest 将 server name 同时写入 `ToolIdentity.root` 和 `ToolPresentation.renderer`。前者标识并分组 server；后者是 UI 的组件分派 key。server name 可由用户配置，可能与 `agent`、`system` 或 `haven_diagnostics` 等专用 renderer key 相同，使 MCP 动态 JSON 误进 builtin 专用组件。ADR 0682 删除了未登记的 admin alias，但仍保留 MCP server name 作为 renderer 的选择。

## 决定

- MCP `ToolPresentation.renderer` 固定为 `mcp`；未知的自定义 renderer 按现有规则使用通用 JSON renderer。
- MCP server name 只由 `ToolIdentity.root` 和 `root_presentation` 表达，继续用于身份与展示分组。
- builtin renderer 继续使用当前显式登记的 key；Skill renderer 继续使用固定 `skills` root。
- 不将 `ToolPresentation.renderer` 收紧为闭合 enum；未知 renderer 仍可由扩展源提供并安全回退。

## 替代方案

- 保持 renderer 等于 server name：拒绝，展示分派和实体身份继续共享一个值域，会发生 key 碰撞。
- 把 MCP server name 从 root/presentation 一并删除：拒绝，丢失 server identity 和分组标签。
- 将 renderer 改成闭合 enum：拒绝，扩展 renderer 仍需要开放值域；由 owner 区分而非封闭整个扩展点。

## 影响与验证

MCP manifest 的 renderer 字段由 server name 改为 `mcp`，UI 统一使用通用 JSON 卡片；root、label、tool name 和 MCP result JSON 不变。manifest IPC shape 不变，无配置/数据库格式变化，无需重置。验证通过：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`（801 passed、2 ignored，另有 7 项 MCP 集成测试通过）、`cargo clippy --locked -p haven-tools -- -D warnings`、`corepack pnpm run check`（0 errors、0 warnings）、`corepack pnpm run test:run`（125 files、987 tests passed）、ADR 索引和 `git diff --check`。Vitest 输出 `TimeoutNaNWarning`，退出码为 0。

## 回滚

撤回 MCP adapter 的固定 renderer 并恢复 server name 作为 renderer，再移除此 ADR、命名规则与路线图说明。无数据迁移。
