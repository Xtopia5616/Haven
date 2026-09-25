# ADR 0339：LLM health 与 transcription 请求语义审计

- 状态：已采纳（2026-09-25）
- 范围：`haven-llm` 的 `HealthCheckRequest`、native transcription、metadata/config route helpers 与 `RequestDescriptor` 使用边界
- 关联：[ADR 0316](0316-llm-model-directory.md)、[ADR 0319](0319-llm-request-capability-semantics.md)、[ADR 0327](0327-llm-raw-stream-executor.md)、[ADR 0328](0328-llm-aggregated-stream-executor.md)、[ADR 0329](0329-llm-request-descriptor-execution-boundary.md)

## 背景

ADR 0329 将 `RequestDescriptor` 从 primary route resolution 贯穿到 complete、embedding、raw stream 与 aggregated stream executor，并将 health/native transcription、metadata/config helper 列为待评估范围。本切片检查这些入口是否在执行边界重复从 `RequestKind` 推断 capability 或 call-purpose。要求保持 public Router API、原 `RequestKind` route key、模型选择、usage owner 和现有 health/STT/config 行为。

## 审计结论与决定

1. **不新增 wrapper 或公共类型。** 没有发现需要再次推导请求 capability/call-purpose 的执行边界；现在的代码已在正确的 Router 路由边界构造同一 crate-private descriptor。
2. `health_check(HealthCheckRequest)` 保留公开 DTO 的 `RequestKind` 字段，并在 `with_request_permit` 前构造 `RequestDescriptor`。model directory 用该 descriptor 验证原 route key 和要求的 capability；选中 client 后调用的是不接收 route 语义的 `LlmClient::health_check`。Router 继续按既有结果投影更新 endpoint health/cooldown。health probe 是有意的 provider 网络操作，不把 descriptor 传入 adapter。
3. `transcribe_audio` 保留专用公共 API。它先用原 `RequestKind::Transcription` 检查配置以保留既有用户错误文本，再以 `RequestDescriptor::from(Transcription)` 解析唯一 native route，随后对已选 client 执行现有 retry/timeout/outcome 路径。`RequestKind::required_capability()` 是配置 route 和 descriptor 唯一共用的能力映射，因此前置配置检查与执行 route filter 不会各自维护一份映射。native `UnsupportedCapability` 才会回退；fallback 以独立 `AudioChat` route/capability 执行，不借用 transcription capability。
4. metadata/config helpers 保留 `RequestKind`。`ModelDirectory` 的 endpoint/context/configured checks 和 Router 的 context-window/output-budget/cost helpers 读取 Router 唯一的 `RouterConfig` snapshot，经 `RouterConfig::route` 做现有 credential/capability 配置检查；它们不调用 LLM client、不生成 usage，也不投影 health/cooldown。`capability_profile_for_request` 只读取已选择 adapter 的本地 wire profile。`connection_status` 与 `prewarm_all` 会显式运行 health probe，属于 health 执行入口而非只读 metadata helper。
5. 保持 `RequestDescriptor.purpose: RequestKind`、public request DTO、Router API、provider adapter、health/circuit/rate-limit、native STT fallback、usage `LlmCallKind` owner、retry/timeout/permit、wire 和错误文本不变。

## 必须保持的不变量

- `RequestKind::required_capability()` 是用途到模型 capability 的唯一映射；descriptor mapping 测试覆盖全部 RequestKind。
- HealthCheck 与 native Transcription 必须通过各自 route key 和声明 capability 才可调用；仅声明 `AudioInput` 的 model 不能冒充 `Transcription`。
- 未配置或 capability 不匹配时 fail closed；native transcription 路由缺失不能隐式触发 `AudioChat` fallback。fallback 只响应已选择 native adapter 返回的 `UnsupportedCapability`。
- metadata/config 查询不触发 provider 调用、usage 变更、health outcome 投影或 cooldown。连接状态和预热仍是显式 health probe。
- `LlmCallKind` usage owner 继续由 Agent/Tools 调用方显式决定；Router 不按 `RequestKind` 或 capability 推断 owner。

## 替代方案

- 给 health/native STT 新建 request wrapper 或专用 executor：执行路径在 route/permit 阶段已经接收 descriptor，后续 adapter call 不再消费路由语义；再包一层不会减少推断或建立新的稳定边界，拒绝。
- 把 descriptor 传入 provider adapter 或改变 public DTO：能力筛选不属于 provider wire，而且违反保持公开 API 的范围，拒绝。
- 把 metadata/config helper 也统一成 provider-call descriptor：这些 helper 查询配置/本地 profile，没有 provider call 或 usage/health outcome，不应伪装成执行能力请求，拒绝。

## 后续工作

- `CompleteRequest`、`PromptRequest`、`StreamRequest`、`HealthCheckRequest` 等 public DTO 仍以 `RequestKind` 承载兼容 route key；当前 `RequestDescriptor.purpose` 也仍是 `RequestKind`。若要分离独立 call-purpose 类型，应作为单独的 API/契约切片设计，继续保留原配置 route key。
- `LlmCallKind` usage role 仍属于 Agent/Tools 调用方。若后续要统一向下传递，必须由调用方显式提供，不能从 Router route、request purpose 或 capability 推断。
- 本审计未要求迁移仓库中单纯选择配置 route 的其他 `RequestKind` 用法；后续只有发现重复执行语义推断时才逐条评估。

## 影响与验证

本切片只增加 route/side-effect contract tests 与文档，无 public API、配置、数据库、IPC、provider wire 或用户数据变化。保留已有 `RequestDescriptor` 全用途映射、unsupported-capability fail-closed 与 native STT fallback 测试；新增 health/native transcription 对错误 capability 的负向用例，以及 metadata helper 不调用 provider、不改 usage、不投影 health/cooldown 的夹具测试。

验收命令：

```sh
cargo fmt --all -- --check
cargo test --locked -p haven-llm
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
git diff --cached --check
```

## 回滚

回滚该单一提交即可删除新增契约测试、ADR 和对应架构/路线图说明。无配置、schema、IPC、provider wire 或用户数据迁移。
