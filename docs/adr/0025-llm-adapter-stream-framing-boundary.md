# ADR 0025：LLM 适配器流式 framing 边界

## 背景

provider adapter 都需要把响应 body 按行切分，并兼容 SSE `data:`、原始 JSON
行、`[DONE]`、注释行和 EOF 残留数据。此前这段 framing 逻辑与 provider 分发、
wire 映射和 embedding helper 一起位于 `adapters/mod.rs`，共享行为难以单独
测试，适配器也容易各自复制一套读取逻辑。

## 决定

- 新增内部 `adapters/stream.rs`，唯一拥有通用 `StreamChunk` 空基线、`LineMode`
  和 `spawn_line_reader`。
- `SseDataOnly` 只转发 `data:` payload，忽略 SSE event/comment；`SseOrRaw`
  继续兼容 `data:` 与非标准 JSON lines。provider 仍负责解释 JSON payload 和
  维护各自的流式状态机。
- `adapters/mod.rs` 通过 crate 内 re-export 保持现有 adapter 调用入口；本次不
  改变 provider wire 载荷、chunk 顺序、EOF flush 或错误语义。
- HTTP client、认证、状态错误和 header timeout 仍由 `transport.rs` 负责；
  stream 模块只处理响应 framing，不拥有 HTTP 请求、重试或 endpoint 路由。

## 替代方案

- 每个 provider 继续内联行读取：会重复 SSE/NDJSON 边界处理并造成语义漂移，拒绝。
- 让 stream 模块解析 provider JSON：会把 provider 语义错误地提升到共享层，拒绝。
- 改成跨 crate streaming trait：当前边界只服务 `haven-llm` 内部，trait 会扩大
  API 与测试替身面，暂不采用。

## 影响

这是 `haven-llm` 内部模块重组。对外 `LlmClient`、provider adapter、stream
payload、错误类型和配置格式不变，不需要配置、数据库或缓存重置。

## 验证

```text
cargo fmt --all -- --check
cargo check --locked -p haven-llm
cargo test --locked -p haven-llm --lib -- --test-threads=1
cargo clippy --locked -p haven-llm --lib -- -D warnings
```

重点回归 SSE event/comment 过滤、两种 framing 模式、`[DONE]` 忽略和 EOF
残留 payload flush。

## 回滚与重置

代码回滚时删除 `adapters/stream.rs` 和模块登记，把其函数与单元测试恢复到
`adapters/mod.rs`；本次不改变 schema、序列化、配置或持久化数据，不需要用户
重置。
