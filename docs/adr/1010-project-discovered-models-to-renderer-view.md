# 1010：将模型发现结果投影为 renderer view

## 状态

已采纳并实施（2026-10-12）。

## 背景

`discover_models` 的 generated Rust `ModelInfo` 直接从 command adapter 进入设置页和媒体设置页，导致 renderer 消费 wire snake_case，`id` 也没有表明它是 Provider 分配的外部模型身份。配置 Provider 连接名与模型供应商身份已有区分约定（ADR 0872），但模型列表 view 未表达这一区别。

## 决定

- UI command adapter 校验完整 `ModelInfo` 行、Rust `u32` context window、有限非负价格和目录内唯一模型 ID。
- renderer 使用 `DiscoveredModelView`：外部 `id` 投影为 `providerModelId` 并原样保留，wire `provider` 投影为 `modelProvider`，展示名、能力与价格字段转为 camelCase。
- Provider 连接名仍只用于目录 cache key 和 discovery request 的 `providerName`；不拿模型供应商身份替代连接名。

## 影响

不改变 Rust command、generated DTO、Provider ID 或持久化设置字段。未知 wire 扩展字段不会进入 renderer view；错误行或重复模型 ID 使本次发现失败，不覆盖上一次目录。

## 替代方案

- renderer 继续直接消费 `ModelInfo`：拒绝；会把 snake_case wire 形状和含义不清的 `id` 泄漏到 UI owner。
- 规范化、trim 或改写 Provider model ID：拒绝；模型 ID 是 provider 外部身份，必须原样传回模型配置和请求路由。

## 验证

- `corepack pnpm exec vitest run src/lib/modelDiscoveryCommands.test.ts src/lib/views/ModelSettings.test.ts`
- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm run build`

## 回滚

恢复 raw generated DTO 在 renderer 中的消费即可回滚；不得改变外部模型 ID 内容或混用 Provider connection name。
