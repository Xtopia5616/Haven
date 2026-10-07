# ADR 0620：命名 Anthropic request projection 结果

## 状态

已采纳并实施。

## 背景

Anthropic request 组装有两项 multi-value conversion：canonical transcript mapping 返回 provider messages 与可选 system prompt；thinking policy 返回两个独立 wire fields `thinking` 与 `output_config`。request builder 与测试原以 tuple 位置解释这两组结果。

## 决定

1. transcript mapping 返回 `AnthropicMessageConversion { messages, system }`。
2. Anthropic thinking policy 返回 `AnthropicThinkingConfig { thinking, output_config }`。
3. request builder 与测试从字段名读取值；两个类型由 Anthropic adapter 自己拥有。
4. 保持 message mapping、system prompt cache control、thinking budget 和 request JSON 不变。

## 替代方案

- 保留 tuples：拒绝，两组字段都会进入 request builder 的不同处理逻辑，位置约定没有必要。
- 合并两个结果：拒绝，transcript conversion 和 model-specific thinking policy 是不同阶段的 owner。
- 复用通用 LLM config projection：拒绝，其字段分别是 Anthropic Messages API 专用 wire shape，不能表达 OpenAI-compatible chat 或 Responses API 契约。

## 影响与验证

- 这是 `haven-llm` Anthropic adapter 内部 Rust 类型调整，wire payload、缓存边界和 provider 行为不变。
- 命名审计 §5.7 保持 Active；其他 provider adapter 的 mapping 结果与其余项目命名仍需逐域审计。
- 验证：LLM fmt、locked check、strict Clippy、crate tests、ADR 索引及 staged diff 检查。

## 回滚

恢复两个 tuple 返回并同步 request builder 与测试；无需 wire 或持久化迁移。
