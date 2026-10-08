# ADR 0779：Media 与 Window renderer 复用闭合输出值

## 状态

已采纳并实施（2026-10-08）。

## 背景

Media renderer 的 operation 来自闭合 `MediaOperation`，但 FilesTool 处理受管媒体时会将结果根字段重写为 Files 的 `read` 或 `summary` operation，再由 renderer registry 根据嵌套 `media` 选择 Media 组件。Window renderer 的 operation 来自 `WindowOperation`；`wait.condition` 来自 `WaitCondition`；截图格式由 Windows 截图 producer 固定为 `png`；UI tree 的 `control_type` 由 UI Automation 控件 ID 映射到有限名称，未知 ID 映射为 `Unknown`。这些 UI props 与 nested guards 之前接受任意字符串。

## 决定

1. `ToolMediaOperation` 覆盖全部 `MediaOperation` 与 Files wrapper 的 `read` / `summary` 输出；Media props 和 guard 共用此类型。
2. `ToolWindowOperation`、`ToolWindowWaitCondition`、`ToolWindowFormat` 和 `ToolWindowControlType` 分别表达 window producer 的闭合值域；`control_type` 集合包含运行时未知控件对应的 `Unknown`。
3. 未知 operation、condition、format 或控件类型回退通用 JSON renderer，原 payload 仍可见。Media/Window content 与窗口标题、元素名称继续保持动态字符串/JSON 输入。

## 影响与回滚

只收紧 UI builtin renderer props 与 nested validation，不改 Media/Files/Window ToolResult JSON、IPC、数据库或副作用。合法 Files read/summary media handoff 保持专用 Media renderer。移除 presentation tuples/guards 并恢复开放 props 可回滚。

## 验收

UI contract tests 覆盖 Files 包装后的 media `read` operation、合法 UIA `Unknown`、wait condition，以及未知 operation/condition/format/control type 的 JSON fallback；执行 Svelte type check、UI 全量测试与 ADR 索引检查。
