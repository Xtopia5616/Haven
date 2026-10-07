# ADR 0619：命名 chat thinking extras

## 状态

已采纳并实施。

## 背景

LLM provider feature policy 的 `chat_thinking_extras` 根据 endpoint vendor 与 `reasoning_effort` 生成两个可选 chat request 字段：vendor-specific `thinking` object 与规范化的 `reasoning_effort` 字符串。OpenAI-compatible request builder 与 policy tests 都按 tuple 位置读取它们；Kimi 子策略也返回同一对值。

## 决定

1. 定义 `ChatThinkingExtras { thinking, reasoning_effort }`，由 provider feature policy 和 Kimi 子策略返回。
2. OpenAI-compatible request builder 与测试通过字段名读取值。
3. 保留 DeepSeek、Kimi、OpenAI 和其它 provider 的现有分支、归一化和 wire JSON。

## 替代方案

- 保留 tuple：拒绝，两个字段都进入 request builder 的条件和 wire 字段，调用方依赖顺序不必要。
- 使用通用 `ReasoningConfig`：拒绝，该值只表达 chat-completions 的 vendor extras，不覆盖 Responses API 或 Anthropic 的不同 wire shapes。
- 复用 Anthropic thinking 配置结果：拒绝，Anthropic 的两个值具有不同协议形状与 token-budget 语义。

## 影响与验证

- 这是 `haven-llm` 内部 Rust 类型调整，provider 请求字段与兼容行为不变。
- 命名审计 §5.7 保持 Active；其他 provider adapter mapping 和全仓符号仍待逐域审计。
- 验证：LLM fmt、locked check、strict Clippy、crate tests、ADR 索引及 staged diff 检查。

## 回滚

恢复 `(thinking, reasoning_effort)` 返回值并同步 request builder 与 policy tests；无需 wire 或持久化迁移。
