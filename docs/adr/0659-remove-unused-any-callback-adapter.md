# ADR 0659：删除无消费者的 UI any callback helper

## 背景

`typedCallbacks.ts` 的 `withStringValue`、`withNumberValue`、`withBooleanValue` 与 `withEventValue` 都有生产组件消费者，用来固定 Svelte markup callback 的输入类型。`withAnyValue` 是唯一接受并返回 `(value: any) => void` 的 helper，全仓只有定义没有调用方；它既没有解决实际边界需求，也绕开了其余 helper 建立的类型约束。

## 决定

- 删除无消费者的 `withAnyValue`，不保留兼容导出。
- 保留有调用方的四个具名 typed callback adapter，其行为不变。

## 验证

- `corepack pnpm run check`
- `corepack pnpm exec prettier --check src/lib/typedCallbacks.ts`
- 全仓 `rg` 确认不存在 `withAnyValue` 引用。

## 回滚与重置

回滚时可恢复单一 helper 定义。本次没有 runtime、IPC 或持久化变化。
