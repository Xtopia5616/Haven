# ADR 0450：移除过时的向量流式 API

- 状态：Implemented
- 日期：2026-10-04
- 范围：`LlmClient` 未使用的 vector-capped streaming methods 与 LLM 测试 helper 暴露
- 关联：ADR 0098、0241、0246

## 背景

`LlmClient` 保留了 `chat_stream_output_cap` 和 `chat_stream_with_tools_output_cap` 两个以 `Vec` 传入的 capped stream 方法，且四个 provider adapter 分别实现了它们。全仓库没有方法调用点；当前原始 stream owner 调用 `chat_stream`，retryable aggregated stream owner 只调用 shared request boundary（包括独立 guidance 入口）。旧 vector 方法曾是共享请求切片前的实现；共享默认现已 fail-closed，不再委托旧方法，因此这些实现既不承担 fallback，也不能服务当前 retry 路径。

`LlmRouter::set_request_limit_for_test` 与 `rate_limit_deadline_for_test` 只有 router 单测调用，但作为未加 `#[cfg(test)]` 的隐藏 public 方法进入生产 API。

## 决定

1. 从 `LlmClient` 删除两个未使用的 vector-capped streaming methods，并从 Anthropic、Gemini、OpenAI Chat、OpenAI Responses adapters 及无调用的测试 mock 中删除对应实现。
2. 保留 `chat_stream` 原始流入口和 `chat_stream_with_tools_output_cap_shared` / `_shared_guidance` retry boundary。provider 请求、output cap、取消、首 chunk 与 stream-rule retry 行为不变。
3. 将两个 `LlmRouter` test-only helper 限定在 `#[cfg(test)]`；测试覆盖仍然保留。
4. 不保留 source-compatibility wrappers。仓库无调用方，Haven 当前没有稳定的外部 Rust API 承诺；外部实现若存在需迁移到当前 stream boundary。

## 替代方案

- 将 shared boundary 默认实现回退到旧 vector methods：每次 retry 都重新物化完整请求，违反不可变共享快照与 fail-closed 的 retry allocation 边界，拒绝。
- 保留未使用的公开方法以保护未知下游：会保留第二套请求入口及所有适配器重复实现；项目测试版不承诺该 API 稳定性，拒绝。
- 只删除 adapter overrides 而保留 trait methods：默认仍暴露无仓库消费者的 public surface，并在调用时仅返回 unsupported，拒绝。

## 影响与验证

- 这是 `haven-llm` Rust source API 收窄；不改变 provider wire、数据库、事件、IPC、配置或用户数据，无需重置。
- 全仓搜索确认旧 capped vector 方法无调用；测试 mock overrides 也没有被方法调用。
- 运行 `cargo test --locked -p haven-llm`、Agent workspace tests 与严格 Clippy；全 workspace test/check 进一步验证其它 crate 的 LlmClient implementors。
- 共享 retry、取消、provider output cap、`Retry-After` 和 fail-closed 默认行为由现有测试覆盖。

## 回滚

如有真实外部 consumer，可恢复对应 trait method 与 adapter mapping，但必须保持共享 retry boundary 的 allocation 与 fail-closed 语义；无需数据重置。
