# ADR 0976：按受限状态命名 ToolRun 数量上限

## 状态

已采纳并实施。

## 背景

`scheduled_tool_runs_max` 实际只限制尚未触发、处于 `Waiting` 状态的定时 ToolRun；`background_max_tool_runs` 实际只限制处于 `Running` 状态的后台 ToolRun。旧名把实体类型、运行方式和受限状态混在一起，且“并发/数量”文案不能说明终态历史是否占用配额。

## 决定

1. 配置字段和 ToolRunService 内部字段分别改为 `max_pending_scheduled_tool_runs` 与 `max_running_background_tool_runs`，UI 标签说明对应状态。
2. 等待中的定时任务和运行中的后台任务仍按原规则计数；默认值保持 32 和 64，拒绝门槛不变。
3. 旧 TOML key 不添加 alias。需保留配置时，将 `[context_limits].scheduled_tool_runs_max` 改为 `max_pending_scheduled_tool_runs`，将 `background_max_tool_runs` 改为 `max_running_background_tool_runs`；数据库不变。

## 替代方案

- 只修正文案并保留字段名：拒绝；配置 key 与实现变量仍无法说明配额覆盖的状态。
- 把上限改为所有 scheduled/background ToolRun 总数：拒绝；这会改变 admission 行为，并让终态历史或已触发运行占用不相关的配额。

## 影响与验证

- 配置 DTO、Settings IPC contract、设置页与 ToolRunService 使用同一组状态语义名称。
- 配置测试覆盖新 key 解析和两个旧 key 拒绝；默认值、计数逻辑及错误门槛保持原样。
- 不改数据库、ToolRun 状态机或持久化格式。

## 回滚

恢复旧配置字段并同步回退 Settings contract、UI、测试、发布说明与 ADR。使用新 key 的配置需在回滚前改回 `scheduled_tool_runs_max` / `background_max_tool_runs`；数据库无需处理。
