# ADR 0673：区分工具与 endpoint 熔断器 owner

## 背景

Tools 和 LLM 都有 `CircuitState`，枚举值都是 Closed、Open、HalfOpen，但其状态属于不同 owner。Tools 的 `ToolCircuitBreaker` 为每个工具隔离失败计数，阈值与冷却时间可配置；LLM 熔断器属于模型 endpoint 健康记录，另有 endpoint 健康统计、对熔断打开前已准入请求的过期完成过滤，以及手动重试重置。两边目前也使用不同阈值和策略。

相同状态词描述的是常见熔断器阶段，不足以说明实例归属、计数责任或状态转换策略。将两套 owner 合成一个类型会掩盖这些差异，且没有可证明等价的共享转换契约。

## 决定

- Tools 内部 `CircuitState` 改为 `ToolCircuitState`；公开类型继续使用 `ToolCircuitBreaker`。
- LLM endpoint health 内部 `CircuitState` 改为 `EndpointCircuitState`，`CircuitBreaker` 改为 `EndpointCircuitBreaker`。
- 保持工具执行与模型 endpoint 健康的熔断策略及计数各自归属，不抽取共同状态 owner。
- 不保留旧符号 alias。此变更仅重命名 Rust 符号，不改变阈值、冷却、健康度统计、运行行为或持久化 / IPC 契约。

## 考虑过的方案

- 保留泛名：调用点无法直接辨别熔断状态归属。
- 建立共享的 circuit state/breaker：两者状态转换与计数职责未形成同一契约，当前抽象会丢失 endpoint 的过期结果过滤和健康统计边界。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --locked -p haven-tools -p haven-llm`
- ADR 索引检查与 `git diff --check`
- 未运行测试；本轮只执行格式与编译门禁。

## 回滚与重置

同步恢复两个模块内的 Rust 符号名即可回滚。没有配置、持久化或 wire shape 变化，无需重置。
