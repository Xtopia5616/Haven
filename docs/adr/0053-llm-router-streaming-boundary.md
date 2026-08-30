# ADR 0053：LLM router 流式聚合边界

## 背景

`haven-llm::router` 同时负责 endpoint 选择、健康状态编排、并发 permit、流式
重试和 chunk 聚合。流式上下文估算、首 chunk/空闲超时、规则门禁、回调解耦和
响应聚合是独立的请求执行策略，继续内嵌在 router 中会让主路由文件保持热点，
也难以单独回归流式语义。

## 决定

- 新增内部 `crates/llm/src/streaming.rs`，集中承载流式上下文、prompt 大小估算、
  动态 idle timeout、stream rule 检查、chunk 回调消费、响应聚合和“首 chunk 前”
  重试。
- `router.rs` 继续负责 endpoint 选择、健康/熔断记录、并发 permit、主备切换和
  总超时；通过内部函数调用流式模块，不改变公开 API。
- 保持首 chunk grace、长上下文 idle scaling、规则 abort/warn、已产生输出后不
  重放以及 callback 顺序等既有语义。

## 替代方案

- 继续把流式执行细节留在 `router.rs`：会延续路由与流式策略混合，拒绝。
- 把 endpoint 选择、健康状态和 fallback 一起下沉：边界过大，且会改变现有
  router 的并发与故障记录责任，拒绝。

## 影响

这是 LLM 内部模块重组；router 主文件减少约 357 行，provider wire、流式响应、
重试/failover、IPC 和持久化契约不变。

## 验证

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --locked -p haven-llm -- --test-threads=1
```

重点回归首 chunk grace、idle scaling、规则 abort、chunk 转发、流式 usage 和
主备切换测试。

## 回滚

将 `streaming.rs` 中的内部函数重新并回 `router.rs`，恢复原调用位置；不需要
数据库、配置或用户数据迁移。
