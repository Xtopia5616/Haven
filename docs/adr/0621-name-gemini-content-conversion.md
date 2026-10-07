# ADR 0621：命名 Gemini content conversion 结果

## 状态

已采纳并实施。

## 背景

Gemini `convert_contents` 将 provider-neutral canonical messages 转换为 Gemini `contents`，并从 System 消息单独构造 `system_instruction`。Request builder、guidance append path 和 adapter tests 原从 `(contents, system_instruction)` tuple 读取不同阶段的 wire 字段。

## 决定

1. 由 Gemini adapter 定义 `GeminiContentConversion { contents, system_instruction }` 并从 `convert_contents` 返回。
2. Request builder、guidance append 和 tests 按字段读取转换结果。
3. 保持 tool call / function response 次序、system prompt cache section 投影和 Gemini wire payload 不变。

## 替代方案

- 保留 tuple：拒绝，provider content 与 system instruction 进入 request 的不同字段，调用方不应依赖位置。
- 复用 Anthropic message conversion：拒绝，两个 provider 有不同 wire types、tool result 规则和 system instruction 形状。
- 合并到通用 LLM request model：拒绝，转换结果是 Gemini adapter 内部的 wire projection，不是跨 provider 业务契约。

## 影响与验证

- 这是 `haven-llm` Gemini adapter 内部 Rust 类型调整，生成请求与 provider 兼容行为不变。
- 命名审计 §5.7 保持 Active；其它 provider adapter 与项目剩余符号仍待逐域审计。
- 验证：LLM fmt、locked check、strict Clippy、crate tests、ADR 索引及 staged diff 检查。

## 回滚

恢复 `(contents, system_instruction)` tuple 并同步 request builder、guidance append 和测试；无需 wire 或持久化迁移。
