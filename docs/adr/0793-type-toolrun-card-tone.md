# ADR 0793：收窄 ToolRun card tone

## 状态

已采纳并实施（2026-10-08）。

## 背景

`projectToolRunCard` 按 generated `ToolRunStatus` 将 background/scheduled 状态映射为 `success`、`error`、`running`、`scheduled` 或 `neutral`。该 projection 被 ToolRun center 与时间线 renderer 消费，tone 直接写入 `data-tone` / `data-variant`，但类型曾是开放 `string`。

## 决定

将唯一 tone vocabulary 命名为 `ToolRunCardTone`，由 `ToolRunCardProjection.tone` 及两个状态映射函数共同使用。保持现有 status-label、tone 映射和 CSS selectors 不变。

## 影响与回滚

仅收窄 UI 内部 projection，不改变 IPC、事件或持久化。未来增加 tone 时需显式扩展类型并更新 renderer 样式；回滚可恢复 `tone: string`。

## 验收

UI 类型检查与全量测试；projection 测试覆盖 running、failed、cancelled、waiting 与缺省状态的 tone 映射。无 IPC、配置或持久化变更。
