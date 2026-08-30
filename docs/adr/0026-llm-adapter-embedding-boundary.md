# ADR 0026：LLM 适配器 embedding 边界

## 背景

OpenAI Chat、OpenAI Responses 及其兼容 provider 共用 embedding 请求协议，
包括 `/embeddings` URL 规则、请求体、按 `index` 排序、数量校验和 usage 映射。
这些逻辑此前与适配器注册、web search 和厂商扩展一起放在
`adapters/mod.rs`，embedding 契约难以独立演进和测试。

## 决定

- 新增内部 `adapters/embedding.rs`，唯一拥有 OpenAI-compatible embedding 的
  请求体、URL 拼接、响应解析、数量校验和 usage 转换。
- provider adapter 继续决定 endpoint、`ensure_v1` 规则、认证 header 和模型名，
  通过 crate 内 re-export 调用共享 embedding 入口。
- `embedding` 直接复用 `transport::send_request`，不实现第二套 HTTP 状态错误、
  重试或总超时；请求级 timeout 仍使用既有 endpoint 配置。
- 保持空输入短路、向量顺序、模型回退、usage 字段和错误分类不变；Gemini、
  Anthropic 等非 OpenAI-compatible provider 不进入此模块。

## 替代方案

- 各 OpenAI-compatible adapter 保留一份 embedding 实现：会造成 URL、排序和
  usage 语义漂移，拒绝。
- 让 router 负责 provider wire embedding：会扩大路由层对协议细节的所有权，拒绝。
- 引入跨 crate embedding trait：当前调用方只在 `haven-llm` 内部，trait 会扩大
  公共 API 与测试替身面，暂不采用。

## 影响

这是 `haven-llm` 内部模块重组。对外 `LlmClient`、embedding 请求/响应、错误类型、
配置格式和持久化用量不变，不需要配置、数据库或缓存重置。

## 验证

```text
cargo fmt --all -- --check
cargo check --locked -p haven-llm
cargo test --locked -p haven-llm --lib -- --test-threads=1
cargo clippy --locked -p haven-llm --lib -- -D warnings
```

重点回归 URL 兼容规则、按 index 排序、数量不匹配、usage 映射、空输入短路和
OpenAI Responses 适配器的 HTTP 集成测试。

## 回滚与重置

代码回滚时删除 `adapters/embedding.rs` 和模块登记，把其函数与测试恢复到
`adapters/mod.rs`；本次不改变 schema、序列化、配置或持久化数据，不需要用户
重置。
