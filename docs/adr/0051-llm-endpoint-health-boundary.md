# ADR 0051：LLM endpoint 健康与熔断边界

## 背景

`haven-llm::router` 同时负责 endpoint 选择、请求策略、流式聚合以及 endpoint 健康
状态。熔断器、连续失败统计、半开探测和 role 到健康槽位的映射是独立的运行时策略，
继续内嵌在路由实现中会让健康状态难以单独审查和测试。

## 决定

- 新增内部 `crates/llm/src/endpoint_health.rs`，集中承载 `CircuitBreaker`、
  `EndpointHealth`、role 索引与六个 endpoint 健康槽位的初始化。
- `LlmRouter` 继续拥有健康状态的并发存储、请求前检查和成功/失败时机；通过内部
  API 调用健康模块，不改变 endpoint 选择、fallback、重试、限流冷却或流式逻辑。
- 保持连续失败阈值、30 秒冷却、HalfOpen 探测、并发旧请求成功不提前关闭熔断器等
  既有语义；不改变公开 API、provider 协议、配置格式或持久化行为。

## 替代方案

- 继续把熔断器和健康状态留在 `router.rs`：会延续路由与健康策略混合，拒绝。
- 引入第三方熔断 crate：会改变时间/状态语义并增加依赖，拒绝。
- 把整个请求 permit、限流和 fallback 一起下沉：边界过大且会混合不同策略，暂不采用。

## 影响

这是 `haven-llm` 内部运行时边界拆分。`LlmRouter` 对外行为与六角色槽位顺序保持
不变，不需要配置、数据库或缓存重置。

## 验证

```text
cargo fmt --all -- --check
cargo check --locked -p haven-llm
cargo test --locked -p haven-llm
cargo clippy --locked -p haven-llm --lib -- -D warnings
```

既有 router 测试继续覆盖熔断状态转移、健康恢复、role 映射和请求路径。

## 回滚与重置

代码回滚时删除 `endpoint_health.rs` 与模块登记，恢复 `router.rs` 中的健康/熔断
实现；本次不改变配置格式、provider 协议或持久化数据，不需要用户重置。
