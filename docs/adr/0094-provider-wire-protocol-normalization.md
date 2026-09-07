# ADR 0094：模型 provider wire 协议规范化

- 状态：Accepted
- 日期：2026-09-07
- 关联：[ADR 0028](0028-llm-adapter-provider-features-boundary.md)、[ADR 0093](0093-provider-tool-schema-projections.md)

## 背景

`haven-llm` 以统一的 `CanonicalMessage` / `LlmResponse` 承接多个模型
provider，但各厂商在字段命名、工具调用身份、思考状态、结构化输出和流式
终止事件上并不兼容。仅依赖 OpenAI-compatible 的近似格式会丢失 Gemini 的
`thoughtSignature`，也会把 DeepSeek 的 Responses `response.incomplete` 当成
异常 EOF；向 Anthropic 发送错误的 thinking 形态还会导致 400。

## 决定

1. provider wire 字段只在 `haven-llm` adapter 内映射；统一层继续使用
   snake_case Rust 字段和 canonical 类型。
2. Gemini 使用官方 REST lowerCamel JSON：`generationConfig`、`maxOutputTokens`、
   `functionDeclarations`、`functionCall`、`functionResponse`、`thoughtSignature`
   和 `inlineData`。函数调用 ID 与 thought signature 原样保存并在后续请求中
   回传；模型 URL 去除可能重复的 `models/` 前缀。
3. Anthropic 按已知 Claude 模型代际选择 thinking wire：4.6+ 使用
   `thinking.type=adaptive` 与 `output_config.effort`，旧的 thinking-capable
   模型使用满足最小值和 `budget_tokens < max_tokens` 的 manual budget；未知
   模型不猜测能力。Anthropic 只回传自己的签名 thinking blocks。
4. DeepSeek Chat thinking 请求省略官方明确声明无效的 sampling 参数；DeepSeek
   Responses 发送 `text.format`、`output_config.effort`，不发送不受支持的
   `prompt_cache_key`，并处理 `response.completed`、`response.incomplete` 和
   `response.failed` 三类终止事件。
5. Responses 的 `response_format` 在适配器边界转换成 `text: {format: ...}`；
   `top_p` 仅在未启用 reasoning 时发送，避免向 thinking 模式传递无效采样项。
6. provider-opaque thinking state 继续通过 canonical `thinking_blocks` 传递，
   但每个 adapter 只回传自己认可的载荷，禁止跨 provider 直接复用原始块。

## 官方依据

- [OpenAI Chat API reference](https://developers.openai.com/api/reference/resources/chat)
- [OpenAI Responses API reference](https://platform.openai.com/docs/api-reference/responses)
- [Anthropic extended thinking](https://platform.claude.com/docs/en/build-with-claude/extended-thinking)
- [Anthropic effort](https://platform.claude.com/docs/en/build-with-claude/effort)
- [Google Gemini generateContent](https://ai.google.dev/api/generate-content)
- [DeepSeek Responses API](https://api-docs.deepseek.com/api/create-response/)
- [DeepSeek Thinking Mode](https://api-docs.deepseek.com/guides/thinking_mode/)

## 替代方案

- 在 `router` 或 `agent` 中按 provider 分支：拒绝，会把 wire 协议泄漏到编排
  层并破坏 adapter 的单一职责。
- 所有 provider 强行使用 OpenAI Chat wire：拒绝，无法表达 Gemini signature、
  Anthropic thinking block 和 DeepSeek Responses 语义。
- 对未知 Claude 模型默认发送 adaptive thinking：拒绝，官方明确指出旧模型会
  400；未知能力必须保持不发送。

## 影响与验证

请求字段和响应解析更贴近官方协议；工具执行、canonical transcript 和本地
schema 校验保持不变。新增正向回归测试覆盖 Gemini wire casing/signature、
Anthropic thinking 代际、DeepSeek sampling/Responses fields 以及 incomplete
stream event。

验证命令：

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --locked -p haven-llm --lib
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
```

## 回滚与重置

回退代码和本 ADR 即可；不修改数据库、配置 schema、session snapshot 或已存储
工具参数，不需要用户重置数据。
