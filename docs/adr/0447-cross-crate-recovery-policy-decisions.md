# ADR 0447：跨 crate 纯恢复策略决策模型

- 状态：Implemented
- 日期：2026-10-04
- 范围：`haven-common` 共享恢复策略决策；LLM、Tools、Agent、Memory、Action 的重试 owner 适配
- 关联：[ADR 0023](0023-llm-request-policy-boundary.md)、[ADR 0060](0060-tool-execution-outcomes-and-retry-policy.md)、[ADR 0268](0268-memory-outbox-retry-backoff.md)、[ADR 0334](0334-action-terminal-persistence-retry-policy.md)

## 背景

多个 crate 各自维护重试上限、指数退避、deadline、`Retry-After` 与终止结果的决策逻辑。具体生命周期必须保持分离：LLM 请求有可重试 provider error 分类；工具重放受幂等和 outcome certainty 约束；durable outbox 与 Action 终态修复在确认前不能丢工作；启动恢复还受应用取消控制。复制数学与 stop 规则会造成策略不一致，而将所有路径迁入一个通用 Job executor 会混合队列、事务、ack 和副作用 owner。

## 决定

1. `haven-common::retry` 定义纯数据与纯函数：`BackoffPolicy`、`RecoveryPolicy`、`RecoverySignal`、`RecoveryDecision` 与 `RecoveryStopReason`。`max_attempts` 包含初始尝试，`None` 表示无次数预算；deadline 使用调用方传入的 monotonic `Instant`。provider `Retry-After` 优先于本地 delay cap。
2. 业务 owner 分类失败并构造 typed signal。`PermanentFailure`、`OutcomeUnknown`、`Cancelled`、`Succeeded` 与 `Terminal` 不会被决策器重试；只有 owner 明确传入 `Retryable` 才能生成下一次尝试。每个 owner 保留其配置、错误映射及风险判断。
3. 决策器不读取时钟、不 sleep、不执行取消、不持有队列或数据库、不确认 durable marker，也不调用任何 operation。调用方继续拥有重试执行和生命周期；本 ADR 不建立统一状态机或 job executor。
4. LLM request 与首个输出前的 stream retry、Tools 的幂等工具重试、Agent 的同一工具失败预算和 completion delivery、Session status 写入、Memory fact outbox、Action inline store retry 与 terminal/quarantine repair 接入该模型。MemoryRuntime 保留恢复时序，但退避计算改用共享 `BackoffPolicy`。
5. 保持各 owner 现有的最大次数、错误分类、deadline、退避区间、`Retry-After`、取消和 durable acknowledgement 行为，包括 Action store 的 3 次/50 ms inline retry、malformed-row repair 的 1 秒起步/30 秒封顶无限 retry、Session status 的 3 次/10 ms retry，以及 Agent completion delivery 的 100 ms retry。Memory outbox 仍在成功完成前保留 marker；Action repair 仍在提交成功、terminal arbitration 或 shutdown 后停止。该切片不改变事实提取错误的分类/无限重试行为。

## 替代方案

- 只提取 exponential delay 公式：无法统一 attempt budget、deadline、typed terminal outcomes 与 provider `Retry-After`。
- 创建跨 crate retry service 或通用 Job 状态机：会把 sleep/cancel、durable acknowledgement、工具幂等性与事务边界从原 owner 移走，违反单向依赖并扩大生命周期语义，本 ADR 不采用。
- 让 common 判断哪些领域错误可重试：common 不拥有 LLM、tool 或 persistence 错误语义；由调用方映射 typed signal。

## 影响与验证

- 新增 `haven-common::retry`，无新增依赖、数据库 schema、ID、IPC、配置或用户数据变化。
- 纯策略测试覆盖尝试计数、stop precedence、deadline、指数退避、jitter cap 与 `Retry-After`。
- 各 owner 的行为测试继续固定其重试资格、取消、顺序与 durable acknowledgement 契约。
- 验收：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 回滚

各 owner 可恢复本地决策实现并移除 `haven-common::retry` 导入；无需 schema 或用户数据重置。回滚时保留 owner 原有错误映射、幂等、取消、marker/ack 与事务语义。
