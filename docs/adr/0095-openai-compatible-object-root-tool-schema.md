# ADR 0095：OpenAI-compatible 工具 schema 的 object-root 投影

- 状态：Accepted
- 日期：2026-09-07
- 替代：[ADR 0092](0092-xai-tool-schema-projection.md)、[ADR 0093](0093-provider-tool-schema-projections.md)
- 关联：[ADR 0094](0094-provider-wire-protocol-normalization.md)

## 背景

内置工具中有多个按 operation/scope 分支表达的根级 `oneOf`，且 `schedule`、
`system` 还包含嵌套联合。MCP 的 `inputSchema` 由外部服务动态提供，可能是
`null`、非对象或根级 `anyOf`/`allOf`。OpenAI Responses 明确要求 function
tool 的 `parameters` 使用 JSON Schema 对象；OpenAI Chat-compatible 网关也不
能假定全部接受根级联合。此前仅 xAI Chat 和 Responses 做 object-root 投影，
普通 Chat 与未知网关仍可能在开始采样前收到 400。

## 决定

1. `ToolDef.input_schema` 和 `haven-tools` 的完整 schema 继续是本地执行边界。
   provider 投影只改变模型可见的 wire schema；工具调用执行前仍使用完整 schema
   做校验。
2. OpenAI Chat、xAI Chat、OpenAI Responses 以及未知 OpenAI-compatible 网关统一
   经过 `project_tool_parameters_for_object_root`：非对象根归一为安全的空对象，
   根级 `anyOf`/`oneOf`/`allOf` 移除，分支 properties 合并，所有分支共同必填项
   保留。
3. 若 root union 的 object 分支共享 `operation`、`scope` 或其他可识别判别字段，
   原始联合约束放到该字段的 `dependentSchemas` 下。这样根不再直接包含 union
   keyword，但 `schedule` 的 delay/due_at 互斥和 `system` 的嵌套 operation 分支
   仍会发送给模型。没有可靠判别字段的动态 schema 只能安全放宽，不能凭空发明
   输入字段或变更本地执行契约。
4. Prompt cache key 对 Chat 和 Responses 都基于实际 provider 投影后的 schema；
   Gemini 的 OpenAPI 子集投影与 Anthropic 的清洗路径保持独立。

## 替代方案

- 只为 xAI 做投影：拒绝，普通 Chat fallback 和未知 gateway 仍暴露同一类根级
  schema 风险。
- 删除所有工具的 root/nested union：拒绝，会削弱本地安全契约，并把 provider
  限制扩散到 `haven-tools`。
- 对没有判别字段的任意 root union 猜测一个 selector：拒绝，动态 MCP schema
  的字段语义未知，猜测会使模型调用和本地验证产生不一致。

## 影响与验证

所有 OpenAI-compatible tool request 都满足 object-root 边界；判别型联合不会因
投影丢失嵌套约束而额外制造 schedule/system 的本地校验失败。无法判别的动态
schema 仍是有意的 provider-facing widening，执行前完整 schema 校验不变。

验证覆盖：LLM schema 投影单元测试、OpenAI Chat/Responses adapter 测试，以及
workspace 格式化、check、Clippy 和 workspace 测试。

## 回滚与重置

回退代码和本 ADR 即可；不修改数据库、配置、session snapshot 或已持久化的工具
参数，不需要用户重置数据。
