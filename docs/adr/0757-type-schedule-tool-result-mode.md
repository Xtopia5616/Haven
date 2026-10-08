# ADR 0757：Schedule ToolResult 复用闭合执行模式

## 状态

已采纳并实施。

## 背景

Schedule set 与 pending-list ToolResult 都从 Rust `ScheduleMode` 输出 `tool` / `continue`。list row 的 `tool_run_id`、`title`、`body`、`mode` 与 `due_at` 都来自必填 `ScheduledToolRunView` 字段；恢复时无效持久 mode 会被拒绝/隔离。UI 的 `ToolScheduleResult` props 与 guard 却把 mode 当作任意字符串，并允许 list row 缺少 renderer 所消费的必填字段。guard 还检查了 root `title` / `body`，但组件不读取这些字段。

## 决定

- `toolResultPresentation.ts` 定义 `ToolScheduleMode` tuple、派生类型和值守卫；Schedule props 与 nested/root mode validation 共用该值源。
- `scheduled_tool_runs[]` 的专用 renderer row 必须包含字符串 `tool_run_id`、`title`、`body`、`due_at` 及已知 `mode`；其它动态字段（例如 `tool_args`）不参与 renderer gate。
- set root 的可选 `mode` 也按同一闭合集合校验；移除对不被组件读取的 root `title` / `body` 字段校验。共享 `scheduleModeLabel(unknown)` 继续服务其它开放 ToolRun view 边界。

## 替代方案

继续用开放字符串会让未知模式显示成状态标签；为 `ToolResult.output` 建全局 DTO 会越过逐工具动态 JSON 的边界。将通用 ToolRun label helper 一并收窄会混淆持久 IPC view 与 Schedule builtin ToolResult 的不同 owner。

## 影响与验证

只收紧 UI Schedule 专用 renderer props 与 guard，不改 schedule producer、ToolResult wire、IPC 或持久化。测试覆盖无效 root/row mode 回退、必需 row 字段和未消费动态字段不导致误降级；UI check 与 test:run 通过。

## 回滚

恢复开放 mode props/guard 与可选 row 字段校验即可。没有 IPC、数据或持久化迁移。
