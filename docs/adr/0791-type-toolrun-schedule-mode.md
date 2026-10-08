# ADR 0791：类型化 ToolRun 的定时模式

## 状态

已采纳并实施（2026-10-08）。

## 背景

定时 ToolRun 的 domain owner `ScheduleMode` 只允许 `tool` / `continue`，但 service view、lifecycle payload、App `ToolRunEvent` 与前端 `ToolRunPayload` 将该字段逐层放宽为字符串。UI mapper 因此接受任意值，ToolRun 时间线还能把未知 mode 原样显示。持久 `tool_runs.mode` 是文本列，历史读取需要在 App 边界解析后才能进入稳定 UI contract。

## 决定

1. Tools 的 scheduled view、ToolRun board view 与 lifecycle payload 使用 `ScheduleMode`。
2. App `ToolRunEvent.mode` 使用同一枚举，自动生成 TypeScript `ScheduleMode` 和值列表；序列化字符串仍为 `tool` / `continue`。
3. 持久历史模式由 `ScheduleMode::parse` 解析；无效字符串不进入 UI event。前端 ToolRun mapper 复用 generated enum 并拒绝其它值；ToolResult schedule renderer 也复用生成值域，不再维护重复 tuple。

## 影响与回滚

IPC JSON 值及数据库列格式不变。该改动只收紧内部 projection 和 renderer-facing 类型；如未来增加模式，扩展 Tools enum 后由 IPC generator 同步 UI contract。

## 验收

Rust 序列化测试保持 wire 值不变并覆盖无效历史 mode，UI contract 测试覆盖合法值、未知值与 null；运行 IPC contract/event checks、Rust workspace 门禁和 UI check/test。
