# ADR 0682：删除未登记的 Tool renderer 别名

## 状态

已采纳并实施。

## 背景

当前 builtin manifest 的管理 renderer 是 `haven_diagnostics`、`haven_config`、`haven_skills`、`haven_tools` 和 `haven_mcp`。Skill adapter 使用工具 root，MCP adapter 使用 server name。UI result registry 还把 `haven`、`admin`、`settings` 三个未由当前 producer 发出的 renderer 值统一路由到 `ToolAdminResult`，并按动态 `operation` 前缀再次分流到 ToolRun/定时任务组件。该隐式兼容路径会把名称恰好相同的 MCP server 错误解释为 Admin payload。

## 决定

只把五个当前管理 renderer key 映射到 `ToolAdminResult`。删除 `haven`、`admin`、`settings` 别名和按 `operation` 字符串进行的二次路由。其余未登记 renderer 保持开放字符串输入，但统一使用通用 JSON renderer。

## 替代方案

- 保留多个 key 映射同一个管理组件：拒绝，没有当前 manifest producer 或独立结果语义，且可能误分类 MCP server。
- 收紧 `ToolPresentation.renderer` 成闭合 enum：拒绝，MCP renderer 仍由 server name 提供；未知扩展值需安全地退回 JSON 展示。

## 影响与验证

- 只改变未登记 renderer 值的 UI 选择；当前 builtin admin key、MCP server 名、Skill root 和通用 JSON fallback 保持明确语义。
- 不涉及 IPC 字段、配置或持久化，无需重置。
- 已执行 Svelte 类型检查；测试未运行。

## 回滚

恢复已删除的 renderer aliases 与 payload 字符串分支即可；无需数据重置。
