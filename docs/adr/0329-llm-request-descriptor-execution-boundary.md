# ADR 0329：LLM 请求 descriptor 贯穿执行边界

- 状态：已采纳（2026-09-25）
- 范围：`haven-llm` 的 `RequestDescriptor`、primary route resolution 与抽取出的请求执行器
- 关联：[ADR 0316](0316-llm-model-directory.md)、[ADR 0318](0318-llm-call-executor.md)、[ADR 0319](0319-llm-request-capability-semantics.md)、[ADR 0327](0327-llm-raw-stream-executor.md)、[ADR 0328](0328-llm-aggregated-stream-executor.md)

## 背景

ADR 0319 引入 crate-private `RequestDescriptor`，但它只在 `ModelDirectory` 构造 primary route 时短暂存在。route 解析返回 model id 和 client 后，`CallExecutor`、`StreamExecutor` 与 `AggregatedStreamExecutor` 无法观察逻辑用途及其所需 capability，请求语义在路由到执行边界之间丢失。

`RequestKind` 仍是配置 policy 的稳定 route key；模型 `Capability` 是 primary route 的筛选条件。usage owner `LlmCallKind` 则由 Agent/Tools 调用方确定，同一 request kind 可以由不同 owner 发起，Router 无法可靠推断。

## 决定与所有权

1. 将 `RequestDescriptor` 放入 `request_descriptor.rs` 作为 crate-private 请求语义类型。`From<RequestKind>` 继续调用 `RequestKind::required_capability()`；不增加第二套 capability mapping，也不把内部类型放入 `haven-common`。
2. `ModelDirectory` 的 primary route 仍以原 `RequestKind` 为 key，并保存筛选时使用的 descriptor。执行解析接受显式 descriptor，只有它与 route 中的用途/capability 组合完全匹配时才返回 model/client；不匹配时沿用 `no configured model for {request}` 配置错误并 fail closed。
3. Router 在执行请求的 route preparation 边界创建 descriptor，并将同一值传入 `CallExecutor`（complete 与 embedding）、`StreamExecutor`（raw stream 建流）和 `AggregatedStreamExecutor`。executor 记录非敏感用途/capability trace 字段，但不选择 route、不读取 Router config，也不拥有 health、permit 或 cooldown 状态。
4. Provider adapters 仍只接收既有 provider-neutral 请求数据并负责 wire mapping；不接收 `RequestDescriptor`，不改 provider request、retry、timeout、usage accounting 或错误文本。
5. 不把 `LlmCallKind` 填入 descriptor。usage owner 由调用方的 Agent/Tools 路径决定；后续若要统一 usage role，应从该调用方边界设计显式传递。health check、native transcription 及 metadata/config helper 仍以 `RequestKind` 表达其专用路径。

## 必须保持的不变量

- `RequestKind` 仍是配置 policy 和 primary route 的 key，serde/string 形式和 public Router/request DTO 不变。
- `RequestKind::required_capability()` 是唯一用途到模型 capability 的映射。production route 继续要求已声明 capability 和可用凭据；注入 route 仍要求 capability 匹配。
- route descriptor 与请求 descriptor 不匹配时，不得执行 provider call；不允许通过默认 adapter fallback 将执行请求降级成其他 capability。
- Router 仍是 route、config snapshot、permit、circuit、health 与 cooldown 的唯一 owner；各 executor 继续复用原 retry/timeout/outcome implementation。
- provider wire、模型选择、路由策略、取消、stream 生命周期、usage 记录及错误文本保持不变；不改变 embedding/health/prompt 既有路径契约。
- `LlmCallKind` 继续由调用方设置并按既有格式投影到 durable usage 与 UI；此切片不声称完成 usage-role 或全仓 call-purpose 迁移。

## 替代方案

- 在每个 executor 中从 `RequestKind` 单独推导 capability：会复制语义映射，后续修改容易漂移，拒绝。
- 让 Router 根据 request kind 推断 `LlmCallKind`：media、tool 和 agent 调用可能共享同一 request kind，归属错误，拒绝。
- 将 descriptor 传到 provider adapter 或公共 DTO：能力是 Router/model route 的语义，不参与 provider wire，也不需要扩展 common/API 面，延期。
- 一次迁移 health/transcription、metadata/config helper 及 Agent/Tools 所有调用者：会扩大行为面；保留为后续独立评估。

## 影响与验证

本切片只改变 `haven-llm` 内部 route/executor 参数及架构文档，无配置、数据库、IPC、公共 API、provider wire 或用户数据变化。测试覆盖完整 RequestKind-to-capability mapping、descriptor 在三个 executor 边界上的保留、route descriptor 不匹配时 fail closed、以及不支持 capability 的注入 route 被拒绝；既有执行器和 Router 行为测试继续保留。

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

尚未完成的部分包括：让 public `CompleteRequest`、`PromptRequest`、`StreamRequest` 与其他专用入口直接携带统一 descriptor（目前这些 DTO 保留兼容的 `RequestKind` route key）；将 descriptor 评估扩展到 health/native transcription 和 metadata/config helper；以及从 Agent/Tools 调用方安全传递 `LlmCallKind`/usage role。后续工作必须继续以原 `RequestKind` 路由，不能把 usage role 从 capability 或 request purpose 推断。

## 回滚

回滚该单一提交并恢复 executor 仅接收 model/client/policy，删除 route descriptor association 与新增模块/测试即可。没有配置、数据库、IPC、provider wire 或用户数据迁移；无需重置数据。
