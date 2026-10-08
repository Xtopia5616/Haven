# ADR 0807：从数值输入组件派生包装层 Props

## 状态

已采纳并实施（2026-10-08）。

## 背景

`MaterialNumberFieldWithUnit` 将 value、min、max、step、onChange 与 id 原样传给 `MaterialNumberField`，但两份 Props 接口独立声明了这六个字段。两边已有字段类型当前相同，未来新增或调整底层输入字段时，wrapper 契约可能不跟随。

## 决定

wrapper 的数值输入 Props 从 `ComponentProps<typeof MaterialNumberField>` 派生。移除继承的 `width` 后，由 wrapper 按自己的组合容器职责声明 `width`；`unit` 与 `className` 继续由 wrapper 单独拥有。

## 影响与回滚

只统一编译期 Props 来源；HTML、数值行为、布局与调用点不变。无需数据迁移或重置。回滚只需恢复 wrapper 的显式字段声明。

## 验收

运行 UI 类型检查、完整 UI 测试和 ADR 索引检查；类型检查确认 wrapper 继续接受现有调用字段并与底层输入契约保持同步。
