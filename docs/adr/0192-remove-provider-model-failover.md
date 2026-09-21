# ADR 0192：删除 provider/model failover

## 状态

已接受（2026-09-21）

## 背景

`RequestPolicy` 曾为一个 request 保存 primary 与有序 fallback 模型，`LlmRouter`
会在传输、超时、服务端或限流失败后切换到下一个模型。这个切换通常同时改变
provider、model、wire endpoint 以及 provider prompt-cache namespace。即使请求文本
相同，备用端点也不能安全复用 primary 的缓存；对 ReAct 流式回合还会引入请求身份和
已见输出的歧义。

## 决定

- 每个 `RequestKind` 只配置一个 `primary` model；删除 `RequestPolicy.fallbacks`。
- `LlmRouter` 只在选定的 provider/model 上执行既有 retry、timeout、rate-limit pacing
  和 circuit breaker；失败直接返回该端点的错误，不切换 provider/model/cache namespace。
- 删除设置页的 fallback 编辑器以及旧角色迁移生成的 fallback 列表。
- 媒体能力投影、native STT → multimodal chat 等 capability fallback 仍属于媒体/能力
  边界，不是 provider/model failover；它们不得偷偷选择另一个 request-policy 候选。

## 影响与重置

这是 LLM 配置契约的破坏性变化。旧 `request_policies[].fallbacks` 在加载时不再参与
路由，设置保存后会被移除；如需清理配置残留，按
[`docs/release-and-reset.md`](../release-and-reset.md) 删除数据根目录并重新配置模型。
已有会话、消息和 provider cache 不需要迁移。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-common`
- `cargo test --locked -p haven-llm --lib`
- `cd ui && corepack pnpm run check`
- `cd ui && corepack pnpm run test:run`
