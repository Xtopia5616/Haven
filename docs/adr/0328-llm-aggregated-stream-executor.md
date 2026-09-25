# ADR 0328：LLM 聚合流执行器

- 状态：已采纳（2026-09-25）
- 范围：`haven-llm` 的 `LlmRouter::chat_stream_with_tools_aggregated*` 内部执行边界
- 关联：[ADR 0241](0241-aggregated-stream-request-object.md)、[ADR 0246](0246-llm-request-outcome-projection.md)、[ADR 0318](0318-llm-call-executor.md)、[ADR 0319](0319-llm-request-capability-semantics.md)、[ADR 0327](0327-llm-raw-stream-executor.md)

## 背景

raw `chat_stream` 已由 ADR 0327 的 `StreamExecutor` 承接建流重试、总 timeout 与 permit 包装。aggregated streaming 的逻辑请求仍跨越 Router 和 `streaming.rs`：Router 负责 request policy、校验、总 timeout、最终 health/cooldown 投影及 StreamRule guidance retry；`streaming.rs` 则包含共享 `StreamContext`、首次 `on_chunk` 交付前的重试和单条 provider stream 的消费/聚合。

这使同一逻辑请求的 attempt 状态机有两个 owner。此切片把多 attempt 协调收敛到 crate-private aggregated executor，同时保留 Router 的路由与运行状态 owner，并继续复用现有 `streaming.rs`、`request_pipeline.rs` 和 Router outcome projector。

## 决定与所有权

1. 新增 crate-private `AggregatedStreamExecutor`。Router 传入已选 client、单份 `RequestPolicy`、当前 stream rules 借用和初始 idle timeout；executor 不解析 route、不读取配置、不申请 permit，也不存储 health/circuit/cooldown/semaphore 状态。
2. `AggregatedStreamExecutor` 拥有 `StreamContext`、active attempt hooks、首次 `on_chunk` 交付前的现有 retry loop、StreamRule abort 后的 guidance retry 和聚合请求总 timeout。它复用 `retry_delay`、`RetryPolicy`、`execute_with_timeout` 及 `streaming.rs` 单条 provider stream 消费/聚合逻辑，不建立第二套重试、timeout、usage 或 stream aggregation 实现。
3. Router 保留 route/client 选择、唯一配置真源与策略 snapshot、模型 permit/cooldown/circuit、stream rules 和 health/rate-limit 状态。permit 仍由 `with_request_permit` 覆盖完整聚合流执行。Router 通过闭包提供最终结果投影；`Cancelled` 继续跳过健康失败记录。
4. 首次 request policy 与 idle timeout 仍在 attempt hook 初始通知后从同一 Router config snapshot 读取。guidance retry 仍在调用 attempt-start hook 后重新读取当前 `stream_idle_timeout_secs`；executor 只通过 Router 提供的闭包读取该值，不持有配置。
5. raw `chat_stream` 生命周期仍归 ADR 0327 的 `StreamExecutor`；本 ADR 不迁移 raw stream、不改 public Router API、`StreamRequest` DTO 或 provider adapter。

## 必须保持的不变量

- retry 只使用 Router 为该请求捕获的重试预算，且只重试所选 model/client；首次 `on_chunk` 回调已经交付 chunk 后不重放 provider attempt。
- provider 消费阶段和 guidance retry 使用原始 `Arc` 消息/工具快照；每次 stream 调用仍传递相同 message/tool 内容、顺序和 `max_output_tokens`。
- 单条 stream 的 chunk 顺序、回调时序、文本/tool/reasoning/web-search/thinking 累积、usage 与 finish reason 仍由 `streaming.rs` 生成。
- guidance 文本仍作为尾部 User 内容传给既有 shared guidance adapter boundary；StreamRule、警告和错误文本不变。
- content validation 仍发生在聚合总 timeout 前，validation error 不投影 health；总 timeout 名称与文本仍为 `router streaming total timeout after {timeout_secs}s`。
- 取消仍覆盖 permit/cooldown 等待、provider 建流、重试 backoff 与 stream 消费；取消优先级不变，`Cancelled` 不记健康失败或 rate-limit cooldown。
- 最终成功/失败和 429 cooldown 仍只投影一次；outcome projection 仍位于原总 timeout 区间内。
- 初始 stream policy/idle snapshot 和 guidance retry 的 idle 配置刷新时点不变；permit 仍保持到聚合执行结束。
- 不改变 provider wire、错误文本、output cap、重试次数、usage 数据、公共 API、配置、数据库、IPC 或持久数据。

## 替代方案

- 将路由、配置、permit 和 health 状态一起移进 executor：会建立第二个运行态 owner，拒绝。
- 只把 `StreamContext` 或 retry helper 搬到新文件，保留 Router 中的 guidance retry/timeout/outcome 编排：逻辑请求状态机仍分散，不能形成明确执行边界，拒绝。
- 复用 raw `StreamExecutor` 承担 aggregated stream：两者的消费、取消和 permit 生命周期不同；raw executor 只重试建流，拒绝。

## 影响与验证

本切片只增加 crate-private 执行模块、行为测试和架构文档；无 public API、provider wire、配置、schema、IPC 或用户数据变化。保留普通/工具聚合流、共享消息工具快照、首次 `on_chunk` 交付前 retry、取消、usage 累积和 StreamRule guidance 测试，并补足 executor timeout/outcome 边界与 Router 聚合 permit 生命周期验证。

验收命令：

```sh
cargo fmt --all -- --check
cargo test --locked -p haven-llm
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
git diff --cached --check
```

## 后续工作

阶段 6 的普通调用、raw stream 与 aggregated stream 执行器切片已完成。`RequestDescriptor` 仍未全贯穿到 `CompleteRequest`、`PromptRequest`、`StreamRequest`、embedding/health-check、metadata/config helpers 和仓库其他 `RequestKind` 调用点；该语义迁移需单独评估并保留配置 route key 与 `LlmCallKind` usage owner。

## 回滚

回滚本提交并恢复 Router 中的共享 `StreamContext`、首输出前重试、guidance retry、总 timeout 和 outcome closure 调用；`streaming.rs` 恢复原 retry helper。无配置、数据库、IPC、provider wire 或用户数据迁移。
