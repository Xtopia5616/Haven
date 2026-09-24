# ADR 0252：PromptRequest 迁移与旧 prompt wrapper 删除

- 状态：已采纳（2026-09-24）
- 范围：`haven-llm::LlmRouter` 的 one-shot prompt 调用方
- 关联：[ADR 0234](0234-llm-complete-request-object.md)、[ADR 0250](0250-remove-unused-prompt-output-cap-wrapper.md)

## 背景

`PromptRequest` 已提供拥有数据的 one-shot prompt 请求边界，但标题生成和后台记忆推理
仍调用 `(RequestKind, &str, &str)` 形式的 `chat_with_prompt`。只保留新入口会让 Router
继续暴露两套等价请求形态，也让调用方承担 prompt 参数的组合责任。

## 决定

1. 标题生成与记忆推理统一构造 `PromptRequest`，通过
   `LlmRouter::chat_with_prompt_request` 调用。
2. 删除旧的 `chat_with_prompt` wrapper；全仓生产代码和测试不再保留三参数入口。
3. `PromptRequest` 继续只表达 request kind、system prompt 和 user prompt；实际 provider
   路由、健康/限流投影和错误语义仍由 `complete`/Router 所有。

## 影响与验证

这次迁移删除一个等价的兼容 API，不改变 FastChat 路由、消息角色、文本、错误、健康状态
或限流状态。标题测试增加完整 system/user 内容断言；全仓符号搜索确认旧入口调用为零。
验证包括格式检查、workspace 严格 Clippy 和全 workspace 测试。

## 回滚

回退本切片提交并恢复 wrapper 即可；没有 schema、IPC 或用户数据格式变化，无需数据库重置。
