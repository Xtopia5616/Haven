# ADR 0747：Schedule 列表 renderer 对齐 due_at 字段

## 状态

已采纳并实施。

## 背景

`schedule.set` 的单项响应使用 `fires_at`，但 `schedule.list` 的 `ScheduledToolRunView::to_json` 输出 `due_at`。UI 列表 renderer 和 nested validator 沿用了单项响应字段名，因而列表时间不显示，也未校验 producer 实际写出的 `due_at` 值。

## 决定

- `ToolScheduleResult` 列表行按 `due_at` 读取和声明时间字段。
- builtin renderer validator 对列表行的可选 `due_at` 执行字符串形状校验。
- `schedule.set` root `fires_at` 保持不变，列表和单项操作保留各自 producer 字段名。
- malformed list row 继续回退 JsonView 并保留原值。

## 影响与验证

仅修复 UI 对已有动态 ToolResult 字段的消费，不改变 Rust producer、wire、持久化或调度语义。测试覆盖列表 `due_at` 的呈现与非字符串值的 JSON fallback；UI 类型检查和全量测试通过。

## 回滚

回滚 UI 列表读取/校验改动即可；没有数据或契约迁移。
