# ADR 0084：删除 Balanced Model 与模型级 fallback

## 背景

Haven 原先为聊天请求维护独立的 `balanced_model` 角色，并在默认端点失败后切换到该端点。
这套路径同时扩展了路由健康槽位、重试预算、Agent 状态事件、设置页模型卡片和 IPC 契约，
但并没有提供足够的实际价值；失败时还会隐藏最初的 provider 错误。

## 决定

- 删除 `EndpointRole::BalancedModel`、对应配置字段、设置页入口和 API-key 状态字段。
- 聊天、工具聊天和流式聊天只对选定端点执行既有重试策略；重试耗尽后直接返回该端点的错误。
- 删除 `fallback_retry_max_retries`、`AllEndpointsFailed` 以及 Balanced Model 激活事件和通知。
- 记忆事实抽取改用已有的 `small_model`；其他专用端点（image/audio/embedding）保持原有路由。
- 与模型选路无关的本地解析、媒体能力和关键词召回 fallback 保持不变。

## 替代方案

- 保留 Balanced Model 作为故障切换：会继续维护重复的端点配置和错误聚合路径，不采用。
- 将 Balanced Model 合并为默认端点的隐式别名：会保留双重配置语义，不采用。

## 影响与重置

这是配置和内部 IPC 契约的破坏性变更。旧 `balanced_model` 角色和
`fallback_retry_max_retries` 不再被运行时读取，保存配置后不会写回；如需清理旧配置残留，
按 [`docs/release-and-reset.md`](../release-and-reset.md) 删除数据根目录并重新配置模型。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cd ui && corepack pnpm run check`
- `cd ui && corepack pnpm run test:run`
