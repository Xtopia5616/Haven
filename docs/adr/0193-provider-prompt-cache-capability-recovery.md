# ADR 0193：Provider prompt cache 能力恢复与 Gemini 显式缓存

## 背景

OpenAI-compatible Chat / Responses endpoint 可能拒绝可选的
`prompt_cache_key`。原实现把拒绝结果永久写入 adapter 状态，导致网关之后恢复能力
时仍然失去稳定路由提示。Gemini 的 `generateContent` 没有等价的 routing key；其
显式缓存通过 `cachedContents` 创建资源，再在生成请求中使用 `cachedContent`。

## 决定

1. OpenAI Chat 和 Responses 的 `prompt_cache_key` 拒绝只进入 5 分钟负缓存窗口；
   窗口到期后下一次请求重新探测，成功立即恢复 key。拒绝请求仍只重试一次且不向
   调用方暴露兼容性错误。
2. Gemini 为完整的 `systemInstruction` 与当前工具 projection 计算进程内 fingerprint，
   创建一个带 1 小时 TTL 的 `cachedContents` 资源并复用其 name。使用缓存时从
   `generateContent` 请求移除 `systemInstruction` 和 `tools`，因为 Gemini 不允许
   与 `cachedContent` 同时设置它们；动态 system 内容因此包含在 fingerprint 中，
   保持语义不变。
3. Gemini 缓存创建失败、缓存过期或 provider 拒绝 `cachedContent` 时，恢复原来的
   system split/direct 请求；同一 fingerprint 进入 5 分钟负缓存窗口。缓存状态只保留
   一个 active entry，不写入会话、配置或数据库。

## 影响与验证

缓存资源是 provider 侧资源，Haven 通过 TTL 限制旧资源的最长留存，并只在 adapter
内保留一个 active entry；请求取消仍由现有 reqwest/tokio 请求边界处理。cache key、prompt 与 provider
原始响应不进入持久化诊断。

官方依据：

- [Google Gemini caching API](https://ai.google.dev/api/caching)
- [Google Gemini generateContent API](https://ai.google.dev/api/generate-content)

验证命令：

```text
cargo check --locked -p haven-llm
cargo test --locked -p haven-llm --lib
cargo clippy --locked -p haven-llm --lib -- -D warnings
```

## 回滚

回退本 ADR 对应代码提交即可；不修改数据库、配置 schema 或已有会话数据。
