# ADR 0319：LLM 请求用途与模型能力语义分离

- 状态：已采纳（2026-09-25）
- 范围：`haven-llm` 的 `ModelDirectory` primary route filtering
- 关联：[ADR 0316](0316-llm-model-directory.md)、[ADR 0318](0318-llm-call-executor.md)

## 背景

`RequestKind` 已经是逻辑调用用途和配置路由键，例如 `chat`、`fast_chat`、`audio_chat` 与 `transcription`。模型声明的 `Capability` 则是 provider/model 能力，例如 `Chat`、`FastChat`、`AudioInput` 与 `Transcription`。此前 `ModelDirectory` 构造 primary route 时直接从 `RequestKind` 推导 capability 并检查模型声明，两个语义只在一个表达式中出现。

UI/durable usage 的 owner 已有独立类型 `LlmCallKind`（`Agent`、`Media`、`Tool`）；usage detail 同时保留 request kind 与 call kind。该 owner 不属于路由选择。

## 决定

1. 保留 `RequestKind` 作为逻辑调用用途和现有配置/路由键。其 snake_case 字符串、配置 JSON 字段、`CompleteRequest` 及 Router 调用点不变。
2. 在 `ModelDirectory` 加入 crate-private `RequestDescriptor`，分别持有 `purpose: RequestKind` 与 `required_capability: Capability`。构造 primary route 时用 `purpose` 作为原路由表 key，只用显式 `required_capability` 过滤模型 capability。
3. Descriptor 的 capability 映射继续以 `RequestKind::required_capability() -> Capability` 为唯一映射来源；配置层已有的 `LlmConfig::route_model` 与 `RouterConfig::route` 继续按同一映射校验。
4. `LlmCallKind` 继续独立表达 usage owner，不进入 Router `RequestKind` 或 descriptor。
5. 本 ADR 只完成语义类型的第一步。请求 DTO、Router 的 public/internal 方法、streaming、metadata/config route helpers 与仓库其他 `RequestKind` 用途不做全仓迁移；后续 descriptor 扩展应另立切片并保留配置边界上的原字符串。

## 必须保持的不变量

- 每个逻辑用途仍使用原 `RequestKind` 找到配置 policy 和 primary model；capability 不能替代 route key。
- 所有 `RequestKind` 到 provider/model `Capability` 的映射明确且有测试；`AudioChat` 要求 `AudioInput`，`Transcription` 要求 `Transcription`，`Chat`/`FastChat` 各自独立。
- production routes 仍要求凭据与 capability 都匹配；注入 client routes 只跳过凭据检查，仍要求 capability 匹配。
- `RequestKind`、`RequestPolicy` 的序列化字符串/形状、数据库 schema、IPC、provider wire 与 usage 数据不变。
- `LlmCallKind` 仍单独标记 `Agent`/`Media`/`Tool` usage owner。

## 替代方案

- 把所有 `RequestKind` 调用点立即换成新 public request descriptor：涉及配置、请求 DTO、Router、streaming 和仓库调用者，超出此最小切片。
- 把 `LlmCallKind` 并入 `RequestKind`：混合执行路由用途和 usage owner，拒绝。
- 以 model capability 作为 route key：一个 capability 可服务多个逻辑用途，且会丢失独立配置 policy，拒绝。

## 影响与验证

这是 crate-private 类型和测试变化，不要求配置、数据库、IPC、provider wire 或用户数据重置。测试覆盖所有用途到 capability 的映射、相似用途的映射区分、注入 route 的 capability filtering、完整请求的原用途路由，以及稳定的 `RequestKind`/`RequestPolicy` 序列化。

验收命令：

```sh
cargo fmt --all -- --check
cargo test --locked -p haven-common
cargo test --locked -p haven-llm
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
git diff --cached --check
```

## 后续迁移范围

`RequestKind` 仍出现在 `CompleteRequest`、`PromptRequest`、`StreamRequest`、health-check/embedding 路径、Router API、配置 route helpers、usage detail request field 和 Agent/Tools 调用点。若后续需要让完整 descriptor 贯穿执行边界，应分别评估 complete、streaming、embedding/health-check、metadata lookup 与 usage projection 的需要，逐条保留原配置选择、能力过滤和 `LlmCallKind` owner 语义；不得借此改变 provider wire、IPC 或配置 JSON。

## 回滚

回滚该提交并删除 descriptor 与新增测试/文档即可。没有配置、数据库、IPC、provider wire 或用户数据迁移。
