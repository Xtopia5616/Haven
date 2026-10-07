# ADR 0681：按命令 owner 命名前端 IPC 请求类型

## 状态

已采纳并实施。

## 背景

`contracts/tools.ts` 中的 `SetEnabledRequest` 从 `set_tool_enabled` 派生，却也被 Skill 开关 wrapper 使用；`McpNameRequest` 从 `reconnect_mcp` 派生，却也被 MCP 删除 wrapper 使用。这几组 command 当前碰巧有相同字段，但 alias 让 wrapper 的 request type owner 指向另一个操作，名字也没有表达实际命令。

## 决定

为四个操作分别定义其准确的 generated request alias：`SetSkillEnabledRequest`、`SetToolEnabledRequest`、`ReconnectMcpRequest` 和 `RemoveMcpServerRequest`。wrapper 参数使用与实际 invoke command 一一对应的 alias。即使未来字段形状相同，也不共享从相邻命令派生的 alias。

## 替代方案

- 保留通用 `SetEnabledRequest` / `McpNameRequest`：拒绝，类型来源被绑定到其中一个命令，而其它调用方依赖偶然相同的字段形状。
- 用手写 `{ name: string; enabled: boolean }`：拒绝，会复制 generated command contract 并引入漂移来源。

## 影响与验证

- 只改前端 TypeScript alias 与 wrapper 参数类型，不改 command name、wire 字段、行为或持久数据。
- 无兼容 alias。
- 已执行 Svelte 类型检查；测试未运行。

## 回滚

恢复旧 alias 与 wrapper 参数引用即可，无需数据重置。
