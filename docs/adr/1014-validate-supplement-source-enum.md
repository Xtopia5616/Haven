# 1014：校验 supplement 事件来源枚举

## 状态

已采纳并实施（2026-10-12）。

## 背景

Rust `AgentSupplementEvent.inject_source` 使用 Common `InjectSource` 闭合枚举，但 UI mapper 将其降为任意字符串，测试也接受 Rust 无法发送的 `future_source`。这与事件 mapper 对其它闭合 enum 的边界校验规则不一致，也让 reducer 看到未定义的领域来源值。

## 决定

- IPC 生成器显式生成 Common `InjectSource` 的 TypeScript union 与 runtime value tuple。
- Agent event mapper 在映射 `inject_source` 前校验生成值集，拒绝未知值；缺省值仍保持可选。
- 用 IPC event checker 固定生成类型、runtime 校验和 mapper 接口之间的关系。

## 影响

不改变 Rust enum、事件 JSON、持久化数据或已知来源的 reducer 行为。有效来源继续映射；畸形来源事件被丢弃，不进入 UI 状态。无需数据库重置。

## 替代方案

- 保持任意字符串以便未来版本兼容：拒绝；本地应用将 Rust backend 与 UI 一起发布，当前 enum 是双方共同的闭合 wire contract。
- 把未知来源映射为默认来源：拒绝；这会隐藏 producer/consumer contract drift。

## 验证

- `corepack pnpm exec vitest run src/lib/contracts/agent.test.ts`
- `corepack pnpm run check`
- `pwsh -NoProfile -File scripts/generate-ipc-contracts.ps1 -Check`
- `pwsh -NoProfile -File scripts/check-ipc-events.ps1`

## 回滚

移除 `InjectSource` 的生成注册和 UI 值集校验，并恢复字符串 DTO；这会重新允许未定义来源进入 reducer。
