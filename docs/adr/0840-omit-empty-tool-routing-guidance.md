# ADR 0840：只展示有信息量的工具目录指引

## 状态

Accepted — 2026-10-09

## 背景

`tool_catalog describe` 会把工具的 `when_to_use` 与 `when_not_to_use` 一并展示给模型。MCP、Skill 和部分内置 operation view 的“不要使用”文本只是重复说明工具已经加载、名称已区分 operation，或 schema 不接受固定字段；这些事实已经由当前工具 surface 与参数 schema 表达。

## 决定

- `ToolPrompt.when_not_to_use` 为空表示没有额外的路由建议。
- `tool_catalog describe` 只输出非空指引；两项都为空时不输出 `guidance`。
- MCP、Skill 和具名内置 operation view 不再添加重复的通用禁用句。确有助于选择更合适 operation 的提示继续保留。
- 工具加载状态、schema 校验、权限与执行策略仍由各自的运行时 owner 决定。

## 替代方案

- 对所有工具保留固定的 `when_not_to_use` 句子：拒绝。重复指引占用模型上下文，也容易把描述性建议变成看似强制的规则。
- 删除全部工具指引：拒绝。具体 `when_to_use` 和有意义的替代 operation 说明仍能帮助路由。

## 影响与验证

只调整 prompt 元数据及模型可见的 `tool_catalog describe` 展示，不改变 provider schema、工具加载、授权、执行、IPC 或持久化数据，无需数据重置。验证包括 Common 与 Tools 测试、严格 Clippy、格式检查和差异检查。

## 回滚

恢复各适配器的通用指引并在 `tool_catalog describe` 中重新输出两个字段即可；无需数据重置。
