# ADR 1068：按配置 horizon 限制定时任务

## 状态

已接受（2026-10-13）

## 背景

定时 ToolRun 的绝对 `due_at` 仍使用固定的一年上限；`delay_secs` 则没有经过同一个配置 horizon 检查。Settings 已提供可调整的最远排期，造成界面值和执行行为不一致。绝对时间文档只说带 timezone，没有明确要求显式偏移。

## 决定

- `due_at` 和 `delay_secs` 都按 `context_limits.scheduled_tool_runs_due_horizon_secs` 校验；允许范围为 0 至 10 年，边界由 Common、Settings 和运行时共同执行。
- horizon 为 0 时拒绝所有未来的计时触发。后台任务直接启动和 `watch_tool_run_id` 依赖触发不使用该计时 horizon。
- `due_at` 必须是带显式 timezone offset 的 RFC3339 时间。时长比较保留亚秒精度；Timer 实际等待秒数仍使用既有整数秒语义。
- 错误信息报告实际配置的 horizon 秒数，不再写死“一年”。

## 影响

较旧配置中的排期 horizon 超过 10 年时按配置加载策略备份并回退默认值；Settings 更新拒绝越界值。配置 key、ToolRun schema、IPC 和数据库均不变，无需重置数据。

## 验证

- Policy 单测覆盖绝对/相对时间、0/短/最大 horizon、亚秒超界、依赖触发例外和 timezone 缺失。
- schedule Tool schema 测试确认所有 `due_at` 描述均要求显式偏移。
- 通过 Tools crate 的目标 check、Clippy、测试；本轮结束后另跑全量门禁。
