# ADR 0028：LLM 适配器 provider feature 边界

## 背景

DeepSeek、Kimi/Moonshot、MiMo 和 OpenRouter 的识别，以及 thinking/reasoning
参数映射、reasoning echo 和长度限制，会被多个 OpenAI-compatible adapter
共享。此前这些决策函数与 adapter 注册、embedding、web search 等实现混在
`adapters/mod.rs`，provider feature 规则难以单独审查。

## 决定

- 新增内部 `adapters/provider_features.rs`，集中拥有 vendor haystack 检测、
  thinking/reasoning 参数决策、Anthropic thinking block 重建和 reasoning tail
  限制。
- OpenAI Chat 与 Responses adapter 通过 crate 内 re-export 使用同一份 feature
  决策；`transport.rs` 只复用 OpenRouter 判断，不反向拥有 provider 参数。
- 保持 DeepSeek effort 映射、Kimi 版本分支、disabled 行为、reasoning echo
  provider 集合和字符级 tail 截断不变；不改变请求 schema、路由、重试或用量。

## 替代方案

- 每个 adapter 自己判断 vendor 和 thinking 参数：会造成 Chat/Responses 语义
  分叉，拒绝。
- 让 router 负责 provider wire feature：会把 provider 协议细节泄漏到路由层，拒绝。
- 将 feature 规则提升为跨 crate trait：当前只服务 `haven-llm` 内部，trait 会
  扩大公共 API 和测试替身面，暂不采用。

## 影响

这是 `haven-llm` 内部模块重组。对外 `LlmClient`、provider 请求/响应、错误类型、
配置格式和持久化用量不变，不需要配置、数据库或缓存重置。

## 验证

```text
cargo fmt --all -- --check
cargo check --locked -p haven-llm
cargo test --locked -p haven-llm --lib -- --test-threads=1
cargo clippy --locked -p haven-llm --lib -- -D warnings
```

重点回归 vendor 识别、DeepSeek/Kimi reasoning 映射、echo 判定、Anthropic
thinking block 重建和多字节 reasoning tail。

## 回滚与重置

代码回滚时删除 `adapters/provider_features.rs` 和模块登记，把其函数与测试
恢复到 `adapters/mod.rs`；本次不改变 schema、序列化、配置或持久化数据，不需要
用户重置。
