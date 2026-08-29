# ADR 0024：LLM 适配器传输边界

## 背景

各 LLM adapter 都需要创建带代理的 HTTP client、生成认证头、限制流式响应头
等待、转换非 2xx 响应和执行健康检查。这些逻辑集中在
`adapters/mod.rs`，使 provider 协议分发、传输策略和 wire 映射混在同一热点
文件中；修改共享安全/超时语义时也容易只覆盖部分 adapter。

## 决定

- 新增内部 `adapters/transport.rs`，唯一拥有通用 reqwest client、认证/归因
  header、stream header timeout、HTTP 状态错误转换和模型健康检查。
- `adapters/mod.rs` 只保留适配器注册、线协议能力、provider 扩展和流式通用
  数据处理；各 provider 仍通过 crate 内 re-export 使用稳定的传输入口。
- `transport` 不解析 provider payload、不选择 endpoint、不实现重试；请求级
  重试/总超时继续由 `request_pipeline.rs` 和 router 负责。
- 保持现有 header、Retry-After、错误分类、流式 header 等行为和 provider wire
  载荷不变。

## 替代方案

- 让每个 provider 保留自己的 HTTP helper：会造成认证、错误和超时语义漂移，
  拒绝。
- 把整个 `adapters/mod.rs` 下沉为 transport：会让传输层反向拥有 provider
  分发和协议扩展，边界过宽，拒绝。
- 引入跨 crate HTTP trait：当前共享对象仅在 `haven-llm` 内，trait 会扩大
  API 和测试替身面，暂不采用。

## 影响

这是 `haven-llm` 内部模块重组。对外 `LlmClient`、adapter 选择、请求 payload、
错误类型、配置格式和持久化用量不变，不需要配置、数据库或缓存重置。

## 验证

```text
cargo fmt --all -- --check
cargo check --locked -p haven-llm
cargo test --locked -p haven-llm --lib -- --test-threads=1
cargo clippy --locked -p haven-llm --lib -- -D warnings
```

重点回归各 provider 的认证头、健康检查、非 2xx 错误、流式 header 超时和既有
adapter wire 测试。

## 回滚与重置

代码回滚时删除 `adapters/transport.rs` 和模块登记，把其函数恢复到
`adapters/mod.rs`；本次不改变 schema、序列化、配置或持久化数据，不需要用户
重置。
