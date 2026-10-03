# ADR 0438：按官方契约传递 Provider 缓存路由提示

## 状态

已采纳（2026-10-03）。

## 背景

缓存命中首先要求 provider 收到完全相同的可缓存前缀。Haven 之前把 OpenAI 的 `prompt_cache_key` 当作通用兼容字段尝试发送到多种 OpenAI-compatible endpoint；这既不符合每家的协议，也会造成可避免的参数拒绝。xAI Chat Completions 官方使用会话亲和请求头，和 xAI Responses 的 body 字段不同。

已有诊断显示，同一会话多次请求的缓存率可以达到 97–99%，但恢复后的首个请求曾反复只有约 9,856 个缓存 token。累计样本 1,082,385 个 prompt token 中命中 477,184 个，约 44.1%。每轮恢复时变化的秒级本地时钟已从稳定 system prompt 移除（ADR 0436）；工具定义实际扩充仍会改变请求前缀，属于可预期的一次性缓存断点（ADR 0435）。Provider routing key 只帮助请求落到更可能复用的缓存路由，不能覆盖前缀变化，也不保证命中。

## 决定

- 仅在官方 OpenAI Chat Completions 和 Responses endpoint 传 `prompt_cache_key`；xAI Responses 也按其官方 Responses 契约传该字段。
- xAI Chat Completions 使用官方 `x-grok-conv-id` 请求头。由模型、稳定 system prompt 和首条用户输入的稳定身份派生不含原文的哈希；有消息 ID 时纳入该 ID，无 ID 时以首条输入内容作为稳定回退。后续对话追加、工具集变化和 memory 刷新不会轮换该会话亲和 ID。
- DeepSeek 使用 provider 自动前缀缓存，不发送 `prompt_cache_key`。通用 OpenAI-compatible 网关和本地兼容服务也不默认接收该字段；只有增加了其官方文档支持及明确适配后才可启用。
- Anthropic 继续使用其 `cache_control` breakpoint；Gemini 继续使用隐式缓存和其官方 `cachedContent` 资源机制。不给这两者注入 OpenAI 路由字段。
- 精确的请求前缀仍决定可复用内容。OpenAI prompt key 按请求模型、稳定系统段和 provider 请求面派生；会话状态、memory 等易变内容不进入 key。工具定义或媒体表示改变时允许切换路由 key。

## 替代方案

- 对所有 OpenAI-compatible endpoint 发送同名 JSON 字段：协议支持不同，拒绝。
- xAI Chat 沿用 OpenAI body 字段：xAI Chat 文档定义的是 `x-grok-conv-id` 请求头，拒绝。
- 把每轮消息或完整 prompt 哈希为 xAI 会话 ID：会随追加消息或 memory 更新变化，违背会话亲和语义，拒绝。
- 将 Anthropic/Gemini 缓存强行映射到通用 key：各自原生缓存契约不同，拒绝。

## 影响与验证

OpenAI 和 xAI 的官方端点得到协议匹配的稳定路由提示；DeepSeek、Anthropic、Gemini 和未核实的兼容网关继续使用各自已支持的缓存机制，不再收到无官方依据的参数。哈希只用于进程内请求，不持久化。任何 key 都不改变 provider 的精确前缀要求，也不承诺缓存率达到固定比例。

按需求未运行构建或测试；提交前执行 `git diff --check` 检查空白错误。

## 回滚

回滚相关 adapter 和本 ADR 的变更即可；不涉及数据库、配置或用户数据迁移。

## 官方契约

- [OpenAI Prompt Caching](https://developers.openai.com/api/docs/guides/prompt-caching)
- [xAI Prompt Caching](https://docs.x.ai/developers/advanced-api-usage/prompt-caching)
- [DeepSeek KV Cache](https://api-docs.deepseek.com/guides/kv_cache/)
- [DeepSeek Responses API](https://api-docs.deepseek.com/guides/responses_api/)
- [Anthropic Prompt Caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)
- [Gemini Context Caching](https://ai.google.dev/gemini-api/docs/caching)
