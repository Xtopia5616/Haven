# ADR 0772：CountChip 使用数值数量 prop

## 状态

已采纳并实施（2026-10-08）。

## 背景

`CountChip.count` 表示列表、过滤结果或历史总数。所有生产调用点传递 `.length` 或 `number` 值；原 prop 额外接受字符串，并用 `Number()` 隐式转换。

## 决定

1. `count` 只接受 `number`，缺省仍为 0。
2. 有限值继续向下取整并 clamp 到非负范围；负数、`NaN` 与无穷值显示为 0。
3. 不接受 numeric string 作为数量，也不改变标签、前缀、ARIA live 或现有生产计数来源。

## 验收与回滚

UI type check 验证消费者传入数值；新测试覆盖 fractional、negative、`NaN` 与 positive infinity 的显示。无 IPC、配置或持久数据变更；回滚 `count` 类型与数值格式化逻辑即可恢复原行为。
