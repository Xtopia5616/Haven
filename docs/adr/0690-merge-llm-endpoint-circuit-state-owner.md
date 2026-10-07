# ADR 0690：合并 LLM endpoint 熔断状态 owner

## 状态

已采纳并实施。

## 背景

`EndpointHealth` 包含一个 `EndpointCircuitBreaker`，并另存 `consecutive_failures`、
`last_failure_time` 和 `is_healthy`。全仓消费者检查显示，这些外层字段只有 wrapper 与
单元测试使用；生产 `LlmRouter` 只依赖 breaker 的请求准入、成功/失败记录和手动重试。
外层 streak 还会累计 breaker 已打开后的迟到失败，但没有报告或其它生产消费者读取该值。
因此 wrapper 形成未使用的第二份状态，ADR 0673 中“endpoint health statistics”的描述
也超过当前实际契约。

## 决定

- 删除 `EndpointHealth` wrapper，将 `EndpointCircuitBreaker` 作为 endpoint 状态的唯一 owner。
- endpoint map 改为 `EndpointCircuitBreakerMap`；Router 字段改为 `endpoint_circuits`，实现文件改为 `endpoint_circuit_breaker.rs`。
- 删除只覆盖未消费 `is_healthy` 镜像状态的测试；请求结果、Open/HalfOpen 转换、过期成功过滤和手动重试由 breaker 直接覆盖。
- 保持 Tools `ToolCircuitBreaker` 独立。它与 LLM endpoint breaker 仍有不同配置和状态作用域，见 ADR 0673。

## 替代方案

- 只删除 `last_failure_time`，保留外层计数和 `is_healthy`：拒绝。它们没有生产消费者，并重复由 breaker 状态表达的连续失败门槛。
- 合并 Tools 与 LLM 的 breaker：拒绝。两者分别按工具和模型 endpoint 隔离，阈值配置与状态用途不同。

## 影响与验证

- 变化限于 `haven-llm` 进程内状态结构和测试；endpoint 准入、三次失败门槛、30 秒冷却、HalfOpen 探测、迟到完成过滤和手动 retry 行为不变。
- 不改变 provider、配置、数据库、Tauri/IPC 或用户可见字段；无需数据重置，也不保留内部旧名 alias。
- 验证：`cargo fmt --all -- --check`、`cargo test --locked -p haven-llm`、`cargo clippy --locked -p haven-llm -- -D warnings`、ADR 索引检查和 `git diff --check`。

## 回滚

若将来新增正式 endpoint 健康指标，应先定义消费者与统计口径，再把该 metric 作为明确命名的独立 projection 加入；不恢复无消费者的 `EndpointHealth` 包装。
