# ADR 0327：LLM Router raw stream 执行器第一刀

- 状态：已采纳（2026-09-25）
- 范围：`haven-llm` raw `LlmRouter::chat_stream` 的 provider 建流执行边界
- 关联：[ADR 0241](0241-aggregated-stream-request-object.md)、[ADR 0246](0246-llm-request-outcome-projection.md)、[ADR 0316](0316-llm-model-directory.md)、[ADR 0318](0318-llm-call-executor.md)、[ADR 0319](0319-llm-request-capability-semantics.md)

## 背景

ADR 0318 将普通/工具 complete 和非空 embedding 的一次性调用边界迁入 `CallExecutor`，但 raw `chat_stream` 的建流调用仍与 Router 路由、permit 和健康状态逻辑放在一起。raw stream 具有独立生命周期：重试只覆盖 provider stream 建立；成功返回后，caller 消费数据，后续传输错误不能重放已消费 delta；每模型 permit 必须随返回的 stream 保留至 stream 对象 drop。

本 ADR 是 raw `StreamExecutor` 的第一步，仅收敛该建流边界。带工具的 aggregated streaming 继续由 Router 与 `streaming.rs` 编排 retry-before-output、guidance retry、idle timeout、cancellation 和 chunk/attempt callbacks。

## 决定与所有权

1. 新增 crate-private `StreamExecutor`，接收 Router 已解析的 `model_id`、`Arc<dyn LlmClient>`、单一 `RequestPolicy` 与 Router 已申请的 `OwnedSemaphorePermit`。执行器不选择 route、不读取 config，也不保存 health、circuit、rate-limit 或 semaphore 状态。
2. `StreamExecutor::chat_stream` 接管 raw provider 建流的 `validate_content`、现有 `execute_with_retry` 和 `router stream` 总 timeout；它只重试 stream 建立，不消费返回的 stream。
3. 执行器把最终建流 `Result` 转成现有 `RequestOutcome`，通过 crate-private closure 投影回 Router 的同一 health/rate-limit owner。validate 失败及 total timeout 继续不投影；provider 建流失败按原时机投影一次。
4. 成功时执行器用 `PermitStream` 包装 provider stream，并把 permit 放入包装器；permit 保持到返回的 stream drop。建流失败、timeout 或 future 被 drop 时，permit 随执行器 future 释放。
5. Router 继续拥有原 public API、RequestKind route/client 解析、permit 获取、cooldown 等待、circuit check、唯一 config snapshot 与 `RequestPolicy` 构造，以及 health/rate-limit 投影实现。
6. 不创建第二套 retry 或 usage 入口；复用 `request_pipeline` 和 Router 投影闭包。聚合 `chat_stream_with_tools_aggregated...` 路径、其 cancellation/guidance/callback 编排和既有测试不迁移。

## 必须保持的不变量

- raw `chat_stream` 的公开签名与返回类型、provider wire、RequestKind 原 route key、已选 model/client identity 和消息 clone 行为不变。
- permit acquisition、cooldown 等待和 circuit check 在策略 snapshot 与 provider 建流之前，且不受 `router stream` total timeout 包围。
- timeout 文本仍为 `router stream total timeout after {timeout_secs}s`；provider error 文本原样返回；validate error 与 outer timeout 不产生 provider outcome 投影。
- retry 仍只发生于 stream 建立边界；stream 成功返回后，caller 消费阶段的传输错误不触发重试。
- provider 建流的最终结果只投影一次；429 cooldown 仍由 Router 的同一结果投影路径更新。
- 成功 stream 在已读到 EOF 后，只要 stream 对象仍存在，permit 仍由包装器持有，直到对象 drop；建流失败释放 permit。
- aggregated stream 的 retry-before-output、guidance、cancellation 优先级、idle/total timeout、hooks 与 health 例外保持原实现。

## 替代方案

- 同时把 aggregated streaming 编排迁入 executor：其 cancel、guidance retry、chunk callbacks 和 partial-output replacement 构成另一条生命周期，扩大切片，延期。
- 让 executor 持有 health/cooldown/semaphore/config：会形成第二个 Router 状态 owner，拒绝。
- 新建 retry、usage 或公共 trait：会重复权威入口或扩大 crate 公共面，拒绝。通过方法级 crate-private closure 投影 outcome。

## 影响与验证

仅增加 crate-private 执行模块和行为测试，并更新架构说明；无 public API、配置、数据库、IPC、provider wire 或用户数据变化。测试覆盖 raw stream permit 生命周期、retry 和消息 clone、timeout 文本与投影、provider 建流错误投影、建流失败释放 permit，以及 Chat/FastChat 的 RequestKind route 选择。现有 aggregated streaming 测试保持在原路径。

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

这只是 raw `StreamExecutor` 的第一刀。aggregated stream executor/capability descriptor 全贯穿均仍待后续独立切片；不应把当前决策视为 aggregated streaming execution ownership 已完成。

## 回滚

回滚该单一提交，将 `PermitStream` 与 raw provider 建流校验/retry/timeout/outcome 调用放回 Router 即可。无配置、数据库、IPC、provider wire 或用户数据迁移；aggregated streaming 路径不受影响。
