# 1015：校验 boolean command 响应

## 状态

已采纳并实施（2026-10-12）。

## 背景

`cancel_tool_run` 与 `is_autostart_enabled` 返回 JSON boolean，但 UI adapter 直接信任 Tauri invoke 的 generated TypeScript 类型。`invoke` 的实际 payload 在运行时仍是不可信值；畸形 truthy/falsy 值可能污染取消结果或设置页的自启动状态。

## 决定

- 新增共享 `parseBooleanResponse`，仅接受原生 boolean。
- `cancel_tool_run` 与 `is_autostart_enabled` 在 command adapter 中校验后再返回。
- IPC 覆盖脚本核对生成的 boolean command 集合和所有当前 renderer caller；未使用的 `delete_tool_run` 不创建无消费者 adapter。

## 影响

不改变 Rust handler、generated command 类型或有效 boolean 响应。畸形响应使命令 promise reject，并沿现有失败处理路径传播。无需数据库重置。

## 替代方案

- 依赖 `Promise<boolean>` 静态类型：拒绝；它不验证运行时 JSON。
- 将非 boolean 值转为 truthy/falsy：拒绝；这会把协议错误伪装成业务状态。

## 验证

- `corepack pnpm exec vitest run src/lib/contracts/boolean.test.ts src/lib/settingsCommands.test.ts src/lib/toolRunCommands.test.ts`
- `corepack pnpm run check`
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`

## 回滚

移除 boolean parser 与 adapter 调用并恢复直接返回 invoke promise；这会重新允许畸形 JSON 值进入 renderer 状态。
