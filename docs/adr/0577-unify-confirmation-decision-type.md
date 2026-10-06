# ADR 0577：统一确认交互的用户决策类型

## 状态

已采纳并实施。

## 背景

`ConfirmationDialog` 的 `onConfirm` callback 产出 `ConfirmationDecision`，根路由 `+layout.svelte::handleConfirm` 消费该值并转换为 `ResolveConfirmationRequest`。两处声明字段完全相同：步骤 ID、批准结果及可选 effect、scope、target。它是 renderer 内组件交互结果，不是 Rust/Tauri wire DTO。

## 决定

在 `confirmationTypes.ts` 单一定义 `ConfirmationDecision`，由确认弹窗 Props 和根路由 handler 共用；该类型继续只表达 UI 决策，根路由仍负责映射为 IPC 请求。

## 替代方案

- 将它放进 Tauri `contracts/` 并复用为命令请求：拒绝，UI 决策含 `approved` 且字段命名/缺省策略与命令 `request_id`、`effect`、`scope`、`target` 不同，需要由路由显式转换。
- 保留两份局部声明：拒绝，同一 callback 两端结构相同，允许独立漂移没有价值。

## 影响与验证

- 仅合并 renderer callback 类型；默认决议、权限范围、命令映射与确认 UI 行为不变。
- 无 Rust、IPC、持久化或安全契约变化。
- 验证：UI `check`、`test:run`、`build`、ADR 索引与差异空白检查。

## 回滚

移除 `confirmationTypes.ts` 并恢复 `ConfirmationDialog` 与根路由中的局部类型声明。
