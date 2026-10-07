# ADR 0683：统一 Admin capability 工具名 owner

## 状态

已采纳并实施。

## 背景

Admin 管理工具有五个 capability，分别登记为 `haven_diagnostics`、`haven_config`、`haven_skills`、`haven_tools` 与 `haven_mcp`。这些名称过去同时写在 `AdminCapability::name()`、`AdminRequest::tool_name()` 和 `model_metadata` 的 surface 映射里，使同一工具身份有多个字符串 owner。MCP model request 与 native MCP request 属于同一 `haven_mcp` capability。

## 决定

`AdminCapability` 是管理工具名的唯一映射 owner，accessor 明确叫 `tool_name`。`model_metadata` 先把 model operation surface 解析为 enum，再取 canonical tool name。删除只被 catalog 关联测试使用、并重复维护工具名字符串的 `AdminRequest::tool_name()`；该测试改用 operation case 自身的 surface key。

## 替代方案

- 保留三处字符串映射：拒绝，同一 `haven_*` 工具名会在 request、catalog 与 operation metadata 之间漂移。
- 让 operation metadata 成为唯一字符串 owner：拒绝，catalog registration 先于具体 operation metadata，仍需要 capability enum 登记工具。

## 影响与验证

- 仅重构 Tools crate 内部映射与 catalog 关联测试引用；工具名、权限 metadata、模型 operation contract 和执行行为不变。删除未被生产调用的公开 Rust helper，不保留兼容入口。
- 无配置、IPC 或持久化变化，无需重置。
- 已执行 Rust workspace 编译、严格 Clippy 与格式检查；测试未运行。

## 回滚

恢复重复 accessor/map 即可；无需数据重置。
