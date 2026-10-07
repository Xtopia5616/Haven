# ADR 0639：从共享状态色派生 StatusBadge 子集

## 状态

已采纳并实施。

## 背景

`statusColors.ts` 是共享状态色语义的 owner，`StatusTone` 包含 `success`、`warning`、`error`、`info`、`tool`、`neutral`。`StatusBadge.svelte` 另手写 `StatusBadgeTone`，重复列出前五类里除 `tool` 外的五个值；Badge 没有 `tool` 的样式，这一限制是组件契约，不是另一套状态词汇。

两份 literal union 会在新增或删除状态时独立漂移。直接让 Badge 使用完整 `StatusTone` 又会接受无组件样式的 `tool`。

## 决定

将 Badge 支持集合写为 `Extract<StatusTone, 'neutral' | 'info' | 'success' | 'warning' | 'error'>`。保留组件局部的 `StatusBadgeTone` 角色名，状态色含义仍由 `StatusTone` 拥有，`tool` 继续不可传给 Badge。

## 替代方案

- 保留两份手写 union：拒绝。相同字面量可能独立演进。
- Badge 直接接受 `StatusTone`：拒绝。`tool` 没有对应 Badge 视觉样式。
- 合并两个角色名：拒绝。通用状态色适用于多种 indicator；Badge 支持的是其明确子集。

## 影响与验证

此变更只把 Svelte 组件 prop 类型改为引用共享 palette 的显式子集；可接受值、CSS 与运行时表现不变。验证：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引与 staged diff 检查。

## 回滚

在 `StatusBadge.svelte` 恢复原有 `StatusBadgeTone` literal union，并撤回对应文档与 ADR 索引；无运行时或持久化数据影响。
