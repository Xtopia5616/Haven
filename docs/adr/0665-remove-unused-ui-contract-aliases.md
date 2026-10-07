# ADR 0665：清理未消费的 UI contracts 类型

## 背景

对 `ui/src` 的生产 TypeScript/Svelte 文件按导出类型声明及引用次数扫描，发现 12 个类型 alias 全仓没有消费者：`BuiltinEnabledFilter`、`HistorySearchRequest`、`HistorySearchPageRequest`、`SessionEventName`，以及 `ToolManifestIdentityWire`、`ToolModelWire`、`ToolPolicyWire`、`ToolPresentationWire`、`ToolRootPresentationWire`、`ToolPromptWire`、`ToolAvailabilityWire`、`ToolManifestWire`。`SessionEventName` 来源的 `SESSION_EVENT_NAMES` 单项数组同样无消费者。Tool wire alias 只重导出 generated 类型；其余也没有独立形状或 runtime 用途。

## 决定

- 删除上述 12 个无消费者 alias 及未使用的 `SESSION_EVENT_NAMES`。
- 删除仅用于这些 alias 的 generated type imports。
- 保留 generated command/event DTO、实际 Session event listener、manifest renderer view 和仍有消费者的 contracts façade。
- 无 runtime、IPC wire、配置或持久化行为变化，无需重置。

## 验证

- `corepack pnpm run check`
- `git diff --check`
- `scripts/check-adr-index.ps1`

## 回滚与重置

回滚只需恢复 UI 类型导出与未使用的事件名单；不涉及用户数据或缓存。
