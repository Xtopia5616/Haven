# ADR 0316：LLM Router 模型目录边界

- 状态：已采纳（2026-09-25）
- 范围：`haven-llm` 内部 `ModelDirectory` 与 `LlmRouter` 的模型路由职责
- 关联：[ADR 0234](0234-llm-complete-request-object.md)、[ADR 0241](0241-aggregated-stream-request-object.md)、[ADR 0242](0242-embedding-and-health-request-objects.md)、[ADR 0246](0246-llm-request-outcome-projection.md)

## 背景

`LlmRouter` 同时保存 provider client map、`RequestKind → primary model id` 路由表，并在多处直接查模型 client 或从 `RouterConfig` 读取路由 metadata。请求执行策略和模型目录职责因此交错，增加 route 相关改动时容易触碰 Router 的健康、并发、重试或 streaming 管线。

## 决定与所有权

1. 新增 crate-private `ModelDirectory`，由它从 `RouterConfig` 构造模型 client map，或从显式注入的 client 集合构造测试目录；它还构造并保存 request kind 到唯一 primary model id 的映射。
2. `ModelDirectory` 提供按 `RequestKind` 选择 client、解析 `(model_id, client)`、按已解析 model id 取 client，以及读取 capability profile、configured model / endpoint 和 context-window metadata 的方法。无路由与 client 缺失仍返回原有 `LlmError::Configuration`；只用于能力查询的 `select_request` 仍保留默认 adapter fallback。
3. 生产路由继续要求 primary model 声明 `RequestKind::required_capability()` 对应的 capability，且 endpoint credential 可用。注入 client 的测试目录只略过 credential 检查，仍要求 capability 匹配。primary id 的空白裁剪和唯一 primary 行为不变。
4. Metadata 方法借用 `LlmRouter` 持有的同一份 `RouterConfig` snapshot，并沿用 `RouterConfig::route` 的 credential/capability 语义；目录不保存第二份配置真源。模型 context window 对应的 output-token clamp 也由目录中的模型 metadata helper 在构造 client 前应用到 Router snapshot。
5. `LlmRouter` 保留 config snapshot、health/circuit breaker、rate-limit cooldown、per-model semaphore、stream rules、retry、timeout、usage、cancellation 与所有请求执行编排。`request_pipeline.rs` 的策略快照和 provider adapter/wire mapping 均不变。
6. 本切片不拆分 `RequestKind` 的 capability / call-purpose / usage-role 职责，也不提取 `CallExecutor` 或 `StreamExecutor`；这些后续工作需要独立的行为边界。

## 必须保持的不变量

- 每个 request kind 最多对应一个 primary model；失败时仍在 Router 的既有同 endpoint 重试策略内处理，不切换 provider/model。
- 多个 request kind 可以共享同一 model id 和同一个 `Arc<dyn LlmClient>`，健康、限流与 semaphore 仍按该 model id 共享。
- credential/capability 过滤、未配置判断、context window fallback、output-token clamp、fallback capability profile、provider wire、retry/timeout/permit/health/cancellation/stream callback 与 usage 语义保持不变。
- `RequestKind::required_capability` 继续作为配置领域的 capability 权威；不增加公共 request/config/IPC 类型。

## 替代方案

- 继续把 map 与查询散落在 Router：保留模型目录和执行策略交错的状态，拒绝。
- 把 `RouterConfig` snapshot 一并移动进目录：会形成第二个配置 owner，并把执行策略配置和模型 identity 绑定，拒绝。
- 同时抽取 complete/stream executor 或重定义 capability/call purpose：扩大本切片并增加行为迁移面，延期到各自独立 slice。

## 影响与验证

这是 `haven-llm` 内部结构调整，没有配置 schema、公开 request struct、RequestKind serde/IPC、provider adapter、wire payload、数据库或用户数据变化。`ModelDirectory` 单测覆盖生产凭据与 capability filtering、注入 route filtering、缺少 route、共享 model/client identity 和 endpoint/context-window metadata；原 Router 行为测试继续保留。

验收命令：

```sh
cargo fmt --all -- --check
cargo test -p haven-llm --locked
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
git diff --check
```

## 回滚

回滚该单一提交即可删除 `ModelDirectory` 并恢复 Router 内的 client map 与 primary route 构造/查询；没有配置、数据库、IPC、provider wire 或用户数据迁移，也不需要重置数据。之后未迁移的 executor 与 capability/call-purpose split 仍保持待办，不需要作为回滚的一部分处理。
