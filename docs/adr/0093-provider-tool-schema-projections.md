# ADR 0093：Provider 工具 schema 投影与 OpenAI-compatible 网关审查

- 状态：Accepted
- 日期：2026-09-07
- 关联：[ADR 0092](0092-xai-tool-schema-projection.md)

## 背景

Haven 内部的工具 schema 是执行前校验的完整契约，但不同 provider 的工具
schema 入口并不接受同一套 JSON Schema。除 xAI 的根级 union 限制外，OpenAI
Responses 还会根据 strict 模式处理 schema，Gemini 只承诺 OpenAPI schema 的
子集；Anthropic 则接收 `input_schema` 对象。

## 决定

1. `ToolDefinition` 的完整 schema 和本地工具注册表继续作为执行边界。所有
   provider 投影都只影响模型可见的 wire schema，不能替代本地参数校验。
2. OpenAI Chat 和 Anthropic 在 adapter 边界做防御性清洗：确保根值是对象，
   清除会让通用 JSON Schema meta-schema 失效的 `null` 结构字段，但保留
   `oneOf`/`anyOf` 等语义。
3. OpenAI Responses 的 function tool 明确发送 `strict: false`，并在发送前将
   根级 `anyOf`/`oneOf`/`allOf` 投影为 object-root schema；这同时避免服务端
   把完整 schema 自动收窄到 strict 子集，并满足 Responses 的 `parameters`
   object 边界。Haven 继续在执行前用完整 schema 做严格校验。
4. Gemini 使用独立的 OpenAPI 子集投影：根/嵌套对象 union 展平为 object，
   `const` 转为单值 `enum`，并去掉 Gemini 不承诺支持的约束关键字。投影后的
   schema 必须稳定地包含对象根或可识别的基础类型。
5. 其他 OpenAI-compatible 网关继续使用 OpenAI Chat wire schema 和上述防御性
   清洗，但暂不自动套用 xAI/Gemini 投影。未知网关的能力差异无法仅靠
   `api_style=openai-chat` 判定，后续按具体网关做兼容性矩阵或能力探测。

## 替代方案

- 对所有 provider 统一删除 union：拒绝，会降低本地契约表达能力并把一个
  provider 的限制扩散到所有 endpoint。
- 对所有 OpenAI-compatible 网关统一做 xAI 投影：拒绝，网关可能支持更完整
  的 JSON Schema，且当前没有足够的兼容性证据证明统一降级安全。
- 开启 OpenAI Responses strict：拒绝，Haven 的 schema 包含条件分支和局部
  可选字段，strict 子集会要求不同的 required 语义。

## 影响与验证

OpenAI Chat、Responses、Anthropic 和 Gemini 的工具请求分别遵循其 wire 边界；
Responses 与 xAI 共用 object-root 投影，工具执行仍由 Haven 的完整 schema
校验。未知 OpenAI-compatible 网关仍是一个明确的审查项：当前代码不会误把它
识别成 xAI 或 Gemini，但也不能保证它接受根级 union。

验证包括 provider adapter 单元测试、LLM crate 全量测试，以及 workspace
格式化、check、Clippy 和 workspace 测试门禁。

## 回滚

回退对应代码提交和本 ADR 即可；本变更不修改数据库、配置或已持久化的工具数据。
