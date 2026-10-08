# ADR 0780：ToolRuns 与 Schedule renderer 复用闭合 operation

## 状态

已采纳并实施（2026-10-08）。

## 背景

ToolRunsTool 输出 `list`、`inspect`、`cancel`；UI 的 ToolRun 回灌路径另外生成 `result_injected` 或 `tool_runs_result_injected`。现有组件也接受上述 operation 的 `tool_runs_` 限定形式。ScheduleTool 只输出 `set`、`list`、`cancel`，UI 同时接受 `schedule_` 限定形式。Props 和 nested guards 之前将这些 discriminator 放宽成任意字符串。ToolRuns badge tone 也通过开放字符串子串猜测状态，尽管 renderer root 状态已由 `ToolRunStatus | 'not_found'` 限定。

## 决定

1. `ToolRunsResultOperation` 枚举 root 与回灌 producer 值及当前支持的限定形式；`normalizeToolRunsOperation` 映射到 `list`、`inspect`、`cancel`、`result_injected`。
2. `ToolScheduleResultOperation` 覆盖 `ScheduleOperation` 的输出及现有 `schedule_` 限定形式，并归一到三种 canonical operation。
3. Props 与 runtime shape guard 共用闭合值域；未知 operation 回退通用 JSON renderer。ToolRuns 状态 tone 使用 exhaustively typed `ToolRunResultStatus` 映射，包括 query-only `not_found`。

## 影响与回滚

只收紧 UI ToolResult presentation，不改 Rust ToolRun/Schedule 输出、event、IPC、数据库或调度行为。现有限定 operation payload 与回灌结果继续专用渲染；恢复开放字符串并移除 normalization 即可回滚。

## 验收

UI contract tests 覆盖 ToolRuns result-injected、限定 operation、Schedule operation 及未知 operation fallback；执行 Svelte type check、UI 全量测试与 ADR 索引检查。
