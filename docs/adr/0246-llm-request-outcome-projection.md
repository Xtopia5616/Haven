# ADR 0246：LLM Router 请求结果投影收口

- 状态：已采纳（2026-09-24）
- 范围：`haven-llm` Router 对逻辑请求结果的模型健康与 rate-limit 状态投影
- 关联：[ADR 0234](0234-llm-complete-request-object.md)、[ADR 0241](0241-aggregated-stream-request-object.md)、[ADR 0242](0242-embedding-and-health-request-objects.md)

## 背景

Router 的 native transcription、普通/工具 complete、embedding、raw stream 建流和 health check 在收到同一类 `Result<T, LlmError>` 后，重复执行相同的成功计数、失败计数和 429 cooldown 投影。该重复代码容易让新请求路径漏掉其中一项。聚合 stream 对 `Cancelled` 有意不记录健康失败，其生命周期例外需要继续显式表达。

## 决定

1. 在 `LlmRouter` 内增加私有泛型 `record_request_outcome`，统一把请求结果投影到模型健康状态和 rate-limit cooldown。
2. 上述五条请求路径在原有位置调用该方法；请求执行顺序、外层 timeout/permit、结果返回和 `with_model_permit` 的 rate-limit 处理保持原样。
3. 聚合 stream 保留显式 outcome 分支，使 `Cancelled` 继续跳过健康失败记录；其余 success/failure 行为不变。
4. 不改变请求 DTO、RequestKind、重试/超时策略、provider adapter、wire mapping、usage 记录或 IPC 契约。

## 替代方案

- 为每种请求路径保留重复的 match：无法保证后续路径完整应用相同的状态投影，拒绝。
- 把 outcome 记录移到 provider adapter 或 `with_model_permit`：会扩大职责或改变 raw stream 的建流/消费生命周期，不纳入本切片。
- 改写聚合 stream 的取消处理：取消并非 provider 健康失败，且不属于本切片目标，拒绝。

## 影响与验证

变化仅限 Router 内部私有实现；provider 请求参数、发送时点、重试、超时、模型选择、取消、health/circuit 与 cooldown 语义保持不变。新增单测覆盖普通失败、成功恢复连续失败计数，以及 429 的 retry-after cooldown；运行 `cargo fmt --all -- --check`、`cargo test --locked -p haven-llm` 和 `cargo clippy --locked -p haven-llm -- -D warnings`。

## 回滚

恢复各请求路径原有的局部 match 并删除 `record_request_outcome`、对应测试与本文索引/路线图记录即可。无数据库、配置、provider wire 或用户数据迁移。
