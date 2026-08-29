# ADR 0023：LLM 请求策略共享边界

## 背景

`haven-llm::router` 的普通聊天、工具聊天、embedding 和流式请求都需要
重试与总超时，但各路径分别展开 `RouterConfig` 字段，流式路径还维护一份
独立的 `StreamRetryPolicy`。这种重复会让新策略只覆盖部分请求类型，或让
超时/重试错误语义发生漂移。

## 决定

- 新增内部 `request_pipeline.rs`，由 `RequestPolicy`/`RetryPolicy` 快照主端点
  与 balanced fallback 的重试预算，并由统一执行函数承载重试和总超时语义。
- `LlmRouter` 继续拥有端点选择、熔断、限流冷却、fallback 和流式聚合；它只
  读取一次配置快照并把 provider 调用交给共享策略执行器。
- 普通聊天、工具聊天、embedding 与流式端点尝试必须使用这套策略；provider
  adapter 只负责协议映射，不读取 RouterConfig 或实现另一套重试。
- 取消 token 继续由调用方传入共享重试执行器；embedding 不启用 balanced
  fallback，因为 fallback 端点是聊天能力，不能产生向量。

## 替代方案

- 继续在 `router.rs` 的每个方法中解包配置：会保留策略漂移风险，拒绝。
- 把端点选择、熔断和 fallback 一起下沉到策略模块：会让共享管线拥有路由
  业务状态，扩大边界，拒绝。
- 让各 provider adapter 自己重试：会造成不同线协议的退避和取消语义不一致，
  也会与 router 的 fallback 重复，拒绝。

## 影响

这是 `haven-llm` 内部边界拆分。对外 `LlmRouter`、`LlmClient`、provider wire
载荷、使用量字段和数据库用量投影均不变，不需要配置、数据库或缓存重置。
请求在开始时捕获策略；运行中的请求不会因设置热更新而改变重试预算。

## 验证

```text
cargo fmt --all -- --check
cargo check --locked -p haven-llm
cargo test --locked -p haven-llm --lib
cargo clippy --locked -p haven-llm --lib -- -D warnings
```

新增测试验证 primary/fallback 的重试预算快照和统一超时错误命名；既有
router 测试继续覆盖聊天、embedding、流式重试、取消、fallback 和熔断行为。

## 回滚与重置

代码回滚时删除 `request_pipeline.rs` 与其模块登记，恢复 `router.rs` 中的
`StreamRetryPolicy`、配置解包、`with_retry` 和总超时实现即可。由于不修改
schema、序列化、配置格式或持久化数据，不需要用户重置。
