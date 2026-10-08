# ADR 0775：ToolRun inspect root status 使用查询结果 union

## 状态

已采纳并实施（2026-10-08）。

## 背景

ToolRuns 列表行的 `status` 是生成 `ToolRunStatus`；单项 inspect 的 producer 复用 lifecycle status，并在找不到或不属于当前 session 时返回 `not_found`。此前 renderer root status 和 nested validator 均使用开放字符串，因此未知值也会进入状态 badge。ADR 0746 已记录 root 不能直接收窄为单独的 `ToolRunStatus`。

## 决定

1. renderer root status 使用 `ToolRunStatus | 'not_found'`，保留可选/null 输入。
2. UI guard 通过生成 `isToolRunStatus` 加 `not_found` 校验，列表行继续只允许 `ToolRunStatus`。
3. 未知动态状态回退通用 JsonView；有效的 `not_found` 仍显示查询结果标签。

## 验收与回滚

UI type check 验证 renderer prop；ToolResultCard contract tests 覆盖未知状态 fallback 与 `not_found` 专用展示。Rust producer/wire 不变；回滚 presentation union、validator 与 prop 类型可恢复开放 root status。
