# ADR 0642：统一通知与状态色词汇来源

## 状态

已采纳并实施。

## 背景

`statusColors.ts::StatusTone` 是 UI 共用的状态色词汇，包含 `info`、`success`、`warning`、`error`、`tool` 与 `neutral`。`notificationStore.ts::NotificationType` 手写其中四个通知等级；`ToolRunCompletionToast.type` 又手写 `info`、`success`、`error`，并将 `warning` 排除在该 projection 范围之外。后两份 literal union 描述了 palette 的子集，存在独立漂移风险。

## 决定

1. 保留通知 API 的语义名 `NotificationType`，将其定义为 `Extract<StatusTone, 'info' | 'success' | 'warning' | 'error'>`。
2. 保留 `ToolRunCompletionToast.type` 的窄范围，定义为 `Extract<NotificationType, 'info' | 'success' | 'error'>`；ToolRun projection 仍不产生 warning toast。
3. palette、通知和具体 toast projection 继续各自承担视觉词汇、通知动作和场景映射职责；只统一值集合的来源。

## 替代方案

- 让 toast 接受完整 `NotificationType`：拒绝。它会无依据地允许当前 projection 产生 warning。
- 让通知 store 直接使用完整 `StatusTone`：拒绝。`tool` 与 `neutral` 不是通知等级。
- 保留两份或三份手写集合：拒绝。相同视觉词汇存在漂移风险。

## 影响与验证

现有接受值、通知去重、持续时间、ToolRun 分类与 toast 映射不变；仅令 subset 约束引用 canonical tone owner。验证：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引与 staged diff 检查。

## 回滚

如需回滚，恢复通知和 ToolRun toast 的 literal unions，并同步撤回命名规范、路线图和 ADR 索引；无 IPC 或持久化影响。
