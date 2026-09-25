# ADR 0354：LLM request purpose 与 usage owner 契约审计

- 状态：已采纳（2026-09-25）
- 范围：`RequestKind`、`RequestDescriptor`、模型 `Capability` 与 `LlmCallKind` 的语义边界
- 关联：[ADR 0316](0316-llm-model-directory.md)、[ADR 0319](0319-llm-request-capability-semantics.md)、[ADR 0329](0329-llm-request-descriptor-execution-boundary.md)、[ADR 0339](0339-llm-health-request-semantics-audit.md)

## 背景

Phase 6 已把 `RequestDescriptor` 贯穿 Router 执行路由，但 public request DTO 和配置继续使用 `RequestKind`，descriptor 的 `purpose` 也继续使用 `RequestKind`。剩余问题是这是否重复维护同一事实、是否应增加 public `CallPurpose`，以及是否应将 usage owner 传入 LLM Router。

## 决定

1. **保留 `RequestKind` 作为 public 逻辑请求类型和配置 route key。** 同一枚举同时表达请求意图并选择配置中的 primary route；它的序列化、配置字段、`RequestPolicy` 和 public request DTO 是现有契约。现在再定义一份一一对应的 public `CallPurpose` 只会复制 variants 与转换表，不能表达新的选择或行为。
2. `RequestDescriptor::purpose` 保持 `RequestKind`，`required_capability` 继续由唯一的 `RequestKind::required_capability()` 映射生成。用途和模型 capability 是不同语义：前者选逻辑 route，后者用于拒绝不具备该能力的模型。`RouterConfig::route` 与 descriptor 共用同一映射函数，没有第二份 capability policy。
3. 删除 `ModelDirectory::PrimaryRoute` 中重复保存的完整 descriptor。primary route map 的 `RequestKind` key 是 route purpose 的唯一存储位置；value 只保存由该 key 映射的 `required_capability` 和 model id。执行解析仍按 descriptor purpose 找 route，再核对显式 capability；不匹配继续 fail closed。
4. `LlmCallKind` 是 usage 记录的运行时 owner（`agent`、`tool`、`media`），与请求 route 正交。Router 的 `LlmCallUsage` 只带请求 `RequestKind` 和 provider usage/model 元数据；Agent/Tools 在知道调用归属后显式设置 `LlmCallKind`，Memory 按两个字段分别持久化。现有持久化用例以相同 `RequestKind::Chat` 写出 `tool` 与 `media` 两类 owner，证明 owner 不能从 route 或 capability 推断。
5. 不把 usage owner 加到 Router/provider adapter，也不改变 public DTO、route key、capability filtering、health/circuit/rate-limit、retry/timeout、usage owner、metadata 查询或 provider wire。只有未来出现独立于配置 route 的新调用语义时，才重新评估 `CallPurpose`，并要求定义明确转换关系与兼容边界。

## 调用点审计

- `LlmClient` 的 provider 方法不接收 `RequestKind` 或 `LlmCallKind`；Router 在 route/permit 边界构造 descriptor，执行器只保留 request purpose/capability 用于执行诊断。Router 暴露的 `LlmCallUsage` 带 `RequestKind`、provider usage、model 和 duration，不携带 owner。
- Agent 的 `record_usage_and_emit` 显式设为 `Agent`；`record_tool_usage` 原样传递 `ToolLlmUsage.call_kind`；`record_media_usage_at_step` 显式设为 `Media`。`UsageRuntime` 只允许 Agent cumulative tracker 接收 `Agent` owner。
- Tools 的文件 summary 以 `FastChat` route 标记 `Tool` owner；媒体 vision 与转写结果标记 `Media` owner。复合媒体路径保留转写返回的 request kind，再在 ToolLlmUsage 边界附加 owner。
- Memory 不从 route 生成 owner：`LlmCallUsageInput` 要求调用者同时给出 `request_kind` 与 `call_kind`，持久化分别投影到既有 `role` 与 `call_kind` 字段，session 累计只统计 `Agent`。回归用例固定相同 `Chat` route 下 `Tool`、`Media` 两条明细的 owner 值。

## 必须保持的不变量

- 每个 `RequestKind` 仍按原配置 primary route 解析；descriptor 与已配置 route 的 capability 不一致时 fail closed。
- capability requirement 的权威映射仍只有 `RequestKind::required_capability()`；AudioChat 与 Transcription 保持各自的 `AudioInput` 和 `Transcription` 能力。
- Router 不依据 route、purpose 或 capability 推断 usage owner；Agent/Tools 持有 owner 选择，Memory 保持字段分离与既有聚合规则。
- health probe、native transcription 与 `UnsupportedCapability` 到独立 AudioChat route 的 fallback 行为不变。

## 替代方案

- 增加与 `RequestKind` 当前一一对应的 public `CallPurpose`：没有第二个调用选择事实，新增类型和双向转换只会增加映射维护及调用点迁移，拒绝。
- 让 Router 集中接收 `LlmCallKind`：相同 Chat route 可由 Agent、普通工具 inference 或媒体路径使用，Router 无法从请求语义判断 owner，拒绝。
- 将 capability 或 `RequestDescriptor` 整体保存在 route map value：map key 已经唯一持有 `RequestKind`；route value 仅需 capability 校验和 model identity，已移除冗余 purpose 副本。

## 影响与验证

变更只调整 crate-private `PrimaryRoute` 表示、强化已有 usage 正交性回归断言，并更新架构记录。没有配置、数据库、IPC、public API、provider wire 或用户数据迁移。现有测试覆盖 RequestKind-to-capability 全量映射、descriptor capability mismatch fail closed、AudioChat/Transcription 独立能力、usage 同 route 不同 owner，以及 health/native transcription 边界。

验收命令：

```sh
cargo fmt --all
cargo test --locked -p haven-llm
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
git diff --cached --check
```

## 回滚

回滚该提交并恢复 `PrimaryRoute` 中完整 descriptor 即可。回归断言和本 ADR/路线图条目可一并撤回；无 public、配置、schema、IPC、provider wire 或用户数据迁移。
