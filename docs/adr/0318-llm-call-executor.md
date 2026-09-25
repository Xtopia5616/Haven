# ADR 0318：LLM Router 一次性调用执行器

- 状态：已采纳（2026-09-25）
- 范围：haven-llm 内部 ordinary/tools complete 与非空 embedding 的执行边界
- 关联：[ADR 0234](0234-llm-complete-request-object.md)、[ADR 0241](0241-aggregated-stream-request-object.md)、[ADR 0242](0242-embedding-and-health-request-objects.md)、[ADR 0246](0246-llm-request-outcome-projection.md)、[ADR 0316](0316-llm-model-directory.md)

## 背景

ADR 0316 将 client 与 primary route 目录从 LlmRouter 拆出后，Router 仍在 complete 和 embedding 路径内直接编排内容校验、重试、总超时和 outcome 投影。把这些一次性调用的共同执行边界放入 crate-private 执行器，可以降低 Router 的职责密度，同时不移动路由和可变运行态。

此前 record_request_outcome 投影 429 cooldown 后，with_model_permit 还会在同一结果返回时再次写 cooldown。重复写入只会略微延后同一 cooldown deadline，但使 rate-limit 投影有两个 owner。本次让 permit wrapper 只负责 acquire/wait/release；各请求在其结果边界投影一次。

## 决定与所有权

1. 新增 crate-private CallExecutor，一次只绑定 Router 已解析的 model_id、Arc<dyn LlmClient> 和单份 RequestPolicy。它不选择 route、不读取 config，也不保存 health、circuit、rate-limit 或 semaphore 状态。
2. CallExecutor::complete 拥有普通/tools 分支选择、validate_content、现有 execute_with_retry 和 router 总超时；最终 provider Result 转成 owned RequestOutcome，再通过 Router 注入的 crate-private 闭包进入同一 health/cooldown 投影实现。校验失败和由外层总 deadline 产生的 timeout 继续不投影，保持原调用边界语义。
3. CallExecutor::embed 拥有非空 embedding 的现有 retry、embedding 总超时和同一个 Router outcome 投影闭包。空 batch 仍在 Router 中 route resolution 前返回空结果，不解析 client、不申请 permit，也不投影健康状态。
4. LlmRouter 继续读取 config snapshot 并解析 client/model；持有 health/circuit、rate-limit cooldown、model permit 与 stream rules。permit wrapper 负责并发等待、已有 cooldown 等待和 permit 生命周期，不再重复写同一 outcome 的 cooldown。
5. 不建立第二套 retry 或 usage 入口。CallExecutor 只复用 request_pipeline 的现有 retry/timeout helper；usage 仍由现有调用者和响应处理流程负责。
6. streaming（raw 与 aggregated）生命周期继续由 Router/streaming.rs 管理：permit、cancel、chunk callbacks、重试边界和取消健康豁免不迁移。RequestKind 的 capability、call purpose 与 UI usage role split 仍是后续独立工作。

## 必须保持的不变量

- 不改变公开 Router API、配置 schema、provider adapter、wire payload、模型 identity、请求 kind、错误文本、usage 投影或 stream 行为。
- plain complete 仍调用 chat_with_output_cap；tools complete 仍调用 chat_with_tools_output_cap；每次重试复用同一份消息/工具数据并保留 output-token cap。
- 一个逻辑 provider 结果只触发一次 Router health/rate-limit outcome 投影；permit/circuit/route 错误不被伪装成 provider result。
- 重试仍只作用于已选中的 client/model；总 timeout 仍包住原请求执行及结果投影范围，不包住 route selection、permit 获取或 cooldown 等待。
- embedding empty-input fast path 不依赖已配置的 embedding route。

## 替代方案

- 将所有状态都移入 executor：会形成第二个 Router 状态 owner，并把配置、熔断和并发策略与调用执行重新耦合，拒绝。
- 给 executor 注入公共 trait 或状态端口：没有跨 crate consumer，扩大公共面，拒绝。使用方法级 crate-private closure 复用 Router 的 outcome helper。
- 顺便抽取 StreamExecutor 或重定义 RequestKind：stream permit/cancellation 生命周期及 capability/call-purpose/usage-role 语义是独立边界，延期。
- 保留 permit wrapper 对 429 的重复写入：冷却结果近似幂等，但会保留双重投影入口，拒绝。

## 影响与验证

本切片只增加 haven-llm 内部模块及行为测试，无配置、数据库、IPC、公共 API、provider wire 或用户数据变化。测试覆盖 plain/tools dispatch、retry、total-timeout 错误文本、最终结果投影单次调用、embedding rate-limit 单次投影，以及 Router 的 empty-input 与共享 cooldown 行为。

验收命令：

- cargo fmt --all -- --check
- cargo test --locked -p haven-llm
- cargo check --workspace --locked
- cargo clippy --workspace --locked -- -D warnings
- cargo test --workspace --locked
- git diff --cached --check

## 回滚

回滚该单一提交并恢复 Router 中的 complete/embedding 执行代码及 permit wrapper 的 cooldown 写入即可。无配置、数据库、IPC、provider wire 或用户数据迁移；未迁移的 streaming executor 与 capability/call-purpose split 继续保持独立待办。
