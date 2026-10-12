# 1013：校验确认命令的终态响应

## 状态

已采纳并实施（2026-10-12）。

## 背景

`resolve_confirmation` 返回 Rust-owned 闭合 enum，但 shell 直接把 generated 静态类型当成 runtime 保证，并将其写入交互 reducer。未知字符串会变成交互状态值，污染只有 `resolved`、`expired`、`stale` 三种终态的 renderer 状态机。

## 决定

- UI 在将确认响应写入 reducer 前，按生成的 `ConfirmationResolutionResult` 值集合进行校验。
- 未知或畸形值使命令处理进入现有失败路径；不写入交互状态，不展示伪造终态。

## 影响

不改变 Rust enum、IPC JSON、权限解析或持久化数据。有效响应行为不变；错误响应不会污染 UI interaction state。无需数据库重置。

## 替代方案

- 依赖 generated TypeScript 类型：拒绝；静态类型不会在 Tauri runtime 校验实际响应。
- 把未知状态映射为 `stale`：拒绝；这会伪造 owner 尚未确认的终态。

## 验证

- `corepack pnpm exec vitest run src/lib/contracts/app.test.ts`
- `corepack pnpm run check`
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`
- 完整 UI 测试与构建在本轮全量验收中复跑。

## 回滚

移除响应 parser 并恢复直接传入 reducer 即可回滚；这会重新允许未知字符串进入交互状态。
